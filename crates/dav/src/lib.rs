//! Calendario y contactos en el cable (capa L3): iCalendar (RFC 5545), vCard (3.0/4.0),
//! recurrencias, iTIP/iMIP (RFC 5546) y cliente WebDAV/CalDAV/CardDAV.
//!
//! Los tipos de aquí son **de cable**: describen lo que viaja por la red o en un `.ics`/`.vcf`.
//! Una tarea posterior los traduce a los tipos del dominio de `iuredesk-model`.
//!
//! # Módulos
//! - [`ical`]: árbol genérico de componentes y propiedades (sirve para iCalendar y vCard) con
//!   lectura y escritura **sin pérdida**: lo que no entendemos se conserva y se vuelve a escribir
//!   tal cual al hacer `PUT`.
//! - [`value`]: escapes de texto, fechas, fechas-hora y duraciones.
//! - [`tz`]: `TZID` → zona IANA (`chrono-tz`), incluidos los nombres de Windows que manda Outlook.
//! - [`calendar`]: vistas tipadas sobre `VEVENT`/`VTODO` (UID, SEQUENCE, fechas, asistentes…).
//! - [`recur`]: expansión de `RRULE`/`RDATE`/`EXDATE` y excepciones `RECURRENCE-ID` en una ventana.
//! - [`vcard`]: vistas tipadas sobre `VCARD`.
//! - [`itip`]: invitaciones por correo: `METHOD`, respuestas `REPLY`, nuevo vs. actualización.
//! - [`dav`]: cliente HTTP (descubrimiento, colecciones, sync RFC 6578, consultas, `PUT`/`DELETE`,
//!   free/busy).
//!
//! # Elección del parser iCalendar/vCard
//! Se evaluaron tres opciones (2026-10):
//!
//! | Opción | A favor | En contra |
//! |---|---|---|
//! | `calcard` 0.3 (Stalwart) | Muy completo: iCal, vCard, JSCalendar, zonas de Windows, su propio motor de recurrencias | ~240 `unwrap`/`expect`/`panic` fuera de pruebas; API grande y en movimiento; normaliza al reescribir (el `PUT` de vuelta no es byte a byte lo que llegó); arrastra `jmap-tools`, `ahash`, `hashify` |
//! | `icalendar` 0.17 | API cómoda para construir | Parser `nom` pensado para leer, no para editar y reescribir; vCard no; propiedades desconocidas se aplanan |
//! | **Propio** (elegido) | El formato de línea de contenido es pequeño (desplegar, `NOMBRE;PARAM=…:VALOR`, plegar a 75 octetos); control total de límites (tamaño, profundidad, número de propiedades) y de la regla «nunca fallar»; conserva orden, grupos vCard, parámetros y propiedades desconocidas | Hay que escribir las vistas tipadas y la tabla de zonas de Windows (hecho aquí) |
//!
//! Las recurrencias usan el crate [`rrule`] (Rust puro, MIT/Apache, con límite interno de
//! iteraciones y validación); encima ponemos nuestros propios topes por consulta ([`recur::Limits`]).
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable,
    clippy::indexing_slicing
)]

pub mod calendar;
pub mod dav;
pub mod ical;
pub mod itip;
pub mod recur;
pub mod tz;
pub mod value;
pub mod vcard;

pub use calendar::{Attendee, EventView, ICalendar, PartStat};
pub use dav::{
    AuthProvider, BasicAuth, BearerAuth, Collection, CollectionKind, DavClient, DavOptions, Discovery, NoAuth,
    Precondition, SyncOutcome,
};
pub use ical::{Component, Param, Property};
pub use itip::{ItipMessage, Method, UpdateKind};
pub use recur::{Instance, Limits};
pub use vcard::VCard;

/// Error del crate. Los mensajes van en español y sin jerga cuando llegan a la interfaz; el
/// detalle técnico viaja en el texto para el registro.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// El archivo o la respuesta iCalendar/vCard no se pudo leer.
    #[error("formato de calendario o contacto no válido: {0}")]
    Parse(String),
    /// Se superó un límite de seguridad (tamaño, profundidad, número de elementos).
    #[error("límite superado: {0}")]
    Limit(String),
    /// Falta un dato obligatorio (UID, DTSTART…).
    #[error("falta un dato obligatorio: {0}")]
    Missing(&'static str),
    /// Regla de recurrencia no válida o que no se puede expandir.
    #[error("regla de repetición no válida: {0}")]
    Recurrence(String),
    /// Error de red (conexión, TLS, tiempo de espera).
    #[error("no se pudo conectar con el servidor: {0}")]
    Network(String),
    /// El servidor rechazó las credenciales.
    #[error("el servidor rechazó el usuario o la contraseña")]
    Unauthorized,
    /// El recurso cambió en el servidor desde la última lectura (HTTP 412).
    #[error("el elemento cambió en el servidor; hay que volver a leerlo")]
    PreconditionFailed,
    /// El recurso no existe (HTTP 404/410).
    #[error("el elemento ya no existe en el servidor")]
    NotFound,
    /// El token de sincronización ya no vale (RFC 6578 `valid-sync-token`).
    #[error("el servidor pidió sincronizar todo de nuevo")]
    InvalidSyncToken,
    /// El servidor respondió con un estado inesperado.
    #[error("el servidor respondió {status}: {detail}")]
    Http {
        /// Código HTTP.
        status: u16,
        /// Contexto (método y URL).
        detail: String,
    },
    /// La respuesta XML no tiene la forma esperada.
    #[error("respuesta del servidor no válida: {0}")]
    Xml(String),
    /// URL no válida.
    #[error("dirección no válida: {0}")]
    Url(String),
    /// El servidor no ofrece la función pedida.
    #[error("el servidor no admite {0}")]
    Unsupported(&'static str),
    /// No se pudo obtener la credencial (proveedor de tokens).
    #[error("no se pudo obtener la credencial: {0}")]
    Auth(String),
    /// El servidor aceptó la credencial pero le faltan permisos (HTTP 403 con
    /// `insufficient_scope`, p. ej. Google CardDAV sin el alcance `carddav`): hay que volver a
    /// iniciar sesión para concederlos.
    #[error("faltan permisos en el inicio de sesión: {0}")]
    InsufficientScope(String),
}

/// Resultado del crate.
pub type Result<T> = std::result::Result<T, Error>;
