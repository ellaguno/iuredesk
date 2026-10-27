//! Asistente ✦ de IureDesk (capa L4, sin interfaz). PLAN.md §8.
//!
//! - [`provider`]: el rasgo [`AiProvider`] (flujo de texto por canal, cancelación, tiempo total,
//!   topes, reintentos ante 429 con `Retry-After`) y [`run_chat`].
//! - [`providers`]: Iurefficient (por omisión, `global_chat` y chat de caso con la sesión de la
//!   suite), Microsoft 365 Copilot (experimental, con sondeo de capacidad), compatible con OpenAI
//!   (OpenRouter, Ollama, llama.cpp) y Anthropic (Messages API con SSE).
//! - [`config`]: proveedores en `<config>/iuredesk/assistant.json`; claves en el llavero.
//! - [`tasks`]: `prepare_*` (sin red: petición + [`DataScope`]) y [`tasks::run`] → [`Proposal`].
//! - [`schema`]: resultados tipados y validados; [`json_repair`]: JSON reparado o rechazado.
//! - [`privacy`]: qué sale del equipo y enmascarado de correos y teléfonos.
//! - [`actions`]: «la IA propone, el usuario confirma»: crear lead, ticket, tarea o evento en
//!   Iurefficient sólo con una propuesta aprobada.
//! - Instrucciones en `prompts/<tarea>.<es|en>.md` ([`prompts`]).
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

pub mod actions;
pub mod config;
pub mod error;
mod http;
pub mod input;
pub mod json_repair;
pub mod privacy;
pub mod prompts;
pub mod provider;
pub mod providers;
pub mod schema;
pub mod tasks;

pub use actions::{ActionOutcome, Approved, ProposedAction};
pub use config::{AssistantConfig, ProviderConfig, ProviderContext, ProviderKind, build_provider};
pub use error::AiError;
pub use input::{Addr, EmailInput, Lang};
pub use privacy::{DataField, DataScope, Redactor, ScopeOptions};
pub use provider::{AiProvider, ChatMessage, ChatRequest, Completion, Limits, Role, StreamEvent, Usage, run_chat};
pub use tasks::{Length, Prepared, Proposal, Target, TaskOptions, Tone};
pub use tokio_util::sync::CancellationToken;
