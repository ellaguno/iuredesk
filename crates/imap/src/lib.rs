//! Cliente IMAP asíncrono (capa L3): IMAP4rev1/IMAP4rev2 sobre tokio + rustls.
//!
//! **Decisión de códec:** propio, no `imap-codec`/`imap-next`. Nos hacía falta tolerancia a los
//! desvíos reales de los servidores (Exchange, Gmail, Dovecot antiguos), topes en cada literal y
//! lista, y una delimitación de respuestas segura ante cancelación para IDLE; con un códec de
//! ~800 líneas probado con propiedades lo controlamos todo sin acoplarnos a la API cambiante de
//! esos crates. La gramática se interpreta en dos pasos: valores genéricos ([`codec::Item`]) y
//! luego respuestas tipadas ([`codec::Untagged`]).
//!
//! Extensiones: IDLE, CONDSTORE, QRESYNC, UIDPLUS, MOVE, ENABLE, ESEARCH, SPECIAL-USE,
//! COMPRESS=DEFLATE (Rust puro), LITERAL+/LITERAL-, SASL-IR, ID, UNSELECT, NOTIFY (RFC 5465) y
//! `X-GM-EXT-1` de Gmail (`X-GM-MSGID`, `X-GM-THRID`, `X-GM-LABELS` y `STORE X-GM-LABELS`).
//!
//! Con la *feature* `test-server`, [`testing`] trae un servidor IMAP falso y programable.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

mod client;
pub mod codec;
mod error;
pub mod mutf7;
pub mod seqset;
#[cfg(feature = "test-server")]
pub mod testing;
mod transport;

pub use client::{
    Capabilities, CopyUid, DEFAULT_IO_TIMEOUT, FetchResult, IDLE_REISSUE, IdleEvent, IdleOutcome, ImapClient,
    ImapConfig, MAX_SEARCH_RESULTS, MailboxInfo, Qresync, SelectOptions, SelectResult, Selected, StoreMode,
    format_internal_date,
};
pub use codec::{BodyStructure, Envelope, FetchData, PartInfo};
pub use error::{ImapError, Result};
pub use iuredesk_net::NetError;
pub use seqset::SeqSet;
pub use transport::Limits;

/// Listas de atributos de `FETCH` habituales.
pub mod fetch_items {
    /// Atributos de Gmail (`X-GM-EXT-1`): id estable, hilo y etiquetas.
    pub const GMAIL: &str = "X-GM-MSGID X-GM-THRID X-GM-LABELS";

    /// Cabeceras como [`headers`] más los atributos de Gmail.
    pub fn gmail_headers(modseq: bool) -> String {
        format!(
            "(UID FLAGS INTERNALDATE RFC822.SIZE ENVELOPE BODYSTRUCTURE {THREAD_HEADERS} {GMAIL}{})",
            if modseq { " MODSEQ" } else { "" }
        )
    }

    /// Banderas y etiquetas de Gmail.
    pub fn gmail_flags(modseq: bool) -> String {
        format!("(UID FLAGS X-GM-MSGID X-GM-LABELS{})", if modseq { " MODSEQ" } else { "" })
    }

    /// Extracto sin bajar el mensaje: tipo de contenido y los primeros `len` bytes del texto
    /// (`BODY.PEEK[TEXT]<0.len>`), que bastan para la vista previa de la lista.
    pub fn preview(len: u64) -> String {
        format!(
            "(UID BODY.PEEK[HEADER.FIELDS (CONTENT-TYPE CONTENT-TRANSFER-ENCODING)] BODY.PEEK[TEXT]<0.{}>)",
            len.clamp(1, 64 * 1024)
        )
    }

    /// Cabeceras de hilo y de lista que no trae `ENVELOPE`.
    pub const THREAD_HEADERS: &str = "BODY.PEEK[HEADER.FIELDS (REFERENCES IN-REPLY-TO LIST-ID)]";

    /// Lo necesario para listar y enhebrar sin descargar el cuerpo.
    pub fn headers(modseq: bool) -> String {
        format!(
            "(UID FLAGS INTERNALDATE RFC822.SIZE ENVELOPE BODYSTRUCTURE {THREAD_HEADERS}{})",
            if modseq { " MODSEQ" } else { "" }
        )
    }

    /// Solo banderas (para comparar con lo local).
    pub fn flags(modseq: bool) -> String {
        format!("(UID FLAGS{})", if modseq { " MODSEQ" } else { "" })
    }

    /// Mensaje completo sin marcarlo como leído.
    pub fn full() -> String {
        "(UID FLAGS BODY.PEEK[])".to_string()
    }

    /// Descarga parcial: `BODY.PEEK[section]<origin.len>`.
    pub fn partial(section: &str, origin: u64, len: u64) -> String {
        let section: String = section.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '.').take(64).collect();
        format!("(UID BODY.PEEK[{section}]<{origin}.{len}>)")
    }
}
