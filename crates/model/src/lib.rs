//! Modelo de datos del PIM: cuentas, carpetas, mensajes, eventos, contactos, notas, tareas (capa L1).
//!
//! Tipos puros, sin E/S, serializables con serde. Los ids son locales (ver [`ids`]); los
//! identificadores remotos de cada proveedor se guardan como texto opaco.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

pub mod calendar;
pub mod contact;
pub mod ids;
pub mod mail;
pub mod note;
pub mod task;
pub mod threading;

pub use calendar::*;
pub use contact::*;
pub use ids::*;
pub use mail::*;
pub use note::*;
pub use task::*;
pub use threading::*;
