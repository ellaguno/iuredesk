//! Cliente WebDAV/CalDAV/CardDAV sobre `reqwest` (rustls) y `quick-xml`.
//!
//! - [`DavClient`]: transporte con tiempos de espera, tope de tamaño de respuesta, redirecciones
//!   seguidas a mano (conservan `PROPFIND`/`REPORT` y su cuerpo) y reintento tras 401 para
//!   tokens OAuth renovables.
//! - Descubrimiento (RFC 6764 + RFC 5397): `.well-known`, `current-user-principal`,
//!   `calendar-home-set`, `addressbook-home-set`, buzón de salida de agenda (RFC 6638).
//! - Colecciones con nombre, color, `getctag`, `sync-token` y componentes admitidos.
//! - Sincronización RFC 6578 (`sync-collection`) con caída a `getctag` + diferencia de ETags.
//! - Consultas `calendar-query` por rango de tiempo, `multiget`, `GET`, `PUT` con
//!   `If-Match`/`If-None-Match`, `DELETE` y disponibilidad (free/busy).
//! - [`quirks`]: iCloud, Google, Fastmail, Nextcloud, Radicale, Stalwart, Iurefficient.

mod auth;
mod client;
mod discovery;
mod ops;
pub mod quirks;
mod sync;
pub mod xml;

pub use auth::{AuthProvider, BasicAuth, BearerAuth, NoAuth};
pub use client::{DavClient, DavOptions, Depth, RawResponse};
pub use discovery::Discovery;
pub use ops::{BusyPeriod, FreeBusyReply, Precondition, PutResult, Resource};
pub use quirks::{Provider, Service};
pub use sync::{Item, SyncMethod, SyncOutcome, SyncPage, SyncState};

use reqwest::Url;

/// Tipo de colección.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CollectionKind {
    /// Calendario (eventos y/o tareas).
    Calendar,
    /// Libreta de direcciones.
    AddressBook,
}

/// Un calendario o una libreta de direcciones del servidor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collection {
    /// URL absoluta.
    pub url: Url,
    /// Tipo.
    pub kind: CollectionKind,
    /// Nombre para mostrar.
    pub display_name: Option<String>,
    /// Descripción.
    pub description: Option<String>,
    /// Color (`#RRGGBB` o `#RRGGBBAA`, como lo manda Apple).
    pub color: Option<String>,
    /// `getctag` (CalendarServer): cambia con cualquier cambio dentro.
    pub ctag: Option<String>,
    /// `sync-token` (RFC 6578), si el servidor lo admite.
    pub sync_token: Option<String>,
    /// Componentes admitidos (`VEVENT`, `VTODO`…); vacío = no lo dijo.
    pub components: Vec<String>,
    /// Sin permiso de escritura (según `current-user-privilege-set`).
    pub read_only: bool,
}
