//! Error de la sincronización PIM, con mensajes para personas en es/en.

/// Error de cualquier operación de este crate.
#[derive(Debug, thiserror::Error)]
pub enum PimError {
    /// Almacén local.
    #[error("almacén: {0}")]
    Store(#[from] iuredesk_store::StoreError),
    /// CalDAV/CardDAV o formato iCalendar/vCard.
    #[error("dav: {0}")]
    Dav(#[from] iuredesk_dav::Error),
    /// Microsoft Graph respondió un error.
    #[error("graph {status}: {detail}")]
    Graph {
        /// Código HTTP.
        status: u16,
        /// Código de error de Graph y contexto.
        detail: String,
    },
    /// Red (conexión, TLS, tiempo de espera).
    #[error("red: {0}")]
    Network(String),
    /// El servidor rechazó la credencial.
    #[error("credencial rechazada")]
    Unauthorized,
    /// El elemento cambió en el servidor (412) y no se pudo resolver.
    #[error("conflicto: el elemento cambió en el servidor")]
    Conflict,
    /// OAuth (token caducado, sin permiso…).
    #[error("oauth: {0}")]
    OAuth(#[from] iuredesk_oauth::Error),
    /// No hay contraseña guardada para la cuenta.
    #[error("no hay contraseña guardada")]
    NoPassword,
    /// La cuenta usa OAuth pero no hay proveedor de tokens.
    #[error("no hay proveedor de tokens OAuth")]
    NoTokenProvider,
    /// Llavero del sistema.
    #[error("llavero: {0}")]
    Secrets(String),
    /// Respuesta o dato con forma inesperada.
    #[error("dato no válido: {0}")]
    Invalid(String),
    /// La colección es de solo lectura.
    #[error("la colección es de solo lectura")]
    ReadOnly,
    /// No existe el elemento local.
    #[error("no existe {0}")]
    NotFound(String),
    /// Cuenta desconocida para la sincronización.
    #[error("cuenta desconocida: {0}")]
    UnknownAccount(String),
    /// La sincronización se está apagando.
    #[error("apagando")]
    Shutdown,
    /// No se pudo arrancar el runtime de fondo.
    #[error("runtime: {0}")]
    Runtime(String),
}

/// Resultado del crate.
pub type Result<T, E = PimError> = std::result::Result<T, E>;

impl PimError {
    /// ¿Vale la pena reintentar más tarde sin que el usuario haga nada?
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Network(_) | Self::Shutdown => true,
            Self::Dav(e) => {
                matches!(e, iuredesk_dav::Error::Network(_) | iuredesk_dav::Error::Http { status: 500..=599 | 429, .. })
            }
            Self::Graph { status, .. } => *status >= 500 || *status == 429,
            Self::OAuth(e) => matches!(e, iuredesk_oauth::Error::Http(_)),
            _ => false,
        }
    }

    /// ¿Hay que pedir de nuevo la contraseña o el inicio de sesión?
    pub fn needs_reauth(&self) -> bool {
        match self {
            Self::Unauthorized | Self::NoPassword => true,
            Self::Dav(iuredesk_dav::Error::Unauthorized) => true,
            Self::Graph { status: 401, .. } => true,
            Self::OAuth(e) => e.needs_reconsent(),
            _ => false,
        }
    }

    /// Mensaje para el usuario, en español (`true`) o inglés.
    pub fn message(&self, spanish: bool) -> String {
        let pick = |en: &str, es: &str| if spanish { es.to_string() } else { en.to_string() };
        if self.needs_reauth() {
            if let Self::OAuth(e) = self {
                return e.message(spanish);
            }
            return pick(
                "The calendar/contacts server rejected your password. Check it (iCloud needs an app-specific password).",
                "El servidor de calendario/contactos rechazó tu contraseña. Revísala (iCloud pide una contraseña específica de app).",
            );
        }
        match self {
            Self::Network(_) | Self::Dav(iuredesk_dav::Error::Network(_)) => pick(
                "Could not reach your calendar server. Check your Internet connection; we will retry.",
                "No se pudo conectar con tu servidor de calendario. Revisa tu conexión; lo intentaremos de nuevo.",
            ),
            Self::Conflict | Self::Dav(iuredesk_dav::Error::PreconditionFailed) => pick(
                "Someone else changed this item at the same time. We kept the server's version.",
                "Alguien más cambió este elemento a la vez. Conservamos la versión del servidor.",
            ),
            Self::ReadOnly => {
                pick("This calendar or address book is read-only.", "Este calendario o libreta es de solo lectura.")
            }
            Self::NoTokenProvider | Self::OAuth(_) => match self {
                Self::OAuth(e) => e.message(spanish),
                _ => pick(
                    "Sign-in is not configured for this account.",
                    "El inicio de sesión no está configurado para esta cuenta.",
                ),
            },
            Self::Secrets(_) => pick("Could not use the system keychain.", "No se pudo usar el llavero del sistema."),
            Self::Store(_) => pick(
                "IureDesk could not save calendar data on this computer.",
                "IureDesk no pudo guardar los datos del calendario en este equipo.",
            ),
            Self::Graph { status, .. } if *status >= 500 || *status == 429 => pick(
                "Microsoft 365 is busy right now; we will retry.",
                "Microsoft 365 está ocupado en este momento; lo intentaremos de nuevo.",
            ),
            _ => pick(
                "The calendar server answered something unexpected. We will retry later.",
                "El servidor de calendario respondió algo inesperado. Lo intentaremos más tarde.",
            ),
        }
    }
}

impl From<reqwest::Error> for PimError {
    fn from(e: reqwest::Error) -> Self {
        Self::Network(if e.is_timeout() { "tiempo de espera agotado".into() } else { e.to_string() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification_and_messages() {
        assert!(PimError::Network("x".into()).is_transient());
        assert!(PimError::Dav(iuredesk_dav::Error::Unauthorized).needs_reauth());
        assert!(PimError::Graph { status: 503, detail: String::new() }.is_transient());
        assert!(!PimError::Conflict.is_transient());
        assert!(PimError::Conflict.message(true).contains("versión del servidor"));
        assert!(PimError::Unauthorized.message(false).contains("app-specific"));
    }
}
