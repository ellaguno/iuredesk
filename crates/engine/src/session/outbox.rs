//! Envío: la bandeja de salida de `iuredesk-compose` con su ventana para deshacer, el envío
//! programado y los reintentos, más un trabajador en el runtime de fondo que la vacía con SMTP
//! (contraseña del llavero u OAuth) y deja una copia local en «Enviados».

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use chrono::Utc;
use iuredesk_compose::{
    Draft, Outbox, OutboxConfig, OutboxEntry, OutboxId, OutgoingMessage, SendOutcome, SendReceipt, SmtpTransport,
    SystemClock, Transport,
};
use iuredesk_model::{AccountId, Flags, FolderRole};
use iuredesk_oauth::BoxFuture;
use iuredesk_store::{NewMessage, Store};

use super::{Inverse, Notice, NoticeKind, Outcome, Session};
use crate::compose::ComposeDraft;
use crate::lang::Text;
use crate::{EngineError, Result};

type Transports = Arc<RwLock<HashMap<AccountId, Arc<dyn Transport>>>>;

/// La bandeja de salida en marcha.
pub(crate) struct Sender {
    pub outbox: Arc<Outbox>,
    transports: Transports,
    wake: Arc<tokio::sync::Notify>,
    rx: Receiver<SendOutcome>,
    stop: Arc<AtomicBool>,
}

impl Sender {
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        self.wake.notify_one();
    }
}

/// Transporte que acepta todo sin red (buzón de ejemplo `--sample`).
#[derive(Debug, Clone, Copy, Default)]
pub struct OfflineTransport;

impl Transport for OfflineTransport {
    fn send<'a>(
        &'a self,
        _msg: &'a OutgoingMessage,
    ) -> BoxFuture<'a, std::result::Result<SendReceipt, iuredesk_compose::SendError>> {
        Box::pin(async { Ok(SendReceipt { server_reply: "250 sin conexión (ejemplo)".into() }) })
    }

    fn saves_to_sent(&self) -> bool {
        true
    }
}

/// Envuelve un transporte: tras enviar, guarda la copia en «Enviados» del almacén local para que
/// se vea al momento. La sincronización la adopta cuando el servidor tenga la suya (mismo
/// Message-ID), sin duplicarla.
struct LocalCopy {
    inner: Arc<dyn Transport>,
    store: Arc<Store>,
    account: AccountId,
}

impl Transport for LocalCopy {
    fn send<'a>(
        &'a self,
        msg: &'a OutgoingMessage,
    ) -> BoxFuture<'a, std::result::Result<SendReceipt, iuredesk_compose::SendError>> {
        Box::pin(async move {
            let r = self.inner.send(msg).await?;
            let sent =
                self.store.folders(self.account).ok().and_then(|f| f.into_iter().find(|f| f.role == FolderRole::Sent));
            if let Some(sent) = sent {
                let (mut env, text) = iuredesk_sync::parse::envelope_from_raw(&msg.raw, sent.id, Utc::now());
                env.flags = Flags { seen: true, ..Default::default() };
                if let Ok(ids) = self.store.put_messages(vec![NewMessage {
                    envelope: env,
                    raw: Some(msg.raw.clone()),
                    body_text: Some(text),
                }]) && let Some(id) = ids.first()
                {
                    let _ = iuredesk_sync::state::add_adopt(&self.store, sent.id, *id);
                }
            }
            Ok(r)
        })
    }

    fn saves_to_sent(&self) -> bool {
        self.inner.saves_to_sent()
    }
}

impl Session {
    /// Ventana para deshacer un envío (preferencias, 0–30 s).
    fn undo_window(&self) -> Duration {
        Duration::from_secs(u64::from(self.prefs.send_undo_secs.min(30)))
    }

    /// Arranca (o rehace, si cambió la ventana para deshacer o las cuentas) la bandeja de salida.
    pub(crate) fn start_outbox(&mut self) {
        if let Some(old) = self.sender.take() {
            old.stop();
        }
        let config = OutboxConfig { undo_window: self.undo_window(), ..Default::default() };
        let outbox = match Outbox::new(Arc::clone(&self.store), Arc::new(SystemClock), config) {
            Ok(mut o) => {
                o.set_sealer(Arc::new(crate::security::Sealer));
                Arc::new(o)
            }
            Err(e) => {
                self.notices.push(Notice::error(Text::new(
                    format!("The outbox could not be opened: {e}"),
                    format!("No se pudo abrir la bandeja de salida: {e}"),
                )));
                return;
            }
        };
        let transports: Transports = Arc::new(RwLock::new(HashMap::new()));
        let wake = Arc::new(tokio::sync::Notify::new());
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel();
        let (ob, tr, wk, st, ui) =
            (Arc::clone(&outbox), Arc::clone(&transports), Arc::clone(&wake), Arc::clone(&stop), self.bg.waker());
        self.bg.handle().spawn(async move {
            loop {
                if st.load(Ordering::Acquire) {
                    break;
                }
                let map: HashMap<AccountId, Arc<dyn Transport>> = tr.read().map(|m| m.clone()).unwrap_or_default();
                if let Ok(outcomes) = ob.process_due(&map).await {
                    let any = !outcomes.is_empty();
                    for o in outcomes {
                        let _ = tx.send(o);
                    }
                    if any {
                        ui();
                    }
                }
                let wait = match ob.next_due() {
                    Ok(Some(t)) => (t - Utc::now()).to_std().unwrap_or(Duration::ZERO),
                    _ => Duration::from_secs(30),
                }
                .clamp(Duration::from_millis(150), Duration::from_secs(30));
                tokio::select! {
                    _ = tokio::time::sleep(wait) => {}
                    _ = wk.notified() => {}
                }
            }
        });
        self.sender = Some(Sender { outbox, transports, wake, rx, stop });
        self.refresh_transports();
    }

    /// Aplica un cambio de la ventana para deshacer el envío (rehace la bandeja de salida; lo ya
    /// encolado conserva su hora).
    pub fn apply_send_prefs(&mut self) {
        self.save_prefs();
        self.start_outbox();
    }

    /// Rehace el transporte de cada cuenta (SMTP, o el de pruebas/ejemplo).
    pub(crate) fn refresh_transports(&mut self) {
        let Some(sender) = &self.sender else { return };
        let mut map: HashMap<AccountId, Arc<dyn Transport>> = HashMap::new();
        if let Some(t) = &self.transport_override {
            for l in &self.labels {
                map.insert(
                    l.id,
                    Arc::new(LocalCopy { inner: Arc::clone(t), store: Arc::clone(&self.store), account: l.id }),
                );
            }
        } else {
            for a in &self.accounts {
                let Some(aid) = self.account_ids.get(&a.id).copied() else { continue };
                let smtp: Arc<dyn Transport> =
                    Arc::new(SmtpTransport::new(a.clone(), Arc::clone(&self.secrets), self.live.tokens.clone()));
                // Microsoft 365 con OAuth: Graph `sendMail` (SMTP queda de reserva para lo que
                // Graph no acepta por tamaño), igual que la sincronización usa Graph.
                let sync_opts = iuredesk_sync::SyncOptions::default();
                let graph = match (&self.live.tokens, iuredesk_sync::choose_backend(a, &sync_opts, true)) {
                    (Some(t), iuredesk_sync::BackendKind::Graph) => iuredesk_graph::GraphClient::new(
                        a.clone(),
                        Arc::clone(t),
                        &sync_opts.graph_base_url,
                        iuredesk_compose::DEFAULT_SMTP_TIMEOUT,
                    )
                    .ok()
                    .map(|c| {
                        Arc::new(iuredesk_graph::GraphSendTransport::new(c).with_fallback(Arc::clone(&smtp)))
                            as Arc<dyn Transport>
                    }),
                    _ => None,
                };
                let inner = graph.unwrap_or(smtp);
                map.insert(aid, Arc::new(LocalCopy { inner, store: Arc::clone(&self.store), account: aid }));
            }
        }
        if let Ok(mut m) = sender.transports.write() {
            *m = map;
        }
        sender.wake.notify_one();
    }

    /// Mensajes en la bandeja de salida.
    pub fn outbox_entries(&self) -> Vec<OutboxEntry> {
        self.sender.as_ref().and_then(|s| s.outbox.entries().ok()).unwrap_or_default()
    }

    /// Envía (tras la ventana para deshacer, o a la hora programada). Devuelve el aviso con
    /// «Deshacer».
    pub fn send(&mut self, cd: &ComposeDraft) -> Result<Outcome> {
        let account = cd.account.or_else(|| self.labels.first().map(|l| l.id)).ok_or(EngineError::NoAccount)?;
        let sender = self.sender.as_ref().ok_or(EngineError::NoAccount)?;
        let id = sender.outbox.queue(account, &cd.draft, &iuredesk_compose::FsResolver).map_err(|e| {
            EngineError::Invalid(e.message(if crate::lang::spanish() {
                iuredesk_compose::Lang::Es
            } else {
                iuredesk_compose::Lang::En
            }))
        })?;
        sender.wake.notify_one();
        if let Some(saved) = cd.saved {
            self.discard_message(saved);
        }
        let window = self.undo_window();
        let scheduled = cd.draft.send_at.filter(|t| *t > Utc::now() + chrono::Duration::seconds(30));
        let text = match scheduled {
            Some(t) => {
                let when = crate::compose::schedule_label(t, Utc::now(), self.zone);
                Text::new(format!("Scheduled for {when}"), format!("Programado para {when}"))
            }
            None => Text::new("Sending…", "Enviando…"),
        };
        self.push_undo(text.clone(), vec![(Inverse::Send(id), None)]);
        self.counts.clear();
        let undo = scheduled.is_some() || !window.is_zero();
        Ok(Outcome::Notice(Notice {
            kind: NoticeKind::Info,
            text,
            undo,
            secs: if scheduled.is_some() { None } else { Some(u32::try_from(window.as_secs().max(1)).unwrap_or(30)) },
        }))
    }

    /// Saca un mensaje de la bandeja de salida y devuelve su borrador para reabrirlo.
    pub fn outbox_cancel(&mut self, id: OutboxId) -> Result<Outcome> {
        let sender = self.sender.as_ref().ok_or(EngineError::NoAccount)?;
        let account = sender.outbox.entry(id).ok().flatten().map(|e| e.account);
        match sender.outbox.cancel(id) {
            Ok(draft) => Ok(Outcome::Compose(Box::new(self.reopen(account, draft)))),
            Err(_) => Ok(Outcome::Notice(Notice::info(
                "It's already on its way and can't be stopped.",
                "Ya salió y no se puede detener.",
            ))),
        }
    }

    /// Reintenta ya un envío fallido.
    pub fn outbox_retry(&mut self, id: OutboxId) {
        if let Some(s) = &self.sender {
            let _ = s.outbox.retry(id);
            s.wake.notify_one();
        }
    }

    fn reopen(&self, account: Option<AccountId>, draft: Draft) -> ComposeDraft {
        ComposeDraft { kind: crate::compose::ComposeKind::New, account, draft, saved: None }
    }

    /// Deshace un envío (desde el aviso «Enviando… Deshacer»).
    pub(crate) fn undo_send(&mut self, id: OutboxId) -> Outcome {
        match self.outbox_cancel(id) {
            Ok(Outcome::Compose(d)) => {
                self.notices.push(Notice::success(
                    "Sending cancelled: the message is back in the editor.",
                    "Envío cancelado: el mensaje volvió al editor.",
                ));
                Outcome::Compose(d)
            }
            Ok(o) => o,
            Err(e) => Outcome::Notice(Notice::error(e.text())),
        }
    }

    /// Resultados del envío (desde [`Session::poll`]).
    pub(crate) fn poll_outbox(&mut self) {
        let Some(sender) = &self.sender else { return };
        let outcomes: Vec<SendOutcome> = sender.rx.try_iter().take(1_000).collect();
        let lang = if crate::lang::spanish() { iuredesk_compose::Lang::Es } else { iuredesk_compose::Lang::En };
        let other =
            if lang == iuredesk_compose::Lang::Es { iuredesk_compose::Lang::En } else { iuredesk_compose::Lang::Es };
        for o in outcomes {
            match o {
                SendOutcome::Sent { .. } => {
                    self.notices.push(Notice::success("Message sent", "Mensaje enviado"));
                    self.counts.clear();
                    self.live.dirty = true;
                    // La copia en Enviados y la marca del original van por la cola de `sync`.
                    let accounts: Vec<AccountId> = self.account_ids.values().copied().collect();
                    for a in accounts {
                        self.kick(a);
                    }
                }
                SendOutcome::Retrying { error, .. } => {
                    let (a, b) = (error.message(lang), error.message(other));
                    let (en, es) = if lang == iuredesk_compose::Lang::Es { (b, a) } else { (a, b) };
                    self.notices.push(Notice::info(
                        format!("Not sent yet: {en} It stays in the Outbox."),
                        format!("Aún no se envió: {es} Queda en la bandeja de salida."),
                    ));
                }
                SendOutcome::Failed { error, .. } => {
                    let (a, b) = (error.message(lang), error.message(other));
                    let (en, es) = if lang == iuredesk_compose::Lang::Es { (b, a) } else { (a, b) };
                    self.notices.push(Notice::error(Text::new(
                        format!("The message could not be sent: {en} Open the Outbox to retry."),
                        format!("No se pudo enviar el mensaje: {es} Abre la bandeja de salida para reintentar."),
                    )));
                }
            }
        }
    }
}
