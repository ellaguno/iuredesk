//! Reglas y ayudantes de la bandeja (capa L3, sin egui): lo que en Outlook son «Reglas», en Gmail
//! «Filtros» y «Posponer», y en Thunderbird «Filtros de mensajes» (PLAN.md §6).
//!
//! - [`rule`]: reglas con condiciones y acciones en lenguaje de cualquier usuario, y su
//!   evaluación ([`eval`]) y aplicación local primero ([`apply`]) por la cola de `sync`.
//! - [`sieve`] y [`managesieve`]: traducción reglas ⇄ Sieve (RFC 5228 + `fileinto`, `imap4flags`,
//!   `vacation`) y cliente ManageSieve (RFC 5804) para que corran en el servidor cuando se puede.
//! - [`snooze`]: «Posponer» con horas de cualquier usuario («Más tarde», «Mañana»…).
//! - [`followup`]: «Recordarme si no responden en N días».
//! - [`iure_mail`]: correo que envía la propia instancia de Iurefficient (avisos, menciones).
//! - [`focus`]: «Enfocados / Otros» por heurísticas locales (sin IA).
//! - [`unsubscribe`]: baja en un clic (RFC 8058) o por correo (`mailto:`).
//! - [`vacation`]: respuesta automática local con protección contra bucles (RFC 3834).
//! - [`book`]: persistencia en el almacén local (`sync_state`).
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

pub mod apply;
pub mod book;
pub mod category;
pub mod error;
pub mod eval;
pub mod facts;
pub mod focus;
pub mod followup;
pub mod iure_mail;
pub mod managesieve;
pub mod rule;
pub mod sieve;
pub mod snooze;
pub mod unsubscribe;
pub mod vacation;

pub use apply::{Applied, ApplyCtx, Effect, UndoStep, apply_plan};
pub use book::{CategoryPrefs, RuleBook, SieveAccount};
pub use category::{Category, CategoryReason};
pub use error::{Result, RulesError};
pub use eval::{CompiledRules, Plan};
pub use facts::{Facts, Headers};
pub use focus::{Focus, FocusReason, Signals};
pub use followup::{FollowState, FollowUp, FollowUpBook};
pub use rule::{Action, Condition, Field, FolderRef, Rule, TextOp};
pub use snooze::{SnoozeBook, SnoozeMode, SnoozePreset, Snoozed};
pub use vacation::Vacation;

/// Texto en los dos idiomas (`en`, `es`) para que la capa de arriba elija.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Bi {
    /// Inglés.
    pub en: String,
    /// Español.
    pub es: String,
}

impl Bi {
    /// Crea un texto bilingüe.
    pub fn new(en: impl Into<String>, es: impl Into<String>) -> Self {
        Self { en: en.into(), es: es.into() }
    }
}

/// Dirección en minúsculas y sin espacios (para comparar).
pub fn norm_addr(addr: &str) -> String {
    addr.trim().trim_matches(|c| c == '<' || c == '>').to_lowercase()
}

/// Dominio de una dirección (`ana@Example.com` → `example.com`).
pub fn domain_of(addr: &str) -> &str {
    addr.rsplit_once('@').map_or("", |(_, d)| d.trim().trim_end_matches('>'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addr_helpers() {
        assert_eq!(norm_addr(" <Ana@Example.COM> "), "ana@example.com");
        assert_eq!(domain_of("ana@Example.com"), "Example.com");
        assert_eq!(domain_of("sin-arroba"), "");
    }
}
