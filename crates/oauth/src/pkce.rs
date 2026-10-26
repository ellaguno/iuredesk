//! PKCE (RFC 7636, método `S256`) y el parámetro `state`, con azar del sistema operativo.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

use crate::Error;

/// `n` bytes aleatorios del SO en base64url sin relleno.
pub(crate) fn random_token(n: usize) -> Result<String, Error> {
    let mut buf = vec![0u8; n];
    getrandom::getrandom(&mut buf).map_err(|e| Error::Listener(format!("sin azar del sistema: {e}")))?;
    Ok(URL_SAFE_NO_PAD.encode(buf))
}

/// Par verificador / desafío.
pub(crate) struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    /// Verificador de 32 bytes aleatorios (43 caracteres, dentro del rango 43–128 de RFC 7636).
    pub fn new() -> Result<Self, Error> {
        let verifier = random_token(32)?;
        let challenge = challenge_s256(&verifier);
        Ok(Self { verifier, challenge })
    }
}

/// `BASE64URL(SHA256(verifier))`.
pub(crate) fn challenge_s256(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Vector del apéndice B de RFC 7636.
    #[test]
    fn rfc7636_vector() {
        assert_eq!(
            challenge_s256("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn verifier_shape() {
        let p = Pkce::new().unwrap();
        assert_eq!(p.verifier.len(), 43);
        assert!(p.verifier.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        assert_ne!(p.verifier, Pkce::new().unwrap().verifier);
    }
}
