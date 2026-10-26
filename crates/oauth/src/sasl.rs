//! Mecanismos SASL con token OAuth: XOAUTH2 (Google/Microsoft) y OAUTHBEARER (RFC 7628).
//!
//! Si el servidor rechaza el token, manda un desafío con un JSON en base64 ([`decode_error`]).
//! El cliente debe contestar para cerrar el intercambio: con una línea vacía en XOAUTH2 y con
//! [`OAUTHBEARER_ERROR_ACK`] (`\x01` en base64) en OAUTHBEARER; el servidor responde entonces el
//! fallo final (IMAP `NO`, SMTP `535`).

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::Deserialize;

use crate::Error;

/// Respuesta del cliente a un desafío de error de OAUTHBEARER (RFC 7628 §3.2.3): `\x01`.
pub const OAUTHBEARER_ERROR_ACK: &str = "AQ==";
/// Tamaño máximo de un desafío de error que se decodifica.
const MAX_CHALLENGE: usize = 8 * 1024;

/// Rechaza lo que rompería el formato (separador `\x01`, saltos de línea, NUL).
fn check(field: &str, value: &str) -> Result<(), Error> {
    if value.is_empty() {
        return Err(Error::Sasl(format!("{field} vacío")));
    }
    if value.chars().any(|c| matches!(c, '\x01' | '\r' | '\n' | '\0')) {
        return Err(Error::Sasl(format!("{field} contiene caracteres de control")));
    }
    Ok(())
}

/// Respuesta inicial XOAUTH2: `base64("user=" user "\x01auth=Bearer " token "\x01\x01")`.
pub fn xoauth2(user: &str, access_token: &str) -> Result<String, Error> {
    check("usuario", user)?;
    check("token", access_token)?;
    Ok(STANDARD.encode(format!("user={user}\x01auth=Bearer {access_token}\x01\x01")))
}

/// Escapa el `authzid` del encabezado GS2 (RFC 5801 §4): `,` → `=2C`, `=` → `=3D`.
fn gs2_escape(s: &str) -> String {
    s.replace('=', "=3D").replace(',', "=2C")
}

/// Respuesta inicial OAUTHBEARER (RFC 7628 §3.1):
/// `base64("n,a=" user ",\x01host=" host "\x01port=" port "\x01auth=Bearer " token "\x01\x01")`.
pub fn oauthbearer(user: &str, host: &str, port: u16, access_token: &str) -> Result<String, Error> {
    check("usuario", user)?;
    check("servidor", host)?;
    check("token", access_token)?;
    Ok(STANDARD.encode(format!(
        "n,a={},\x01host={host}\x01port={port}\x01auth=Bearer {access_token}\x01\x01",
        gs2_escape(user)
    )))
}

/// Error del servidor tras un token rechazado (RFC 7628 §3.2.2; Google y Microsoft usan el mismo
/// JSON con XOAUTH2).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct SaslError {
    /// `invalid_token`, `insufficient_scope`, `invalid_request` o, en XOAUTH2, códigos HTTP (`401`).
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub schemes: Option<String>,
}

impl SaslError {
    /// ¿El token caducó o fue revocado? Entonces conviene invalidar y pedir otro una vez.
    pub fn is_invalid_token(&self) -> bool {
        matches!(self.status.as_str(), "invalid_token" | "401")
    }

    /// ¿Faltan permisos (el usuario no concedió el alcance)? Entonces hay que volver a pedir consentimiento.
    pub fn is_insufficient_scope(&self) -> bool {
        matches!(self.status.as_str(), "insufficient_scope" | "403")
    }
}

/// Decodifica el desafío de error (base64 del JSON). `None` si no es un error reconocible.
pub fn decode_error(challenge: &str) -> Option<SaslError> {
    let c = challenge.trim();
    if c.is_empty() || c.len() > MAX_CHALLENGE {
        return None;
    }
    let bytes = STANDARD.decode(c).ok()?;
    let e: SaslError = serde_json::from_slice(&bytes).ok()?;
    (!e.status.is_empty()).then_some(e)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ejemplo de la documentación de XOAUTH2 de Google.
    #[test]
    fn xoauth2_vector() {
        assert_eq!(
            xoauth2("someuser@example.com", "ya29.vF9dft4qmTc2Nvb3RlckBhdHRhdmlzdGEuY29tCg").unwrap(),
            "dXNlcj1zb21ldXNlckBleGFtcGxlLmNvbQFhdXRoPUJlYXJlciB5YTI5LnZGOWRmdDRxbVRjMk52YjNSbGNrQmhkSFJoZG1semRHRXVZMjl0Q2cBAQ=="
        );
    }

    /// Ejemplo de RFC 7628 §4.1.
    #[test]
    fn oauthbearer_vector() {
        assert_eq!(
            oauthbearer("user@example.com", "server.example.com", 143, "vF9dft4qmTc2Nvb3RlckBhbHRhdmlzdGEuY29tCg==")
                .unwrap(),
            "bixhPXVzZXJAZXhhbXBsZS5jb20sAWhvc3Q9c2VydmVyLmV4YW1wbGUuY29tAXBvcnQ9MTQzAWF1dGg9QmVhcmVyIHZGOWRmdDRxbVRjMk52YjNSbGNrQmhiSFJoZG1semRHRXVZMjl0Q2c9PQEB"
        );
    }

    #[test]
    fn oauthbearer_escapes_authzid() {
        let raw = STANDARD.decode(oauthbearer("a,b=c", "h", 1, "t").unwrap()).unwrap();
        assert!(String::from_utf8(raw).unwrap().starts_with("n,a=a=2Cb=3Dc,\x01"));
    }

    #[test]
    fn rejects_injection() {
        assert!(xoauth2("a\x01auth=x", "t").is_err());
        assert!(xoauth2("a", "t\r\nA1 LOGOUT").is_err());
        assert!(xoauth2("", "t").is_err());
        assert!(oauthbearer("a", "h\x01", 1, "t").is_err());
    }

    /// Ejemplo de RFC 7628 §4.3 y uno de Google con XOAUTH2.
    #[test]
    fn error_vectors() {
        let e = decode_error("eyJzdGF0dXMiOiJpbnZhbGlkX3Rva2VuIiwic2NvcGUiOiJleGFtcGxlX3Njb3BlIiwib3BlbmlkLWNvbmZpZ3VyYXRpb24iOiJodHRwczovL2V4YW1wbGUuY29tLy53ZWxsLWtub3duL29wZW5pZC1jb25maWd1cmF0aW9uIn0=").unwrap();
        assert!(e.is_invalid_token());
        assert_eq!(e.scope.as_deref(), Some("example_scope"));
        let g = decode_error(
            "eyJzdGF0dXMiOiI0MDEiLCJzY2hlbWVzIjoiYmVhcmVyIG1hYyIsInNjb3BlIjoiaHR0cHM6Ly9tYWlsLmdvb2dsZS5jb20vIn0=",
        )
        .unwrap();
        assert!(g.is_invalid_token());
        assert_eq!(g.schemes.as_deref(), Some("bearer mac"));
        assert_eq!(decode_error("no es base64!"), None);
        assert_eq!(decode_error(&STANDARD.encode("{}")), None);
        assert_eq!(decode_error(""), None);
    }

    proptest::proptest! {
        #[test]
        fn decode_error_never_panics(s in ".{0,500}") {
            let _ = decode_error(&s);
        }

        #[test]
        fn xoauth2_round_trips(user in "[a-z0-9.@]{1,40}", token in "[A-Za-z0-9._-]{1,80}") {
            let raw = STANDARD.decode(xoauth2(&user, &token).unwrap()).unwrap();
            proptest::prop_assert_eq!(raw, format!("user={user}\x01auth=Bearer {token}\x01\x01").into_bytes());
        }
    }
}
