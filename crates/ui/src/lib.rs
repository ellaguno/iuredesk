//! Interfaz de escritorio egui (capa L6): riel de módulos, carpetas con el panel «Hoy», lista
//! virtual densa, lectura (HTML con `html-view` en segundo plano), panel contextual, calendario,
//! contactos, notas, tareas, paleta de comandos, asistente de alta y redactor. PLAN.md §6.
//!
//! La UI no guarda datos: lee de [`iuredesk_engine::Session`] y despacha comandos por id.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

mod app;
mod calendar;
mod compose;
mod html;
mod iure;
mod mail;
mod onboarding;
mod pim;
mod rich_edit;
mod settings;
mod widgets;

pub use app::{DeskApp, Notifier, Options};
pub use iurefficient_ui;

/// Abre un enlace en el navegador del sistema (solo `http`, `https` y `mailto`). Nunca bloquea.
pub fn open_url(href: &str) -> bool {
    let h = href.trim();
    let lower = h.to_ascii_lowercase();
    if !(lower.starts_with("https://") || lower.starts_with("http://") || lower.starts_with("mailto:")) {
        return false;
    }
    open::that_detached(h).is_ok()
}
