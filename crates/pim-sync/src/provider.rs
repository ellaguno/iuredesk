//! Rasgo común de los proveedores (CalDAV/CardDAV y Graph) y contexto de una cuenta.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use iuredesk_model::AccountId;
use iuredesk_store::{CollectionRef, PimRef, Store};
use tokio::sync::broadcast;

use crate::account::{Creds, PimAccount};
use crate::error::{PimError, Result};
use crate::ops::PimOp;
use crate::{ConflictResolution, FriendlyError, PimEvent, PimSyncOptions};

/// Futuro en caja (el rasgo se usa como `dyn`).
pub(crate) type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Contexto compartido de una cuenta.
pub(crate) struct Ctx {
    pub store: Arc<Store>,
    pub acct: AccountId,
    pub account: PimAccount,
    pub opts: Arc<PimSyncOptions>,
    pub events: broadcast::Sender<PimEvent>,
    pub creds: Arc<Creds>,
}

impl Ctx {
    pub fn emit(&self, e: PimEvent) {
        let _ = self.events.send(e);
    }

    pub fn emit_error(&self, e: &PimError) {
        self.emit(PimEvent::Error(FriendlyError::new(Some(self.acct), e)));
    }

    pub fn conflict(&self, item: PimRef, summary: &str, resolution: ConflictResolution) {
        self.emit(PimEvent::Conflict { account: self.acct, item, summary: summary.to_string(), resolution });
    }

    /// Aviso de cambio en la colección local.
    pub fn changed(&self, c: CollectionRef) {
        let account = self.acct;
        self.emit(match c {
            CollectionRef::Calendar(calendar) => PimEvent::CalendarChanged { account, calendar },
            CollectionRef::AddressBook(book) => PimEvent::ContactsChanged { account, book },
            CollectionRef::TaskList(list) => PimEvent::TasksChanged { account, list },
        });
    }
}

/// Un proveedor PIM: baja cambios y sube operaciones de la cola.
pub(crate) trait Provider: Send + Sync {
    /// Colecciones (alta/baja/cambios) y cambios de cada una.
    fn sync<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<()>>;

    /// Replica una operación. `Ok` también cuando el conflicto se resolvió (fusión o gana el
    /// servidor); `Err` para reintentar.
    fn push<'a>(&'a self, ctx: &'a Ctx, op: &'a PimOp) -> BoxFuture<'a, Result<()>>;
}

/// Clave del estado de sincronización de una colección remota: un calendario CalDAV con
/// `VEVENT` y `VTODO` da un calendario y una lista de tareas locales con la misma ruta, y el
/// estado vive en el calendario.
pub(crate) fn state_ref(store: &Store, acct: AccountId, c: CollectionRef) -> Result<CollectionRef> {
    let CollectionRef::TaskList(l) = c else { return Ok(c) };
    let Some(list) = store.get_task_list(l)? else { return Ok(c) };
    if list.remote_path.is_empty() {
        return Ok(c);
    }
    Ok(store
        .calendars()?
        .into_iter()
        .find(|cal| cal.account == acct && cal.remote_path == list.remote_path)
        .map_or(c, |cal| CollectionRef::Calendar(cal.id)))
}
