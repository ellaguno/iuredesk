//! Apps hermanas de la suite Iurefficient (capa L4, sin egui).
//!
//! IureDesk habla con IureEditor, IureTranscribe, IureVoz, IureOCR, IureDav e IureProject
//! **por frontera de proceso**: deep links abiertos por el SO, MCP por stdio (`<app> --mcp`) y el
//! socket de control de IureVoz. Nada se enlaza: si una app no está instalada, la acción devuelve
//! [`Outcome::NotInstalled`] con un texto para invitar a instalarla — nunca un error.
//!
//! - [`suite`]: [`Suite::detect`] (qué apps hay y dónde), [`SisterApp`], [`InstallHint`],
//!   [`Outcome`].
//! - [`mcp_client`]: cliente MCP genérico por stdio (JSON-RPC 2.0, negociación 2025-06-18,
//!   `tools/list`, `tools/call`, tiempos de espera, el proceso hijo muere con el cliente).
//! - [`editor`]: abrir en IureEditor (archivo, texto, documento de Iurefficient).
//! - [`transcribe`]: transcribir una grabación con IureTranscribe y seguir el trabajo.
//! - [`ocr`]: PDF buscable + texto de un adjunto escaneado con IureOCR.
//! - [`voice`]: decir, dictar y preguntar sí/no con IureVoz (socket primero, MCP después).
//! - [`dav`]: unidades de IureDav, «Guardar adjunto en Iurefficient» y adjuntos grandes como
//!   enlace.
//!
//! Todas las llamadas son **bloqueantes** (procesos hijos, sockets): la interfaz las lanza en el
//! runtime de fondo (`spawn_blocking`), nunca en el hilo de dibujo. Ver `docs/integraciones.md`.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable,
    clippy::indexing_slicing
)]

pub mod dav;
pub mod editor;
pub mod error;
pub mod mcp_client;
pub mod ocr;
pub mod opener;
pub mod suite;
pub mod transcribe;
pub mod voice;

pub use dav::{DavMount, LARGE_ATTACHMENT_BYTES, LinkAdvice, sanitize_file_name};
pub use editor::{EditorOpened, EditorTarget, editor_link};
pub use error::{IntegrationError, Result};
pub use mcp_client::{McpClient, McpOptions, ServerInfo, ToolInfo, ToolResult};
pub use ocr::{OcrOptions, OcrResult};
pub use opener::{Opener, SystemOpener};
pub use suite::{AppStatus, InstallHint, Outcome, SisterApp, Suite, Text};
pub use transcribe::{JobState, Transcript, TranscriptFile, TranscriptionJob};
