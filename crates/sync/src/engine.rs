//! Motor: un trabajador por cuenta (pasadas, historial antiguo, cola), IDLE, eventos y la API
//! pública ([`SyncEngine`], [`SyncHandle`]).

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use iuredesk_accounts::SecretStore;
use iuredesk_model::account::{Account, AccountKind, Protocol};
use iuredesk_model::{AccountId, Flags, FolderId, MessageId};
use iuredesk_oauth::TokenProvider;
use iuredesk_store::Store;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::error::{Result, SyncError};
use crate::imap_sync::{CycleReport, IdleExit, ImapAccount};
use crate::pool::{Auth, ImapPool};
use crate::pop3_sync::Pop3Account;
use crate::{FriendlyError, SyncEvent, SyncOptions, ops, state};

/// Capacidad del canal de eventos (los suscriptores lentos pierden los más viejos).
const EVENT_CAPACITY: usize = 4_096;
/// Espera máxima al apagar cada tarea.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(10);

/// Contexto compartido de una cuenta.
pub(crate) struct Ctx {
    pub store: Arc<Store>,
    pub account: Account,
    pub acct: AccountId,
    pub opts: Arc<SyncOptions>,
    pub events: broadcast::Sender<SyncEvent>,
}

impl Ctx {
    pub fn emit(&self, e: SyncEvent) {
        let _ = self.events.send(e);
    }

    pub fn emit_error(&self, e: &SyncError) {
        self.emit(SyncEvent::Error(FriendlyError::new(Some(self.acct), e)));
    }
}

/// Orden para el trabajador de una cuenta.
pub(crate) enum Cmd {
    /// Pasada (completa o solo bandeja de entrada); `reply` avisa al terminar.
    Sync { inbox_only: bool, reply: Option<oneshot::Sender<Result<()>>> },
    /// Replicar la cola de operaciones ya.
    Replay,
}

enum Worker {
    Imap(Arc<ImapAccount>),
    Pop3(Arc<Pop3Account>),
}

impl Worker {
    fn ctx(&self) -> &Arc<Ctx> {
        match self {
            Worker::Imap(w) => &w.ctx,
            Worker::Pop3(w) => &w.ctx,
        }
    }

    async fn cycle(&self, inbox_only: bool) -> Result<CycleReport> {
        match self {
            Worker::Imap(w) => w.cycle(inbox_only).await,
            Worker::Pop3(w) => w.cycle().await,
        }
    }

    async fn backfill(&self) -> Result<bool> {
        match self {
            Worker::Imap(w) => w.backfill_slice().await,
            Worker::Pop3(w) => w.cycle().await.map(|r| r.backfill_pending),
        }
    }

    async fn replay(&self) -> Result<()> {
        match self {
            Worker::Imap(w) => w.replay().await,
            // En POP3 no hay estado remoto que tocar: las operaciones son solo locales.
            Worker::Pop3(w) => {
                let ctx = &w.ctx;
                for op in ctx.store.peek_ops(10_000)?.into_iter().filter(|o| o.account == ctx.acct) {
                    // `outbox.send` es de la bandeja de salida: no se toca.
                    if ops::classify(&op.kind, &op.payload).is_none() {
                        continue;
                    }
                    ctx.store.ack_op(op.seq)?;
                }
                Ok(())
            }
        }
    }

    async fn fetch_body(&self, msg: MessageId) -> Result<()> {
        match self {
            Worker::Imap(w) => w.fetch_body(msg).await,
            Worker::Pop3(w) => {
                let env = w.ctx.store.get_envelope(msg)?.ok_or_else(|| SyncError::MessageNotFound(msg.to_string()))?;
                if env.blob.is_some() { Ok(()) } else { Err(SyncError::MessageNotFound(msg.to_string())) }
            }
        }
    }
}

struct AccountSlot {
    id: String,
    acct: AccountId,
    tx: mpsc::Sender<Cmd>,
    worker: Arc<Worker>,
}

struct Inner {
    store: Arc<Store>,
    slots: Vec<AccountSlot>,
    events: broadcast::Sender<SyncEvent>,
    first_rx: Mutex<Option<broadcast::Receiver<SyncEvent>>>,
    shutdown: watch::Sender<bool>,
    rt: Option<tokio::runtime::Handle>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    keepalive: Mutex<Option<oneshot::Sender<()>>>,
    pools: Vec<Arc<ImapPool>>,
}

/// Arranque de la sincronización.
#[derive(Debug)]
pub struct SyncEngine;

/// Mando de la sincronización en marcha. Se puede clonar; las llamadas nunca bloquean la UI.
#[derive(Clone)]
pub struct SyncHandle {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for SyncHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncHandle").field("accounts", &self.inner.slots.len()).finish_non_exhaustive()
    }
}

/// Runtime de fondo: el del llamador si lo hay; si no, uno propio en un hilo que vive mientras
/// viva el mando (y se suelta en ese hilo, nunca dentro de un contexto asíncrono).
fn runtime() -> std::result::Result<(tokio::runtime::Handle, Option<oneshot::Sender<()>>), String> {
    if let Ok(h) = tokio::runtime::Handle::try_current() {
        return Ok((h, None));
    }
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("iuredesk-sync")
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let h = rt.handle().clone();
    let (tx, rx) = oneshot::channel::<()>();
    std::thread::Builder::new()
        .name("iuredesk-sync-rt".into())
        .spawn(move || {
            rt.block_on(async {
                let _ = rx.await;
            });
        })
        .map_err(|e| e.to_string())?;
    Ok((h, Some(tx)))
}

impl SyncEngine {
    /// Arranca con las opciones por omisión. Ver [`SyncEngine::start_with`].
    pub fn start(
        store: Arc<Store>,
        accounts: Vec<Account>,
        token_provider: Option<Arc<dyn TokenProvider>>,
        secrets: Arc<dyn SecretStore>,
    ) -> SyncHandle {
        Self::start_with(store, accounts, token_provider, secrets, SyncOptions::default())
    }

    /// Arranca un trabajador por cuenta de correo (IMAP o POP3) y, si procede, su IDLE. La
    /// primera pasada empieza enseguida. Usa el runtime tokio actual o, si no hay, crea uno propio
    /// en segundo plano.
    pub fn start_with(
        store: Arc<Store>,
        accounts: Vec<Account>,
        token_provider: Option<Arc<dyn TokenProvider>>,
        secrets: Arc<dyn SecretStore>,
        opts: SyncOptions,
    ) -> SyncHandle {
        let (events, first_rx) = broadcast::channel(EVENT_CAPACITY);
        let (shutdown, _) = watch::channel(false);
        let opts = Arc::new(opts);
        let auth = Arc::new(Auth { tokens: token_provider, secrets });
        let mut inner = Inner {
            store: Arc::clone(&store),
            slots: Vec::new(),
            events: events.clone(),
            first_rx: Mutex::new(Some(first_rx)),
            shutdown,
            rt: None,
            tasks: Mutex::new(Vec::new()),
            keepalive: Mutex::new(None),
            pools: Vec::new(),
        };
        let (rt, keepalive) = match runtime() {
            Ok(r) => r,
            Err(e) => {
                let _ = events.send(SyncEvent::Error(FriendlyError::new(None, &SyncError::Runtime(e))));
                return SyncHandle { inner: Arc::new(inner) };
            }
        };
        let mut tasks = Vec::new();
        for account in accounts {
            if account.kind == AccountKind::Iurefficient || !account.incoming.protocol.is_incoming() {
                continue;
            }
            let acct = match state::account_id(&store, &account) {
                Ok(a) => a,
                Err(e) => {
                    let _ = events.send(SyncEvent::Error(FriendlyError::new(None, &e)));
                    continue;
                }
            };
            let ctx = Arc::new(Ctx {
                store: Arc::clone(&store),
                account: account.clone(),
                acct,
                opts: Arc::clone(&opts),
                events: events.clone(),
            });
            let (tx, rx) = mpsc::channel(64);
            let worker = match account.incoming.protocol {
                Protocol::Pop3 => {
                    let policy = opts.pop3_policies.get(&account.id).copied().unwrap_or(opts.pop3_policy);
                    Arc::new(Worker::Pop3(Arc::new(Pop3Account { ctx, auth: Arc::clone(&auth), policy })))
                }
                _ => {
                    let pool = Arc::new(ImapPool::new(account.clone(), Arc::clone(&auth), &opts));
                    inner.pools.push(Arc::clone(&pool));
                    let imap = Arc::new(ImapAccount::new(ctx, Arc::clone(&pool)));
                    if opts.idle && pool.limit >= 3 {
                        let (i, t, s) = (Arc::clone(&imap), tx.clone(), inner.shutdown.subscribe());
                        tasks.push(rt.spawn(run_idle(i, t, s)));
                    }
                    Arc::new(Worker::Imap(imap))
                }
            };
            tasks.push(rt.spawn(run_worker(Arc::clone(&worker), rx, inner.shutdown.subscribe())));
            inner.slots.push(AccountSlot { id: account.id.clone(), acct, tx, worker });
        }
        inner.rt = Some(rt);
        inner.tasks = Mutex::new(tasks);
        inner.keepalive = Mutex::new(keepalive);
        SyncHandle { inner: Arc::new(inner) }
    }
}

fn shutdown_requested(rx: &watch::Receiver<bool>) -> bool {
    *rx.borrow()
}

async fn wait_shutdown(mut rx: watch::Receiver<bool>) {
    let _ = rx.wait_for(|v| *v).await;
}

/// Espera tras fallos seguidos: 30 s, 1 min, 2 min… hasta el intervalo de sondeo (o 30 min si
/// hacen falta credenciales nuevas).
fn retry_delay(failures: u32, e: &SyncError, poll: Duration) -> Duration {
    if e.needs_reauth() {
        return poll.max(Duration::from_secs(30 * 60));
    }
    let secs = 30u64.saturating_mul(2u64.saturating_pow(failures.saturating_sub(1).min(10)));
    Duration::from_secs(secs).min(poll.max(Duration::from_secs(30)))
}

async fn run_worker(w: Arc<Worker>, mut rx: mpsc::Receiver<Cmd>, shutdown: watch::Receiver<bool>) {
    let ctx = Arc::clone(w.ctx());
    let poll = ctx.opts.poll_interval;
    let mut backfill = false;
    let mut next = Instant::now();
    let mut failures = 0u32;
    loop {
        if shutdown_requested(&shutdown) {
            break;
        }
        let wait = if backfill { Duration::ZERO } else { next.saturating_duration_since(Instant::now()) };
        tokio::select! {
            biased;
            _ = wait_shutdown(shutdown.clone()) => break,
            cmd = rx.recv() => match cmd {
                None => break,
                Some(Cmd::Sync { inbox_only, reply }) => {
                    let r = tokio::select! {
                        r = w.cycle(inbox_only) => r,
                        _ = wait_shutdown(shutdown.clone()) => Err(SyncError::Shutdown),
                    };
                    match &r {
                        Ok(rep) => {
                            backfill |= rep.backfill_pending;
                            failures = 0;
                            if !inbox_only {
                                next = Instant::now() + poll;
                            }
                        }
                        Err(SyncError::Shutdown) => {}
                        Err(e) => ctx.emit_error(e),
                    }
                    if let Some(reply) = reply {
                        let _ = reply.send(r.map(|_| ()));
                    }
                }
                Some(Cmd::Replay) => {
                    let r = tokio::select! {
                        r = w.replay() => r,
                        _ = wait_shutdown(shutdown.clone()) => Ok(()),
                    };
                    if let Err(e) = r {
                        ctx.emit_error(&e);
                    }
                }
            },
            _ = tokio::time::sleep(wait) => {
                if backfill {
                    let r = tokio::select! {
                        r = w.backfill() => r,
                        _ = wait_shutdown(shutdown.clone()) => break,
                    };
                    match r {
                        Ok(more) => backfill = more,
                        Err(e) => {
                            ctx.emit_error(&e);
                            backfill = false;
                            failures = failures.saturating_add(1);
                            next = Instant::now() + retry_delay(failures, &e, poll);
                        }
                    }
                } else {
                    let r = tokio::select! {
                        r = w.cycle(false) => r,
                        _ = wait_shutdown(shutdown.clone()) => break,
                    };
                    match r {
                        Ok(rep) => {
                            backfill = rep.backfill_pending;
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
}

async fn run_idle(imap: Arc<ImapAccount>, tx: mpsc::Sender<Cmd>, shutdown: watch::Receiver<bool>) {
    let mut delay = Duration::from_secs(5);
    loop {
        if shutdown_requested(&shutdown) {
            break;
        }
        let r = tokio::select! {
            r = imap.idle_session(&tx, &shutdown) => r,
            _ = wait_shutdown(shutdown.clone()) => break,
        };
        match r {
            Ok(IdleExit::Stopped | IdleExit::Unsupported) => break,
            Err(e) => {
                // Sin ruido: el sondeo sigue funcionando; solo se avisa si son credenciales.
                if e.needs_reauth() {
                    imap.ctx.emit_error(&e);
                }
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {},
                    _ = wait_shutdown(shutdown.clone()) => break,
                }
                delay = delay.saturating_mul(2).min(Duration::from_secs(300));
            }
        }
    }
}

impl SyncHandle {
    /// Eventos de la sincronización. La primera llamada recibe también lo ocurrido desde el
    /// arranque; las siguientes, desde que se suscriben.
    pub fn events(&self) -> broadcast::Receiver<SyncEvent> {
        let first = self.inner.first_rx.lock().unwrap_or_else(PoisonError::into_inner).take();
        first.unwrap_or_else(|| self.inner.events.subscribe())
    }

    /// Almacén.
    pub fn store(&self) -> &Arc<Store> {
        &self.inner.store
    }

    /// Id local de una cuenta (`Account::id`).
    pub fn account_id(&self, account: &str) -> Option<AccountId> {
        self.slot(account).map(|s| s.acct)
    }

    fn slot(&self, account: &str) -> Option<&AccountSlot> {
        self.inner.slots.iter().find(|s| s.id == account)
    }

    fn slot_by_acct(&self, acct: AccountId) -> Option<&AccountSlot> {
        self.inner.slots.iter().find(|s| s.acct == acct)
    }

    /// Pide una pasada completa de la cuenta (no espera).
    pub fn sync_now(&self, account: &str) -> Result<()> {
        let s = self.slot(account).ok_or_else(|| SyncError::UnknownAccount(account.to_string()))?;
        s.tx.try_send(Cmd::Sync { inbox_only: false, reply: None }).map_err(|_| SyncError::Shutdown)
    }

    /// Pide una pasada de todas las cuentas.
    pub fn sync_all(&self) {
        for s in &self.inner.slots {
            let _ = s.tx.try_send(Cmd::Sync { inbox_only: false, reply: None });
        }
    }

    /// Pasada completa de la cuenta, esperando a que termine.
    pub async fn sync_now_wait(&self, account: &str) -> Result<()> {
        let s = self.slot(account).ok_or_else(|| SyncError::UnknownAccount(account.to_string()))?;
        let (tx, rx) = oneshot::channel();
        s.tx.send(Cmd::Sync { inbox_only: false, reply: Some(tx) }).await.map_err(|_| SyncError::Shutdown)?;
        rx.await.map_err(|_| SyncError::Shutdown)?
    }

    /// Baja el cuerpo de un mensaje si aún no está (al abrirlo).
    pub async fn fetch_body(&self, msg: MessageId) -> Result<()> {
        let env = self.inner.store.get_envelope(msg)?.ok_or_else(|| SyncError::MessageNotFound(msg.to_string()))?;
        if env.blob.is_some() {
            return Ok(());
        }
        let s = self.slot_by_acct(env.account).ok_or_else(|| SyncError::UnknownAccount(env.account.to_string()))?;
        let rt = self.inner.rt.as_ref().ok_or(SyncError::Shutdown)?;
        let w = Arc::clone(&s.worker);
        rt.spawn(async move { w.fetch_body(msg).await }).await.map_err(|e| SyncError::Runtime(e.to_string()))?
    }

    fn kick(&self, acct: AccountId) {
        if let Some(s) = self.slot_by_acct(acct) {
            let _ = s.tx.try_send(Cmd::Replay);
        }
    }

    fn account_of(&self, msg: MessageId) -> Option<AccountId> {
        self.inner.store.get_envelope(msg).ok().flatten().map(|e| e.account)
    }

    /// Cambia banderas (local al instante; se replica en segundo plano).
    pub fn set_flags(&self, msg: MessageId, flags: Flags) -> Result<()> {
        ops::set_flags(&self.inner.store, msg, flags)?;
        if let Some(a) = self.account_of(msg) {
            self.kick(a);
        }
        Ok(())
    }

    /// Mueve un mensaje (local al instante; se replica en segundo plano).
    pub fn move_message(&self, msg: MessageId, to: FolderId) -> Result<()> {
        ops::move_message(&self.inner.store, msg, to)?;
        if let Some(a) = self.account_of(msg) {
            self.kick(a);
        }
        Ok(())
    }

    /// Borra definitivamente (local al instante; se replica en segundo plano).
    pub fn delete_message(&self, msg: MessageId) -> Result<()> {
        let acct = self.account_of(msg);
        ops::delete_message(&self.inner.store, msg)?;
        if let Some(a) = acct {
            self.kick(a);
        }
        Ok(())
    }

    /// Guarda un mensaje nuevo en una carpeta (borradores, enviados) y lo sube en segundo plano.
    pub fn append_message(&self, folder: FolderId, raw: Vec<u8>, flags: Flags) -> Result<MessageId> {
        let id = ops::append_message(&self.inner.store, folder, raw, flags)?;
        if let Some(a) = self.account_of(id) {
            self.kick(a);
        }
        Ok(id)
    }

    /// Detiene todo: avisa a las tareas (IDLE termina con `DONE`), las espera y cierra sesiones.
    pub async fn shutdown(self) {
        let _ = self.inner.shutdown.send(true);
        let tasks: Vec<JoinHandle<()>> =
            std::mem::take(&mut *self.inner.tasks.lock().unwrap_or_else(PoisonError::into_inner));
        for t in tasks {
            let _ = tokio::time::timeout(SHUTDOWN_WAIT, t).await;
        }
        let clients: Vec<_> = self.inner.pools.iter().flat_map(|p| p.drain()).collect();
        for c in clients {
            let _ = tokio::time::timeout(Duration::from_secs(2), c.logout()).await;
        }
        let keep = self.inner.keepalive.lock().unwrap_or_else(PoisonError::into_inner).take();
        drop(keep);
    }

    /// [`SyncHandle::shutdown`] desde código síncrono (la interfaz al cerrar). Si se llama
    /// dentro de un runtime, solo avisa y no espera.
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
