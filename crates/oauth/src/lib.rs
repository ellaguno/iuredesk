//! OAuth 2 para IureDesk (capa L3): inicio de sesión con Google y Microsoft desde una app nativa.
//!
//! - [`provider`]: configuración de cada proveedor (Google, Microsoft identity platform v2 y uno
//!   genérico para el futuro) y de dónde salen los *client id* (compilación, entorno o ajustes).
//! - [`flow`]: código de autorización + PKCE para apps nativas (RFC 8252 + RFC 7636) con un
//!   receptor *loopback* en `127.0.0.1` y puerto efímero; abre el navegador del sistema o entrega la
//!   URL para que la UI la muestre.
//! - [`token`]: [`TokenSet`], canje del código y refresco contra el *token endpoint*.
//! - [`manager`]: [`TokenManager`], el [`TokenProvider`] que usarán IMAP/SMTP/Graph: el token de
//!   refresco vive en el llavero (`oauth-refresh`), el de acceso solo en memoria.
//! - [`sasl`]: XOAUTH2 y OAUTHBEARER (RFC 7628) y la lectura de sus respuestas de error.
//!
//! Nada aquí abre diálogos: la UI decide qué mostrar con [`Error::message`].
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

pub mod flow;
mod loopback;
pub mod manager;
mod pkce;
pub mod provider;
pub mod sasl;
pub mod token;

pub use flow::{DEFAULT_LOGIN_TIMEOUT, PendingLogin, start_login};
pub use manager::{BoxFuture, TokenManager, TokenProvider};
pub use provider::{ClientCredentials, MicrosoftTenant, OAuthSettings, ProviderConfig, RedirectHost, TokenAudience};
pub use token::{IdClaims, TokenSet, http_client};
pub use tokio_util::sync::CancellationToken;

/// Error del crate. `Display` en español para registros; [`Error::message`] para el usuario.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("no hay client id configurado para {provider}")]
    MissingClientId { provider: String },
    #[error("configuración OAuth no válida: {0}")]
    Config(String),
    #[error("no se pudo abrir el receptor local: {0}")]
    Listener(String),
    #[error("no se pudo abrir el navegador: {0}")]
    Browser(String),
    #[error("tiempo de espera agotado esperando al navegador")]
    Timeout,
    #[error("inicio de sesión cancelado")]
    Cancelled,
    #[error("el parámetro state no coincide (posible ataque CSRF)")]
    StateMismatch,
    #[error("el usuario no dio permiso: {0}")]
    AccessDenied(String),
    #[error("el proveedor devolvió un error de autorización: {error}")]
    Authorization { error: String, description: Option<String> },
    #[error("respuesta del navegador no válida: {0}")]
    InvalidCallback(String),
    #[error("error de red: {0}")]
    Http(String),
    #[error("el servidor de tokens respondió {status}: {error}")]
    TokenEndpoint { status: u16, error: String, description: Option<String> },
    #[error("hay que volver a dar permiso a {provider} (invalid_grant)")]
    NeedsReconsent { provider: String },
    #[error("respuesta del servidor de tokens no válida: {0}")]
    InvalidResponse(String),
    #[error("respuesta demasiado grande")]
    TooLarge,
    #[error("la cuenta no usa OAuth")]
    NotOAuthAccount,
    #[error("no hay configuración para el proveedor {0}")]
    UnknownProvider(String),
    #[error("no hay token de refresco guardado para la cuenta")]
    NoRefreshToken,
    #[error("llavero: {0}")]
    Secrets(String),
    #[error("dato no válido para SASL: {0}")]
    Sasl(String),
}

impl From<iuredesk_accounts::Error> for Error {
    fn from(e: iuredesk_accounts::Error) -> Self {
        Error::Secrets(e.to_string())
    }
}

impl Error {
    /// Mensaje para el usuario, en español (`true`) o inglés.
    pub fn message(&self, spanish: bool) -> String {
        let pick = |en: String, es: String| if spanish { es } else { en };
        match self {
            Error::MissingClientId { provider } => pick(
                format!("This build of IureDesk has no {provider} sign-in configured. Use an app password or ask your administrator."),
                format!("Esta versión de IureDesk no tiene configurado el inicio de sesión con {provider}. Usa una contraseña de app o consulta a tu administrador."),
            ),
            Error::Config(_) | Error::UnknownProvider(_) => pick(
                "The sign-in settings are not valid.".into(),
                "La configuración de inicio de sesión no es válida.".into(),
            ),
            Error::Listener(_) => pick(
                "IureDesk could not prepare the sign-in (local port). Try again.".into(),
                "IureDesk no pudo preparar el inicio de sesión (puerto local). Inténtalo de nuevo.".into(),
            ),
            Error::Browser(_) => pick(
                "Could not open your browser. Copy the link and open it yourself.".into(),
                "No se pudo abrir tu navegador. Copia el enlace y ábrelo tú.".into(),
            ),
            Error::Timeout => pick(
                "The sign-in took too long. Start again when you are ready.".into(),
                "El inicio de sesión tardó demasiado. Vuelve a empezar cuando estés listo.".into(),
            ),
            Error::Cancelled => pick("Sign-in cancelled.".into(), "Inicio de sesión cancelado.".into()),
            Error::StateMismatch | Error::InvalidCallback(_) => pick(
                "The browser's answer did not match this sign-in, so we ignored it for your safety. Try again.".into(),
                "La respuesta del navegador no correspondía a este inicio de sesión y la ignoramos por seguridad. Inténtalo de nuevo.".into(),
            ),
            Error::AccessDenied(_) => pick(
                "You did not grant IureDesk access. Without it we cannot read your mail.".into(),
                "No diste permiso a IureDesk. Sin él no podemos leer tu correo.".into(),
            ),
            Error::Authorization { .. } => pick(
                "Your provider rejected the sign-in. Your organization may need to approve IureDesk.".into(),
                "Tu proveedor rechazó el inicio de sesión. Puede que tu organización deba aprobar IureDesk.".into(),
            ),
            Error::Http(_) => pick(
                "Could not reach the sign-in server. Check your Internet connection.".into(),
                "No se pudo contactar al servidor de inicio de sesión. Revisa tu conexión a Internet.".into(),
            ),
            Error::TokenEndpoint { .. } | Error::InvalidResponse(_) | Error::TooLarge => pick(
                "The sign-in server answered something unexpected. Try again later.".into(),
                "El servidor de inicio de sesión respondió algo inesperado. Inténtalo más tarde.".into(),
            ),
            Error::NeedsReconsent { provider } => pick(
                format!("Your {provider} sign-in expired or was revoked. Sign in again to keep syncing."),
                format!("Tu sesión de {provider} caducó o se revocó. Vuelve a iniciar sesión para seguir sincronizando."),
            ),
            Error::NotOAuthAccount => pick(
                "This account does not use sign-in with a provider.".into(),
                "Esta cuenta no usa inicio de sesión con un proveedor.".into(),
            ),
            Error::NoRefreshToken => pick(
                "IureDesk has no saved sign-in for this account. Sign in again.".into(),
                "IureDesk no tiene guardado el inicio de sesión de esta cuenta. Vuelve a iniciar sesión.".into(),
            ),
            Error::Secrets(_) => pick(
                "Could not use the system keychain.".into(),
                "No se pudo usar el llavero del sistema.".into(),
            ),
            Error::Sasl(_) => pick(
                "The user name contains characters that cannot be sent to the server.".into(),
                "El nombre de usuario tiene caracteres que no se pueden enviar al servidor.".into(),
            ),
        }
    }

    /// ¿Hay que pedirle al usuario que vuelva a iniciar sesión?
    pub fn needs_reconsent(&self) -> bool {
        matches!(self, Error::NeedsReconsent { .. } | Error::NoRefreshToken)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_are_bilingual() {
        let e = Error::NeedsReconsent { provider: "Google".into() };
        assert!(e.message(true).contains("Vuelve a iniciar sesión"));
        assert!(e.message(false).contains("Sign in again"));
        assert!(e.needs_reconsent());
        assert!(!Error::Timeout.needs_reconsent());
    }
}
