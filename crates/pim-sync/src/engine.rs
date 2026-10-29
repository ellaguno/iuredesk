//! Motor: un trabajador por cuenta (pasadas periódicas, cola de operaciones), eventos y la API
//! pública ([`PimSyncEngine`], [`PimSyncHandle`]). Mismo patrón que `iuredesk_sync`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use chrono::Utc;
use iuredesk_accounts::SecretStore;
use iuredesk_model::{AccountId, Contact, ContactId, Event, EventId, PartStat, Task, TaskId};
use iuredesk_oauth::TokenProvider;
use iuredesk_store::{AccountLabel, Store};
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::account::{Creds, PimAccount, PimSource};
use crate::caldav::{CalDav, server_schedules};
use crate::error::{PimError, Result};
use crate::graph::Graph;
use crate::gtasks::{GoogleTasks, WithTasks};

mod freebusy;
use crate::ops::{self, ImipReply, PimOp};
use crate::provider::{Ctx, Provider};
use crate::{FriendlyError, PimEvent, PimSyncOptions};
pub use freebusy::{BusySlot, FreeBusy};

/// Capacidad del canal de eventos.
const EVENT_CAPACITY: usize = 4_096;
/// Espera máxima al apagar cada tarea.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(10);
/// Operaciones por vuelta de la cola.
const OPS_PER_PASS: usize = 500;
/// Espera máxima entre reintentos de una operación.
const MAX_OP_BACKOFF: Duration = Duration::from_secs(30 * 60);

enum Cmd {
    Sync { reply: Option<oneshot::Sender<Result<()>>> },
    Replay,
}

struct Slot {
    id: String,
    acct: AccountId,
    tx: mpsc::Sender<Cmd>,
    ctx: Arc<Ctx>,
}

struct Inner {
    store: Arc<Store>,
    slots: Vec<Slot>,
    events: broadcast::Sender<PimEvent>,
    first_rx: Mutex<Option<broadcast::Receiver<PimEvent>>>,
    shutdown: watch::Sender<bool>,
    rt: Option<tokio::runtime::Handle>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    keepalive: Mutex<Option<oneshot::Sender<()>>>,
}

/// Arranque de la sincronización PIM.
#[derive(Debug)]
pub struct PimSyncEngine;

/// Mando de la sincronización PIM en marcha. Se puede clonar; nada bloquea la interfaz.
#[derive(Clone)]
pub struct PimSyncHandle {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for PimSyncHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PimSyncHandle").field("accounts", &self.inner.slots.len()).finish_non_exhaustive()
    }
}

fn runtime() -> std::result::Result<(tokio::runtime::Handle, Option<oneshot::Sender<()>>), String> {
    if let Ok(h) = tokio::runtime::Handle::try_current() {
        return Ok((h, None));
    }
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("iuredesk-pim")
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let h = rt.handle().clone();
    let (tx, rx) = oneshot::channel::<()>();
    std::thread::Builder::new()
        .name("iuredesk-pim-rt".into())
        .spawn(move || {
            rt.block_on(async {
                let _ = rx.await;
            });
        })
        .map_err(|e| e.to_string())?;
    Ok((h, Some(tx)))
}

/// Id local de una cuenta: el mismo mapa (`acct-map/<id>`) que usa la sincronización de correo,
/// así correo y calendario de una cuenta comparten `AccountId`.
pub fn account_id(store: &Store, account: &iuredesk_model::account::Account) -> Result<AccountId> {
    let key = format!("acct-map/{}", account.id.chars().take(200).collect::<String>());
    let existing = store.sync_state(&key)?.and_then(|b| <[u8; 8]>::try_from(b.as_slice()).ok()).map(u64::from_le_bytes);
    let id = match existing {
        Some(v) if v != 0 => AccountId(v),
        _ => {
            let id = store.new_account_id()?;
            store.put_sync_state(&key, &id.get().to_le_bytes())?;
            id
        }
    };
    let label = AccountLabel { id, name: account.display_name.clone(), email: account.email.clone() };
    if !store.account_labels()?.contains(&label) {
        store.put_account_label(&label)?;
    }
    Ok(id)
}

impl PimSyncEngine {
    /// Arranca un trabajador por cuenta. La primera pasada empieza enseguida. Usa el runtime
    /// tokio actual o, si no hay, uno propio en segundo plano.
    pub fn start(
        store: Arc<Store>,
        accounts: Vec<PimAccount>,
        tokens: Option<Arc<dyn TokenProvider>>,
        secrets: Arc<dyn SecretStore>,
        opts: PimSyncOptions,
    ) -> PimSyncHandle {
        let (events, first_rx) = broadcast::channel(EVENT_CAPACITY);
        let (shutdown, _) = watch::channel(false);
        let opts = Arc::new(opts);
        let creds = Arc::new(Creds { secrets, tokens });
        let mut inner = Inner {
            store: Arc::clone(&store),
            slots: Vec::new(),
            events: events.clone(),
            first_rx: Mutex::new(Some(first_rx)),
            shutdown,
            rt: None,
            tasks: Mutex::new(Vec::new()),
            keepalive: Mutex::new(None),
        };
        let (rt, keepalive) = match runtime() {
            Ok(r) => r,
            Err(e) => {
                let _ = events.send(PimEvent::Error(FriendlyError::new(None, &PimError::Runtime(e))));
                return PimSyncHandle { inner: Arc::new(inner) };
            }
        };
        let mut tasks = Vec::new();
        for account in accounts {
            let acct = match account_id(&store, &account.account) {
                Ok(a) => a,
                Err(e) => {
                    let _ = events.send(PimEvent::Error(FriendlyError::new(None, &e)));
                    continue;
                }
            };
            let ctx = Arc::new(Ctx {
                store: Arc::clone(&store),
                acct,
                account: account.clone(),
                opts: Arc::clone(&opts),
                events: events.clone(),
                creds: Arc::clone(&creds),
            });
            let (tx, rx) = mpsc::channel(64);
            tasks.push(rt.spawn(run_worker(Arc::clone(&ctx), rx, inner.shutdown.subscribe())));
            inner.slots.push(Slot { id: account.account.id.clone(), acct, tx, ctx });
        }
        inner.rt = Some(rt);
        inner.tasks = Mutex::new(tasks);
        inner.keepalive = Mutex::new(keepalive);
        PimSyncHandle { inner: Arc::new(inner) }
    }
}

/// Proveedor de la cuenta con credenciales frescas (la contraseña pudo cambiar).
async fn provider(ctx: &Ctx) -> Result<Box<dyn Provider>> {
    let main: Option<Box<dyn Provider>> = match &ctx.account.source {
        PimSource::Dav { caldav: None, carddav: None, .. } => None,
        PimSource::Dav { caldav, carddav, auth } => {
            Some(Box::new(CalDav::connect(ctx, caldav.clone(), carddav.clone(), auth).await?))
        }
        PimSource::Graph { base_url } => Some(Box::new(Graph::new(ctx, base_url)?)),
    };
    match (&ctx.account.tasks_api, main) {
        (Some(base), dav) => Ok(Box::new(WithTasks { dav, tasks: GoogleTasks::new(ctx, base)? })),
        (None, Some(p)) => Ok(p),
        (None, None) => Err(PimError::Invalid("la cuenta no tiene calendario, contactos ni tareas".into())),
    }
}

/// Espera tras fallos seguidos: 30 s, 1 min, 2 min… hasta el intervalo (30 min si faltan
/// credenciales).
fn retry_delay(failures: u32, e: &PimError, poll: Duration) -> Duration {
    if e.needs_reauth() {
        return poll.max(Duration::from_secs(30 * 60));
    }
    let secs = 30u64.saturating_mul(2u64.saturating_pow(failures.saturating_sub(1).min(10)));
    Duration::from_secs(secs).min(poll.max(Duration::from_secs(30)))
}

/// Replica la cola de la cuenta en orden. Una operación que falla por la red detiene la vuelta
/// (se conserva el orden); una que falla por otra cosa se reintenta con espera exponencial y,
/// tras varios intentos, se descarta con aviso.
async fn replay(ctx: &Ctx, p: &dyn Provider, backoff: &Mutex<HashMap<u64, Instant>>) -> Result<()> {
    let ops: Vec<_> = ctx
        .store
        .peek_ops(OPS_PER_PASS)?
        .into_iter()
        .filter(|o| o.account == ctx.acct && o.kind.starts_with(ops::OP_PREFIX))
        .collect();
    for op in ops {
        let wait = backoff.lock().unwrap_or_else(PoisonError::into_inner).get(&op.seq).copied();
        if wait.is_some_and(|w| Instant::now() < w) {
            continue;
        }
        let Some(pim) = PimOp::decode(&op.kind, &op.payload) else {
            ctx.store.ack_op(op.seq)?;
            continue;
        };
        match p.push(ctx, &pim).await {
            Ok(()) => {
                ctx.store.ack_op(op.seq)?;
                backoff.lock().unwrap_or_else(PoisonError::into_inner).remove(&op.seq);
            }
            Err(e) => {
                let attempts = ctx.store.fail_op(op.seq, &e.to_string())?;
                if e.is_transient() && attempts < ctx.opts.op_max_attempts {
                    return Err(e);
                }
                if e.needs_reauth() {
                    return Err(e);
                }
                if attempts >= 3 || attempts >= ctx.opts.op_max_attempts {
                    ctx.store.ack_op(op.seq)?;
                    ctx.emit_error(&e);
                } else {
                    let delay = Duration::from_secs(2u64.saturating_pow(attempts.min(20))).min(MAX_OP_BACKOFF);
                    backoff.lock().unwrap_or_else(PoisonError::into_inner).insert(op.seq, Instant::now() + delay);
                }
            }
        }
    }
    Ok(())
}

/// Una pasada: cola → servidor, luego servidor → almacén, luego lo que se haya encolado mientras.
async fn cycle(ctx: &Ctx, backoff: &Mutex<HashMap<u64, Instant>>) -> Result<()> {
    let started = Instant::now();
    let p = provider(ctx).await?;
    replay(ctx, p.as_ref(), backoff).await?;
    p.sync(ctx).await?;
    replay(ctx, p.as_ref(), backoff).await?;
    ctx.emit(PimEvent::Synced { account: ctx.acct, took: started.elapsed() });
    Ok(())
}

fn shutdown_requested(rx: &watch::Receiver<bool>) -> bool {
    *rx.borrow()
}

async fn wait_shutdown(mut rx: watch::Receiver<bool>) {
    let _ = rx.wait_for(|v| *v).await;
}

async fn run_worker(ctx: Arc<Ctx>, mut rx: mpsc::Receiver<Cmd>, shutdown: watch::Receiver<bool>) {
    let poll = ctx.opts.poll_interval;
    let backoff = Mutex::new(HashMap::new());
    let mut next = if ctx.opts.sync_on_start { Instant::now() } else { Instant::now() + poll };
    let mut failures = 0u32;
    loop {
        if shutdown_requested(&shutdown) {
            break;
        }
        let wait = next.saturating_duration_since(Instant::now());
        tokio::select! {
            biased;
            _ = wait_shutdown(shutdown.clone()) => break,
            cmd = rx.recv() => match cmd {
                None => break,
                Some(Cmd::Sync { reply }) => {
                    let r = tokio::select! {
                        r = cycle(&ctx, &backoff) => r,
                        _ = wait_shutdown(shutdown.clone()) => Err(PimError::Shutdown),
                    };
                    match &r {
                        Ok(()) => {
                            failures = 0;
                            next = Instant::now() + poll;
                        }
                        Err(PimError::Shutdown) => {}
                        Err(e) => ctx.emit_error(e),
                    }
                    if let Some(reply) = reply {
                        let _ = reply.send(r);
                    }
                }
                Some(Cmd::Replay) => {
                    let r = tokio::select! {
                        r = async {
                            let p = provider(&ctx).await?;
                            replay(&ctx, p.as_ref(), &backoff).await
                        } => r,
                        _ = wait_shutdown(shutdown.clone()) => Ok(()),
                    };
                    // Sin red no se molesta: la próxima pasada lo reintenta.
                    if let Err(e) = r
                        && !e.is_transient()
                    {
                        ctx.emit_error(&e);
                    }
                }
            },
            _ = tokio::time::sleep(wait) => {
                let r = tokio::select! {
                    r = cycle(&ctx, &backoff) => r,
                    _ = wait_shutdown(shutdown.clone()) => break,
                };
                match r {
                    Ok(()) => {
                        failures = 0;
                        next = Instant::now() + poll;
                    }
                    Err(e) => {
                        ctx.emit_error(&e);
                        failures = failures.saturating_add(1);
                        next = Instant::now() + retry_delay(failures, &e, poll);
                    }
                }
            }
        }
    }
}

impl PimSyncHandle {
    /// Avisos. La primera llamada recibe también lo ocurrido desde el arranque.
    pub fn events(&self) -> broadcast::Receiver<PimEvent> {
        let first = self.inner.first_rx.lock().unwrap_or_else(PoisonError::into_inner).take();
        first.unwrap_or_else(|| self.inner.events.subscribe())
    }

    /// Almacén.
    pub fn store(&self) -> &Arc<Store> {
        &self.inner.store
    }

    /// Id local de una cuenta (`Account::id`).
    pub fn account_id(&self, account: &str) -> Option<AccountId> {
        self.inner.slots.iter().find(|s| s.id == account).map(|s| s.acct)
    }

    fn slot(&self, account: &str) -> Result<&Slot> {
        self.inner.slots.iter().find(|s| s.id == account).ok_or_else(|| PimError::UnknownAccount(account.to_string()))
    }

    fn slot_by_acct(&self, acct: AccountId) -> Option<&Slot> {
        self.inner.slots.iter().find(|s| s.acct == acct)
    }

    /// Pide una pasada de la cuenta (no espera).
    pub fn sync_now(&self, account: &str) -> Result<()> {
        self.slot(account)?.tx.try_send(Cmd::Sync { reply: None }).map_err(|_| PimError::Shutdown)
    }

    /// Pide una pasada de todas las cuentas.
    pub fn sync_all(&self) {
        for s in &self.inner.slots {
            let _ = s.tx.try_send(Cmd::Sync { reply: None });
        }
    }

    /// Pasada de la cuenta, esperando a que termine.
    pub async fn sync_now_wait(&self, account: &str) -> Result<()> {
        let s = self.slot(account)?;
        let (tx, rx) = oneshot::channel();
        s.tx.send(Cmd::Sync { reply: Some(tx) }).await.map_err(|_| PimError::Shutdown)?;
        rx.await.map_err(|_| PimError::Shutdown)?
    }

    fn kick(&self, acct: AccountId) {
        if let Some(s) = self.slot_by_acct(acct) {
            let _ = s.tx.try_send(Cmd::Replay);
        }
    }

    fn calendar_account(&self, cal: iuredesk_model::CalendarId) -> Option<AccountId> {
        self.inner.store.get_calendar(cal).ok().flatten().map(|c| c.account)
    }

    /// Guarda un evento (nuevo si `id` está sin asignar): local al instante, se sube en segundo
    /// plano.
    pub fn save_event(&self, event: &Event) -> Result<EventId> {
        let id = ops::save_event(&self.inner.store, event)?;
        if let Some(a) = self.calendar_account(event.calendar) {
            self.kick(a);
        }
        Ok(id)
    }

    /// Borra un evento (local al instante; se replica en segundo plano).
    pub fn delete_event(&self, id: EventId) -> Result<()> {
        let acct = self.inner.store.get_event(id)?.and_then(|e| self.calendar_account(e.calendar));
        ops::delete_event(&self.inner.store, id)?;
        if let Some(a) = acct {
            self.kick(a);
        }
        Ok(())
    }

    /// Responde a una invitación. En CalDAV sin programación en el servidor (y en eventos
    /// locales) encola además el correo iMIP `mail.imip_reply` para la bandeja de salida; en
    /// Graph responde el servidor.
    pub fn rsvp(&self, id: EventId, partstat: PartStat, comment: Option<&str>) -> Result<Option<ImipReply>> {
        let store = &self.inner.store;
        let ev = store.get_event(id)?.ok_or_else(|| PimError::NotFound(id.to_string()))?;
        let acct = self.calendar_account(ev.calendar);
        let slot = acct.and_then(|a| self.slot_by_acct(a));
        let (me, send_imip, spanish) = match slot {
            Some(s) => {
                let graph = matches!(s.ctx.account.source, PimSource::Graph { .. });
                let schedules = s.ctx.opts.trust_server_scheduling && server_schedules(store, s.acct);
                (s.ctx.account.emails(), !graph && !schedules, s.ctx.opts.spanish)
            }
            // Sin cuenta en marcha no sabemos cuál de los asistentes somos: usar `ops::rsvp`.
            None => return Err(PimError::UnknownAccount(acct.map(|a| a.to_string()).unwrap_or_default())),
        };
        let r = ops::rsvp(store, id, partstat, comment, &me, send_imip, spanish, Utc::now())?;
        if let Some(a) = acct {
            self.kick(a);
        }
        Ok(r)
    }

    /// Guarda un contacto (y su foto nueva, si se da: `Some(&[])` la quita).
    pub fn save_contact(&self, contact: &Contact, photo: Option<&[u8]>) -> Result<ContactId> {
        let id = ops::save_contact(&self.inner.store, contact, photo)?;
        if let Some(b) = self.inner.store.get_address_book(contact.address_book)? {
            self.kick(b.account);
        }
        Ok(id)
    }

    /// Borra un contacto.
    pub fn delete_contact(&self, id: ContactId) -> Result<()> {
        let acct = self
            .inner
            .store
            .get_contact(id)?
            .and_then(|c| self.inner.store.get_address_book(c.address_book).ok().flatten())
            .map(|b| b.account);
        ops::delete_contact(&self.inner.store, id)?;
        if let Some(a) = acct {
            self.kick(a);
        }
        Ok(())
    }

    /// Guarda una tarea.
    pub fn save_task(&self, task: &Task) -> Result<TaskId> {
        let id = ops::save_task(&self.inner.store, task)?;
        if let Some(a) = self.inner.store.get_task_list(task.list)?.and_then(|l| l.account) {
            self.kick(a);
        }
        Ok(id)
    }

    /// Borra una tarea.
    pub fn delete_task(&self, id: TaskId) -> Result<()> {
        let acct = self
            .inner
            .store
            .get_task(id)?
            .and_then(|t| self.inner.store.get_task_list(t.list).ok().flatten())
            .and_then(|l| l.account);
        ops::delete_task(&self.inner.store, id)?;
        if let Some(a) = acct {
            self.kick(a);
        }
        Ok(())
    }

    /// Detiene todo y espera a las tareas.
    pub async fn shutdown(self) {
        let _ = self.inner.shutdown.send(true);
        let tasks: Vec<JoinHandle<()>> =
            std::mem::take(&mut *self.inner.tasks.lock().unwrap_or_else(PoisonError::into_inner));
        for t in tasks {
            let _ = tokio::time::timeout(SHUTDOWN_WAIT, t).await;
        }
        let keep = self.inner.keepalive.lock().unwrap_or_else(PoisonError::into_inner).take();
        drop(keep);
    }

    /// [`PimSyncHandle::shutdown`] desde código síncrono. Dentro de un runtime solo avisa.
    pub fn shutdown_blocking(self) {
        if tokio::runtime::Handle::try_current().is_ok() {
            let _ = self.inner.shutdown.send(true);
            return;
        }
        if let Some(rt) = self.inner.rt.clone() {
            rt.block_on(self.shutdown());
        }
    }
}
