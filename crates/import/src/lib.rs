//! Migración a IureDesk (capa L3): importa correo, contactos, calendario, tareas y notas desde
//! Thunderbird, Apple Mail, Outlook (`.pst`), Gmail Takeout y formatos sueltos (`.mbox`, Maildir,
//! `.eml`, `.emlx`, `.ics`, `.vcf`). Todo local: nada sale de la máquina.
//!
//! # Uso
//! ```no_run
//! # fn main() -> Result<(), iuredesk_import::ImportError> {
//! let store = iuredesk_store::Store::open("/tmp/perfil")?;
//! let report = iuredesk_import::Importer::new(&store)
//!     .run(&["/home/ana/.thunderbird".into()], |ev| println!("{ev:?}"))?;
//! println!("{}", report.summary("es"));
//! # Ok(()) }
//! ```
//!
//! - **Por flujo:** los mbox se leen línea a línea y los `.ics`/`.vcf` por tandas; un mensaje o
//!   un objeto que supera el máximo ([`ImportOptions::max_message_bytes`],
//!   [`ImportOptions::max_object_bytes`]) se omite con motivo. Los mensajes se guardan por lotes
//!   ([`iuredesk_store::Store::put_messages`]).
//! - **Reanudable:** el estado vive en `<almacén>/import/checkpoint.json` (archivos y carpetas
//!   terminados, byte alcanzado en cada mbox) y se escribe tras confirmar cada lote. Cancelar
//!   ([`Importer::cancel_flag`]) o un corte solo repiten el último lote.
//! - **Sin duplicados:** clave `Message-ID` + huella del cuerpo (CRLF/LF y cabeceras volátiles
//!   normalizadas) guardada como id remoto en la carpeta; reimportar no duplica. Contactos, eventos
//!   y tareas por `UID`.
//! - **Fiel:** banderas (leído, destacado, respondido, reenviado, borrador) y fechas del origen;
//!   funciones de carpeta (Entrada, Enviados, Borradores, Papelera, No deseado, Archivo) por nombre
//!   en varios idiomas ([`roles`]).
//! - **Informe:** [`ImportReport`] con recuentos, omitidos con motivo (es/en) y avisos; progreso
//!   por [`ImportEvent`].
//!
//! Detalles por origen: [`thunderbird`], [`apple`], [`pst`], [`mail`] (mbox, Maildir, Takeout).
//! Decisiones (lector PST, SQLite propio): `docs/import.md`.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

pub mod apple;
pub mod checkpoint;
pub mod emlx;
pub mod error;
pub mod headers;
pub mod mail;
pub mod mbox;
pub mod pim;
pub mod pst;
pub mod report;
pub mod roles;
mod sink;
pub mod source;
pub mod sqlite;
pub mod thunderbird;
mod util;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use chrono_tz::Tz;
use iuredesk_store::Store;

pub use error::{ImportError, Result};
pub use report::{Counts, ImportEvent, ImportReport, Notice, Progress, SkipReason, Skipped, SourceKind};
pub use source::{DetectedSource, detect, find_installed};

use crate::checkpoint::Checkpoint;
use crate::sink::Sink;

/// Ajustes de la importación.
#[derive(Clone, Debug)]
pub struct ImportOptions {
    /// Tamaño máximo de un mensaje (los mayores se omiten con motivo). Por omisión 128 MiB.
    pub max_message_bytes: u64,
    /// Tamaño máximo de una tarjeta o un componente de calendario. Por omisión 8 MiB.
    pub max_object_bytes: usize,
    /// Archivos máximos que se recorren al estimar el progreso.
    pub max_files: usize,
    /// Reanudar una importación anterior (por omisión `true`).
    pub resume: bool,
    /// Nombre de la cuenta local para correo suelto (`.mbox`, `.eml`, carpetas). Por omisión
    /// «Importado» / «Imported».
    pub account_name: Option<String>,
    /// Zona para horas «flotantes» y fechas de día completo.
    pub time_zone: Tz,
    /// Idioma de los nombres que se crean (`es` o `en`).
    pub lang: String,
    /// Mensajes sin banderas en el origen (`.eml`, mbox sin `Status`): marcarlos como leídos
    /// (por omisión `true`: quien migra no quiere miles de no leídos).
    pub unknown_as_read: bool,
    /// Mensajes por lote.
    pub batch_messages: usize,
    /// Bytes por lote.
    pub batch_bytes: usize,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            max_message_bytes: 128 * 1024 * 1024,
            max_object_bytes: 8 * 1024 * 1024,
            max_files: 2_000_000,
            resume: true,
            account_name: None,
            time_zone: chrono_tz::UTC,
            lang: "es".into(),
            unknown_as_read: true,
            batch_messages: 200,
            batch_bytes: 32 * 1024 * 1024,
        }
    }
}

impl ImportOptions {
    pub(crate) fn is_en(&self) -> bool {
        self.lang.starts_with("en")
    }

    /// Texto en el idioma de la importación.
    pub(crate) fn text<'s>(&self, en: &'s str, es: &'s str) -> &'s str {
        if self.is_en() { en } else { es }
    }
}

/// Importador.
pub struct Importer<'a> {
    store: &'a Store,
    opts: ImportOptions,
    cancel: Option<Arc<AtomicBool>>,
}

impl<'a> Importer<'a> {
    /// Importador sobre un almacén abierto.
    pub fn new(store: &'a Store) -> Self {
        Self { store, opts: ImportOptions::default(), cancel: None }
    }

    /// Cambia los ajustes.
    pub fn options(mut self, opts: ImportOptions) -> Self {
        self.opts = opts;
        self
    }

    /// Bandera para cancelar desde otro hilo (se comprueba entre elementos).
    pub fn cancel_flag(mut self, flag: Arc<AtomicBool>) -> Self {
        self.cancel = Some(flag);
        self
    }

    /// Importa `paths` (archivos o directorios) y devuelve el informe. `on_event` recibe el
    /// progreso, las omisiones y los avisos (desde este mismo hilo).
    ///
    /// Solo devuelve error si no se puede seguir (el almacén falla, un origen no existe); lo que
    /// falla en un elemento queda en [`ImportReport::skipped`]. Cancelar no es error:
    /// [`ImportReport::cancelled`].
    pub fn run(&self, paths: &[PathBuf], mut on_event: impl FnMut(&ImportEvent)) -> Result<ImportReport> {
        let dir = self.store.root().join("import");
        let checkpoint = if self.opts.resume { Checkpoint::load(&dir)? } else { Checkpoint::disabled() };
        if !self.opts.resume {
            // Empezar de cero también borra el estado guardado (la deduplicación sigue evitando
            // duplicados).
            let mut on_disk = Checkpoint::load(&dir)?;
            if on_disk.has_history() {
                on_disk.clear()?;
            }
        }
        let mut sink = Sink::new(self.store, &self.opts, checkpoint, self.cancel.as_deref(), &mut on_event);
        sink.set_total_bytes(source::estimate_bytes(paths, self.opts.max_files));
        let mut outcome = Ok(());
        for p in paths {
            outcome = source::import_path(&mut sink, p);
            if outcome.is_err() {
                break;
            }
        }
        let flushed = sink.flush();
        let mut report = std::mem::take(&mut sink.report);
        match outcome.and(flushed) {
            Ok(()) => {}
            Err(ImportError::Cancelled) => report.cancelled = true,
            Err(e) => return Err(e),
        }
        Ok(report)
    }

    /// Olvida el estado de reanudación (la próxima importación relee todo; no duplica).
    pub fn reset(&self) -> Result<()> {
        Checkpoint::load(&self.store.root().join("import"))?.clear()
    }
}
