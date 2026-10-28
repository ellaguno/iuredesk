//! Redactor de IureDesk (capa L3): de un borrador a un correo enviado.
//!
//! - [`address`]: direcciones escritas por el usuario → buzones validados (IDNA para el dominio).
//! - [`rich`]: [`RichBody`], el documento del editor (párrafos, estilos, enlaces, listas, citas,
//!   imágenes en línea, títulos, código) que se serializa a HTML apto para correo (estilos en
//!   línea, sin tablas, amable con Outlook) y a `text/plain; format=flowed`. Importa HTML
//!   (html5ever, a lo mejor posible) y Markdown (pulldown-cmark), y exporta Markdown.
//! - [`draft`]: [`Draft`], el borrador: identidad, destinatarios, asunto, cuerpo, adjuntos,
//!   prioridad, acuse de lectura, envío programado e invitación de calendario.
//! - [`reply`]: responder, responder a todos y reenviar a partir del correo crudo original.
//! - [`mime`]: borrador → mensaje RFC 5322 con `mail-builder` (alternative/related/mixed,
//!   RFC 2047/2231, Message-ID por dominio de la identidad, Bcc fuera de las cabeceras).
//! - [`signature`] y [`template`]: firmas por identidad y plantillas guardadas en el almacén.
//! - [`transport`]: el rasgo [`Transport`] y su implementación SMTP con `lettre` (rustls,
//!   465/587, PLAIN/LOGIN/XOAUTH2).
//! - [`outbox`]: bandeja de salida sobre la cola de operaciones del almacén: ventana para deshacer,
//!   envío programado, reintentos con espera creciente y operaciones para `sync` (guardar en
//!   Enviados, marcar el original como respondido o reenviado).
//!
//! Nada aquí abre diálogos ni bloquea la UI: el envío es asíncrono (tokio) y los errores traen un
//! mensaje humano en español e inglés (`message(lang)`).
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable,
    clippy::indexing_slicing
)]

pub mod address;
pub mod draft;
mod encode;
pub mod error;
pub mod lang;
pub mod mime;
pub mod ops;
pub mod outbox;
pub mod reply;
pub mod rich;
pub mod secure;
pub mod signature;
pub mod template;
pub mod transport;

pub use address::{AddressError, needs_smtputf8, parse_address, parse_address_list};
pub use draft::{
    Attachment, AttachmentKind, AttachmentSource, CalendarMethod, CalendarPart, Draft, ForwardMode, Forwarded,
    OriginalKind, OriginalRef, Priority,
};
pub use error::{ComposeError, Result};
pub use lang::Lang;
pub use mime::{
    BuildOptions, BuiltMessage, ContentResolver, FsResolver, SizeAdvice, StoreResolver, build_message,
    generate_message_id,
};
pub use ops::SyncOp;
pub use outbox::{
    Clock, ManualClock, Outbox, OutboxConfig, OutboxEntry, OutboxId, OutboxState, SendOutcome, SystemClock,
    TransportSource,
};
pub use reply::{ReplyMode, ReplyOptions, SubjectStyle, forward, forward_subject, reply, reply_subject};
pub use rich::{Block, Image, Inline, RichBody, Span, Style};
pub use signature::{Signature, SignatureBook, SignaturePlacement};
pub use template::TemplateStore;
pub use transport::{
    DEFAULT_SMTP_TIMEOUT, OutgoingMessage, SendError, SendErrorKind, SendReceipt, SmtpTransport, Transport,
};

/// Valor de la cabecera `User-Agent`.
pub const USER_AGENT: &str = concat!("IureDesk/", env!("CARGO_PKG_VERSION"));
