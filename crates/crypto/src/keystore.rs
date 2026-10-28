//! Almacén de claves: claves OpenPGP propias (cifradas en disco con su contraseña), claves de
//! contactos (Autocrypt, WKD, importadas), identidades S/MIME propias (`.p12` cifrado) y
//! certificados de contactos.
//!
//! En disco solo hay material protegido: la clave secreta OpenPGP va cifrada con S2K (rPGP) y el
//! `.p12` con su propia contraseña. Las contraseñas viven en el llavero del sistema (rasgo
//! [`Vault`]; la app lo implementa con `keyring`).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use chrono::{DateTime, Utc};
use pgp::composed::{SignedPublicKey, SignedSecretKey};
use rand::RngCore;
use serde::{Deserialize, Serialize};

use crate::autocrypt::PreferEncrypt;
use crate::error::{CryptoError, Result};
use crate::{openpgp, smime};

/// Máximo de contactos que se recuerdan.
const MAX_PEERS: usize = 20_000;
/// Nombre del archivo del almacén.
const FILE: &str = "keys.json";

/// Dónde se guardan las contraseñas de las claves (llavero del sistema).
pub trait Vault: Send + Sync {
    fn get(&self, name: &str) -> Result<Option<String>>;
    fn set(&self, name: &str, secret: &str) -> Result<()>;
    fn delete(&self, name: &str) -> Result<()>;
}

/// Llavero en memoria (pruebas; sesiones sin llavero).
#[derive(Debug, Default)]
pub struct MemoryVault(Mutex<HashMap<String, String>>);

impl MemoryVault {
    fn lock(&self) -> MutexGuard<'_, HashMap<String, String>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Vault for MemoryVault {
    fn get(&self, name: &str) -> Result<Option<String>> {
        Ok(self.lock().get(name).cloned())
    }
    fn set(&self, name: &str, secret: &str) -> Result<()> {
        self.lock().insert(name.to_string(), secret.to_string());
        Ok(())
    }
    fn delete(&self, name: &str) -> Result<()> {
        self.lock().remove(name);
        Ok(())
    }
}

/// De dónde salió la clave de un contacto.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeySource {
    Autocrypt,
    Wkd,
    Imported,
}

/// Clave OpenPGP de un contacto.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Peer {
    pub addr: String,
    pub fingerprint: String,
    /// Clave pública en armadura.
    pub key: String,
    pub source: KeySource,
    pub prefer_encrypt: PreferEncrypt,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct OwnPgp {
    fingerprint: String,
    addrs: Vec<String>,
    /// Clave secreta en armadura, protegida con la contraseña del llavero.
    secret: String,
    created: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct OwnSmime {
    fingerprint: String,
    addrs: Vec<String>,
    subject: String,
    /// `.p12` en base64 (cifrado con su contraseña, que va al llavero).
    p12: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SmimePeer {
    cert: String,
    last_seen: DateTime<Utc>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct State {
    #[serde(default)]
    own_pgp: Vec<OwnPgp>,
    #[serde(default)]
    peers: BTreeMap<String, Peer>,
    #[serde(default)]
    own_smime: Vec<OwnSmime>,
    #[serde(default)]
    smime_peers: BTreeMap<String, SmimePeer>,
}

/// Resumen de una clave o identidad propia (para Ajustes).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnKeyInfo {
    pub technology: crate::report::Technology,
    pub fingerprint: String,
    pub addrs: Vec<String>,
    pub label: String,
}

/// Qué pasó al aprender una clave.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Learned {
    /// No se conocía ninguna para esa dirección.
    New,
    /// Es la misma que ya se tenía.
    Same,
    /// Es distinta de la conocida (se reemplazó si la anterior venía de Autocrypt).
    Changed,
}

/// El almacén.
pub struct KeyStore {
    path: Option<PathBuf>,
    vault: Arc<dyn Vault>,
    state: Mutex<State>,
}

impl std::fmt::Debug for KeyStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyStore").field("path", &self.path).finish_non_exhaustive()
    }
}

fn norm(addr: &str) -> String {
    addr.trim().trim_matches(['<', '>']).to_ascii_lowercase()
}

fn random_passphrase() -> String {
    let mut b = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut b);
    STANDARD.encode(b)
}

fn pgp_secret_name(fpr: &str) -> String {
    format!("pgp-{fpr}")
}

fn smime_secret_name(fpr: &str) -> String {
    format!("smime-{}", fpr.get(..32).unwrap_or(fpr))
}

impl KeyStore {
    /// Abre (o crea) el almacén en `dir`.
    pub fn open(dir: &Path, vault: Arc<dyn Vault>) -> Result<Self> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join(FILE);
        let state = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => State::default(),
            Err(e) => return Err(e.into()),
        };
        Ok(KeyStore { path: Some(path), vault, state: Mutex::new(state) })
    }

    /// Almacén solo en memoria.
    pub fn in_memory(vault: Arc<dyn Vault>) -> Self {
        KeyStore { path: None, vault, state: Mutex::new(State::default()) }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn save(&self, state: &State) -> Result<()> {
        let Some(path) = &self.path else { return Ok(()) };
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(state)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    fn passphrase(&self, name: &str) -> Result<String> {
        self.vault.get(name)?.ok_or_else(|| CryptoError::Vault("falta la contraseña de la clave en el llavero".into()))
    }

    // --- OpenPGP propio ------------------------------------------------------------------------

    /// Crea una clave para `name <email>`. La contraseña (aleatoria) va al llavero.
    pub fn generate_pgp(&self, name: &str, email: &str) -> Result<String> {
        let pw = random_passphrase();
        let key = openpgp::generate(name, &norm(email), &pw)?;
        self.add_own_pgp(&key, &pw)
    }

    /// Importa una clave secreta (armadura o binaria) con su contraseña.
    pub fn import_pgp_secret(&self, data: &[u8], passphrase: &str) -> Result<String> {
        let key = openpgp::parse_secret(data)?;
        openpgp::check_passphrase(&key, passphrase)?;
        self.add_own_pgp(&key, passphrase)
    }

    fn add_own_pgp(&self, key: &SignedSecretKey, pw: &str) -> Result<String> {
        let fpr = openpgp::secret_fingerprint(key);
        let addrs = openpgp::emails(&key.to_public_key());
        if addrs.is_empty() {
            return Err(CryptoError::BadKey("la clave no tiene ninguna dirección de correo".into()));
        }
        self.vault.set(&pgp_secret_name(&fpr), pw)?;
        let mut st = self.lock();
        st.own_pgp.retain(|k| k.fingerprint != fpr);
        st.own_pgp.push(OwnPgp {
            fingerprint: fpr.clone(),
            addrs,
            secret: openpgp::armor_secret(key)?,
            created: Utc::now(),
        });
        self.save(&st)?;
        Ok(fpr)
    }

    /// Borra una clave o identidad propia (y su contraseña del llavero).
    pub fn remove_own(&self, fingerprint: &str) -> Result<()> {
        let mut st = self.lock();
        st.own_pgp.retain(|k| k.fingerprint != fingerprint);
        st.own_smime.retain(|k| k.fingerprint != fingerprint);
        self.save(&st)?;
        drop(st);
        self.vault.delete(&pgp_secret_name(fingerprint))?;
        self.vault.delete(&smime_secret_name(fingerprint))
    }

    /// Claves e identidades propias.
    pub fn own_keys(&self) -> Vec<OwnKeyInfo> {
        let st = self.lock();
        let mut out: Vec<OwnKeyInfo> = st
            .own_pgp
            .iter()
            .map(|k| OwnKeyInfo {
                technology: crate::report::Technology::OpenPgp,
                fingerprint: k.fingerprint.clone(),
                addrs: k.addrs.clone(),
                label: k.addrs.join(", "),
            })
            .collect();
        out.extend(st.own_smime.iter().map(|k| OwnKeyInfo {
            technology: crate::report::Technology::Smime,
            fingerprint: k.fingerprint.clone(),
            addrs: k.addrs.clone(),
            label: k.subject.clone(),
        }));
        out
    }

    /// Clave secreta propia para `addr` y su contraseña.
    pub fn own_pgp(&self, addr: &str) -> Option<(SignedSecretKey, String)> {
        let a = norm(addr);
        let armored = self.lock().own_pgp.iter().rev().find(|k| k.addrs.contains(&a)).map(|k| k.secret.clone())?;
        let key = openpgp::parse_secret(armored.as_bytes()).ok()?;
        let pw = self.passphrase(&pgp_secret_name(&openpgp::secret_fingerprint(&key))).ok()?;
        Some((key, pw))
    }

    /// Todas las claves secretas propias (para descifrar).
    pub fn all_own_pgp(&self) -> Vec<(SignedSecretKey, String)> {
        let armored: Vec<String> = self.lock().own_pgp.iter().map(|k| k.secret.clone()).collect();
        armored
            .iter()
            .filter_map(|a| openpgp::parse_secret(a.as_bytes()).ok())
            .filter_map(|k| {
                let pw = self.passphrase(&pgp_secret_name(&openpgp::secret_fingerprint(&k))).ok()?;
                Some((k, pw))
            })
            .collect()
    }

    /// Clave pública propia de `addr`, en armadura (exportar / publicar).
    pub fn export_pgp_public(&self, addr: &str) -> Result<String> {
        let (k, _) = self.own_pgp(addr).ok_or_else(|| CryptoError::NoOwnKey(norm(addr)))?;
        openpgp::armor_public(&k.to_public_key())
    }

    /// Clave secreta propia de `addr` (protegida) y su contraseña, para copia de seguridad.
    pub fn export_pgp_secret(&self, addr: &str) -> Result<(String, String)> {
        let (k, pw) = self.own_pgp(addr).ok_or_else(|| CryptoError::NoOwnKey(norm(addr)))?;
        Ok((openpgp::armor_secret(&k)?, pw))
    }

    // --- Contactos OpenPGP ----------------------------------------------------------------------

    /// Importa claves públicas (una o varias en un archivo). Devuelve las direcciones.
    pub fn import_public(&self, data: &[u8]) -> Result<Vec<String>> {
        let key = openpgp::parse_public(data)?;
        let addrs = openpgp::emails(&key);
        for a in &addrs {
            self.learn(a, &key, KeySource::Imported, PreferEncrypt::NoPreference, Utc::now())?;
        }
        Ok(addrs)
    }

    /// Recuerda la clave de `addr`. Una clave importada o de WKD no la reemplaza Autocrypt.
    pub fn learn(
        &self,
        addr: &str,
        key: &SignedPublicKey,
        source: KeySource,
        prefer: PreferEncrypt,
        when: DateTime<Utc>,
    ) -> Result<Learned> {
        let a = norm(addr);
        let fpr = openpgp::fingerprint(key);
        let mut st = self.lock();
        let outcome = match st.peers.get_mut(&a) {
            Some(p) if p.fingerprint == fpr => {
                if when > p.last_seen {
                    p.last_seen = when;
                    p.prefer_encrypt = prefer;
                }
                if source == KeySource::Imported {
                    p.source = KeySource::Imported;
                }
                Learned::Same
            }
            Some(p) => {
                let replace =
                    source != KeySource::Autocrypt || (p.source == KeySource::Autocrypt && when >= p.last_seen);
                if replace {
                    *p = Peer {
                        addr: a.clone(),
                        fingerprint: fpr,
                        key: openpgp::armor_public(key)?,
                        source,
                        prefer_encrypt: prefer,
                        first_seen: when,
                        last_seen: when,
                    };
                }
                Learned::Changed
            }
            None => {
                if st.peers.len() >= MAX_PEERS {
                    return Err(CryptoError::TooLarge(st.peers.len()));
                }
                st.peers.insert(
                    a.clone(),
                    Peer {
                        addr: a,
                        fingerprint: fpr,
                        key: openpgp::armor_public(key)?,
                        source,
                        prefer_encrypt: prefer,
                        first_seen: when,
                        last_seen: when,
                    },
                );
                Learned::New
            }
        };
        if outcome != Learned::Same || source == KeySource::Imported {
            self.save(&st)?;
        }
        Ok(outcome)
    }

    /// Contacto conocido.
    pub fn peer(&self, addr: &str) -> Option<Peer> {
        self.lock().peers.get(&norm(addr)).cloned()
    }

    /// Clave pública de `addr`: la de un contacto o, si es una dirección propia, la propia.
    pub fn public_key_for(&self, addr: &str) -> Option<SignedPublicKey> {
        if let Some(p) = self.peer(addr) {
            return openpgp::parse_public(p.key.as_bytes()).ok();
        }
        let a = norm(addr);
        let armored = self.lock().own_pgp.iter().rev().find(|k| k.addrs.contains(&a)).map(|k| k.secret.clone())?;
        openpgp::parse_secret(armored.as_bytes()).ok().map(|k| k.to_public_key())
    }

    /// Todas las claves públicas conocidas (contactos y propias), para buscar al firmante.
    pub fn all_public(&self) -> Vec<SignedPublicKey> {
        let (peers, own): (Vec<String>, Vec<String>) = {
            let st = self.lock();
            (st.peers.values().map(|p| p.key.clone()).collect(), st.own_pgp.iter().map(|k| k.secret.clone()).collect())
        };
        let mut out: Vec<SignedPublicKey> =
            peers.iter().filter_map(|k| openpgp::parse_public(k.as_bytes()).ok()).collect();
        out.extend(own.iter().filter_map(|k| openpgp::parse_secret(k.as_bytes()).ok()).map(|k| k.to_public_key()));
        out
    }

    /// Lista de contactos con clave.
    pub fn peers(&self) -> Vec<Peer> {
        self.lock().peers.values().cloned().collect()
    }

    /// Olvida la clave de un contacto.
    pub fn forget_peer(&self, addr: &str) -> Result<()> {
        let mut st = self.lock();
        st.peers.remove(&norm(addr));
        st.smime_peers.remove(&norm(addr));
        self.save(&st)
    }

    // --- S/MIME -------------------------------------------------------------------------------

    /// Importa un `.p12` con su contraseña (comprueba que abre).
    pub fn import_p12(&self, data: &[u8], password: &str) -> Result<OwnKeyInfo> {
        let id = smime::Identity::from_p12(data, password)?;
        let der = id.cert_der()?;
        let fpr = smime::cert_fingerprint(&der);
        let addrs = id.emails();
        if addrs.is_empty() {
            return Err(CryptoError::BadKey("el certificado no tiene ninguna dirección de correo".into()));
        }
        let subject = smime::subject_label(&id.cert);
        self.vault.set(&smime_secret_name(&fpr), password)?;
        let mut st = self.lock();
        st.own_smime.retain(|k| k.fingerprint != fpr);
        st.own_smime.push(OwnSmime {
            fingerprint: fpr.clone(),
            addrs: addrs.clone(),
            subject: subject.clone(),
            p12: STANDARD.encode(data),
        });
        self.save(&st)?;
        Ok(OwnKeyInfo { technology: crate::report::Technology::Smime, fingerprint: fpr, addrs, label: subject })
    }

    /// Identidad S/MIME propia de `addr`.
    pub fn own_smime(&self, addr: &str) -> Option<smime::Identity> {
        let a = norm(addr);
        let (fpr, p12) = self
            .lock()
            .own_smime
            .iter()
            .rev()
            .find(|k| k.addrs.contains(&a))
            .map(|k| (k.fingerprint.clone(), k.p12.clone()))?;
        let pw = self.passphrase(&smime_secret_name(&fpr)).ok()?;
        smime::Identity::from_p12(&STANDARD.decode(p12).ok()?, &pw).ok()
    }

    /// Todas las identidades S/MIME propias (para descifrar).
    pub fn all_own_smime(&self) -> Vec<smime::Identity> {
        let items: Vec<(String, String)> =
            self.lock().own_smime.iter().map(|k| (k.fingerprint.clone(), k.p12.clone())).collect();
        items
            .into_iter()
            .filter_map(|(fpr, p12)| {
                let pw = self.passphrase(&smime_secret_name(&fpr)).ok()?;
                smime::Identity::from_p12(&STANDARD.decode(p12).ok()?, &pw).ok()
            })
            .collect()
    }

    /// Recuerda el certificado de un contacto (de un mensaje firmado verificado).
    pub fn learn_smime(&self, addr: &str, cert_der: &[u8]) -> Result<()> {
        let mut st = self.lock();
        if st.smime_peers.len() >= MAX_PEERS {
            return Ok(());
        }
        st.smime_peers.insert(norm(addr), SmimePeer { cert: STANDARD.encode(cert_der), last_seen: Utc::now() });
        self.save(&st)
    }

    /// Certificado (DER) de un contacto o propio.
    pub fn smime_cert_for(&self, addr: &str) -> Option<Vec<u8>> {
        let a = norm(addr);
        if let Some(p) = self.lock().smime_peers.get(&a) {
            return STANDARD.decode(&p.cert).ok();
        }
        self.own_smime(&a).and_then(|id| id.cert_der().ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_keys_and_peers_persist() {
        let dir = tempfile::tempdir().unwrap();
        let vault: Arc<dyn Vault> = Arc::new(MemoryVault::default());
        let ks = KeyStore::open(dir.path(), Arc::clone(&vault)).unwrap();
        let fpr = ks.generate_pgp("Ana", "Ana@Example.com").unwrap();
        assert!(ks.own_pgp("ana@example.com").is_some());
        // En disco la clave secreta va protegida: sin la contraseña no firma.
        let text = std::fs::read_to_string(dir.path().join(FILE)).unwrap();
        assert!(text.contains("PRIVATE KEY") && !text.contains(&vault.get(&pgp_secret_name(&fpr)).unwrap().unwrap()));
        let (k, _) = ks.own_pgp("ana@example.com").unwrap();
        assert!(openpgp::check_passphrase(&k, "otra").is_err());

        // Contactos.
        let bob = openpgp::generate("Bob", "bob@x.org", "b").unwrap().to_public_key();
        let bob2 = openpgp::generate("Bob", "bob@x.org", "b").unwrap().to_public_key();
        let t0 = Utc::now();
        assert_eq!(ks.learn("bob@x.org", &bob, KeySource::Autocrypt, PreferEncrypt::Mutual, t0).unwrap(), Learned::New);
        assert_eq!(
            ks.learn("BOB@x.org", &bob, KeySource::Autocrypt, PreferEncrypt::Mutual, t0).unwrap(),
            Learned::Same
        );
        assert_eq!(
            ks.learn("bob@x.org", &bob2, KeySource::Autocrypt, PreferEncrypt::Mutual, t0).unwrap(),
            Learned::Changed
        );
        assert_eq!(ks.peer("bob@x.org").unwrap().fingerprint, openpgp::fingerprint(&bob2));

        // Reabrir: todo sigue ahí.
        drop(ks);
        let ks = KeyStore::open(dir.path(), vault).unwrap();
        assert_eq!(ks.own_keys().len(), 1);
        assert!(ks.public_key_for("bob@x.org").is_some());
        assert!(ks.public_key_for("ana@example.com").is_some());
        assert_eq!(ks.all_public().len(), 2);
        let armored = ks.export_pgp_public("ana@example.com").unwrap();
        assert!(armored.contains("PUBLIC KEY"));
        ks.remove_own(&fpr).unwrap();
        assert!(ks.own_pgp("ana@example.com").is_none());
    }

    #[test]
    fn imported_key_is_not_replaced_by_autocrypt() {
        let ks = KeyStore::in_memory(Arc::new(MemoryVault::default()));
        let k1 = openpgp::generate("C", "c@x.org", "p").unwrap().to_public_key();
        let k2 = openpgp::generate("C", "c@x.org", "p").unwrap().to_public_key();
        assert_eq!(ks.import_public(openpgp::armor_public(&k1).unwrap().as_bytes()).unwrap(), vec!["c@x.org"]);
        assert_eq!(
            ks.learn("c@x.org", &k2, KeySource::Autocrypt, PreferEncrypt::Mutual, Utc::now()).unwrap(),
            Learned::Changed
        );
        assert_eq!(ks.peer("c@x.org").unwrap().fingerprint, openpgp::fingerprint(&k1));
    }
}
