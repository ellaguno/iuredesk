//! Correo en vivo: arranca `iuredesk-sync` con las cuentas reales, recoge sus avisos
//! ([`SyncEvent`]) sin bloquear la UI y los convierte en estado (lo que muestra el botón de
//! sincronización del riel) y en refrescos de carpetas y lista.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use iuredesk_model::AccountId;
use iuredesk_model::account::AccountKind;
use iuredesk_oauth::TokenProvider;
use iuredesk_sync::{Phase, SyncEngine, SyncEvent, SyncHandle};

use super::{Event, Notice, Session};
use crate::lang::Text;

/// Cada cuánto, como mucho, se refrescan carpetas y lista mientras llegan cambios.
const REFRESH_EVERY: Duration = Duration::from_millis(700);

/// Estado de la sincronización de una cuenta.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AccountSync {
    /// Arrancando (aún sin noticias del servidor).
    Starting,
    /// Trabajando: fase y avance.
    Working { phase: Phase, done: u64, total: u64 },
    /// Al día.
    Ready { at: DateTime<Utc>, backfill: bool },
    /// Sin conexión (se reintenta solo).
    Offline(Text),
    /// Error que necesita al usuario (contraseña, permisos…).
    Error { text: Text, reauth: bool },
}

impl AccountSync {
    /// Texto corto para la ayuda del botón y los ajustes.
    pub fn label(&self) -> String {
        match self {
            AccountSync::Starting => crate::lang::pick("Connecting…", "Conectando…").to_string(),
            AccountSync::Working { phase, done, total } => {
                let what = match phase {
                    Phase::Folders => crate::lang::pick("Reading folders…", "Leyendo carpetas…"),
                    Phase::Headers => crate::lang::pick("Downloading recent mail…", "Descargando correos recientes…"),
                    Phase::Backfill => crate::lang::pick("Downloading older mail…", "Descargando correos antiguos…"),
                    Phase::Bodies => crate::lang::pick("Downloading messages…", "Descargando mensajes…"),
                    Phase::Ops => crate::lang::pick("Sending your changes…", "Enviando tus cambios…"),
                };
                if *total > 0 { format!("{what} {done}/{total}") } else { what.to_string() }
            }
            AccountSync::Ready { backfill: true, .. } => {
                crate::lang::pick("Up to date (older mail still downloading)", "Al día (bajando correo antiguo)")
                    .to_string()
            }
            AccountSync::Ready { .. } => crate::lang::pick("Up to date", "Al día").to_string(),
            AccountSync::Offline(t) => t.get().to_string(),
            AccountSync::Error { text, .. } => text.get().to_string(),
        }
    }
}

/// Estado global para el botón de sincronización del riel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncStatus {
    /// Sin cuentas que sincronizar (buzón de ejemplo, pruebas).
    Off,
    /// Todo al día.
    Idle,
    /// Trabajando (texto del avance).
    Busy(String),
    /// Sin conexión.
    Offline(String),
    /// Error con su mensaje y si hay que volver a iniciar sesión.
    Error { text: String, reauth: bool },
}

/// Parte «en vivo» de la sesión.
pub(crate) struct Live {
    pub handle: Option<SyncHandle>,
    generation: u64,
    tx: Sender<(u64, SyncEvent)>,
    rx: Receiver<(u64, SyncEvent)>,
    pub status: HashMap<AccountId, AccountSync>,
    pub dirty: bool,
    pub last_refresh: Option<Instant>,
    pub tokens: Option<Arc<dyn TokenProvider>>,
}

impl Default for Live {
    fn default() -> Self {
        let (tx, rx) = channel();
        Live {
            handle: None,
            generation: 0,
            tx,
            rx,
            status: HashMap::new(),
            dirty: false,
            last_refresh: None,
            tokens: None,
        }
    }
}

impl Session {
    /// (Re)arranca la sincronización con las cuentas actuales. Sin opciones de sincronización
    /// (buzón de ejemplo, pruebas) no hace nada. Nunca bloquea: el apagado del motor anterior se
    /// espera en segundo plano.
    pub fn restart_sync(&mut self) {
        let Some(opts) = self.sync_options.clone() else { return };
        if let Some(old) = self.live.handle.take() {
            self.bg.handle().spawn(async move {
                let _ = tokio::time::timeout(Duration::from_secs(15), old.shutdown()).await;
            });
        }
        self.live.generation = self.live.generation.wrapping_add(1);
        let accounts: Vec<_> = self
            .accounts
            .iter()
            .filter(|a| a.kind != AccountKind::Iurefficient && a.incoming.protocol.is_incoming())
            .cloned()
            .collect();
        self.live.status.retain(|aid, _| self.account_ids.values().any(|a| a == aid));
        if accounts.is_empty() {
            self.restart_pim();
            return;
        }
        if self.live.tokens.is_none()
            && let Ok(http) = iuredesk_oauth::http_client()
        {
            let tm = iuredesk_oauth::TokenManager::from_settings(
                http,
                Arc::clone(&self.secrets),
                &iuredesk_oauth::OAuthSettings::default(),
            );
            self.live.tokens = Some(Arc::new(tm));
        }
        for a in &accounts {
            if let Some(aid) = self.account_ids.get(&a.id) {
                self.live.status.insert(*aid, AccountSync::Starting);
            }
        }
        let handle = SyncEngine::start_with(
            Arc::clone(&self.store),
            accounts,
            self.live.tokens.clone(),
            Arc::clone(&self.secrets),
            opts,
        );
        // Los avisos pasan a un canal síncrono y despiertan a la UI.
        let mut events = handle.events();
        let (tx, generation, wake) = (self.live.tx.clone(), self.live.generation, self.bg.waker());
        self.bg.handle().spawn(async move {
            loop {
                match events.recv().await {
                    Ok(ev) => {
                        if tx.send((generation, ev)).is_err() {
                            break;
                        }
                        wake();
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        self.live.handle = Some(handle);
        self.restart_pim();
    }

    /// Pide una pasada de todas las cuentas ya («Reintentar», botón de sincronización).
    pub fn sync_now(&mut self) {
        if let Some(h) = &self.live.handle {
            for s in self.live.status.values_mut() {
                if matches!(s, AccountSync::Offline(_) | AccountSync::Error { .. }) {
                    *s = AccountSync::Starting;
                }
            }
            h.sync_all();
        }
        if let Some(p) = &self.pim.handle {
            p.sync_all();
        }
    }

    /// ¿Hay sincronización en marcha?
    pub fn is_live(&self) -> bool {
        self.live.handle.is_some()
    }

    /// Estado de una cuenta.
    /// Un error del correo manda; si no, el de calendario/contactos (BUG-GCAL); si no, el del
    /// correo.
    pub fn account_sync(&self, aid: AccountId) -> Option<&AccountSync> {
        let mail = self.live.status.get(&aid);
        if matches!(mail, Some(AccountSync::Error { .. })) {
            return mail;
        }
        self.pim_error(aid).or(mail)
    }

    /// Estado global para el botón del riel.
    pub fn sync_status(&self) -> SyncStatus {
        if self.live.handle.is_none() {
            return SyncStatus::Off;
        }
        let all: Vec<&AccountSync> = self.live.status.values().chain(self.pim.errors.values()).collect();
        if let Some(AccountSync::Error { text, reauth }) =
            all.iter().find(|s| matches!(s, AccountSync::Error { .. })).copied()
        {
            return SyncStatus::Error { text: text.get().to_string(), reauth: *reauth };
        }
        if let Some(AccountSync::Offline(t)) = all.iter().find(|s| matches!(s, AccountSync::Offline(_))).copied() {
            return SyncStatus::Offline(t.get().to_string());
        }
        if let Some(s) = all.iter().find(|s| matches!(s, AccountSync::Starting | AccountSync::Working { .. })) {
            return SyncStatus::Busy(s.label());
        }
        SyncStatus::Idle
    }

    /// Detiene la sincronización y la bandeja de salida (al cerrar la aplicación). Espera como
    /// mucho unos segundos.
    pub fn shutdown(&mut self) {
        if let Some(s) = self.sender.take() {
            s.stop();
        }
        let mail = self.live.handle.take();
        let pim = self.pim.handle.take();
        if mail.is_some() || pim.is_some() {
            let rt = self.bg.handle();
            let _ = std::thread::Builder::new().name("iuredesk-shutdown".into()).spawn(move || {
                rt.block_on(async move {
                    if let Some(h) = mail {
                        let _ = tokio::time::timeout(Duration::from_secs(4), h.shutdown()).await;
                    }
                    if let Some(p) = pim {
                        let _ = tokio::time::timeout(Duration::from_secs(4), p.shutdown()).await;
                    }
                });
            });
        }
    }

    /// Atiende los avisos de la sincronización (desde [`Session::poll`]).
    pub(crate) fn poll_sync(&mut self, out: &mut Vec<Event>) {
        let gen_now = self.live.generation;
        let mut got = 0usize;
        while let Ok((generation, ev)) = self.live.rx.try_recv() {
            got += 1;
            if generation != gen_now {
                continue;
            }
            self.on_sync_event(ev, out);
            if got > 10_000 {
                break;
            }
        }
        if self.live.dirty && self.live.last_refresh.is_none_or(|t| t.elapsed() >= REFRESH_EVERY) {
            self.live.dirty = false;
            self.live.last_refresh = Some(Instant::now());
            self.refresh_from_sync();
        }
    }

    /// Falta para el próximo refresco pendiente.
    pub(crate) fn sync_deadline(&self) -> Option<Duration> {
        if !self.live.dirty {
            return None;
        }
        Some(self.live.last_refresh.map_or(Duration::ZERO, |t| REFRESH_EVERY.saturating_sub(t.elapsed())))
    }

    /// Atiende un aviso de la sincronización como si hubiera llegado del motor (pruebas).
    #[doc(hidden)]
    pub fn feed_sync_event(&mut self, ev: SyncEvent) -> Vec<Event> {
        let mut out = Vec::new();
        self.on_sync_event(ev, &mut out);
        out
    }

    fn on_sync_event(&mut self, ev: SyncEvent, out: &mut Vec<Event>) {
        match ev {
            SyncEvent::FolderUpdated { .. } => self.live.dirty = true,
            SyncEvent::Progress { account, phase, done, total, .. } => {
                // Replicar la cola o bajar un cuerpo fuera de una pasada no termina con `Synced`:
                // al completar, la cuenta vuelve a estar al día.
                let prev = self.live.status.get(&account).cloned();
                let side_job = matches!(phase, Phase::Ops | Phase::Bodies);
                let state = match prev {
                    // Al día: replicar un cambio o bajar un cuerpo no enciende el giro.
                    Some(ready @ AccountSync::Ready { .. }) if side_job => ready,
                    Some(AccountSync::Working { phase: Phase::Ops | Phase::Bodies, .. })
                        if side_job && done >= total =>
                    {
                        AccountSync::Ready { at: Utc::now(), backfill: false }
                    }
                    _ => AccountSync::Working { phase, done, total },
                };
                self.live.status.insert(account, state);
                if matches!(phase, Phase::Headers | Phase::Backfill | Phase::Folders) {
                    self.live.dirty = true;
                }
            }
            SyncEvent::Synced { account, backfill_pending, .. } => {
                self.live.status.insert(account, AccountSync::Ready { at: Utc::now(), backfill: backfill_pending });
                self.live.dirty = true;
            }
            SyncEvent::NewMail { account, count, sample } => {
                self.live.dirty = true;
                let who = self.labels.iter().find(|l| l.id == account).map(|l| l.email.clone()).unwrap_or_default();
                let summary = if count == 1 {
                    Text::new("New message", "Correo nuevo")
                } else {
                    Text::new(format!("{count} new messages"), format!("{count} correos nuevos"))
                };
                let detail =
                    sample.iter().take(3).map(|s| format!("{} — {}", s.from, s.subject)).collect::<Vec<_>>().join("\n");
                out.push(Event::NewMail { summary, detail, account: who });
            }
            SyncEvent::Error(f) => {
                let text = Text::new(f.en.clone(), f.es.clone());
                let state = if f.transient && !f.needs_reauth {
                    AccountSync::Offline(text.clone())
                } else {
                    AccountSync::Error { text: text.clone(), reauth: f.needs_reauth }
                };
                match f.account {
                    Some(a) => {
                        // Un error al bajar un cuerpo o replicar una operación no cambia el estado
                        // si la cuenta ya estaba al día.
                        let was_ok = matches!(self.live.status.get(&a), Some(AccountSync::Ready { .. }));
                        if !(was_ok && f.transient && !f.needs_reauth) {
                            self.live.status.insert(a, state);
                        }
                    }
                    None => self.notices.push(Notice::error(text)),
                }
            }
        }
    }

    /// Relee carpetas y recalcula la lista sin perder la selección.
    fn refresh_from_sync(&mut self) {
        if let Ok(l) = self.store.account_labels() {
            self.labels = l;
        }
        if let Ok(f) = self.store.all_folders() {
            self.folders = f;
        }
        self.counts.clear();
        self.envelopes.clear();
        self.data_gen = self.data_gen.wrapping_add(1);
        // La selección y el foco se conservan; la lista se recalcula en segundo plano.
        if self.search.trim().is_empty() {
            self.rebuild_rows();
        }
    }
}
