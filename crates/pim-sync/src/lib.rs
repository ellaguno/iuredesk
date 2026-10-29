//! Sincronización de calendario, contactos y tareas (capa L3): servidor → almacén local, y
//! ediciones locales → servidor por la cola de operaciones (PLAN.md §4, §5).
//!
//! # Proveedores (PLAN.md §5)
//! | Cuenta | Vía | Autenticación |
//! |---|---|---|
//! | iCloud | CalDAV + CardDAV (`caldav.icloud.com`, `contacts.icloud.com`) | contraseña de app (Basic) |
//! | Fastmail, Nextcloud, genérico | CalDAV + CardDAV (URL de la cuenta) | contraseña (Basic) |
//! | Google | CalDAV + CardDAV (`apidata.googleusercontent.com`, `googleapis.com/carddav`) + Google Tasks API (`tasks/v1`: Google CalDAV no sirve `VTODO`) | OAuth Bearer ([`iuredesk_oauth::TokenAudience::Api`]) |
//! | Microsoft 365 | Microsoft Graph: calendarios (delta de `calendarView`), contactos (delta), To Do (delta) | OAuth Bearer ([`iuredesk_oauth::TokenAudience::Api`]) |
//!
//! Los dos caminos están detrás del mismo rasgo interno (`provider::Provider`): `sync` (bajar) y
//! `push` (subir una operación de la cola).
//!
//! # CalDAV/CardDAV
//! - **Descubrimiento una vez** (RFC 6764 → principal → `calendar-home-set` /
//!   `addressbook-home-set`, buzón de salida de agenda) guardado en `sync_state`; luego, en cada
//!   pasada, lista de colecciones (alta, cambio de nombre/color, baja) y **sincronización
//!   incremental** de cada una: RFC 6578 `sync-collection` con caída a `getctag` + diferencia de
//!   ETags; descarga por `multiget`.
//! - El **objeto crudo** (`.ics`/`.vcf`) y su ETag se guardan junto al elemento
//!   ([`iuredesk_store::RawObject`]): al subir una edición se *parchea* ese objeto
//!   ([`map::ical::patch_event`], [`map::vcard::patch_contact`]) y se manda con `If-Match`.
//! - **412**: se relee del servidor y se fusiona a tres bandas ([`merge`]); si los cambios no
//!   chocan se vuelve a subir, si chocan **gana el servidor** y se avisa con
//!   [`PimEvent::Conflict`].
//!
//! # Microsoft Graph
//! Eventos por `calendarView/delta` en una ventana móvil (por omisión −90 / +400 días): Graph
//! entrega cada repetición como un evento propio (las series ya vienen expandidas), así que se
//! guardan sin `RRULE`; editar una repetición crea la excepción en el servidor. Contactos por
//! `/me/contacts/delta` (sin foto), To Do por `/me/todo/lists/{id}/tasks/delta`. Las subidas son
//! `PATCH` solo con los campos cambiados y `If-Match`; las respuestas a invitaciones van por
//! `/accept`, `/decline`, `/tentativelyAccept` (el servidor manda el correo al organizador).
//!
//! # Para la interfaz
//! - Leer apariciones: [`occurrences_between`] (reemplaza la expansión ingenua del motor).
//! - Editar: [`PimSyncHandle::save_event`], [`PimSyncHandle::rsvp`], … (o las funciones de
//!   [`ops`] si no hay sincronización en marcha). Contrato de la cola en [`ops`], incluido el
//!   `mail.imip_reply` para la bandeja de salida.
//! - Avisos: [`PimSyncHandle::events`].
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

pub mod account;
mod caldav;
mod engine;
pub mod error;
mod graph;
mod gtasks;
pub mod map;
pub mod merge;
mod occurrences;
pub mod ops;
mod provider;
pub mod series;

use std::time::Duration;

use chrono_tz::Tz;
use iuredesk_model::{AccountId, AddressBookId, CalendarId, TaskListId};
use iuredesk_store::PimRef;

pub use account::{DavAuth, GRAPH_BASE, PimAccount, PimSource};
pub use engine::{BusySlot, FreeBusy, PimSyncEngine, PimSyncHandle, account_id};
pub use error::{PimError, Result};
pub use gtasks::{GOOGLE_TASKS_BASE, GOOGLE_TASKS_LIST_PREFIX};
pub use occurrences::{MAX_OCCURRENCES, occurrences_between};
pub use ops::{ImipReply, OP_IMIP_REPLY, PimOp};
pub use series::SeriesScope;

/// Ajustes de la sincronización PIM.
#[derive(Clone, Debug)]
pub struct PimSyncOptions {
    /// Cada cuánto se sincroniza cada cuenta.
    pub poll_interval: Duration,
    /// Primera pasada al arrancar (si no, al cumplirse `poll_interval` o con `sync_now`).
    pub sync_on_start: bool,
    /// Zona del usuario: ubica horas flotantes y días completos.
    pub user_tz: Tz,
    /// Intentos máximos de una operación encolada.
    pub op_max_attempts: u32,
    /// Tiempo de cada petición de red.
    pub timeout: Duration,
    /// Ventana de eventos de Graph: días hacia atrás.
    pub graph_past_days: i64,
    /// Ventana de eventos de Graph: días hacia adelante.
    pub graph_future_days: i64,
    /// Páginas máximas de un delta de Graph por pasada.
    pub graph_max_pages: usize,
    /// Si el servidor CalDAV programa las invitaciones (RFC 6638, tiene buzón de salida: iCloud,
    /// Google, Fastmail…), no encolar además el correo iMIP al responder.
    pub trust_server_scheduling: bool,
    /// Idioma de los textos generados (asunto de las respuestas iMIP).
    pub spanish: bool,
}

impl Default for PimSyncOptions {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(10 * 60),
            sync_on_start: true,
            user_tz: chrono_tz::UTC,
            op_max_attempts: 8,
            timeout: Duration::from_secs(60),
            graph_past_days: 90,
            graph_future_days: 400,
            graph_max_pages: 200,
            trust_server_scheduling: true,
            spanish: true,
        }
    }
}

/// Cómo se resolvió un conflicto.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConflictResolution {
    /// Los cambios no chocaban: se juntaron y se subió el resultado.
    Merged,
    /// Chocaban (o el servidor lo borró): se quedó la versión del servidor.
    ServerWon,
}

/// Error explicado para el usuario.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FriendlyError {
    /// Cuenta afectada.
    pub account: Option<AccountId>,
    /// Mensaje en español.
    pub es: String,
    /// Mensaje en inglés.
    pub en: String,
    /// Se reintentará solo.
    pub transient: bool,
    /// Hay que volver a pedir la contraseña o iniciar sesión.
    pub needs_reauth: bool,
    /// Detalle técnico (para el registro).
    pub detail: String,
}

impl FriendlyError {
    pub(crate) fn new(account: Option<AccountId>, e: &PimError) -> Self {
        Self {
            account,
            es: e.message(true),
            en: e.message(false),
            transient: e.is_transient(),
            needs_reauth: e.needs_reauth(),
            detail: e.to_string(),
        }
    }
}

/// Aviso de la sincronización PIM (mismo patrón que `iuredesk_sync::SyncEvent`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PimEvent {
    /// Cambió la lista de calendarios, libretas o listas de tareas de la cuenta.
    CollectionsChanged {
        /// Cuenta.
        account: AccountId,
    },
    /// Cambiaron eventos de un calendario.
    CalendarChanged {
        /// Cuenta.
        account: AccountId,
        /// Calendario.
        calendar: CalendarId,
    },
    /// Cambiaron contactos de una libreta.
    ContactsChanged {
        /// Cuenta.
        account: AccountId,
        /// Libreta.
        book: AddressBookId,
    },
    /// Cambiaron tareas de una lista.
    TasksChanged {
        /// Cuenta.
        account: AccountId,
        /// Lista.
        list: TaskListId,
    },
    /// Un cambio local chocó con uno del servidor.
    Conflict {
        /// Cuenta.
        account: AccountId,
        /// Elemento. Con id sin asignar si ya no existe en local (un borrado que el servidor
        /// rechazó porque el elemento cambió: volverá en la próxima pasada).
        item: PimRef,
        /// Título para mostrar.
        summary: String,
        /// Qué se hizo.
        resolution: ConflictResolution,
    },
    /// Terminó una pasada de la cuenta.
    Synced {
        /// Cuenta.
        account: AccountId,
        /// Duración.
        took: Duration,
    },
    /// Error, ya explicado en es/en.
    Error(FriendlyError),
}
