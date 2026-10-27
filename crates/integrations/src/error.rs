//! Errores de las integraciones.
//!
//! Que una app no esté instalada **no** es un error (ver [`crate::Outcome`]); esto es para lo
//! que falla de verdad: la app se cerró a medias, respondió basura, tardó demasiado…

use std::time::Duration;

/// Resultado de las integraciones.
pub type Result<T> = std::result::Result<T, IntegrationError>;

/// Fallo al hablar con una app hermana.
#[derive(Debug, thiserror::Error)]
pub enum IntegrationError {
    /// Error de E/S (lanzar el proceso, leer el socket, escribir un archivo).
    #[error("E/S: {0}")]
    Io(#[from] std::io::Error),
    /// El otro lado no habla el protocolo esperado.
    #[error("protocolo: {0}")]
    Protocol(String),
    /// Error JSON-RPC devuelto por el servidor.
    #[error("el servidor respondió con el error {code}: {message}")]
    Rpc {
        /// Código JSON-RPC.
        code: i64,
        /// Mensaje del servidor.
        message: String,
    },
    /// La herramienta MCP falló (`isError: true`): el texto es el que dio la app.
    #[error("{0}")]
    Tool(String),
    /// Se agotó el tiempo de espera.
    #[error("sin respuesta en {0:?}")]
    Timeout(Duration),
    /// El proceso hijo terminó o cerró su salida.
    #[error("la app se cerró: {0}")]
    Closed(String),
    /// Datos de entrada no válidos (ruta relativa, texto demasiado largo…).
    #[error("entrada no válida: {0}")]
    InvalidInput(String),
}

impl IntegrationError {
    /// Mensaje para el usuario, en español o en inglés.
    pub fn human(&self, spanish: bool) -> String {
        let pick = |en: &str, es: &str| if spanish { es.to_string() } else { en.to_string() };
        match self {
            Self::Io(e) => {
                if spanish {
                    format!("No se pudo completar la operación: {e}")
                } else {
                    format!("The operation could not be completed: {e}")
                }
            }
            Self::Protocol(_) | Self::Rpc { .. } => pick(
                "The other app answered something unexpected. Updating it may help.",
                "La otra app respondió algo inesperado. Actualizarla puede ayudar.",
            ),
            Self::Tool(t) => t.clone(),
            Self::Timeout(_) => {
                pick("The other app took too long to answer.", "La otra app tardó demasiado en responder.")
            }
            Self::Closed(_) => pick("The other app closed unexpectedly.", "La otra app se cerró de forma inesperada."),
            Self::InvalidInput(m) => m.clone(),
        }
    }
}
