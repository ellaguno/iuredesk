//! Correo de Exchange Server local por Exchange Web Services (F8.2): árbol de carpetas,
//! `SyncFolderItems` por carpeta (estado en `sync_state`), datos por lote con `GetItem`, MIME bajo
//! demanda, banderas, mover/borrar, borradores, categorías → etiquetas, historial por fecha y
//! envío con `SendAndSaveCopy`.
//!
//! La primera vez de una carpeta no se recorre todo con `GetItem` (un buzón con 100 000 mensajes
//! serían miles de llamadas): se baja lo reciente con `FindItem` y los ids se recorren con
//! `SyncFolderItems` solo para llegar a un `SyncState` al día (en tandas acotadas que siguen en la
//! próxima pasada); al terminar se recoge lo que llegó mientras tanto. Lo anterior a la ventana
//! inicial es historial (`find_before`).
//!
//! En EWS el id de un mensaje **cambia** al moverlo: lo que se mueve desde aquí adopta el id nuevo;
//! lo movido por otro cliente llega como borrado en una carpeta y alta en otra.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use iuredesk_accounts::{SecretPurpose, SecretStore};
use iuredesk_compose::{OutgoingMessage, SendError, SendErrorKind, SendReceipt, Transport, with_bcc_header};
use iuredesk_ews::{BasicAuth, BoxFuture as EwsFuture, EwsAuth, EwsClient, EwsError, ItemInfo, MessageDisposition};
use iuredesk_model::account::{Account, AuthMethod, Security};
use iuredesk_model::{Flags, Folder, FolderId, FolderRole, Mailbox, MessageEnvelope, MessageId, make_preview};
use iuredesk_store::{NewMessage, ProviderMeta};

use crate::backend::{BoxFuture, MailBackend};
use crate::engine::Ctx;
use crate::error::{Result, SyncError};
use crate::imap_sync::CycleReport;
use crate::ops::{MailOp, QueueWork, SyncOp, classify, pending_messages};
use crate::parse::{clean_id, enrich, parse_body};
use crate::state::{self, EwsFolderState, ews_key};
use crate::{NewMailSample, Phase, SyncEvent};

/// Carpetas máximas.
const MAX_FOLDERS: usize = 2_000;
/// Operaciones por pasada.
const OPS_PER_PASS: usize = 500;
/// Espera máxima entre reintentos de una operación.
const MAX_OP_BACKOFF: Duration = Duration::from_secs(15 * 60);
/// Mensajes que se bajan la primera vez (lo reciente) por carpeta.
const INITIAL_MAX: u32 = 1_000;
/// Llamadas a `SyncFolderItems` por carpeta y pasada (recorrido inicial y cambios).
const SYNC_CALLS_PER_PASS: usize = 20;
/// Mensajes por página del historial antiguo.
const BACKFILL_PAGE: usize = 100;
/// Margen al recoger lo que llegó durante el recorrido inicial (relojes que no coinciden).
const CATCH_UP_MARGIN_MS: i64 = 10 * 60 * 1000;
/// Carpetas con nombre de EWS y su función.
const WELL_KNOWN: &[(&str, FolderRole)] = &[
    ("inbox", FolderRole::Inbox),
    ("sentitems", FolderRole::Sent),
    ("drafts", FolderRole::Drafts),
    ("deleteditems", FolderRole::Trash),
    ("junkemail", FolderRole::Junk),
    ("archive", FolderRole::Archive),
];

/// Usuario y contraseña del llavero (la contraseña se lee la primera vez y tras un rechazo).
pub(crate) struct SecretAuth {
    secrets: Arc<dyn SecretStore>,
    account: String,
    username: String,
    /// Cabecera `Authorization` ya armada.
    cache: Mutex<Option<String>>,
}

impl SecretAuth {
    pub(crate) fn new(secrets: Arc<dyn SecretStore>, account: &Account) -> Self {
        Self {
            secrets,
            account: account.id.clone(),
            username: account.incoming.username.clone(),
            cache: Mutex::new(None),
        }
    }
}

impl EwsAuth for SecretAuth {
    fn authorization(&self) -> EwsFuture<'_, iuredesk_ews::Result<String>> {
        Box::pin(async move {
            let cached = self.cache.lock().unwrap_or_else(PoisonError::into_inner).clone();
            if let Some(h) = cached {
                return Ok(h);
            }
            let (secrets, id) = (Arc::clone(&self.secrets), self.account.clone());
            let pass = tokio::task::spawn_blocking(move || secrets.get(&id, SecretPurpose::Incoming))
                .await
                .map_err(|e| EwsError::Auth(e.to_string()))?
                .map_err(|e| EwsError::Auth(e.to_string()))?
                .ok_or(EwsError::Unauthorized)?;
            let header = BasicAuth::new(&self.username, &pass).header().to_string();
            *self.cache.lock().unwrap_or_else(PoisonError::into_inner) = Some(header.clone());
            Ok(header)
        })
    }

    fn invalidate(&self) {
        *self.cache.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

/// Cliente de EWS de una cuenta con su contraseña del llavero.
pub(crate) fn client_for(account: &Account, secrets: Arc<dyn SecretStore>, timeout: Duration) -> Result<EwsClient> {
    if matches!(account.incoming.auth, AuthMethod::OAuth2 { .. }) {
        return Err(SyncError::Ews(EwsError::Auth(
            "Exchange local con inicio de sesión de la organización aún no está disponible".into(),
        )));
    }
    let i = &account.incoming;
    let url = iuredesk_ews::server_url(&i.host, i.port, i.security != Security::Plain)?;
    Ok(EwsClient::new(&url, Arc::new(SecretAuth::new(secrets, account)), timeout)?)
}

fn addr(a: &iuredesk_ews::Addr) -> Mailbox {
    Mailbox::new(a.name.clone(), a.email.clone())
}

/// Banderas desde un mensaje de EWS.
fn flags_of(i: &ItemInfo) -> Flags {
    Flags {
        seen: i.is_read.unwrap_or(false),
        flagged: i.flagged.unwrap_or(false),
        draft: i.is_draft.unwrap_or(false),
        ..Default::default()
    }
}

/// Sobre a partir de un mensaje de EWS.
pub(crate) fn envelope_from_ews(i: &ItemInfo, folder: FolderId) -> MessageEnvelope {
    let from: Vec<Mailbox> = i.from.as_ref().or(i.sender.as_ref()).map(addr).into_iter().collect();
    let mut env = MessageEnvelope {
        folder,
        remote_id: Some(i.id.clone()),
        message_id: i.internet_message_id.as_deref().and_then(clean_id),
        from,
        to: i.to.iter().map(addr).collect(),
        cc: i.cc.iter().map(addr).collect(),
        reply_to: i.reply_to.iter().map(addr).collect(),
        subject: i.subject.clone(),
        date: i.received.or(i.sent).unwrap_or_else(Utc::now),
        flags: flags_of(i),
        keywords: i.categories.clone(),
        has_attachments: i.has_attachments,
        preview: make_preview(i.preview.as_deref().unwrap_or_default()),
        size: i.size.unwrap_or(0),
        ..Default::default()
    };
    env.clamp();
    env
}

/// Clave de ordenación de carpetas (la bandeja de entrada primero).
fn role_rank(r: &FolderRole) -> u8 {
    match r {
        FolderRole::Inbox => 0,
        FolderRole::Sent => 1,
        FolderRole::Drafts => 2,
        FolderRole::Archive => 3,
        FolderRole::Custom => 4,
        FolderRole::Junk => 5,
        FolderRole::Trash => 6,
    }
}

/// Carpetas de EWS con nombre (ids reales) y la raíz del buzón.
#[derive(Clone, Default)]
struct Known {
    roles: HashMap<String, FolderRole>,
    root: Option<String>,
    outbox: Option<String>,
}

/// Una cuenta de correo por EWS.
pub(crate) struct EwsAccount {
    pub ctx: Arc<Ctx>,
    client: EwsClient,
    known: Mutex<Option<Known>>,
    backoff: Mutex<HashMap<u64, Instant>>,
}

impl EwsAccount {
    pub fn new(ctx: Arc<Ctx>, secrets: Arc<dyn SecretStore>) -> Result<Self> {
        let client = client_for(&ctx.account, secrets, ctx.opts.io_timeout)?;
        Ok(Self::with_client(ctx, client))
    }

    pub(crate) fn with_client(ctx: Arc<Ctx>, client: EwsClient) -> Self {
        Self { ctx, client, known: Mutex::new(None), backoff: Mutex::new(HashMap::new()) }
    }

    // ---------------------------------------------------------------- carpetas

    async fn known(&self) -> Result<Known> {
        if let Some(k) = self.known.lock().unwrap_or_else(PoisonError::into_inner).clone() {
            return Ok(k);
        }
        let mut names: Vec<&str> = WELL_KNOWN.iter().map(|(n, _)| *n).collect();
        names.extend(["msgfolderroot", "outbox"]);
        let found = self.client.distinguished_folders(&names).await?;
        let mut k = Known::default();
        for (name, id) in found {
            let Some(id) = id else { continue };
            match name.as_str() {
                "msgfolderroot" => k.root = Some(id),
                "outbox" => k.outbox = Some(id),
                n => {
                    if let Some((_, role)) = WELL_KNOWN.iter().find(|(w, _)| *w == n) {
                        k.roles.insert(id, *role);
                    }
                }
            }
        }
        *self.known.lock().unwrap_or_else(PoisonError::into_inner) = Some(k.clone());
        Ok(k)
    }

    /// Árbol de carpetas de correo → carpetas locales (crea, renombra, borra las que ya no existen).
    async fn sync_folder_list(&self) -> Result<Vec<Folder>> {
        let ctx = &self.ctx;
        let known = self.known().await?;
        let mut remote = Vec::new();
        let mut st: Option<String> = None;
        for _ in 0..20 {
            let h = self.client.sync_folder_hierarchy(st.as_deref()).await?;
            remote.extend(h.changed.into_iter().filter(|f| f.is_mail()));
            st = Some(h.state);
            if h.last || remote.len() >= MAX_FOLDERS {
                break;
            }
        }
        remote.truncate(MAX_FOLDERS);
        // La bandeja de salida del servidor no se muestra: IureDesk tiene la suya.
        remote.retain(|f| Some(&f.id) != known.outbox.as_ref());
        let pending = crate::ops::pending_folders(&ctx.store, ctx.acct);
        let local = ctx.store.folders(ctx.acct)?;
        let mut by_path: HashMap<String, Folder> = local.into_iter().map(|f| (f.remote_path.clone(), f)).collect();
        let mut out: Vec<(Folder, Option<String>)> = Vec::new();
        for r in &remote {
            if pending.remote.contains(&r.id) && !by_path.contains_key(&r.id) {
                continue;
            }
            let parent = r.parent_id.clone().filter(|p| Some(p) != known.root.as_ref());
            if let Some(f) = by_path.get(&r.id).filter(|f| pending.local.contains(&f.id)).cloned() {
                by_path.remove(&r.id);
                out.push((f, parent));
                continue;
            }
            let role = known.roles.get(&r.id).copied().unwrap_or(FolderRole::Custom);
            let f = match by_path.remove(&r.id) {
                Some(mut f) => {
                    if f.name != r.name || f.role != role {
                        f.name.clone_from(&r.name);
                        f.role = role;
                        ctx.store.put_folder(&f)?;
                    }
                    f
                }
                None => {
                    let mut f = Folder {
                        account: ctx.acct,
                        name: r.name.clone(),
                        remote_path: r.id.clone(),
                        role,
                        ..Default::default()
                    };
                    f.id = ctx.store.put_folder(&f)?;
                    ctx.emit(SyncEvent::FolderUpdated { account: ctx.acct, folder: f.id });
                    f
                }
            };
            out.push((f, parent));
        }
        let ids: HashMap<String, FolderId> = out.iter().map(|(f, _)| (f.remote_path.clone(), f.id)).collect();
        for (f, parent) in &mut out {
            let p = parent.as_ref().and_then(|p| ids.get(p).copied());
            if f.parent != p {
                f.parent = p;
                ctx.store.put_folder(f)?;
            }
        }
        // Solo se borra lo local si la lista es creíble (trae la bandeja de entrada).
        if out.iter().any(|(f, _)| f.role == FolderRole::Inbox) {
            for (_, gone) in by_path.into_iter().filter(|(_, f)| !pending.local.contains(&f.id)) {
                ctx.store.delete_folder(gone.id)?;
                let _ = ctx.store.delete_sync_state(&ews_key(ctx.acct, gone.id));
                ctx.emit(SyncEvent::FolderUpdated { account: ctx.acct, folder: gone.id });
            }
        }
        Ok(out.into_iter().map(|(f, _)| f).collect())
    }

    // ---------------------------------------------------------------- pasada

    async fn cycle_inner(&self, inbox_only: bool) -> Result<CycleReport> {
        let ctx = &self.ctx;
        let mut folders = if inbox_only {
            ctx.store.folders(ctx.acct)?.into_iter().filter(|f| f.role == FolderRole::Inbox).collect()
        } else {
            self.sync_folder_list().await?
        };
        if folders.is_empty() {
            folders = self.sync_folder_list().await?;
        }
        self.replay_inner().await?;
        let pending = pending_messages(&ctx.store, ctx.acct);
        folders.sort_by(|a, b| role_rank(&a.role).cmp(&role_rank(&b.role)).then_with(|| a.name.cmp(&b.name)));
        let mut report = CycleReport::default();
        let mut initial = false;
        for f in folders.iter().filter(|f| !inbox_only || f.role == FolderRole::Inbox) {
            match self.sync_folder(f, &pending).await {
                Ok((first, more)) => {
                    initial |= first;
                    report.backfill_pending |= more;
                }
                Err(e @ SyncError::Ews(EwsError::ServerBusy { .. } | EwsError::Unauthorized)) => return Err(e),
                Err(e) => ctx.emit_error(&e),
            }
        }
        if !inbox_only {
            for f in ctx.store.folders(ctx.acct)? {
                if let Some(st) = state::load::<EwsFolderState>(&ctx.store, &ews_key(ctx.acct, f.id)) {
                    report.backfill_pending |= st.initial_done && !st.backfill_done && ctx.opts.backfill;
                }
            }
        }
        if initial {
            ctx.store.rethread_account(ctx.acct)?;
        }
        Ok(report)
    }

    /// Una carpeta. Devuelve (primera vez con mensajes, queda historial).
    async fn sync_folder(&self, f: &Folder, pending: &HashSet<MessageId>) -> Result<(bool, bool)> {
        let ctx = &self.ctx;
        let key = ews_key(ctx.acct, f.id);
        let mut st: EwsFolderState = state::load(&ctx.store, &key).unwrap_or_default();
        let mut inserted = Vec::new();
        let first = !st.initial_done;
        if first {
            inserted.extend(self.initial(f, &mut st).await?);
            state::save(&ctx.store, &key, &st)?;
        } else {
            let mut changed = false;
            for _ in 0..SYNC_CALLS_PER_PASS {
                let page = self.client.sync_folder_items(&f.remote_path, st.sync_state.as_deref(), 512).await?;
                let (new, ch) = self.apply_changes(f, &page, pending).await?;
                inserted.extend(new);
                changed |= ch;
                st.sync_state = Some(page.state);
                state::save(&ctx.store, &key, &st)?;
                if page.last {
                    break;
                }
            }
            if changed && inserted.is_empty() {
                ctx.emit(SyncEvent::FolderUpdated { account: ctx.acct, folder: f.id });
            }
        }
        if f.role == FolderRole::Inbox && !inserted.is_empty() {
            self.prefetch(&inserted).await;
        }
        if !inserted.is_empty() {
            ctx.emit(SyncEvent::FolderUpdated { account: ctx.acct, folder: f.id });
        }
        Ok((first && !inserted.is_empty(), st.initial_done && !st.backfill_done && ctx.opts.backfill))
    }

    /// Primera vez: lo reciente y el recorrido de ids hasta un `SyncState` al día.
    async fn initial(&self, f: &Folder, st: &mut EwsFolderState) -> Result<Vec<(MessageId, DateTime<Utc>)>> {
        let ctx = &self.ctx;
        let mut inserted = Vec::new();
        if st.started_ms.is_none() {
            let now = Utc::now();
            let since = now - chrono::Duration::days(i64::from(ctx.opts.initial_days));
            let found = self.client.find_since(&f.remote_path, since, INITIAL_MAX).await?;
            inserted.extend(self.insert_ids(f, found.iter().map(|(id, _)| id.clone()).collect(), false).await?);
            let oldest = found.iter().filter_map(|(_, d)| *d).min();
            st.started_ms = Some(now.timestamp_millis());
            // Si se llegó al tope, lo que quede de la ventana es historial.
            st.oldest_ms = Some(match oldest {
                Some(d) if found.len() >= INITIAL_MAX as usize => d.timestamp_millis(),
                _ => since.timestamp_millis(),
            });
        }
        for _ in 0..SYNC_CALLS_PER_PASS {
            let page = self.client.sync_folder_items(&f.remote_path, st.sync_state.as_deref(), 512).await?;
            st.sync_state = Some(page.state);
            if page.last {
                // Lo que llegó mientras se recorría (con margen; lo que ya está se salta).
                let from = st.started_ms.unwrap_or(0).saturating_sub(CATCH_UP_MARGIN_MS);
                if let Some(at) = DateTime::<Utc>::from_timestamp_millis(from) {
                    let found = self.client.find_since(&f.remote_path, at, INITIAL_MAX).await?;
                    inserted.extend(self.insert_ids(f, found.into_iter().map(|(id, _)| id).collect(), false).await?);
                }
                st.initial_done = true;
                break;
            }
        }
        Ok(inserted)
    }

    /// Aplica una página de cambios. Devuelve (insertados, algo cambió).
    async fn apply_changes(
        &self,
        f: &Folder,
        page: &iuredesk_ews::ItemSync,
        pending: &HashSet<MessageId>,
    ) -> Result<(Vec<(MessageId, DateTime<Utc>)>, bool)> {
        let ctx = &self.ctx;
        let mut changed = false;
        for id in &page.deleted {
            if let Some(local) = ctx.store.message_by_remote(f.id, id)?
                && !pending.contains(&local)
            {
                ctx.store.remove_from_folder(local, f.id)?;
                changed = true;
            }
        }
        for (id, read) in &page.read_changes {
            if let Some(local) = ctx.store.message_by_remote(f.id, id)?
                && !pending.contains(&local)
                && let Some(env) = ctx.store.get_envelope(local)?
                && env.flags.seen != *read
            {
                ctx.store.set_flags(local, Flags { seen: *read, ..env.flags })?;
                changed = true;
            }
        }
        // Cambiados que ya están: sus banderas y categorías; los que no, se insertan.
        let mut refresh = Vec::new();
        let mut fresh = Vec::new();
        for (id, _) in page.updated.iter().chain(&page.created) {
            match ctx.store.message_by_remote(f.id, id)? {
                Some(local) if !pending.contains(&local) => refresh.push((local, id.clone())),
                Some(_) => {}
                None => fresh.push(id.clone()),
            }
        }
        if !refresh.is_empty() {
            let ids: Vec<String> = refresh.iter().map(|(_, id)| id.clone()).collect();
            for info in self.client.get_summaries(&ids).await? {
                if let Some((local, _)) = refresh.iter().find(|(_, id)| *id == info.id) {
                    changed |= self.update_from_server(*local, &info)?;
                }
            }
        }
        let notify = f.role == FolderRole::Inbox;
        let inserted = self.insert_ids(f, fresh, notify).await?;
        Ok((inserted, changed))
    }

    /// Banderas y categorías del servidor (gana el servidor salvo operación local pendiente).
    fn update_from_server(&self, local: MessageId, i: &ItemInfo) -> Result<bool> {
        let store = &self.ctx.store;
        let Some(env) = store.get_envelope(local)? else { return Ok(false) };
        let mut changed = false;
        let mut server = flags_of(i);
        server.answered = env.flags.answered;
        server.forwarded = env.flags.forwarded;
        if i.is_read.is_some() && env.flags != server {
            store.set_flags(local, server)?;
            changed = true;
        }
        changed |= store.set_keywords(local, i.categories.clone())?;
        Ok(changed)
    }

    /// Pide los datos de `ids` y los inserta (los que ya están se saltan).
    async fn insert_ids(&self, f: &Folder, ids: Vec<String>, notify: bool) -> Result<Vec<(MessageId, DateTime<Utc>)>> {
        let ctx = &self.ctx;
        let mut want = Vec::new();
        for id in ids {
            if ctx.store.message_by_remote(f.id, &id)?.is_none() && !want.contains(&id) {
                want.push(id);
            }
        }
        if want.is_empty() {
            return Ok(Vec::new());
        }
        let items = self.client.get_summaries(&want).await?;
        self.insert(f, &items, notify)
    }

    /// Inserta mensajes nuevos de una carpeta.
    fn insert(&self, f: &Folder, items: &[ItemInfo], notify: bool) -> Result<Vec<(MessageId, DateTime<Utc>)>> {
        let ctx = &self.ctx;
        let mut batch = Vec::new();
        let mut metas = Vec::new();
        // Copias locales que esperan su id (la de «Enviados» que deja la bandeja de salida).
        let mut adopt = state::adopt_list(&ctx.store, f.id);
        let adopt_before = adopt.len();
        let mut waiting: HashMap<String, MessageId> = HashMap::new();
        for a in &adopt {
            if let Some(e) = ctx.store.get_envelope(*a)?
                && e.folder == f.id
                && e.remote_id.is_none()
                && let Some(mid) = e.message_id
            {
                waiting.insert(mid, *a);
            }
        }
        for i in items {
            let mut env = envelope_from_ews(i, f.id);
            if let Some(local) = env.message_id.as_ref().and_then(|m| waiting.remove(m)) {
                ctx.store.set_remote_id(local, Some(i.id.clone()))?;
                adopt.retain(|a| *a != local);
                continue;
            }
            let hint = i.conversation_id.as_deref().filter(|c| !c.is_empty()).map(|c| format!("conv:{c}"));
            if let Some(h) = &hint {
                env.thread = ctx.store.thread_by_hint(ctx.acct, h)?;
            }
            metas.push(ProviderMeta { provider_id: None, thread_hint: hint });
            batch.push(NewMessage { envelope: env, raw: None, body_text: None });
        }
        if adopt.len() != adopt_before {
            state::set_adopt(&ctx.store, f.id, &adopt)?;
        }
        let samples: Vec<(String, String, DateTime<Utc>)> = batch
            .iter()
            .map(|n| (n.envelope.sender_display().to_string(), n.envelope.subject.clone(), n.envelope.date))
            .collect();
        let ids = if batch.is_empty() { Vec::new() } else { ctx.store.put_messages(batch)? };
        let pm: Vec<(MessageId, ProviderMeta)> = ids.iter().copied().zip(metas).collect();
        ctx.store.set_provider_meta(&pm)?;
        let mut out = Vec::new();
        let mut sample = Vec::new();
        for (id, (from, subject, d)) in ids.into_iter().zip(samples) {
            out.push((id, d));
            if sample.len() < 3 {
                sample.push(NewMailSample { message: id, from, subject });
            }
        }
        ctx.emit(SyncEvent::Progress {
            account: ctx.acct,
            phase: Phase::Headers,
            folder: Some(f.id),
            done: out.len() as u64,
            total: items.len() as u64,
        });
        if notify && !out.is_empty() {
            ctx.emit(SyncEvent::NewMail { account: ctx.acct, count: out.len(), sample });
            ctx.emit(SyncEvent::Arrived {
                account: ctx.acct,
                folder: f.id,
                messages: out.iter().map(|(id, _)| *id).collect(),
            });
        }
        Ok(out)
    }

    /// Precarga los cuerpos más recientes (los fallos no cuentan: se bajan al abrir).
    async fn prefetch(&self, inserted: &[(MessageId, DateTime<Utc>)]) {
        let mut pick: Vec<(MessageId, DateTime<Utc>)> = inserted.to_vec();
        pick.sort_unstable_by_key(|b| std::cmp::Reverse(b.1));
        pick.truncate(self.ctx.opts.prefetch_recent);
        for (id, _) in pick {
            if self.download(id).await.is_err() {
                break;
            }
        }
    }

    /// Baja el MIME y lo guarda.
    async fn download(&self, msg: MessageId) -> Result<()> {
        let store = &self.ctx.store;
        let mut env = store.get_envelope(msg)?.ok_or_else(|| SyncError::MessageNotFound(msg.to_string()))?;
        if env.blob.is_some() {
            return Ok(());
        }
        let id = env.remote_id.clone().ok_or_else(|| SyncError::MessageNotFound(msg.to_string()))?;
        let raw = self.client.get_mime(&id).await?;
        if raw.is_empty() {
            return Err(SyncError::MessageNotFound(msg.to_string()));
        }
        let body = parse_body(&raw);
        let text = body.body_text.clone();
        let preview = env.preview.clone();
        enrich(&mut env, body);
        if env.preview.is_empty() {
            env.preview = preview;
        }
        env.size = raw.len() as u64;
        store.put_messages(vec![NewMessage { envelope: env, raw: Some(raw), body_text: Some(text) }])?;
        Ok(())
    }

    // ---------------------------------------------------------------- historial

    async fn backfill_inner(&self) -> Result<bool> {
        let ctx = &self.ctx;
        let mut budget = ctx.opts.backfill_slice.max(1);
        let mut folders = ctx.store.folders(ctx.acct)?;
        folders.sort_by(|a, b| role_rank(&a.role).cmp(&role_rank(&b.role)).then_with(|| a.name.cmp(&b.name)));
        let mut more = false;
        let mut any = false;
        for f in folders {
            let key = ews_key(ctx.acct, f.id);
            let Some(mut st) = state::load::<EwsFolderState>(&ctx.store, &key) else { continue };
            if !st.initial_done || st.backfill_done {
                continue;
            }
            if budget == 0 {
                more = true;
                break;
            }
            let before = st.oldest_ms.and_then(DateTime::<Utc>::from_timestamp_millis).unwrap_or_else(Utc::now);
            let top = budget.min(BACKFILL_PAGE);
            let found = self.client.find_before(&f.remote_path, before, u32::try_from(top).unwrap_or(100)).await?;
            let inserted = self.insert_ids(&f, found.iter().map(|(id, _)| id.clone()).collect(), false).await?;
            budget = budget.saturating_sub(found.len().max(1));
            match found.iter().filter_map(|(_, d)| *d).min() {
                Some(d) if found.len() >= top => {
                    // Mismo milisegundo en el borde: se avanza 1 ms para no repetir la página.
                    st.oldest_ms = Some(d.timestamp_millis().min(before.timestamp_millis().saturating_sub(1)));
                    more = true;
                }
                _ => {
                    st.backfill_done = true;
                    any = true;
                }
            }
            state::save(&ctx.store, &key, &st)?;
            if !inserted.is_empty() {
                ctx.emit(SyncEvent::FolderUpdated { account: ctx.acct, folder: f.id });
            }
        }
        if any || !more {
            ctx.store.rethread_account(ctx.acct)?;
        }
        Ok(more)
    }

    // ---------------------------------------------------------------- cola

    async fn replay_inner(&self) -> Result<()> {
        let ctx = &self.ctx;
        let ops: Vec<_> = ctx.store.peek_ops(OPS_PER_PASS)?.into_iter().filter(|o| o.account == ctx.acct).collect();
        let total = ops.len() as u64;
        for (i, op) in ops.into_iter().enumerate() {
            let wait = self.backoff.lock().unwrap_or_else(PoisonError::into_inner).get(&op.seq).copied();
            if wait.is_some_and(|w| Instant::now() < w) {
                break;
            }
            let work = match classify(&op.kind, &op.payload) {
                None => continue,
                Some(None) => {
                    ctx.store.ack_op(op.seq)?;
                    continue;
                }
                Some(Some(w)) => w,
            };
            let r = match &work {
                QueueWork::Mail(m) => self.apply_op(m).await,
                // `SendAndSaveCopy` deja la copia en «Enviados»; EWS no tiene «respondido» que marcar
                // desde MIME.
                QueueWork::Compose(
                    SyncOp::AppendToSent { .. } | SyncOp::MarkAnswered { .. } | SyncOp::MarkForwarded { .. },
                ) => Ok(()),
            };
            match r {
                Ok(()) => {
                    ctx.store.ack_op(op.seq)?;
                    self.backoff.lock().unwrap_or_else(PoisonError::into_inner).remove(&op.seq);
                }
                Err(e) => {
                    let attempts = ctx.store.fail_op(op.seq, &e.to_string())?;
                    let busy = matches!(e, SyncError::Ews(EwsError::ServerBusy { .. }));
                    let permanent = !e.is_transient();
                    if (permanent && attempts >= 3) || attempts >= ctx.opts.op_max_attempts {
                        ctx.store.ack_op(op.seq)?;
                        ctx.emit_error(&e);
                    } else {
                        let secs = 2u64.saturating_pow(attempts.min(20));
                        let delay = Duration::from_secs(secs).min(MAX_OP_BACKOFF);
                        self.backoff
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .insert(op.seq, Instant::now() + delay);
                    }
                    if busy || e.needs_reauth() {
                        return Err(e);
                    }
                    break;
                }
            }
            ctx.emit(SyncEvent::Progress {
                account: ctx.acct,
                phase: Phase::Ops,
                folder: None,
                done: (i as u64).saturating_add(1),
                total,
            });
        }
        Ok(())
    }

    /// Id real de una carpeta creada en local (el destino de un mover).
    fn real_folder(&self, remote: &str, local: FolderId) -> Result<String> {
        if !remote.starts_with(crate::ops::LOCAL_FOLDER_PREFIX) {
            return Ok(remote.to_string());
        }
        match self.ctx.store.get_folder(local)? {
            Some(f) if !f.remote_path.starts_with(crate::ops::LOCAL_FOLDER_PREFIX) => Ok(f.remote_path),
            _ => Err(SyncError::MessageNotFound(remote.to_string())),
        }
    }

    async fn apply_op(&self, op: &MailOp) -> Result<()> {
        let store = &self.ctx.store;
        match op {
            MailOp::RemoteFlags { remote, add, remove, .. } => {
                let has = |l: &[String], f: &str| l.iter().any(|x| x.eq_ignore_ascii_case(f));
                let read = (has(add, "\\Seen") || has(remove, "\\Seen")).then(|| has(add, "\\Seen"));
                let flagged = (has(add, "\\Flagged") || has(remove, "\\Flagged")).then(|| has(add, "\\Flagged"));
                self.client.update_flags(remote, read, flagged, None).await?;
                Ok(())
            }
            MailOp::RemoteMove { msg, remote, to, to_folder } => {
                let to = self.real_folder(to, *to_folder)?;
                // En EWS el id cambia al mover: se adopta el nuevo.
                if let Some(new) = self.client.move_item(remote, &to).await?
                    && let Some(env) = store.get_envelope(*msg)?
                    && env.folder == *to_folder
                {
                    store.set_remote_id(*msg, Some(new))?;
                }
                Ok(())
            }
            MailOp::RemoteDelete { remote } => {
                self.client.delete_item(remote, true).await?;
                Ok(())
            }
            MailOp::Append { msg, folder, folder_id, .. } => {
                let Ok(raw) = store.get_raw(*msg) else { return Ok(()) };
                let draft = store.get_folder(*folder_id)?.is_some_and(|f| f.role == FolderRole::Drafts);
                let target = self.real_folder(folder, *folder_id)?;
                let id = self.client.create_from_mime(&raw, &target, MessageDisposition::Save { draft }).await?;
                if let Some(id) = id
                    && let Some(env) = store.get_envelope(*msg)?
                    && env.folder == *folder_id
                    && env.remote_id.is_none()
                {
                    store.set_remote_id(*msg, Some(id))?;
                }
                Ok(())
            }
            MailOp::CreateFolder { folder_id, name, parent, .. } => {
                let known = self.known().await?;
                let parent = match parent.as_deref().filter(|p| !p.starts_with(crate::ops::LOCAL_FOLDER_PREFIX)) {
                    Some(p) => p.to_string(),
                    None => known.root.unwrap_or_else(|| "msgfolderroot".to_string()),
                };
                match self.client.create_folder(&parent, name).await {
                    Ok(id) => {
                        if let Some(mut f) = store.get_folder(*folder_id)? {
                            f.remote_path = id;
                            store.put_folder(&f)?;
                            self.ctx.emit(SyncEvent::FolderUpdated { account: self.ctx.acct, folder: f.id });
                        }
                        Ok(())
                    }
                    // Ya existe con ese nombre: se adopta en la próxima lista.
                    Err(EwsError::Soap { code, .. }) if code == "ErrorFolderExists" => {
                        if let Some(f) = store.get_folder(*folder_id)? {
                            store.delete_folder(f.id)?;
                        }
                        Ok(())
                    }
                    Err(e) => Err(e.into()),
                }
            }
            MailOp::RenameFolder { folder_id, name, .. } => {
                let Some(f) = store.get_folder(*folder_id)? else { return Ok(()) };
                if f.remote_path.starts_with(crate::ops::LOCAL_FOLDER_PREFIX) {
                    return Ok(());
                }
                self.client.rename_folder(&f.remote_path, name).await?;
                Ok(())
            }
            MailOp::DeleteFolder { remote } => {
                if remote.starts_with(crate::ops::LOCAL_FOLDER_PREFIX) {
                    return Ok(());
                }
                self.client.delete_folder(remote).await?;
                Ok(())
            }
            // Operaciones de IMAP (UIDs, etiquetas de Gmail): no aplican a EWS.
            MailOp::Flags { .. } | MailOp::Move { .. } | MailOp::Expunge { .. } | MailOp::Labels { .. } => Ok(()),
        }
    }
}

impl MailBackend for EwsAccount {
    fn ctx(&self) -> &Arc<Ctx> {
        &self.ctx
    }

    fn cycle(&self, inbox_only: bool) -> BoxFuture<'_, Result<CycleReport>> {
        Box::pin(async move {
            let started = Instant::now();
            let r = self.cycle_inner(inbox_only).await;
            if let Ok(rep) = &r {
                self.ctx.emit(SyncEvent::Synced {
                    account: self.ctx.acct,
                    took: started.elapsed(),
                    backfill_pending: rep.backfill_pending,
                });
            }
            r
        })
    }

    fn backfill(&self) -> BoxFuture<'_, Result<bool>> {
        Box::pin(self.backfill_inner())
    }

    fn replay(&self) -> BoxFuture<'_, Result<()>> {
        Box::pin(self.replay_inner())
    }

    fn fetch_body(&self, msg: MessageId) -> BoxFuture<'_, Result<()>> {
        Box::pin(self.download(msg))
    }
}

// -------------------------------------------------------------------- envío

/// Envío por EWS (`CreateItem` con `SendAndSaveCopy`): Exchange guarda la copia en «Elementos
/// enviados», así que la bandeja de salida no encola `AppendToSent`. Como Graph, toma los
/// destinatarios del MIME: los ocultos del sobre van en un `Bcc:`.
pub struct EwsSendTransport {
    client: EwsClient,
    host: String,
}

impl std::fmt::Debug for EwsSendTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EwsSendTransport").field("client", &self.client).finish_non_exhaustive()
    }
}

impl EwsSendTransport {
    /// Transporte de una cuenta de Exchange local con su contraseña del llavero.
    pub fn for_account(account: &Account, secrets: Arc<dyn SecretStore>, timeout: Duration) -> Result<Self> {
        Ok(Self { client: client_for(account, secrets, timeout)?, host: account.incoming.host.clone() })
    }

    /// Transporte sobre un cliente ya hecho (pruebas).
    pub fn new(client: EwsClient) -> Self {
        Self { client, host: "Exchange".into() }
    }
}

fn map_send_error(e: &EwsError, host: &str) -> SendError {
    let kind = match e {
        EwsError::Network(_) => SendErrorKind::Network,
        EwsError::ServerBusy { .. } => SendErrorKind::TemporarilyRejected,
        EwsError::Unauthorized | EwsError::Auth(_) => SendErrorKind::Auth,
        EwsError::TooLarge => SendErrorKind::TooLarge,
        EwsError::Soap { code, .. } if code.contains("TooLarge") || code.contains("SizeExceeded") => {
            SendErrorKind::TooLarge
        }
        EwsError::Http { status } if *status >= 500 => SendErrorKind::TemporarilyRejected,
        _ => SendErrorKind::Rejected,
    };
    let mut err = SendError::new(kind, e.to_string());
    if let EwsError::Http { status } = e {
        err.code = Some(*status);
    }
    err.host = host.to_string();
    err
}

impl Transport for EwsSendTransport {
    fn send<'a>(
        &'a self,
        msg: &'a OutgoingMessage,
    ) -> iuredesk_oauth::BoxFuture<'a, std::result::Result<SendReceipt, SendError>> {
        Box::pin(async move {
            let mime = with_bcc_header(&msg.raw, &msg.recipients);
            self.client
                .create_from_mime(&mime, "sentitems", MessageDisposition::SendAndSaveCopy)
                .await
                .map_err(|e| map_send_error(&e, &self.host))?;
            Ok(SendReceipt { server_reply: "Accepted (Exchange Web Services SendAndSaveCopy)".into() })
        })
    }

    fn saves_to_sent(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ews_item_to_envelope() {
        let i = ItemInfo {
            id: "AAMk=".into(),
            subject: "Contrato".into(),
            sender: Some(iuredesk_ews::Addr { name: "Ana".into(), email: "ana@contoso.com".into() }),
            received: DateTime::parse_from_rfc3339("2026-10-01T10:00:00Z").ok().map(|d| d.with_timezone(&Utc)),
            is_read: Some(false),
            flagged: Some(true),
            categories: vec!["Cliente".into()],
            internet_message_id: Some("<a@contoso.com>".into()),
            preview: Some("Hola   Ana".into()),
            size: Some(10),
            ..Default::default()
        };
        let e = envelope_from_ews(&i, FolderId(3));
        assert_eq!(e.remote_id.as_deref(), Some("AAMk="));
        assert_eq!(e.from[0].display(), "Ana", "sin From se usa Sender");
        assert!(e.flags.flagged && !e.flags.seen);
        assert_eq!(e.message_id.as_deref(), Some("a@contoso.com"));
        assert_eq!(e.preview, "Hola Ana");
    }

    #[test]
    fn send_errors() {
        assert_eq!(map_send_error(&EwsError::Unauthorized, "h").kind, SendErrorKind::Auth);
        assert_eq!(
            map_send_error(&EwsError::ServerBusy { retry_after_ms: None }, "h").kind,
            SendErrorKind::TemporarilyRejected
        );
        let big = EwsError::Soap { code: "ErrorMessageSizeExceeded".into(), message: String::new() };
        assert_eq!(map_send_error(&big, "h").kind, SendErrorKind::TooLarge);
    }
}
