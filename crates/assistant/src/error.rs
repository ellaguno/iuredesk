//! Errores del asistente, con mensajes en lenguaje humano (es/en a elección de la UI).

use std::time::Duration;

/// Error del crate.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum AiError {
    /// El proveedor no está configurado o no está disponible (sin sesión, sin licencia, sin plugin).
    #[error("no disponible: {0}")]
    NotAvailable(String),
    /// Falta la clave o el servidor la rechazó (401/403).
    #[error("credenciales rechazadas por el proveedor: {0}")]
    Auth(String),
    /// Se agotó la cuota del plan (Iurefficient `quota_exceeded`, OpenAI `insufficient_quota`).
    #[error("se agotó la cuota de IA: {0}")]
    Quota(String),
    /// 429 / 503 / 529: demasiadas peticiones o servicio saturado. `retry_after` si el servidor lo dijo.
    #[error("el proveedor pide esperar (demasiadas peticiones)")]
    RateLimited { retry_after: Option<Duration> },
    /// Otro error HTTP con el mensaje del servidor (recortado).
    #[error("el proveedor respondió {status}: {message}")]
    Http { status: u16, message: String },
    /// Sin red, DNS, TLS.
    #[error("no se pudo conectar con el proveedor: {0}")]
    Network(String),
    /// Se agotó el tiempo de la petición.
    #[error("el proveedor tardó demasiado en responder")]
    Timeout,
    /// El usuario canceló.
    #[error("cancelado")]
    Cancelled,
    /// Entrada o respuesta por encima de los topes configurados.
    #[error("demasiado grande: {0}")]
    TooLarge(String),
    /// La respuesta no es JSON válido ni se pudo reparar, o no cumple el esquema.
    #[error("respuesta de la IA no válida: {0}")]
    InvalidOutput(String),
    /// Flujo de eventos mal formado o cortado.
    #[error("flujo de respuesta mal formado: {0}")]
    Protocol(String),
    /// Configuración no válida o archivo ilegible.
    #[error("configuración del asistente: {0}")]
    Config(String),
    /// Llavero del sistema.
    #[error("llavero del sistema: {0}")]
    Secret(String),
    /// La propuesta no fue aprobada o no es ejecutable.
    #[error("acción no válida: {0}")]
    Action(String),
}

impl AiError {
    /// ¿Vale la pena reintentar automáticamente?
    pub fn is_retryable(&self) -> bool {
        matches!(self, AiError::RateLimited { .. })
    }

    /// Mensaje corto para la interfaz en el idioma pedido.
    pub fn human(&self, spanish: bool) -> String {
        let pick = |en: &str, es: &str| if spanish { es.to_string() } else { en.to_string() };
        match self {
            AiError::NotAvailable(_) => pick("This assistant is not available", "Este asistente no está disponible"),
            AiError::Auth(_) => pick("The AI provider rejected the key", "El proveedor de IA rechazó la clave"),
            AiError::Quota(_) => pick("The AI quota has been reached", "Se alcanzó la cuota de IA"),
            AiError::RateLimited { .. } => pick(
                "The AI provider is busy; try again shortly",
                "El proveedor de IA está saturado; inténtalo en un momento",
            ),
            AiError::Http { .. } | AiError::Protocol(_) => {
                pick("The AI provider returned an error", "El proveedor de IA devolvió un error")
            }
            AiError::Network(_) => {
                pick("Could not reach the AI provider", "No se pudo conectar con el proveedor de IA")
            }
            AiError::Timeout => pick("The AI provider took too long", "El proveedor de IA tardó demasiado"),
            AiError::Cancelled => pick("Cancelled", "Cancelado"),
            AiError::TooLarge(_) => pick("Too much text for the assistant", "Demasiado texto para el asistente"),
            AiError::InvalidOutput(_) => {
                pick("The AI answer could not be understood", "No se entendió la respuesta de la IA")
            }
            AiError::Config(_) => pick("Assistant settings are not valid", "Los ajustes del asistente no son válidos"),
            AiError::Secret(_) => {
                pick("Could not read the key from the keychain", "No se pudo leer la clave del llavero")
            }
            AiError::Action(_) => pick("That action cannot be performed", "Esa acción no se puede realizar"),
        }
    }

    /// Desde un error de `reqwest` (red, tiempo, cuerpo).
    pub(crate) fn from_reqwest(e: &reqwest::Error) -> Self {
        if e.is_timeout() { AiError::Timeout } else { AiError::Network(e.to_string()) }
    }
}

/// Resultado del crate.
pub type Result<T, E = AiError> = std::result::Result<T, E>;
