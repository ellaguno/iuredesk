//! Prueba de conexión para el asistente de alta: entra al servidor, comprueba la contraseña y
//! sale, sin tocar nada. Cada paso con tiempo de espera y cada fallo con un mensaje humano en
//! español e inglés.
//!
//! - IMAP: máquina de estados propia y mínima (CAPABILITY, STARTTLS, AUTHENTICATE PLAIN/XOAUTH2 o
//!   LOGIN, `LIST "" "*"` acotado, LOGOUT).
//! - POP3: propia (STLS, USER/PASS o APOP, AUTH XOAUTH2, STAT, QUIT).
//! - SMTP: `lettre` (rustls + tokio), `test_connection` con autenticación.

mod conn;
mod imap;
mod pop3;
mod smtp;

use std::time::Duration;

use iuredesk_model::account::{AuthMethod, Protocol, ServerConfig};

/// Tiempo por defecto de cada operación de red (conectar, leer, escribir).
pub const IO_TIMEOUT: Duration = Duration::from_secs(10);
/// Tiempo total por defecto de una prueba.
pub const TOTAL_TIMEOUT: Duration = Duration::from_secs(30);
/// Carpetas que se guardan en el informe (se cuentan todas hasta el tope de líneas).
const MAX_FOLDERS: usize = 50;

/// Opciones de la prueba.
#[derive(Debug, Clone, Copy)]
pub struct ProbeOptions {
    pub io_timeout: Duration,
    pub total_timeout: Duration,
}

impl Default for ProbeOptions {
    fn default() -> Self {
        Self { io_timeout: IO_TIMEOUT, total_timeout: TOTAL_TIMEOUT }
    }
}

/// Lo que se aprendió del servidor.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProbeReport {
    pub greeting: String,
    pub capabilities: Vec<String>,
    /// Hasta 50 carpetas (IMAP).
    pub folders: Vec<String>,
    /// Carpetas en total (IMAP).
    pub folder_count: usize,
    /// Mensajes en el buzón (POP3 `STAT`).
    pub message_count: Option<u64>,
}

/// Fallo de una prueba de conexión.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProbeError {
    #[error("no se encontró el servidor {host}")]
    HostNotFound { host: String },
    #[error("el servidor {host} rechazó la conexión")]
    ConnectionRefused { host: String },
    #[error("tiempo de espera agotado")]
    Timeout,
    #[error("certificado TLS no válido: {0}")]
    TlsCertificate(String),
    #[error("error de TLS: {0}")]
    Tls(String),
    #[error("el servidor no ofrece STARTTLS")]
    StartTlsUnavailable,
    #[error("usuario o contraseña incorrectos: {0}")]
    AuthFailed(String),
    #[error("se requiere una contraseña de app: {0}")]
    AppPasswordRequired(String),
    #[error("el proveedor exige OAuth: {0}")]
    OAuthRequired(String),
    #[error("demasiadas conexiones: {0}")]
    TooManyConnections(String),
    #[error("el servidor no admite el método de autenticación")]
    AuthUnsupported,
    #[error("respuesta inesperada del servidor: {0}")]
    Protocol(String),
    #[error("error de red: {0}")]
    Io(String),
    #[error("protocolo no válido para esta prueba")]
    WrongProtocol,
}

impl ProbeError {
    /// Mensaje para el usuario, en español (`true`) o inglés.
    pub fn message(&self, spanish: bool) -> String {
        let pick = |en: String, es: String| if spanish { es } else { en };
        match self {
            ProbeError::HostNotFound { host } => pick(
                format!("Server “{host}” was not found. Check the name and your Internet connection."),
                format!("No se encontró el servidor «{host}». Revisa el nombre y tu conexión a Internet."),
            ),
            ProbeError::ConnectionRefused { host } => pick(
                format!("“{host}” refused the connection. The port or security setting may be wrong."),
                format!("«{host}» rechazó la conexión. Puede que el puerto o la seguridad no sean los correctos."),
            ),
            ProbeError::Timeout => pick(
                "The server took too long to answer. Try again or check the port.".into(),
                "El servidor tardó demasiado en responder. Inténtalo de nuevo o revisa el puerto.".into(),
            ),
            ProbeError::TlsCertificate(_) => pick(
                "The server's security certificate is not valid (expired, self-signed or for another name). We did not send your password.".into(),
                "El certificado de seguridad del servidor no es válido (caducado, autofirmado o de otro nombre). No enviamos tu contraseña.".into(),
            ),
            ProbeError::Tls(_) => pick(
                "Could not establish a secure connection. Try the other security option (SSL/TLS or STARTTLS).".into(),
                "No se pudo establecer una conexión segura. Prueba la otra opción de seguridad (SSL/TLS o STARTTLS).".into(),
            ),
            ProbeError::StartTlsUnavailable => pick(
                "The server does not offer STARTTLS. Use SSL/TLS (port 993/995/465).".into(),
                "El servidor no ofrece STARTTLS. Usa SSL/TLS (puerto 993/995/465).".into(),
            ),
            ProbeError::AuthFailed(_) => pick(
                "Wrong user name or password.".into(),
                "Usuario o contraseña incorrectos.".into(),
            ),
            ProbeError::AppPasswordRequired(_) => pick(
                "Your provider requires an app password, not your usual one. Create it in your account's security settings.".into(),
                "Tu proveedor requiere una contraseña de app, no la de siempre. Créala en la configuración de seguridad de tu cuenta.".into(),
            ),
            ProbeError::OAuthRequired(_) => pick(
                "Your provider no longer accepts passwords here: sign in with your account (OAuth).".into(),
                "Tu proveedor ya no acepta contraseñas aquí: inicia sesión con tu cuenta (OAuth).".into(),
            ),
            ProbeError::TooManyConnections(_) => pick(
                "The server says there are too many open connections. Close other mail apps or wait a few minutes.".into(),
                "El servidor dice que hay demasiadas conexiones abiertas. Cierra otros programas de correo o espera unos minutos.".into(),
            ),
            ProbeError::AuthUnsupported => pick(
                "The server does not support this sign-in method.".into(),
                "El servidor no admite este método de inicio de sesión.".into(),
            ),
            ProbeError::Protocol(_) | ProbeError::Io(_) => pick(
                "The server answered something unexpected. Check the server name and port.".into(),
                "El servidor respondió algo inesperado. Revisa el nombre del servidor y el puerto.".into(),
            ),
            ProbeError::WrongProtocol => pick(
                "This test does not apply to that protocol.".into(),
                "Esta prueba no aplica a ese protocolo.".into(),
            ),
        }
    }
}

/// Clasifica el texto de un rechazo del servidor al autenticar (IMAP `NO`, POP3 `-ERR`, SMTP 5xx).
pub fn classify_auth_failure(text: &str) -> ProbeError {
    let t = text.to_ascii_lowercase();
    let detail = text.chars().take(300).collect::<String>();
    let any = |keys: &[&str]| keys.iter().any(|k| t.contains(k));
    if any(&[
        "too many",
        "[limit]",
        "maximum number of connections",
        "simultaneous",
        "connection limit",
        "4.7.0 too many",
    ]) {
        ProbeError::TooManyConnections(detail)
    } else if any(&[
        "application-specific password",
        "app-specific password",
        "app password",
        "apppassword",
        "application password",
        "web login required",
        "#auth007",
    ]) {
        ProbeError::AppPasswordRequired(detail)
    } else if any(&[
        "basicauthblocked",
        "basic authentication is disabled",
        "5.7.139",
        "please use oauth",
        "oauth required",
    ]) {
        ProbeError::OAuthRequired(detail)
    } else {
        ProbeError::AuthFailed(detail)
    }
}

/// Recorta el texto que se muestra o se guarda de una respuesta.
pub(crate) fn clip(s: &str) -> String {
    s.chars().take(300).collect()
}

/// Rechaza secretos o usuarios que romperían el protocolo (saltos de línea, NUL).
pub(crate) fn safe_token(s: &str) -> Result<(), ProbeError> {
    if s.chars().any(|c| matches!(c, '\r' | '\n' | '\0')) {
        Err(ProbeError::Protocol("el usuario o la contraseña contienen caracteres no válidos".into()))
    } else {
        Ok(())
    }
}

/// `user=…\x01auth=Bearer …\x01\x01` en base64 (SASL XOAUTH2).
pub(crate) fn xoauth2(user: &str, token: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(format!("user={user}\x01auth=Bearer {token}\x01\x01"))
}

/// `\0usuario\0contraseña` en base64 (SASL PLAIN).
pub(crate) fn sasl_plain(user: &str, pass: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(format!("\0{user}\0{pass}"))
}

/// Prueba el servidor entrante (IMAP o POP3) con `secret` (contraseña o token OAuth según
/// `cfg.auth`).
pub async fn test_incoming(cfg: &ServerConfig, secret: &str) -> Result<ProbeReport, ProbeError> {
    test_incoming_with(cfg, secret, ProbeOptions::default()).await
}

/// [`test_incoming`] con opciones.
pub async fn test_incoming_with(
    cfg: &ServerConfig,
    secret: &str,
    opts: ProbeOptions,
) -> Result<ProbeReport, ProbeError> {
    safe_token(&cfg.username)?;
    safe_token(secret)?;
    let oauth = matches!(cfg.auth, AuthMethod::OAuth2 { .. });
    let fut = async {
        match cfg.protocol {
            Protocol::Imap => imap::probe(cfg, secret, oauth, opts).await,
            Protocol::Pop3 => pop3::probe(cfg, secret, oauth, opts).await,
            Protocol::Smtp => Err(ProbeError::WrongProtocol),
        }
    };
    tokio::time::timeout(opts.total_timeout, fut).await.unwrap_or(Err(ProbeError::Timeout))
}

/// Prueba el servidor saliente (SMTP): conecta, cifra, autentica y sale.
pub async fn test_outgoing(cfg: &ServerConfig, secret: &str) -> Result<ProbeReport, ProbeError> {
    test_outgoing_with(cfg, secret, ProbeOptions::default()).await
}

/// [`test_outgoing`] con opciones.
pub async fn test_outgoing_with(
    cfg: &ServerConfig,
    secret: &str,
    opts: ProbeOptions,
) -> Result<ProbeReport, ProbeError> {
    if cfg.protocol != Protocol::Smtp {
        return Err(ProbeError::WrongProtocol);
    }
    safe_token(&cfg.username)?;
    safe_token(secret)?;
    let oauth = matches!(cfg.auth, AuthMethod::OAuth2 { .. });
    tokio::time::timeout(opts.total_timeout, smtp::probe(cfg, secret, oauth, opts))
        .await
        .unwrap_or(Err(ProbeError::Timeout))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_real_server_texts() {
        let c = |s: &str| classify_auth_failure(s);
        assert!(matches!(
            c(
                "[ALERT] Application-specific password required: https://support.google.com/accounts/answer/185833 (Failure)"
            ),
            ProbeError::AppPasswordRequired(_)
        ));
        assert!(matches!(c("[AUTHENTICATIONFAILED] Invalid credentials (Failure)"), ProbeError::AuthFailed(_)));
        assert!(matches!(
            c("[UNAVAILABLE] Too many simultaneous connections. (Failure)"),
            ProbeError::TooManyConnections(_)
        ));
        assert!(matches!(c("421 4.7.0 Too many connections, try again later"), ProbeError::TooManyConnections(_)));
        assert!(matches!(
            c("535 5.7.139 Authentication unsuccessful, basic authentication is disabled. [BasicAuthBlocked]"),
            ProbeError::OAuthRequired(_)
        ));
        assert!(matches!(
            c("[AUTHENTICATIONFAILED] (#AUTH007) Bad scripted login"),
            ProbeError::AppPasswordRequired(_)
        ));
        assert!(matches!(c("-ERR [AUTH] Authentication failed."), ProbeError::AuthFailed(_)));
    }

    #[test]
    fn messages_are_bilingual_and_human() {
        let errors = [
            ProbeError::HostNotFound { host: "imap.x.com".into() },
            ProbeError::Timeout,
            ProbeError::TlsCertificate("Expired".into()),
            ProbeError::AuthFailed("x".into()),
            ProbeError::AppPasswordRequired("x".into()),
            ProbeError::TooManyConnections("x".into()),
        ];
        for e in errors {
            let (es, en) = (e.message(true), e.message(false));
            assert_ne!(es, en);
            assert!(!es.is_empty() && !en.is_empty());
        }
        assert!(ProbeError::AppPasswordRequired(String::new()).message(true).contains("contraseña de app"));
        assert!(ProbeError::HostNotFound { host: "h".into() }.message(true).contains("No se encontró"));
    }

    #[test]
    fn sasl_encodings() {
        assert_eq!(sasl_plain("u", "p"), "AHUAcA==");
        assert_eq!(xoauth2("a@b.c", "t"), "dXNlcj1hQGIuYwFhdXRoPUJlYXJlciB0AQE=");
        assert!(safe_token("ok").is_ok());
        assert!(safe_token("a\r\nb").is_err());
    }
}
