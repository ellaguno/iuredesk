//! Integración con el escritorio de cada sistema (capa L3, sin interfaz), R1.2:
//!
//! - [`notify`]: notificaciones del sistema con clic que abre el elemento (Linux: D-Bus con
//!   `notify-rust`/zbus; Windows: avisos de WinRT con el `AppUserModelID` del instalador;
//!   macOS: `UNUserNotificationCenter` por objc2, solo dentro del `.app`; el clic lo recibe
//!   `iuredesk-macos-events`).
//! - [`autostart`]: «Abrir IureDesk al iniciar sesión» (Linux: `~/.config/autostart`; Windows:
//!   `HKCU\…\Run`; macOS: `~/Library/LaunchAgents`).
//! - [`memory`]: memoria del proceso en Windows y macOS (en Linux la lee la UI de `/proc`).
//!
//! Todo con las bibliotecas del SO por enlaces en Rust (sin C ni `unsafe` propio; el de macOS
//! vive en `iuredesk-macos-events`); lo que un
//! sistema no permite devuelve `None` o un error con texto humano, nunca se cae.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

pub mod autostart;
pub mod memory;
pub mod notify;

/// Identificador de la app: id de AppStream y de Flatpak, `CFBundleIdentifier` en macOS y
/// `AppUserModelID` en Windows (el acceso directo del instalador debe llevar este mismo).
pub const APP_ID: &str = "com.iurefficient.IureDesk";

/// Argumento con el que arranca la sesión del sistema (ventana minimizada).
pub const MINIMIZED_ARG: &str = "--minimized";

/// Fallo de la integración con el sistema.
#[derive(Debug, thiserror::Error)]
pub enum DesktopError {
    /// Archivo o registro.
    #[error("{0}")]
    Io(#[from] std::io::Error),
    /// El sistema no lo admite (o no se encontró dónde guardarlo).
    #[error("no disponible en este sistema: {0}")]
    Unsupported(&'static str),
    /// Error del sistema (Windows).
    #[error("{0}")]
    System(String),
}

impl DesktopError {
    /// Mensaje para el usuario (inglés, español).
    pub fn human(&self) -> (String, String) {
        match self {
            DesktopError::Io(e) => {
                (format!("The setting could not be saved ({e})."), format!("No se pudo guardar el ajuste ({e})."))
            }
            DesktopError::Unsupported(_) => {
                ("This system does not allow it.".to_string(), "Este sistema no lo permite.".to_string())
            }
            DesktopError::System(e) => {
                (format!("The system refused the change ({e})."), format!("El sistema no aceptó el cambio ({e})."))
            }
        }
    }
}
