//! Error de la sincronización y su traducción a mensajes humanos (es/en).

use iuredesk_accounts::probe::{ProbeError, classify_auth_failure};
use iuredesk_imap::{ImapError, NetError};
use iuredesk_pop3::Pop3Error;

/// Fallo de una operación de sincronización.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SyncError {
    /// IMAP.
    #[error("IMAP: {0}")]
    Imap(#[from] ImapError),
    /// POP3.
    #[error("POP3: {0}")]
    Pop3(#[from] Pop3Error),
    /// Almacén local.
    #[error("almacén local: {0}")]
    Store(String),
    /// OAuth.
    #[error("OAuth: {0}")]
    OAuth(#[from] iuredesk_oauth::Error),
    /// Llavero.
    #[error("llavero: {0}")]
    Secrets(String),
    /// No hay contraseña guardada para la cuenta.
    #[error("no hay contraseña guardada para la cuenta")]
    NoPassword,
    /// La cuenta usa OAuth y no hay proveedor de tokens.
    #[error("la cuenta usa OAuth y no hay proveedor de tokens")]
    NoTokenProvider,
    /// Cuenta desconocida.
    #[error("cuenta desconocida: {0}")]
    UnknownAccount(String),
    /// Mensaje desconocido o sin id remoto.
    #[error("mensaje no encontrado: {0}")]
    MessageNotFound(String),
    /// El motor ya se detuvo.
    #[error("la sincronización está detenida")]
    Shutdown,
    /// No se pudo crear el runtime de fondo.
    #[error("runtime: {0}")]
    Runtime(String),
    /// Microsoft Graph.
    #[error("Graph: {0}")]
    Graph(#[from] iuredesk_graph::GraphError),
    /// La carpeta no es una etiqueta (Gmail) o la cuenta no usa etiquetas.
    #[error("no es una etiqueta: {0}")]
    NotALabel(String),
    /// Nombre de carpeta vacío, demasiado largo o con caracteres que el servidor no admite.
    #[error("nombre de carpeta no válido: {0}")]
    InvalidFolderName(String),
    /// Ya hay una carpeta con ese nombre en el mismo sitio.
    #[error("ya existe la carpeta {0}")]
    FolderExists(String),
    /// Las carpetas del sistema (Entrada, Enviados…) no se renombran ni se borran.
    #[error("carpeta del sistema")]
    SystemFolder,
    /// La cuenta no tiene carpetas en el servidor (POP3).
    #[error("la cuenta no admite carpetas")]
    NoFolders,
}

impl From<iuredesk_store::StoreError> for SyncError {
    fn from(e: iuredesk_store::StoreError) -> Self {
        SyncError::Store(e.to_string())
    }
}

/// Resultado de la sincronización.
pub type Result<T, E = SyncError> = std::result::Result<T, E>;

fn net_probe(e: &NetError) -> ProbeError {
    match e {
        NetError::HostNotFound { host } => ProbeError::HostNotFound { host: host.clone() },
        NetError::ConnectionRefused { host } => ProbeError::ConnectionRefused { host: host.clone() },
        NetError::Timeout => ProbeError::Timeout,
        NetError::TlsCertificate(c) => ProbeError::TlsCertificate(c.clone()),
        NetError::Tls(t) => ProbeError::Tls(t.clone()),
        NetError::Closed | NetError::Protocol(_) => ProbeError::Protocol(String::new()),
        NetError::Io(i) => ProbeError::Io(i.clone()),
    }
}

impl SyncError {
    /// `true` si conviene reintentar más tarde sin intervención del usuario.
    pub fn is_transient(&self) -> bool {
        match self {
            SyncError::Imap(e) => e.is_transient(),
            SyncError::Pop3(e) => e.is_transient(),
            SyncError::OAuth(e) => matches!(e, iuredesk_oauth::Error::Http(_) | iuredesk_oauth::Error::Timeout),
            SyncError::Store(_) | SyncError::Secrets(_) => true,
            SyncError::Graph(e) => e.is_transient(),
            _ => false,
        }
    }

    /// `true` si hay que volver a pedir la contraseña o iniciar sesión.
    pub fn needs_reauth(&self) -> bool {
        match self {
            SyncError::Imap(e) => e.is_auth(),
            SyncError::Pop3(e) => e.is_auth(),
            SyncError::OAuth(e) => e.needs_reconsent(),
            SyncError::NoPassword | SyncError::NoTokenProvider => true,
            SyncError::Graph(e) => e.needs_reauth(),
            _ => false,
        }
    }

    /// Mensaje para el usuario, en español (`true`) o inglés.
    pub fn message(&self, spanish: bool) -> String {
        let pick = |en: &str, es: &str| if spanish { es.to_string() } else { en.to_string() };
        match self {
            SyncError::Imap(ImapError::Net(n)) => net_probe(n).message(spanish),
            SyncError::Pop3(Pop3Error::Net(n)) => net_probe(n).message(spanish),
            SyncError::Imap(ImapError::AuthFailed { text, .. }) => classify_auth_failure(text).message(spanish),
            SyncError::Pop3(Pop3Error::AuthFailed(text)) => classify_auth_failure(text).message(spanish),
            SyncError::Imap(ImapError::Bye(text)) => {
                let c = classify_auth_failure(text);
                if matches!(c, ProbeError::TooManyConnections(_)) {
                    c.message(spanish)
                } else {
                    pick(
                        "The mail server closed the connection. We will try again shortly.",
                        "El servidor de correo cerró la conexión. Lo intentaremos de nuevo en un momento.",
                    )
                }
            }
            SyncError::Imap(ImapError::StartTlsUnavailable) | SyncError::Pop3(Pop3Error::StartTlsUnavailable) => {
                ProbeError::StartTlsUnavailable.message(spanish)
            }
            SyncError::Imap(ImapError::TooLarge(_)) | SyncError::Pop3(Pop3Error::TooLarge(_)) => pick(
                "A message is too large to download. It was skipped.",
                "Un mensaje es demasiado grande para descargarlo. Se omitió.",
            ),
            SyncError::Imap(ImapError::No { text, .. }) => {
                if spanish {
                    format!("El servidor de correo rechazó la operación: {}", clip(text))
                } else {
                    format!("The mail server refused the operation: {}", clip(text))
                }
            }
            SyncError::Pop3(Pop3Error::Unsupported(_)) | SyncError::Imap(ImapError::Unsupported(_)) => pick(
                "The mail server lacks a feature IureDesk needs.",
                "Al servidor de correo le falta una función que IureDesk necesita.",
            ),
            SyncError::Imap(_) | SyncError::Pop3(_) => ProbeError::Protocol(String::new()).message(spanish),
            SyncError::OAuth(e) => e.message(spanish),
            SyncError::Store(_) => pick(
                "Could not save mail on this computer (disk full or damaged?).",
                "No se pudo guardar el correo en este equipo (¿disco lleno o dañado?).",
            ),
            SyncError::Secrets(_) => {
                pick("Could not use the system keychain.", "No se pudo usar el llavero del sistema.")
            }
            SyncError::NoPassword => pick(
                "IureDesk has no saved password for this account. Enter it again.",
                "IureDesk no tiene guardada la contraseña de esta cuenta. Escríbela de nuevo.",
            ),
            SyncError::NoTokenProvider => pick(
                "Sign in with your provider to sync this account.",
                "Inicia sesión con tu proveedor para sincronizar esta cuenta.",
            ),
            SyncError::UnknownAccount(_) => pick("That account no longer exists.", "Esa cuenta ya no existe."),
            SyncError::MessageNotFound(_) => {
                pick("That message is no longer on the server.", "Ese mensaje ya no está en el servidor.")
            }
            SyncError::Shutdown => pick("Sync is stopped.", "La sincronización está detenida."),
            SyncError::Runtime(_) => pick("Sync could not start.", "La sincronización no pudo arrancar."),
            SyncError::Graph(iuredesk_graph::GraphError::Throttled { .. }) => pick(
                "Microsoft 365 asked us to slow down. Mail will sync again in a moment.",
                "Microsoft 365 pidió ir más despacio. El correo se sincronizará de nuevo en un momento.",
            ),
            SyncError::Graph(iuredesk_graph::GraphError::Network(_)) => pick(
                "Couldn't reach Microsoft 365. Check your internet connection; it will retry automatically.",
                "No se pudo conectar con Microsoft 365. Revisa tu conexión a internet; se reintentará solo.",
            ),
            SyncError::Graph(e) if e.needs_reauth() => pick(
                "Microsoft 365 needs you to sign in again.",
                "Microsoft 365 necesita que vuelvas a iniciar sesión.",
            ),
            SyncError::Graph(iuredesk_graph::GraphError::Http { status: 404, .. }) => {
                pick("That message is no longer on the server.", "Ese mensaje ya no está en el servidor.")
            }
            SyncError::Graph(e) => {
                if spanish {
                    format!("Microsoft 365 rechazó la operación ({e}).")
                } else {
                    format!("Microsoft 365 refused the operation ({e}).")
                }
            }
            SyncError::InvalidFolderName(_) => pick(
                "That folder name can't be used. Avoid empty names, '/', '*' and '%'.",
                "Ese nombre de carpeta no se puede usar. Evita nombres vacíos, «/», «*» y «%».",
            ),
            SyncError::FolderExists(n) => {
                if spanish {
                    format!("Ya hay una carpeta llamada «{}».", clip(n))
                } else {
                    format!("There is already a folder called \"{}\".", clip(n))
                }
            }
            SyncError::SystemFolder => pick(
                "System folders (Inbox, Sent, Trash…) can't be renamed or deleted.",
                "Las carpetas del sistema (Entrada, Enviados, Papelera…) no se pueden renombrar ni borrar.",
            ),
            SyncError::NoFolders => pick(
                "This account (POP3) has no folders on the server.",
                "Esta cuenta (POP3) no tiene carpetas en el servidor.",
            ),
            SyncError::NotALabel(_) => {
                pick("That folder is not a Gmail label.", "Esa carpeta no es una etiqueta de Gmail.")
            }
        }
    }
}

fn clip(s: &str) -> String {
    s.chars().take(200).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn friendly_and_classified() {
        let e = SyncError::Imap(ImapError::AuthFailed {
            text: "[ALERT] Application-specific password required".into(),
            code: None,
            sasl: None,
        });
        assert!(e.message(true).contains("contraseña de app"));
        assert!(e.needs_reauth());
        let e = SyncError::Imap(ImapError::Net(NetError::Timeout));
        assert!(e.is_transient());
        assert_ne!(e.message(true), e.message(false));
        let e = SyncError::Imap(ImapError::Bye("[UNAVAILABLE] Too many simultaneous connections".into()));
        assert!(e.message(false).contains("too many"));
    }
}
