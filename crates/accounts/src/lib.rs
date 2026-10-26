//! Cuentas, llavero del SO y sesión compartida de Iurefficient (capa L3).
//!
//! - [`store`]: la lista de cuentas (sin secretos) en `accounts.json`, con escritura atómica.
//! - [`secrets`]: contraseñas y tokens en el llavero del sistema (servicio `iuredesk`).
//! - [`iurefficient`]: la sesión activa de la suite, compartida con las demás apps.
//! - [`probe`]: prueba de conexión IMAP/POP3/SMTP para el asistente de alta.
//! - El descubrimiento de servidores está en `iuredesk-autoconfig` (reexportado como [`autoconfig`]).
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

pub mod iurefficient;
pub mod probe;
pub mod secrets;
pub mod store;

pub use iuredesk_autoconfig as autoconfig;
pub use iurefficient::{IurefficientStatus, SharedSession, iurefficient_session};
pub use probe::{ProbeError, ProbeOptions, ProbeReport, test_incoming, test_outgoing};
pub use secrets::{KeyringStore, MemoryStore, SecretPurpose, SecretStore};
pub use store::AccountStore;

/// Error del crate.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("no hay carpeta de configuración en este sistema")]
    NoConfigDir,
    #[error("error de archivo: {0}")]
    Io(String),
    #[error("archivo de cuentas no válido: {0}")]
    Json(String),
    #[error("archivo de cuentas demasiado grande")]
    TooLarge,
    #[error("llavero del sistema: {0}")]
    Keyring(String),
}
