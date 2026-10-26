//! Error del cliente IMAP.

use iuredesk_net::NetError;

use crate::codec::{Code, ParseError};

/// Fallo de una operación IMAP.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ImapError {
    /// Fallo de red (conexión, TLS, tiempo de espera, cierre).
    #[error(transparent)]
    Net(#[from] NetError),
    /// El servidor respondió `NO`.
    #[error("el servidor rechazó {command}: {text}")]
    No {
        /// Orden (sin argumentos).
        command: String,
        /// Código de respuesta.
        code: Option<Code>,
        /// Texto del servidor.
        text: String,
    },
    /// El servidor respondió `BAD`.
    #[error("el servidor no entendió {command}: {text}")]
    Bad {
        /// Orden (sin argumentos).
        command: String,
        /// Texto del servidor.
        text: String,
    },
    /// El servidor cerró la sesión (`BYE`).
    #[error("el servidor cerró la sesión: {0}")]
    Bye(String),
    /// Usuario, contraseña o token rechazados.
    #[error("autenticación rechazada: {text}")]
    AuthFailed {
        /// Texto del servidor (sirve para clasificar: contraseña de app, OAuth…).
        text: String,
        /// Código de respuesta (`AUTHENTICATIONFAILED`, `UNAVAILABLE`…).
        code: Option<Code>,
        /// Reto SASL de error (base64), si el servidor lo mandó (XOAUTH2/OAUTHBEARER).
        sasl: Option<String>,
    },
    /// Falta una capacidad necesaria.
    #[error("el servidor no admite {0}")]
    Unsupported(String),
    /// Se pidió STARTTLS y el servidor no lo ofrece.
    #[error("el servidor no ofrece STARTTLS")]
    StartTlsUnavailable,
    /// Respuesta que no respeta la gramática.
    #[error(transparent)]
    Parse(#[from] ParseError),
    /// Respuesta, línea o literal por encima del tope.
    #[error("{0} demasiado grande")]
    TooLarge(&'static str),
    /// Secuencia inesperada (p. ej. continuación donde no tocaba).
    #[error("respuesta inesperada del servidor: {0}")]
    Protocol(String),
    /// Orden que exige un buzón seleccionado.
    #[error("no hay buzón seleccionado")]
    NoMailbox,
    /// Argumento que no se puede enviar (saltos de línea, NUL…).
    #[error("dato no válido: {0}")]
    InvalidInput(String),
}

impl ImapError {
    /// `true` si conviene reintentar más tarde (red, tiempo, cierre, servidor ocupado).
    pub fn is_transient(&self) -> bool {
        match self {
            ImapError::Net(e) => !matches!(e, NetError::TlsCertificate(_) | NetError::HostNotFound { .. }),
            ImapError::Bye(_) | ImapError::TooLarge(_) => true,
            ImapError::No { code: Some(Code::Other(c, _)), .. } => {
                matches!(c.as_str(), "UNAVAILABLE" | "INUSE" | "LIMIT" | "SERVERBUG" | "OVERQUOTA")
            }
            ImapError::AuthFailed { code: Some(Code::Other(c, _)), .. } => c == "UNAVAILABLE",
            _ => false,
        }
    }

    /// `true` si el fallo es de credenciales.
    pub fn is_auth(&self) -> bool {
        matches!(self, ImapError::AuthFailed { .. })
    }

    /// `true` si la conexión ya no sirve y hay que abrir otra.
    pub fn is_fatal_for_connection(&self) -> bool {
        matches!(
            self,
            ImapError::Net(_)
                | ImapError::Bye(_)
                | ImapError::Parse(_)
                | ImapError::TooLarge(_)
                | ImapError::Protocol(_)
        )
    }
}

/// Resultado del cliente IMAP.
pub type Result<T, E = ImapError> = std::result::Result<T, E>;
