//! Errores del crate, con mensaje para el usuario en los dos idiomas.

use crate::Bi;

/// Error de reglas, Sieve, posponer o baja de listas.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RulesError {
    /// Almacén local.
    #[error("almacén: {0}")]
    Store(String),
    /// Dato no válido (regla incompleta, expresión regular rota…).
    #[error("dato no válido: {0}")]
    Invalid(String),
    /// Red (ManageSieve, baja en un clic).
    #[error("red: {0}")]
    Net(String),
    /// El servidor respondió `NO`/`BYE` o algo que no es ManageSieve.
    #[error("el servidor respondió: {0}")]
    Server(String),
    /// Credenciales rechazadas.
    #[error("el servidor rechazó el usuario o la contraseña")]
    Auth,
    /// No se puede expresar en Sieve (se queda en local).
    #[error("no se puede expresar en Sieve: {0}")]
    NotExpressible(String),
    /// Script Sieve con un error de sintaxis.
    #[error("Sieve, línea {line}: {msg}")]
    Syntax {
        /// Línea (desde 1).
        line: usize,
        /// Qué falló.
        msg: String,
    },
    /// Tiempo de espera agotado.
    #[error("tiempo de espera agotado")]
    Timeout,
}

impl RulesError {
    /// Mensaje en lenguaje humano.
    pub fn text(&self) -> Bi {
        match self {
            RulesError::Store(e) => Bi::new(
                format!("Your local mailbox could not be updated ({e})."),
                format!("No se pudo actualizar tu buzón local ({e})."),
            ),
            RulesError::Invalid(e) => {
                Bi::new(format!("This rule is incomplete: {e}"), format!("Esta regla está incompleta: {e}"))
            }
            RulesError::Net(e) => Bi::new(
                format!("The server could not be reached ({e}). Check your connection."),
                format!("No se pudo contactar al servidor ({e}). Revisa tu conexión."),
            ),
            RulesError::Server(e) => Bi::new(
                format!("The server did not accept the request: {e}"),
                format!("El servidor no aceptó la petición: {e}"),
            ),
            RulesError::Auth => Bi::new(
                "The server rejected your user name or password.",
                "El servidor rechazó tu usuario o contraseña.",
            ),
            RulesError::NotExpressible(e) => Bi::new(
                format!("This rule can only run on this computer: {e}"),
                format!("Esta regla solo puede ejecutarse en este equipo: {e}"),
            ),
            RulesError::Syntax { line, msg } => Bi::new(
                format!("The server filter has an error on line {line}: {msg}"),
                format!("El filtro del servidor tiene un error en la línea {line}: {msg}"),
            ),
            RulesError::Timeout => Bi::new("The server took too long to answer.", "El servidor tardó demasiado."),
        }
    }
}

impl From<iuredesk_store::StoreError> for RulesError {
    fn from(e: iuredesk_store::StoreError) -> Self {
        RulesError::Store(e.to_string())
    }
}

impl From<iuredesk_sync::SyncError> for RulesError {
    fn from(e: iuredesk_sync::SyncError) -> Self {
        RulesError::Store(e.to_string())
    }
}

impl From<serde_json::Error> for RulesError {
    fn from(e: serde_json::Error) -> Self {
        RulesError::Store(e.to_string())
    }
}

/// Resultado del crate.
pub type Result<T, E = RulesError> = std::result::Result<T, E>;
