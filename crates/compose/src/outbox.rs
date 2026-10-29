//! Bandeja de salida sobre la cola de operaciones del almacén.
//!
//! - [`Outbox::queue`] copia los adjuntos al almacén de blobs (el envío no depende de que el
//!   archivo siga en su sitio), comprueba que el mensaje se puede construir, guarda el borrador y
//!   encola una operación `outbox.send`. El id de la entrada es el número de secuencia de la cola.
//! - **Deshacer**: nada sale antes de `undo_until` (ventana configurable de 0 a 30 s). Hasta que
//!   empieza a enviarse, [`Outbox::cancel`] la saca de la cola y devuelve el borrador para volver
//!   a abrirlo en el editor (también sirve para mensajes programados o atascados sin red).
//! - **Programado**: con `Draft::send_at`, la entrada espera hasta esa hora.
//! - **Reintentos**: los errores transitorios (red, 4xx) se reintentan con espera exponencial; los
//!   permanentes (5xx, credenciales) dejan la entrada en estado [`OutboxState::Failed`] con un
//!   mensaje humano, hasta que el usuario la reintenta o la cancela.
//! - **Tras enviar**: se encola para `sync` un `AppendToSent` (salvo que el proveedor ya guarde la
//!   copia) y se marca el original como respondido o reenviado (en local al momento y en el
//!   servidor vía `sync`).
//!
//! El reloj es inyectable ([`Clock`]) para probar la ventana y la programación sin esperar.
//! El estado mutable de cada entrada vive en el estado de sincronización del almacén
//! (`outbox/<id>`); si el programa se cierra a mitad de un envío, al abrir la entrada vuelve a
//! «en espera» (puede duplicarse un envío, nunca perderse).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use chrono::{DateTime, Local, Utc};
use iuredesk_model::{AccountId, BlobHash};
use iuredesk_store::Store;
use serde::{Deserialize, Serialize};

use crate::draft::{AttachmentSource, Draft, OriginalKind};
use crate::error::{ComposeError, Result};
use crate::mime::{BuildOptions, ContentResolver, MAX_MESSAGE_BYTES, StoreResolver, generate_message_id};
use crate::ops::{OP_SEND, OP_SYNC, SyncOp};
use crate::transport::{OutgoingMessage, SendError, SendErrorKind, Transport};

/// Id de una entrada de la bandeja de salida (número de secuencia de la cola).
pub type OutboxId = u64;

/// Ventana máxima para deshacer.
pub const MAX_UNDO_WINDOW: Duration = Duration::from_secs(30);
const STATE_PREFIX: &str = "outbox/";

/// Fuente de la hora actual.
pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

/// Reloj del sistema.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Reloj manual para pruebas (y para simulaciones de la UI).
#[derive(Debug, Clone)]
pub struct ManualClock(Arc<Mutex<DateTime<Utc>>>);

impl ManualClock {
    pub fn new(t: DateTime<Utc>) -> Self {
        Self(Arc::new(Mutex::new(t)))
    }

    pub fn set(&self, t: DateTime<Utc>) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = t;
    }

    pub fn advance(&self, d: Duration) {
        let mut g = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        *g = add(*g, d);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> DateTime<Utc> {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn add(t: DateTime<Utc>, d: Duration) -> DateTime<Utc> {
    chrono::Duration::from_std(d).ok().and_then(|d| t.checked_add_signed(d)).unwrap_or(t)
}

/// Ajustes de la bandeja de salida.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutboxConfig {
    /// Ventana para deshacer (se recorta a [`MAX_UNDO_WINDOW`]).
    pub undo_window: Duration,
    /// Intentos ante errores transitorios antes de rendirse.
    pub max_attempts: u32,
    /// Primera espera entre intentos; se duplica en cada fallo.
    pub backoff_base: Duration,
    /// Espera máxima entre intentos.
    pub backoff_max: Duration,
}

impl Default for OutboxConfig {
    fn default() -> Self {
        Self {
            undo_window: Duration::from_secs(10),
            max_attempts: 10,
            backoff_base: Duration::from_secs(30),
            backoff_max: Duration::from_secs(30 * 60),
        }
    }
}

impl OutboxConfig {
    /// Espera antes del intento `attempts + 1` (tras `attempts` fallos, ≥ 1).
    pub fn backoff(&self, attempts: u32) -> Duration {
        let exp = attempts.saturating_sub(1).min(30);
        let factor = 1u32.checked_shl(exp).unwrap_or(u32::MAX);
        self.backoff_base.saturating_mul(factor).min(self.backoff_max)
    }

    fn undo(&self) -> Duration {
        self.undo_window.min(MAX_UNDO_WINDOW)
    }
}

/// Estado de una entrada.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum OutboxState {
    /// Esperando su hora (ventana de deshacer, envío programado o reintento).
    Waiting,
    /// Enviándose ahora.
    Sending,
    /// Error permanente: espera a que el usuario actúe.
    Failed,
}

/// Una entrada de la bandeja de salida.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxEntry {
    pub id: OutboxId,
    pub account: AccountId,
    /// Blob con el borrador (JSON).
    pub draft_blob: BlobHash,
    /// `Message-ID` fijo para todos los intentos.
    pub message_id: String,
    /// Asunto (para la lista).
    pub subject: String,
    /// Destinatarios visibles (para la lista).
    pub to: String,
    pub queued_at: DateTime<Utc>,
    /// Hasta cuándo se puede deshacer sin que haya salido nada.
    pub undo_until: DateTime<Utc>,
    /// No antes de esta hora.
    pub not_before: DateTime<Utc>,
    /// ¿Lo programó el usuario para más tarde?
    pub scheduled: bool,
    pub attempts: u32,
    pub state: OutboxState,
    /// Último error (con su mensaje humano vía [`SendError::message`]).
    pub last_error: Option<SendError>,
}

/// Lo guardado en el blob: el borrador con su Message-ID (así dos borradores iguales no
/// comparten blob).
#[derive(Serialize, Deserialize)]
struct QueuedDraft {
    message_id: String,
    draft: Draft,
}

/// Carga de la operación `outbox.send` (inmutable).
#[derive(Serialize, Deserialize)]
struct SendPayload {
    draft_blob: BlobHash,
    message_id: String,
}

/// Resultado de procesar una entrada.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SendOutcome {
    Sent { id: OutboxId, message_id: String },
    Retrying { id: OutboxId, at: DateTime<Utc>, error: SendError },
    Failed { id: OutboxId, error: SendError },
}

/// De dónde sale el transporte de cada cuenta.
pub trait TransportSource: Send + Sync {
    fn transport(&self, account: AccountId) -> Option<Arc<dyn Transport>>;
}

impl TransportSource for HashMap<AccountId, Arc<dyn Transport>> {
    fn transport(&self, account: AccountId) -> Option<Arc<dyn Transport>> {
        self.get(&account).cloned()
    }
}

/// La bandeja de salida.
pub struct Outbox {
    store: Arc<Store>,
    clock: Arc<dyn Clock>,
    config: OutboxConfig,
    in_flight: Mutex<HashSet<OutboxId>>,
    /// Firma / cifrado al enviar (F5.4).
    sealer: Option<Arc<dyn crate::secure::MessageSealer>>,
}

impl std::fmt::Debug for Outbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Outbox").field("config", &self.config).finish_non_exhaustive()
    }
}

fn state_key(id: OutboxId) -> String {
    format!("{STATE_PREFIX}{id:020}")
}

impl Outbox {
    /// Abre la bandeja. Las entradas que quedaron «enviándose» (cierre a mitad) vuelven a espera.
    pub fn new(store: Arc<Store>, clock: Arc<dyn Clock>, config: OutboxConfig) -> Result<Self> {
        let outbox = Self { store, clock, config, in_flight: Mutex::new(HashSet::new()), sealer: None };
        for mut e in outbox.entries()? {
            if e.state == OutboxState::Sending {
                e.state = OutboxState::Waiting;
                outbox.put_entry(&e)?;
            }
        }
        Ok(outbox)
    }

    /// Instala quien firma / cifra los mensajes (F5.4).
    pub fn set_sealer(&mut self, sealer: Arc<dyn crate::secure::MessageSealer>) {
        self.sealer = Some(sealer);
    }

    pub fn config(&self) -> &OutboxConfig {
        &self.config
    }

    /// Cambia la ventana para deshacer (afecta a lo que se encole después).
    pub fn set_undo_window(&mut self, d: Duration) {
        self.config.undo_window = d.min(MAX_UNDO_WINDOW);
    }

    fn put_entry(&self, e: &OutboxEntry) -> Result<()> {
        self.store.put_sync_state(&state_key(e.id), &serde_json::to_vec(e)?)?;
        Ok(())
    }

    fn in_flight(&self) -> std::sync::MutexGuard<'_, HashSet<OutboxId>> {
        self.in_flight.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Encola un borrador. `resolver` lee los adjuntos que aún no están en el almacén.
    pub fn queue(&self, account: AccountId, draft: &Draft, resolver: &dyn ContentResolver) -> Result<OutboxId> {
        draft.validate()?;
        let mut d = draft.clone();
        let blobs = self.store.blobs();
        for a in &mut d.attachments {
            if !matches!(a.source, AttachmentSource::Blob(_)) {
                let data = resolver
                    .load(&a.source, MAX_MESSAGE_BYTES)
                    .map_err(|reason| ComposeError::Attachment { name: a.filename.clone(), reason })?;
                a.size = data.len() as u64;
                a.source = AttachmentSource::Blob(blobs.put(&data)?);
            }
        }
        if let Some(f) = &mut d.forwarded
            && !matches!(f.source, AttachmentSource::Blob(_))
        {
            let data = resolver
                .load(&f.source, MAX_MESSAGE_BYTES)
                .map_err(|reason| ComposeError::Attachment { name: f.filename.clone(), reason })?;
            f.source = AttachmentSource::Blob(blobs.put(&data)?);
        }
        let message_id = generate_message_id(&d.identity.email);
        // Construir ya avisa ahora (y no en segundo plano) de lo que impediría el envío.
        let now = self.clock.now();
        let opts = BuildOptions {
            date: now.fixed_offset(),
            message_id: Some(message_id.clone()),
            max_size: MAX_MESSAGE_BYTES,
            keep_bcc: false,
        };
        crate::secure::build_sealed(&d, &StoreResolver { store: &self.store }, &opts, self.sealer.as_ref())?;

        let to = d.to.iter().map(|m| m.display().to_string()).collect::<Vec<_>>().join(", ");
        let subject = d.subject.clone();
        let send_at = d.send_at;
        let queued = QueuedDraft { message_id: message_id.clone(), draft: d };
        let draft_blob = blobs.put(&serde_json::to_vec(&queued)?)?;
        let payload = serde_json::to_vec(&SendPayload { draft_blob, message_id: message_id.clone() })?;
        let id = self.store.enqueue_op(account, OP_SEND, &payload)?;
        let undo_until = add(now, self.config.undo());
        let not_before = match send_at {
            Some(t) if t > undo_until => t,
            _ => undo_until,
        };
        let entry = OutboxEntry {
            id,
            account,
            draft_blob,
            message_id,
            subject,
            to,
            queued_at: now,
            undo_until,
            not_before,
            scheduled: send_at.is_some_and(|t| t > undo_until),
            attempts: 0,
            state: OutboxState::Waiting,
            last_error: None,
        };
        self.put_entry(&entry)?;
        Ok(id)
    }

    /// Entradas pendientes, en orden de cola.
    pub fn entries(&self) -> Result<Vec<OutboxEntry>> {
        let mut out = Vec::new();
        for op in self.store.peek_ops(10_000)? {
            if op.kind != OP_SEND {
                continue;
            }
            match self.store.sync_state(&state_key(op.seq))? {
                Some(b) => match serde_json::from_slice::<OutboxEntry>(&b) {
                    Ok(e) => out.push(e),
                    Err(_) => out.extend(self.rebuild_entry(op.seq, op.account, &op.payload, op.created)),
                },
                None => out.extend(self.rebuild_entry(op.seq, op.account, &op.payload, op.created)),
            }
        }
        Ok(out)
    }

    /// Estado perdido o dañado: se reconstruye desde la operación (en espera, sin ventana).
    fn rebuild_entry(
        &self,
        id: OutboxId,
        account: AccountId,
        payload: &[u8],
        created: DateTime<Utc>,
    ) -> Option<OutboxEntry> {
        let p: SendPayload = serde_json::from_slice(payload).ok()?;
        Some(OutboxEntry {
            id,
            account,
            draft_blob: p.draft_blob,
            message_id: p.message_id,
            subject: String::new(),
            to: String::new(),
            queued_at: created,
            undo_until: created,
            not_before: created,
            scheduled: false,
            attempts: 0,
            state: OutboxState::Waiting,
            last_error: None,
        })
    }

    /// Una entrada.
    pub fn entry(&self, id: OutboxId) -> Result<Option<OutboxEntry>> {
        Ok(self.entries()?.into_iter().find(|e| e.id == id))
    }

    /// Próxima hora en que habrá algo que enviar (para dormir hasta entonces).
    pub fn next_due(&self) -> Result<Option<DateTime<Utc>>> {
        Ok(self.entries()?.iter().filter(|e| e.state == OutboxState::Waiting).map(|e| e.not_before).min())
    }

    fn load_draft(&self, blob: &BlobHash) -> Result<QueuedDraft> {
        let bytes = self.store.blobs().get(blob)?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// Cancela una entrada que aún no se está enviando y devuelve su borrador.
    pub fn cancel(&self, id: OutboxId) -> Result<Draft> {
        let mut flight = self.in_flight();
        if flight.contains(&id) {
            return Err(ComposeError::NotCancellable);
        }
        let Some(entry) = self.entry(id)? else {
            return Err(ComposeError::NotCancellable);
        };
        if entry.state == OutboxState::Sending {
            return Err(ComposeError::NotCancellable);
        }
        // Mientras se cancela, nadie más la toma.
        flight.insert(id);
        let result = (|| {
            let q = self.load_draft(&entry.draft_blob)?;
            self.store.ack_op(id)?;
            self.store.delete_sync_state(&state_key(id))?;
            let _ = self.store.blobs().remove(&entry.draft_blob);
            Ok(q.draft)
        })();
        flight.remove(&id);
        result
    }

    /// Vuelve a intentar ya una entrada fallida (o en espera de reintento).
    pub fn retry(&self, id: OutboxId) -> Result<()> {
        let Some(mut e) = self.entry(id)? else {
            return Err(ComposeError::NotFound { what: "el mensaje", name: id.to_string() });
        };
        if e.state == OutboxState::Sending {
            return Ok(());
        }
        e.state = OutboxState::Waiting;
        e.attempts = 0;
        e.not_before = self.clock.now();
        self.put_entry(&e)
    }

    /// Envía lo que ya toca. Devuelve qué pasó con cada entrada procesada.
    pub async fn process_due(&self, transports: &dyn TransportSource) -> Result<Vec<SendOutcome>> {
        let now = self.clock.now();
        let due: Vec<OutboxEntry> =
            self.entries()?.into_iter().filter(|e| e.state == OutboxState::Waiting && e.not_before <= now).collect();
        let mut outcomes = Vec::with_capacity(due.len());
        for mut entry in due {
            {
                let mut flight = self.in_flight();
                if !flight.insert(entry.id) {
                    continue;
                }
            }
            // ¿La canceló alguien justo antes?
            if self.entry(entry.id)?.is_none() {
                self.in_flight().remove(&entry.id);
                continue;
            }
            entry.state = OutboxState::Sending;
            let res = match self.put_entry(&entry) {
                Ok(()) => self.send_one(&mut entry, transports).await,
                Err(e) => Err(e),
            };
            self.in_flight().remove(&entry.id);
            outcomes.push(res?);
        }
        Ok(outcomes)
    }

    async fn send_one(&self, entry: &mut OutboxEntry, transports: &dyn TransportSource) -> Result<SendOutcome> {
        let now = self.clock.now();
        let Some(transport) = transports.transport(entry.account) else {
            let err = SendError::new(SendErrorKind::Config, "la cuenta no tiene transporte de envío");
            return self.fail(entry, err, now);
        };
        let queued = match self.load_draft(&entry.draft_blob) {
            Ok(q) => q,
            Err(e) => return self.fail(entry, SendError::new(SendErrorKind::Config, e.to_string()), now),
        };
        let opts = BuildOptions {
            date: now.with_timezone(&Local).fixed_offset(),
            message_id: Some(entry.message_id.clone()),
            max_size: MAX_MESSAGE_BYTES,
            keep_bcc: false,
        };
        let built = match crate::secure::build_sealed(
            &queued.draft,
            &StoreResolver { store: &self.store },
            &opts,
            self.sealer.as_ref(),
        ) {
            Ok(b) => b,
            Err(e) => return self.fail(entry, SendError::new(SendErrorKind::Config, e.to_string()), now),
        };
        let outgoing = OutgoingMessage::from(&built);
        match transport.send(&outgoing).await {
            Ok(_) => {
                self.after_sent(entry, &queued.draft, &built.raw, transport.saves_to_sent())?;
                Ok(SendOutcome::Sent { id: entry.id, message_id: entry.message_id.clone() })
            }
            Err(err) => self.fail(entry, err, self.clock.now()),
        }
    }

    /// Registra un fallo: reintento con espera o fallo permanente.
    fn fail(&self, entry: &mut OutboxEntry, err: SendError, now: DateTime<Utc>) -> Result<SendOutcome> {
        entry.attempts = entry.attempts.saturating_add(1);
        let _ = self.store.fail_op(entry.id, &err.to_string());
        entry.last_error = Some(err.clone());
        if err.is_transient() && entry.attempts < self.config.max_attempts {
            entry.state = OutboxState::Waiting;
            entry.not_before = add(now, self.config.backoff(entry.attempts));
            self.put_entry(entry)?;
            return Ok(SendOutcome::Retrying { id: entry.id, at: entry.not_before, error: err });
        }
        entry.state = OutboxState::Failed;
        self.put_entry(entry)?;
        Ok(SendOutcome::Failed { id: entry.id, error: err })
    }

    /// Tras un envío correcto: copia en Enviados, original marcado y entrada fuera de la cola.
    fn after_sent(&self, entry: &OutboxEntry, draft: &Draft, raw: &[u8], provider_saves: bool) -> Result<()> {
        // Primero se saca de la cola: si algo de lo siguiente falla, al menos no se reenvía.
        self.store.ack_op(entry.id)?;
        self.store.delete_sync_state(&state_key(entry.id))?;
        if !provider_saves {
            let hash = self.store.blobs().put(raw)?;
            let op = SyncOp::AppendToSent { raw: hash, size: raw.len() as u64, message_id: entry.message_id.clone() };
            self.store.enqueue_op(entry.account, OP_SYNC, &op.encode())?;
        }
        if let Some(orig) = &draft.original
            && let Some(mid) = orig.message
        {
            if let Some(env) = self.store.get_envelope(mid)? {
                let mut flags = env.flags;
                match orig.kind {
                    OriginalKind::Reply => flags.answered = true,
                    OriginalKind::Forward => flags.forwarded = true,
                }
                self.store.set_flags(mid, flags)?;
            }
            let op = match orig.kind {
                OriginalKind::Reply => SyncOp::MarkAnswered { message: mid },
                OriginalKind::Forward => SyncOp::MarkForwarded { message: mid },
            };
            self.store.enqueue_op(entry.account, OP_SYNC, &op.encode())?;
        }
        let _ = self.store.blobs().remove(&entry.draft_blob);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_caps() {
        let c = OutboxConfig::default();
        assert_eq!(c.backoff(1), Duration::from_secs(30));
        assert_eq!(c.backoff(2), Duration::from_secs(60));
        assert_eq!(c.backoff(3), Duration::from_secs(120));
        assert_eq!(c.backoff(100), Duration::from_secs(1800));
        assert_eq!(c.backoff(0), Duration::from_secs(30));
    }

    #[test]
    fn manual_clock() {
        let t = Utc::now();
        let c = ManualClock::new(t);
        c.advance(Duration::from_secs(5));
        assert_eq!(c.now(), add(t, Duration::from_secs(5)));
    }
}
