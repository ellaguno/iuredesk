//! Sincronización offline-first por cuenta (capa L3): servidor → almacén local, y cola de
//! operaciones locales → servidor (PLAN.md §4 «Flujo de datos», D6, D8).
//!
//! ## Estrategia
//! - **Primera sincronización de un buzón enorme**: lo más nuevo primero. Cabeceras de los
//!   últimos 30 días (como mucho 2 000 mensajes) y, en segundo plano, lo antiguo por lotes del UID
//!   más alto al más bajo. Los cuerpos se bajan al abrir ([`SyncHandle::fetch_body`]) y se
//!   precargan los N más recientes de la bandeja de entrada.
//! - **Incremental**: QRESYNC (`VANISHED` + cambios desde el último MODSEQ) o CONDSTORE
//!   (`CHANGEDSINCE`); sin ellos, UIDVALIDITY + UIDNEXT + comparación de banderas en una ventana de
//!   los últimos mensajes y un repaso completo periódico. Los borrados sin QRESYNC se detectan
//!   porque `EXISTS` no cuadra y se comparan los UID.
//! - **UIDVALIDITY cambiado**: la carpeta local se vacía (mismo id) y se baja de nuevo.
//! - **Cola de operaciones** (banderas, mover, borrar, subir): se replica antes de leer, en
//!   orden, con reintentos y espera exponencial. Conflictos: **gana el servidor** salvo que el
//!   mensaje tenga una operación local pendiente.
//! - **IDLE** en la bandeja de entrada (reemitido cada 29 min) y sondeo periódico del resto.
//! - **POP3**: descarga incremental por `UIDL` con «dejar copia» y «borrar tras N días».
//! - Tope de conexiones por cuenta (Gmail ≤ 15).
//!
//! Los cambios se anuncian por un canal de [`SyncEvent`] para la interfaz y las notificaciones.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

mod engine;
mod error;
mod imap_sync;
pub mod ops;
pub mod parse;
mod pool;
mod pop3_sync;
pub mod state;

use std::collections::HashMap;
use std::time::Duration;

use iuredesk_model::{AccountId, FolderId, MessageId};

pub use engine::{SyncEngine, SyncHandle};
pub use error::{Result, SyncError};
pub use iuredesk_pop3::Pop3Policy;

/// Ajustes de la sincronización.
#[derive(Clone, Debug)]
pub struct SyncOptions {
    /// Días de la ventana inicial.
    pub initial_days: u32,
    /// Mensajes máximos de la ventana inicial por carpeta.
    pub initial_max: usize,
    /// Mensajes por `UID FETCH` de cabeceras.
    pub header_batch: usize,
    /// Descargar lo antiguo en segundo plano.
    pub backfill: bool,
    /// Mensajes antiguos por tanda (entre tandas se atienden órdenes).
    pub backfill_slice: usize,
    /// Cuerpos que se precargan (los más recientes de la bandeja de entrada).
    pub prefetch_recent: usize,
    /// Tamaño máximo de un mensaje para precargarlo.
    pub prefetch_max_size: u64,
    /// Sondeo de todas las carpetas.
    pub poll_interval: Duration,
    /// Usar IDLE en la bandeja de entrada.
    pub idle: bool,
    /// Reemisión de IDLE.
    pub idle_reissue: Duration,
    /// Mensajes recientes cuyas banderas se comparan sin CONDSTORE.
    pub flag_window: usize,
    /// Cada cuánto se comparan todas las banderas sin CONDSTORE.
    pub full_rescan_every: Duration,
    /// Conexiones simultáneas por cuenta (se recorta al tope del proveedor).
    pub max_connections: usize,
    /// Intentos máximos de una operación encolada.
    pub op_max_attempts: u32,
    /// Tiempo de cada operación de red.
    pub io_timeout: Duration,
    /// Pedir COMPRESS=DEFLATE.
    pub compress: bool,
    /// Política POP3 por omisión.
    pub pop3_policy: Pop3Policy,
    /// Política POP3 por cuenta (`Account::id`).
    pub pop3_policies: HashMap<String, Pop3Policy>,
    /// Mensajes POP3 por pasada.
    pub pop3_max_per_cycle: usize,
}

impl Default for SyncOptions {
    fn default() -> Self {
        Self {
            initial_days: 30,
            initial_max: 2_000,
            header_batch: 250,
            backfill: true,
            backfill_slice: 2_000,
            prefetch_recent: 50,
            prefetch_max_size: 2 * 1024 * 1024,
            poll_interval: Duration::from_secs(5 * 60),
            idle: true,
            idle_reissue: iuredesk_imap::IDLE_REISSUE,
            flag_window: 2_000,
            full_rescan_every: Duration::from_secs(6 * 3600),
            max_connections: 4,
            op_max_attempts: 8,
            io_timeout: Duration::from_secs(60),
            compress: true,
            pop3_policy: Pop3Policy::default(),
            pop3_policies: HashMap::new(),
            pop3_max_per_cycle: 500,
        }
    }
}

/// Fase de trabajo para [`SyncEvent::Progress`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Lista de carpetas.
    Folders,
    /// Cabeceras recientes.
    Headers,
    /// Mensajes antiguos.
    Backfill,
    /// Cuerpos.
    Bodies,
    /// Cola de operaciones.
    Ops,
}

/// Muestra de correo nuevo para la notificación.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewMailSample {
    /// Mensaje.
    pub message: MessageId,
    /// Remitente para mostrar.
    pub from: String,
    /// Asunto.
    pub subject: String,
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
    /// Detalle técnico (para el registro, no para la interfaz).
    pub detail: String,
}

impl FriendlyError {
    pub(crate) fn new(account: Option<AccountId>, e: &SyncError) -> Self {
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

/// Aviso de la sincronización.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncEvent {
    /// Cambió el contenido o la lista de una carpeta.
    FolderUpdated {
        /// Cuenta.
        account: AccountId,
        /// Carpeta.
        folder: FolderId,
    },
    /// Correo nuevo en la bandeja de entrada (no se avisa en la primera sincronización).
    NewMail {
        /// Cuenta.
        account: AccountId,
        /// Mensajes nuevos.
        count: usize,
        /// Hasta 3 para mostrar.
        sample: Vec<NewMailSample>,
    },
    /// Avance.
    Progress {
        /// Cuenta.
        account: AccountId,
        /// Fase.
        phase: Phase,
        /// Carpeta, si aplica.
        folder: Option<FolderId>,
        /// Hecho.
        done: u64,
        /// Total previsto.
        total: u64,
    },
    /// Terminó una pasada de la cuenta.
    Synced {
        /// Cuenta.
        account: AccountId,
        /// Duración.
        took: Duration,
        /// Queda historial antiguo por bajar.
        backfill_pending: bool,
    },
    /// Error, ya explicado en es/en.
    Error(FriendlyError),
}
