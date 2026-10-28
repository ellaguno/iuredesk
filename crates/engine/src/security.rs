//! Correo seguro en el motor (F5.4): une `iuredesk-crypto` con la sesión.
//!
//! - Un único [`Crypto`] por proceso ([`install`]); el lector de cuerpos ([`crate::body::parse`])
//!   lo usa para descifrar/verificar y el envío ([`Sealer`]) para firmar/cifrar.
//! - Las contraseñas de las claves van al llavero del sistema a través del [`SecretStore`] de la
//!   sesión (propósito `crypto-key`).

use std::path::Path;
use std::sync::{Arc, RwLock};

use iuredesk_accounts::{SecretPurpose, SecretStore};
use iuredesk_compose::secure::{MessageSealer, SecurityRequest};
use iuredesk_crypto::smime::TrustStore;
use iuredesk_crypto::{Capabilities, Crypto, CryptoError, MemoryVault, Opened, SealRequest, Vault};

pub use iuredesk_crypto::{SecurityReport, SignatureState, Technology, Tone};

static CRYPTO: RwLock<Option<Arc<Crypto>>> = RwLock::new(None);

/// Instala el correo seguro del proceso (lo hace la sesión al arrancar).
pub fn install(c: Arc<Crypto>) {
    if let Ok(mut g) = CRYPTO.write() {
        *g = Some(c);
    }
}

/// El correo seguro del proceso, si está instalado.
pub fn get() -> Option<Arc<Crypto>> {
    CRYPTO.read().ok().and_then(|g| g.clone())
}

/// Llavero de las claves sobre el [`SecretStore`] de la sesión.
struct SecretVault(Arc<dyn SecretStore>);

fn vault_err(e: iuredesk_accounts::Error) -> CryptoError {
    CryptoError::Vault(e.to_string())
}

impl Vault for SecretVault {
    fn get(&self, name: &str) -> iuredesk_crypto::Result<Option<String>> {
        self.0.get(name, SecretPurpose::CryptoKey).map_err(vault_err)
    }
    fn set(&self, name: &str, secret: &str) -> iuredesk_crypto::Result<()> {
        self.0.set(name, SecretPurpose::CryptoKey, secret).map_err(vault_err)
    }
    fn delete(&self, name: &str) -> iuredesk_crypto::Result<()> {
        self.0.delete(name, SecretPurpose::CryptoKey).map_err(vault_err)
    }
}

/// Prepara el correo seguro: almacén en `dir` (o en memoria) y raíces del sistema.
pub fn init(dir: Option<&Path>, secrets: Arc<dyn SecretStore>) {
    // Sin carpeta (pruebas, buzón de ejemplo) no se reemplaza uno ya instalado.
    if dir.is_none() && get().is_some() {
        return;
    }
    let vault: Arc<dyn Vault> = Arc::new(SecretVault(secrets));
    let crypto = match dir {
        Some(d) => Crypto::open_dir(d, Arc::clone(&vault))
            .unwrap_or_else(|_| Crypto::in_memory(Arc::new(MemoryVault::default()), TrustStore::system())),
        None => Crypto::in_memory(vault, TrustStore::empty()),
    };
    install(Arc::new(crypto));
}

/// Abre un mensaje (descifra/verifica). Sin correo seguro instalado, lo deja igual.
pub fn open(raw: &[u8]) -> Opened {
    match get() {
        Some(c) => c.open(raw),
        None => Opened { display: raw.to_vec(), report: None },
    }
}

/// Qué se puede hacer desde `from` hacia `recipients`.
pub fn capabilities(from: &str, recipients: &[String]) -> Capabilities {
    get().map(|c| c.capabilities(from, recipients)).unwrap_or_default()
}

/// Firma / cifra al enviar.
pub struct Sealer;

impl MessageSealer for Sealer {
    fn seal(&self, raw: &[u8], recipients: &[String], req: SecurityRequest) -> Result<Vec<u8>, (String, String)> {
        let Some(c) = get() else {
            if req.sign || req.encrypt {
                return Err(("El correo seguro no está disponible.".into(), "Secure mail isn't available.".into()));
            }
            return Ok(raw.to_vec());
        };
        c.seal(raw, recipients, SealRequest { sign: req.sign, encrypt: req.encrypt, prefer_smime: false })
            .map_err(|e| (e.message(true), e.message(false)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealer_and_open_round_trip() {
        let secrets: Arc<dyn SecretStore> = Arc::new(iuredesk_accounts::MemoryStore::default());
        init(None, secrets);
        let c = get().unwrap();
        c.keys.generate_pgp("Ana", "ana-sealtest@x.mx").unwrap();
        let raw = b"From: ana-sealtest@x.mx\r\nTo: ana-sealtest@x.mx\r\nSubject: s\r\nContent-Type: text/plain\r\n\r\nhola\r\n";
        let sealed =
            Sealer.seal(raw, &["ana-sealtest@x.mx".into()], SecurityRequest { sign: true, encrypt: true }).unwrap();
        assert!(!String::from_utf8_lossy(&sealed).contains("hola"));
        let body = crate::body::parse(&sealed);
        assert!(body.text.contains("hola"));
        let r = body.security.unwrap();
        assert!(r.encrypted);
        assert_eq!(r.signature.unwrap().state, SignatureState::Verified);
        // Falta la clave de un destinatario: error humano en español.
        let e = Sealer.seal(raw, &["otro@y.mx".into()], SecurityRequest { sign: false, encrypt: true }).unwrap_err();
        assert!(e.0.contains("otro@y.mx"));
    }
}
