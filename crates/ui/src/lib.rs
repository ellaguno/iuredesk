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
mod assistant;
mod calendar;
mod compose;
mod diagnostics;
mod event_editor;
mod header;
mod html;
mod import_ui;
mod invitation;
mod iure;
mod mail;
mod meeting;
mod onboarding;
mod opened;
mod pim;
mod reminders;
mod rich_edit;
mod rules_ui;
mod security_ui;
mod server_form;
mod settings;
mod suite_ui;
mod today;
mod widgets;

pub use app::{DeskApp, Incoming, Notifier, Options};
pub use diagnostics::FrameSummary;
pub use header::{ButtonLayout, ControlStyle, WindowButton, WindowChrome};
pub use iurefficient_ui;

thread_local! {
    /// Enlaces «abiertos» en este hilo cuando una prueba los captura (no se lanza el navegador).
    static CAPTURED_URLS: std::cell::RefCell<Option<Vec<String>>> = const { std::cell::RefCell::new(None) };
}

/// Pruebas: desde ahora, en este hilo, [`open_url`] guarda los enlaces en vez de abrirlos.
#[doc(hidden)]
pub fn capture_opened_urls() {
    CAPTURED_URLS.with(|c| *c.borrow_mut() = Some(Vec::new()));
}

/// Pruebas: los enlaces que se habrían abierto desde [`capture_opened_urls`].
#[doc(hidden)]
pub fn opened_urls() -> Vec<String> {
    CAPTURED_URLS.with(|c| c.borrow().clone().unwrap_or_default())
}

/// Abre un enlace con el manejador del sistema (solo `http`, `https` y `mailto`; en Linux,
/// `xdg-open` y después `gio`). Nunca bloquea. `false` si no se pudo lanzar.
pub fn open_url(href: &str) -> bool {
    let h = href.trim();
    let lower = h.to_ascii_lowercase();
    if !(lower.starts_with("https://") || lower.starts_with("http://") || lower.starts_with("mailto:")) {
        return false;
    }
    let captured = CAPTURED_URLS.with(|c| {
        c.borrow_mut().as_mut().map(|v| {
            v.push(h.to_string());
        })
    });
    if captured.is_some() {
        return true;
    }
    open::that_detached(h).is_ok()
}
