//! Operaciones OpenPGP con rPGP (`pgp`, Rust puro, MIT/Apache-2.0): generar, importar y exportar
//! claves; firmar y verificar firmas separadas; cifrar (y firmar dentro) y descifrar.
//!
//! Las claves nuevas son v4 Ed25519 (firma y certificación) + Curve25519 ECDH (cifrado): lo que
//! entienden GnuPG, Thunderbird, Delta Chat y K-9/Thunderbird para Android.

use std::io::Cursor;

use pgp::composed::{
    ArmorOptions, CleartextSignedMessage, Deserializable, DetachedSignature, EncryptionCaps, KeyType, Message,
    MessageBuilder, SecretKeyParamsBuilder, SignedPublicKey, SignedSecretKey, SubkeyParamsBuilder,
};
use pgp::crypto::ecc_curve::ECCCurve;
use pgp::crypto::hash::HashAlgorithm;
use pgp::crypto::sym::SymmetricKeyAlgorithm;
use pgp::packet::Signature;
use pgp::ser::Serialize;
use pgp::types::{KeyDetails, Password, VerifyingKey};

use crate::error::{CryptoError, Result, guard};

/// Tamaño máximo de una clave que se acepta (armadura incluida).
pub const MAX_KEY_BYTES: usize = 512 * 1024;
/// Tamaño máximo de un mensaje que se cifra o descifra.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;

/// Huella en hexadecimal (mayúsculas, sin espacios).
pub fn fingerprint(k: &impl KeyDetails) -> String {
    format!("{:X}", k.fingerprint())
}

/// Huella de una clave secreta.
pub fn secret_fingerprint(k: &SignedSecretKey) -> String {
    fingerprint(&k.primary_key)
}

/// Huella agrupada de 4 en 4 para mostrar («ABCD 1234 …»).
pub fn pretty_fingerprint(hex: &str) -> String {
    let chars: Vec<char> = hex.chars().collect();
    chars.chunks(4).map(|c| c.iter().collect::<String>()).collect::<Vec<_>>().join(" ")
}

/// Genera una clave nueva para `name <email>`, protegida con `passphrase`.
pub fn generate(name: &str, email: &str, passphrase: &str) -> Result<SignedSecretKey> {
    let uid = if name.trim().is_empty() { format!("<{email}>") } else { format!("{} <{email}>", name.trim()) };
    guard("generar clave", || {
        let mut enc = SubkeyParamsBuilder::default();
        enc.key_type(KeyType::ECDH(ECCCurve::Curve25519Legacy))
            .can_sign(false)
            .can_encrypt(EncryptionCaps::All)
            .can_authenticate(false)
            .passphrase(Some(passphrase.to_string()));
        let enc = enc.build().map_err(|e| CryptoError::Internal(e.to_string()))?;
        let mut params = SecretKeyParamsBuilder::default();
        params
            .key_type(KeyType::Ed25519Legacy)
            .can_certify(true)
            .can_sign(true)
            .can_encrypt(EncryptionCaps::None)
            .primary_user_id(uid)
            .passphrase(Some(passphrase.to_string()))
            .subkeys(vec![enc]);
        let params = params.build().map_err(|e| CryptoError::Internal(e.to_string()))?;
        Ok(params.generate(rand::thread_rng())?)
    })
}

fn check_size(bytes: &[u8]) -> Result<()> {
    if bytes.len() > MAX_KEY_BYTES { Err(CryptoError::TooLarge(bytes.len())) } else { Ok(()) }
}

/// Lee una clave pública (armadura o binaria) y comprueba sus autofirmas.
pub fn parse_public(bytes: &[u8]) -> Result<SignedPublicKey> {
    check_size(bytes)?;
    guard("leer clave pública", || {
        let (key, _) = SignedPublicKey::from_reader_single(Cursor::new(bytes))
            .map_err(|e| CryptoError::BadKey(e.to_string()))?;
        key.verify_bindings().map_err(|e| CryptoError::BadKey(e.to_string()))?;
        Ok(key)
    })
}

/// Lee una clave secreta (armadura o binaria).
pub fn parse_secret(bytes: &[u8]) -> Result<SignedSecretKey> {
    check_size(bytes)?;
    guard("leer clave secreta", || {
        let (key, _) = SignedSecretKey::from_reader_single(Cursor::new(bytes))
            .map_err(|e| CryptoError::BadKey(e.to_string()))?;
        key.verify_bindings().map_err(|e| CryptoError::BadKey(e.to_string()))?;
        Ok(key)
    })
}

/// Clave pública en armadura ASCII.
pub fn armor_public(k: &SignedPublicKey) -> Result<String> {
    Ok(k.to_armored_string(ArmorOptions::default())?)
}

/// Clave secreta en armadura ASCII (sigue protegida con su contraseña).
pub fn armor_secret(k: &SignedSecretKey) -> Result<String> {
    Ok(k.to_armored_string(ArmorOptions::default())?)
}

/// Clave pública en binario (para `keydata=` de Autocrypt).
pub fn binary_public(k: &SignedPublicKey) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    k.to_writer(&mut out)?;
    Ok(out)
}

/// Comprueba que `passphrase` abre la clave (firma algo de prueba).
pub fn check_passphrase(k: &SignedSecretKey, passphrase: &str) -> Result<()> {
    guard("comprobar contraseña", || {
        DetachedSignature::sign_binary_data(
            rand::thread_rng(),
            &signing_key(k),
            &Password::from(passphrase),
            HashAlgorithm::Sha256,
            &b"iuredesk"[..],
        )
        .map(|_| ())
        .map_err(|_| CryptoError::BadPassword)
    })
}

/// Direcciones de correo de los User ID de la clave (en minúsculas).
pub fn emails(k: &SignedPublicKey) -> Vec<String> {
    k.details
        .users
        .iter()
        .filter_map(|u| u.id.as_str())
        .filter_map(|s| {
            let a = match (s.rfind('<'), s.rfind('>')) {
                (Some(o), Some(c)) if c > o => s.get(o.saturating_add(1)..c)?,
                _ => s,
            };
            let a = a.trim().to_ascii_lowercase();
            a.contains('@').then_some(a)
        })
        .collect()
}

/// User ID principal («Ana <ana@x.com>»).
pub fn user_label(k: &SignedPublicKey) -> Option<String> {
    let users = &k.details.users;
    users.iter().find(|u| u.is_primary()).or_else(|| users.first()).and_then(|u| u.id.as_str()).map(str::to_string)
}

/// Clave con la que se firma: la subclave de firma si la hay; si no, la primaria.
fn signing_key(k: &SignedSecretKey) -> Box<&dyn pgp::types::SigningKey> {
    for sub in &k.secret_subkeys {
        if sub.signatures.iter().any(|s| s.key_flags().sign()) {
            return Box::new(&sub.key);
        }
    }
    Box::new(&k.primary_key)
}

/// ¿Tiene la clave alguna (sub)clave de cifrado?
pub fn can_encrypt(k: &SignedPublicKey) -> bool {
    k.public_subkeys.iter().any(|s| {
        s.key.algorithm().can_encrypt()
            && s.signatures.iter().any(|sig| sig.key_flags().encrypt_comms() || sig.key_flags().encrypt_storage())
    }) || k.primary_key.algorithm().can_encrypt()
}

/// Firma separada (armadura) de `data`.
pub fn sign_detached(k: &SignedSecretKey, passphrase: &str, data: &[u8]) -> Result<String> {
    guard("firmar", || {
        let sig = DetachedSignature::sign_binary_data(
            rand::thread_rng(),
            &signing_key(k),
            &Password::from(passphrase),
            HashAlgorithm::Sha256,
            data,
        )
        // Con una clave protegida, el fallo típico es la contraseña (rPGP lo reporta como
        // «invalid input» al no poder descifrar el material secreto).
        .map_err(|_| CryptoError::BadPassword)?;
        Ok(sig.to_armored_string(ArmorOptions::default())?)
    })
}

/// Resultado de comprobar una firma contra un conjunto de claves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verification {
    /// Firma válida con la clave `candidates[i]`.
    Good(usize),
    /// Una de las claves es la emisora pero la firma no corresponde al contenido.
    Bad(String),
    /// Ninguna clave conocida es la emisora (huella o id de la emisora, si viene).
    NoKey(Option<String>),
}

/// ¿Verifica `sig` sobre `data` con alguna (sub)clave de `k`? `Err` = la clave es la emisora pero
/// la firma no vale.
fn verify_with(sig: &Signature, k: &SignedPublicKey, data: &[u8]) -> Option<std::result::Result<(), String>> {
    let no_issuer = sig.issuer_fingerprint().is_empty() && sig.issuer_key_id().is_empty();
    let mut issuer_found = false;
    // `true` si verifica; marca `issuer_found` si la clave es la emisora.
    fn attempt<K: VerifyingKey>(sig: &Signature, key: &K, data: &[u8], no_issuer: bool, found: &mut bool) -> bool {
        let fp = key.fingerprint();
        let id = key.legacy_key_id();
        let mine =
            sig.issuer_fingerprint().iter().any(|f| **f == fp) || sig.issuer_key_id().iter().any(|i| **i == id);
        if !no_issuer && !mine {
            return false;
        }
        *found = true;
        sig.verify(key, data).is_ok()
    }
    if attempt(sig, &k.primary_key, data, no_issuer, &mut issuer_found) {
        return Some(Ok(()));
    }
    for sub in &k.public_subkeys {
        if attempt(sig, &sub.key, data, no_issuer, &mut issuer_found) {
            return Some(Ok(()));
        }
    }
    if issuer_found && !no_issuer { Some(Err("la firma no corresponde al contenido".into())) } else { None }
}

fn issuer_hint(sig: &Signature) -> Option<String> {
    sig.issuer_fingerprint()
        .first()
        .map(|f| format!("{f:X}"))
        .or_else(|| sig.issuer_key_id().first().map(|k| k.as_ref().iter().map(|b| format!("{b:02X}")).collect()))
}

fn verify_signatures(sigs: &[Signature], data: &[u8], candidates: &[&SignedPublicKey]) -> Verification {
    let mut bad = None;
    for sig in sigs.iter().take(16) {
        for (i, k) in candidates.iter().enumerate() {
            match verify_with(sig, k, data) {
                Some(Ok(())) => return Verification::Good(i),
                Some(Err(e)) => bad = Some(e),
                None => {}
            }
        }
    }
    match bad {
        Some(e) => Verification::Bad(e),
        None => Verification::NoKey(sigs.first().and_then(issuer_hint)),
    }
}

/// Comprueba una firma separada (armadura o binaria) sobre `data`.
pub fn verify_detached(signature: &[u8], data: &[u8], candidates: &[&SignedPublicKey]) -> Result<Verification> {
    check_size(signature)?;
    guard("verificar firma", || {
        let (sig, _) = DetachedSignature::from_reader_single(Cursor::new(signature))
            .map_err(|e| CryptoError::Malformed(e.to_string()))?;
        Ok(verify_signatures(std::slice::from_ref(&sig.signature), data, candidates))
    })
}

/// Comprueba un mensaje firmado en claro (`-----BEGIN PGP SIGNED MESSAGE-----`). Devuelve el texto
/// y el resultado.
pub fn verify_cleartext(armored: &str, candidates: &[&SignedPublicKey]) -> Result<(String, Verification)> {
    check_size(armored.as_bytes())?;
    guard("verificar texto firmado", || {
        let (msg, _) = CleartextSignedMessage::from_string(armored).map_err(|e| CryptoError::Malformed(e.to_string()))?;
        let text = msg.signed_text();
        let v = verify_signatures(msg.signatures(), text.as_bytes(), candidates);
        Ok((msg.text().to_string(), v))
    })
}

/// Cifra `data` para `recipients` (cada uno en su subclave de cifrado), con firma interna opcional.
/// Devuelve el mensaje en armadura.
pub fn encrypt(
    data: &[u8],
    recipients: &[&SignedPublicKey],
    signer: Option<(&SignedSecretKey, &str)>,
) -> Result<String> {
    if data.len() > MAX_MESSAGE_BYTES {
        return Err(CryptoError::TooLarge(data.len()));
    }
    if recipients.is_empty() {
        return Err(CryptoError::MissingRecipientKeys(Vec::new()));
    }
    guard("cifrar", || {
        let mut rng = rand::thread_rng();
        let mut b = MessageBuilder::from_bytes("", data.to_vec()).seipd_v1(&mut rng, SymmetricKeyAlgorithm::AES256);
        for r in recipients {
            let mut used = false;
            for sub in &r.public_subkeys {
                let flagged =
                    sub.signatures.iter().any(|s| s.key_flags().encrypt_comms() || s.key_flags().encrypt_storage());
                if flagged && sub.key.algorithm().can_encrypt() {
                    b.encrypt_to_key(&mut rng, &sub.key)?;
                    used = true;
                }
            }
            if !used {
                if r.primary_key.algorithm().can_encrypt() {
                    b.encrypt_to_key(&mut rng, &r.primary_key)?;
                } else {
                    return Err(CryptoError::BadKey(format!("{} no tiene subclave de cifrado", fingerprint(*r))));
                }
            }
        }
        if let Some((k, pw)) = signer {
            b.sign(*signing_key(k), Password::from(pw), HashAlgorithm::Sha256);
        }
        Ok(b.to_armored_string(&mut rng, ArmorOptions::default())?)
    })
}

/// Resultado de descifrar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decrypted {
    pub data: Vec<u8>,
    /// Firma interna (cifrado + firma en un solo mensaje OpenPGP), si la había.
    pub signature: Option<Verification>,
}

/// Descifra un mensaje (armadura o binario) con alguna de las claves propias y comprueba la firma
/// interna contra `candidates`.
pub fn decrypt(msg: &[u8], keys: &[(&SignedSecretKey, &str)], candidates: &[&SignedPublicKey]) -> Result<Decrypted> {
    if msg.len() > MAX_MESSAGE_BYTES {
        return Err(CryptoError::TooLarge(msg.len()));
    }
    if keys.is_empty() {
        return Err(CryptoError::NoOwnKey(String::new()));
    }
    guard("descifrar", || {
        let (parsed, _) =
            Message::from_reader(Cursor::new(msg)).map_err(|e| CryptoError::Malformed(e.to_string()))?;
        let pws: Vec<Password> = keys.iter().map(|(_, p)| Password::from(*p)).collect();
        let mut m = parsed
            .decrypt_with_keys(pws.iter().collect(), keys.iter().map(|(k, _)| *k).collect())
            .map_err(|e| CryptoError::Malformed(format!("no se pudo descifrar: {e}")))?;
        let mut depth = 0;
        while m.is_compressed() && depth < 4 {
            m = m.decompress()?;
            depth += 1;
        }
        let data = m.as_data_vec().map_err(|e| CryptoError::Malformed(e.to_string()))?;
        if data.len() > MAX_MESSAGE_BYTES {
            return Err(CryptoError::TooLarge(data.len()));
        }
        let signature = if m.is_signed() {
            let mut found = None;
            for (i, k) in candidates.iter().enumerate() {
                let mut keys: Vec<&dyn VerifyingKey> = vec![&k.primary_key];
                keys.extend(k.public_subkeys.iter().map(|s| &s.key as &dyn VerifyingKey));
                if keys.into_iter().any(|key| m.verify(key).is_ok()) {
                    found = Some(Verification::Good(i));
                    break;
                }
            }
            Some(found.unwrap_or(Verification::NoKey(None)))
        } else {
            None
        };
        Ok(Decrypted { data, signature })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_sign_verify_encrypt_decrypt() {
        let alice = generate("Alice", "alice@example.com", "pw-a").unwrap();
        let bob = generate("", "bob@example.org", "pw-b").unwrap();
        let alice_pub = alice.to_public_key();
        let bob_pub = bob.to_public_key();
        assert_eq!(emails(&alice_pub), vec!["alice@example.com"]);
        assert_eq!(user_label(&bob_pub).as_deref(), Some("<bob@example.org>"));
        assert!(can_encrypt(&bob_pub));
        assert!(check_passphrase(&alice, "pw-a").is_ok());
        assert_eq!(check_passphrase(&alice, "mala"), Err(CryptoError::BadPassword));

        // Exportar e importar.
        let armored = armor_public(&alice_pub).unwrap();
        let back = parse_public(armored.as_bytes()).unwrap();
        assert_eq!(fingerprint(&back), fingerprint(&alice_pub));
        let sec = armor_secret(&alice).unwrap();
        assert!(sec.contains("PRIVATE KEY"));
        assert_eq!(secret_fingerprint(&parse_secret(sec.as_bytes()).unwrap()), secret_fingerprint(&alice));
        let bin = binary_public(&alice_pub).unwrap();
        assert_eq!(fingerprint(&parse_public(&bin).unwrap()), fingerprint(&alice_pub));

        // Firma separada.
        let sig = sign_detached(&alice, "pw-a", b"hola").unwrap();
        assert_eq!(verify_detached(sig.as_bytes(), b"hola", &[&bob_pub, &alice_pub]).unwrap(), Verification::Good(1));
        assert!(matches!(verify_detached(sig.as_bytes(), b"hola!", &[&alice_pub]).unwrap(), Verification::Bad(_)));
        assert!(matches!(verify_detached(sig.as_bytes(), b"hola", &[&bob_pub]).unwrap(), Verification::NoKey(Some(_))));
        assert_eq!(sign_detached(&alice, "mala", b"x"), Err(CryptoError::BadPassword));

        // Cifrar + firmar, descifrar + verificar.
        let enc = encrypt(b"secreto", &[&bob_pub, &alice_pub], Some((&alice, "pw-a"))).unwrap();
        assert!(enc.contains("BEGIN PGP MESSAGE"));
        let d = decrypt(enc.as_bytes(), &[(&bob, "pw-b")], &[&alice_pub]).unwrap();
        assert_eq!(d.data, b"secreto");
        assert_eq!(d.signature, Some(Verification::Good(0)));
        // El remitente también puede leer su copia.
        assert_eq!(decrypt(enc.as_bytes(), &[(&alice, "pw-a")], &[]).unwrap().data, b"secreto");
        // Un tercero no.
        let eve = generate("Eve", "eve@example.net", "pw-e").unwrap();
        assert!(decrypt(enc.as_bytes(), &[(&eve, "pw-e")], &[]).is_err());
    }

    #[test]
    fn garbage_is_an_error() {
        assert!(parse_public(b"-----BEGIN PGP PUBLIC KEY BLOCK-----\n\nAAAA\n-----END PGP PUBLIC KEY BLOCK-----\n").is_err());
        assert!(parse_public(&[0xff; 100]).is_err());
        assert!(verify_detached(b"nada", b"x", &[]).is_err());
        assert!(verify_cleartext("hola", &[]).is_err());
        assert_eq!(pretty_fingerprint("ABCDEF12"), "ABCD EF12");
    }
}
