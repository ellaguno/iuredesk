//! Web Key Directory (draft-koch-openpgp-webkey-service): buscar la clave pública de una dirección
//! en el dominio de su correo, por HTTPS (reqwest + rustls). Método avanzado
//! (`openpgpkey.<dominio>`) y, si falla, directo.

use std::time::Duration;

use pgp::composed::SignedPublicKey;
use sha1::{Digest, Sha1};

use crate::error::{CryptoError, Result};
use crate::openpgp;

/// Tiempo máximo de cada consulta.
const TIMEOUT: Duration = Duration::from_secs(10);
/// Tamaño máximo de la respuesta.
const MAX_BYTES: usize = 256 * 1024;

const ZBASE32: &[u8; 32] = b"ybndrfg8ejkmcpqxot1uwisza345h769";

/// z-base-32 (lo usa WKD para el hash del nombre local).
pub fn zbase32(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().saturating_mul(8).div_ceil(5));
    let mut buffer: u32 = 0;
    let mut bits = 0u32;
    for &b in data {
        buffer = (buffer << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            let idx = ((buffer >> (bits - 5)) & 0x1f) as usize;
            out.push(ZBASE32.get(idx).copied().map_or('y', char::from));
            bits -= 5;
        }
        buffer &= (1 << bits) - 1;
    }
    if bits > 0 {
        let idx = ((buffer << (5 - bits)) & 0x1f) as usize;
        out.push(ZBASE32.get(idx).copied().map_or('y', char::from));
    }
    out
}

/// Separa y valida una dirección: (nombre local, dominio en minúsculas).
fn split_addr(email: &str) -> Result<(String, String)> {
    let email = email.trim();
    let (local, domain) =
        email.rsplit_once('@').ok_or_else(|| CryptoError::Malformed(format!("dirección no válida: {email}")))?;
    let domain = domain.to_ascii_lowercase();
    let ok_domain = !domain.is_empty()
        && domain.len() <= 253
        && domain.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.');
    if local.is_empty() || local.len() > 64 || !ok_domain {
        return Err(CryptoError::Malformed(format!("dirección no válida: {email}")));
    }
    Ok((local.to_string(), domain))
}

fn percent(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
            b => format!("%{b:02X}"),
        })
        .collect()
}

/// URLs de WKD para `email`: (avanzada, directa).
pub fn urls(email: &str) -> Result<(String, String)> {
    let (local, domain) = split_addr(email)?;
    let hash = zbase32(&Sha1::digest(local.to_ascii_lowercase().as_bytes()));
    let l = percent(&local);
    Ok((
        format!("https://openpgpkey.{domain}/.well-known/openpgpkey/{domain}/hu/{hash}?l={l}"),
        format!("https://{domain}/.well-known/openpgpkey/hu/{hash}?l={l}"),
    ))
}

/// Cliente WKD.
#[derive(Clone, Debug)]
pub struct WkdClient {
    http: reqwest::Client,
    /// Solo pruebas: base (p. ej. `http://127.0.0.1:1234`) que sustituye a los dominios reales.
    base: Option<String>,
}

impl WkdClient {
    pub fn new() -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .connect_timeout(TIMEOUT)
            .user_agent("IureDesk")
            .build()
            .map_err(|e| CryptoError::Network(e.to_string()))?;
        Ok(WkdClient { http, base: None })
    }

    /// Cliente que consulta `base` con la ruta del método directo (servidores de prueba).
    pub fn with_base(base: &str) -> Result<Self> {
        let mut c = Self::new()?;
        c.base = Some(base.trim_end_matches('/').to_string());
        Ok(c)
    }

    async fn get(&self, url: &str) -> Result<Option<Vec<u8>>> {
        let mut resp = match self.http.get(url).send().await {
            Ok(r) => r,
            Err(e) if e.is_connect() || e.is_timeout() => return Ok(None),
            Err(e) => return Err(CryptoError::Network(e.to_string())),
        };
        if !resp.status().is_success() {
            return Ok(None);
        }
        let mut body = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(|e| CryptoError::Network(e.to_string()))? {
            body.extend_from_slice(&chunk);
            if body.len() > MAX_BYTES {
                return Err(CryptoError::TooLarge(body.len()));
            }
        }
        Ok(Some(body))
    }

    /// Busca la clave de `email`. `Ok(None)` si el dominio no publica ninguna para esa dirección.
    pub async fn lookup(&self, email: &str) -> Result<Option<SignedPublicKey>> {
        let (advanced, direct) = urls(email)?;
        let candidates = match &self.base {
            Some(base) => {
                let path = direct.split_once("/.well-known/").map(|(_, p)| p).unwrap_or_default();
                vec![format!("{base}/.well-known/{path}")]
            }
            None => vec![advanced, direct],
        };
        let want = email.trim().to_ascii_lowercase();
        for url in candidates {
            let Some(body) = self.get(&url).await? else { continue };
            // La respuesta puede traer varias claves: vale la primera con un User ID de `email`.
            if let Ok(k) = openpgp::parse_public(&body) {
                if openpgp::emails(&k).iter().any(|e| *e == want) {
                    return Ok(Some(k));
                }
                return Err(CryptoError::BadKey(format!("la clave publicada no es de {want}")));
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vector() {
        // Ejemplo del borrador: «Joe.Doe@Example.ORG» → iy9q119eutrkn8s1mk4r39qejnbu3n5q
        let (adv, direct) = urls("Joe.Doe@Example.ORG").unwrap();
        assert_eq!(
            adv,
            "https://openpgpkey.example.org/.well-known/openpgpkey/example.org/hu/iy9q119eutrkn8s1mk4r39qejnbu3n5q?l=Joe.Doe"
        );
        assert_eq!(direct, "https://example.org/.well-known/openpgpkey/hu/iy9q119eutrkn8s1mk4r39qejnbu3n5q?l=Joe.Doe");
        assert!(urls("sin-arroba").is_err());
        assert!(urls("a@dominio/raro").is_err());
    }
}
