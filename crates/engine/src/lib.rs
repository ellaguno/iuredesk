//! Session, registro de comandos, deshacer, i18n (capa L4).
//!
//! Sin egui: la interfaz (L6) lee el estado de [`Session`] y despacha comandos por id
//! ([`command::COMMANDS`]). Lo lento corre en el runtime de fondo ([`background`]).
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

pub mod background;
pub mod body;
pub mod calendar;
pub mod command;
pub mod compose;
pub mod context;
pub mod iure;
pub mod lang;
pub mod onboarding;
pub mod prefs;
pub mod session;
pub mod time;

pub use background::{JobId, JobOutput};
/// Reexportados para la UI y la app (redactor, envío, sincronización).
pub use chrono_tz;
pub use command::{COMMANDS, Chord, CommandSpec};
pub use compose::{ComposeDraft, ComposeKind, Mailto};
pub use iuredesk_compose;
pub use iuredesk_pim_sync::SeriesScope;
pub use iuredesk_sync;
pub use lang::{Text, pick};
pub use prefs::{DensityPref, Prefs, ShortcutScheme, ThemePref};
pub use session::{
    AccountRow, AccountSync, CalState, Event, Fetched, FolderSel, IurePanel, IureSetup, LeadSuggestions, ListRow,
    LoginState, Module, Notice, NoticeKind, OfflineTransport, Outcome, PimBackend, Session, SessionConfig, SyncStatus,
    occurrence_start,
};
pub use time::Zone;

/// Error del motor.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EngineError {
    #[error("almacén: {0}")]
    Store(String),
    #[error("archivo: {0}")]
    Io(String),
    #[error("comando desconocido: {0}")]
    UnknownCommand(String),
    #[error("no hay mensaje seleccionado")]
    NoSelection,
    #[error("no hay ninguna cuenta configurada")]
    NoAccount,
    #[error("dato no válido: {0}")]
    Invalid(String),
}

impl EngineError {
    /// Mensaje para el usuario, en lenguaje humano y en los dos idiomas.
    pub fn text(&self) -> Text {
        match self {
            EngineError::Store(e) => Text::new(
                format!("Your local mailbox could not be updated ({e})."),
                format!("No se pudo actualizar tu buzón local ({e})."),
            ),
            EngineError::Io(e) => {
                Text::new(format!("A file could not be saved ({e})."), format!("No se pudo guardar un archivo ({e})."))
            }
            EngineError::UnknownCommand(c) => {
                Text::new(format!("Unknown command: {c}"), format!("Comando desconocido: {c}"))
            }
            EngineError::NoSelection => Text::new("Select a message first.", "Primero selecciona un mensaje."),
            EngineError::NoAccount => Text::new("Add an account first.", "Primero agrega una cuenta."),
            EngineError::Invalid(e) => Text::new(format!("Invalid data: {e}"), format!("Dato no válido: {e}")),
        }
    }
}

/// Resultado del motor.
pub type Result<T, E = EngineError> = std::result::Result<T, E>;

/// Clave para ordenar nombres sin distinguir mayúsculas ni acentos («Álvaro» junto a «Alberto»).
pub fn sort_key(s: &str) -> String {
    s.chars()
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'á' | 'à' | 'ä' | 'â' => 'a',
            'é' | 'è' | 'ë' | 'ê' => 'e',
            'í' | 'ì' | 'ï' | 'î' => 'i',
            'ó' | 'ò' | 'ö' | 'ô' => 'o',
            'ú' | 'ù' | 'ü' | 'û' => 'u',
            'ñ' => 'n',
            c => c,
        })
        .collect()
}
