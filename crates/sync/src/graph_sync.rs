//! Correo de Microsoft 365 por Microsoft Graph (preferido a IMAP con OAuth): árbol de
//! `mailFolders`, delta por carpeta (`@removed` incluido; el `deltaLink` se guarda en
//! `sync_state`), MIME (`$value`) bajo demanda, banderas, mover/borrar/archivar, borradores,
//! categorías → etiquetas (`keywords`). Los ids son inmutables (`Prefer: IdType="ImmutableId"`):
//! no cambian al mover, así que un mensaje movido conserva su cuerpo y su hilo.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use base64::Engine as _;
use chrono::{DateTime, Utc};
use iuredesk_graph::Method;
use iuredesk_graph::{Body, GraphClient, GraphError, enc};
use iuredesk_model::{Flags, Folder, FolderId, FolderRole, Mailbox, MessageEnvelope, MessageId, make_preview};
use iuredesk_oauth::TokenProvider;
use iuredesk_store::{NewMessage, ProviderMeta};
use serde_json::{Value, json};

use crate::backend::{BoxFuture, MailBackend};
use crate::engine::Ctx;
use crate::error::{Result, SyncError};
use crate::imap_sync::CycleReport;
use crate::ops::{MailOp, QueueWork, SyncOp, classify, pending_messages};
use crate::parse::{clean_id, enrich, parse_body};
use crate::state::{self, GraphFolderState, graph_key};
use crate::{NewMailSample, Phase, SyncEvent};

/// Carpetas máximas.
const MAX_FOLDERS: usize = 2_000;
/// Profundidad máxima del árbol de carpetas.
const MAX_DEPTH: usize = 12;
/// Operaciones por pasada.
const OPS_PER_PASS: usize = 500;
/// Espera máxima entre reintentos de una operación.
const MAX_OP_BACKOFF: Duration = Duration::from_secs(15 * 60);
/// Consultas por `@removed` en una pasada (para distinguir «movido» de «borrado»).
const MAX_REMOVED_LOOKUPS: usize = 200;
/// Mensajes por página del historial antiguo.
const BACKFILL_PAGE: usize = 100;
/// Propiedades que se piden de cada mensaje.
const SELECT: &str = "subject,from,sender,toRecipients,ccRecipients,bccRecipients,replyTo,receivedDateTime,\
sentDateTime,isRead,isDraft,flag,categories,hasAttachments,internetMessageId,conversationId,bodyPreview,\
parentFolderId";
/// Carpetas con nombre conocido de Graph y su función.
const WELL_KNOWN: &[(&str, FolderRole)] = &[
    ("inbox", FolderRole::Inbox),
    ("sentitems", FolderRole::Sent),
    ("drafts", FolderRole::Drafts),
    ("deleteditems", FolderRole::Trash),
    ("junkemail", FolderRole::Junk),
    ("archive", FolderRole::Archive),
];

/// Una cuenta de correo por Graph.
pub(crate) struct GraphAccount {
    pub ctx: Arc<Ctx>,
    client: GraphClient,
    roles: Mutex<Option<HashMap<String, FolderRole>>>,
    backoff: Mutex<HashMap<u64, Instant>>,
}

fn s<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str)
}

fn mailbox(v: &Value) -> Option<Mailbox> {
    let e = v.get("emailAddress")?;
    let addr = s(e, "address").unwrap_or_default();
    let name = s(e, "name").unwrap_or_default();
    if addr.is_empty() && name.is_empty() {
        return None;
    }
    Some(Mailbox::new(name.chars().take(300).collect::<String>(), addr.chars().take(320).collect::<String>()))
}

fn mailboxes(v: &Value, k: &str) -> Vec<Mailbox> {
    v.get(k).and_then(Value::as_array).map(|a| a.iter().take(500).filter_map(mailbox).collect()).unwrap_or_default()
}

fn date(v: &Value, k: &str) -> Option<DateTime<Utc>> {
    s(v, k).and_then(|d| DateTime::parse_from_rfc3339(d).ok()).map(|d| d.with_timezone(&Utc))
}

/// Banderas desde un mensaje de Graph.
fn flags_of(v: &Value) -> Flags {
    Flags {
        seen: v.get("isRead").and_then(Value::as_bool).unwrap_or(false),
        flagged: v.get("flag").and_then(|f| s(f, "flagStatus")).is_some_and(|f| f.eq_ignore_ascii_case("flagged")),
        draft: v.get("isDraft").and_then(Value::as_bool).unwrap_or(false),
        ..Default::default()
    }
}

fn categories(v: &Value) -> Vec<String> {
    v.get("categories")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).take(100).map(|c| c.chars().take(255).collect()).collect())
        .unwrap_or_default()
}

/// Sobre a partir del JSON de un mensaje de Graph.
pub(crate) fn envelope_from_graph(v: &Value, folder: FolderId) -> Option<MessageEnvelope> {
    let id = s(v, "id").filter(|i| !i.is_empty() && i.len() <= 1_000)?;
    let received = date(v, "receivedDateTime").or_else(|| date(v, "sentDateTime")).unwrap_or_else(Utc::now);
    let mut from = mailboxes(v, "from");
    if from.is_empty() {
        from = v.get("from").and_then(mailbox).or_else(|| v.get("sender").and_then(mailbox)).into_iter().collect();
    }
    let mut env = MessageEnvelope {
        folder,
        remote_id: Some(id.to_string()),
        message_id: s(v, "internetMessageId").and_then(clean_id),
        from,
        to: mailboxes(v, "toRecipients"),
        cc: mailboxes(v, "ccRecipients"),
        bcc: mailboxes(v, "bccRecipients"),
        reply_to: mailboxes(v, "replyTo"),
        subject: s(v, "subject").unwrap_or_default().to_string(),
        date: received,
        flags: flags_of(v),
        keywords: categories(v),
        has_attachments: v.get("hasAttachments").and_then(Value::as_bool).unwrap_or(false),
        preview: make_preview(s(v, "bodyPreview").unwrap_or_default()),
        ..Default::default()
    };
    env.clamp();
    Some(env)
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

impl GraphAccount {
    pub fn new(ctx: Arc<Ctx>, tokens: Arc<dyn TokenProvider>) -> Result<Self> {
        let client = GraphClient::new(ctx.account.clone(), tokens, &ctx.opts.graph_base_url, ctx.opts.io_timeout)?
            .with_prefer("IdType=\"ImmutableId\", odata.maxpagesize=100")
            .with_max_pages(ctx.opts.graph_max_pages);
        Ok(Self { ctx, client, roles: Mutex::new(None), backoff: Mutex::new(HashMap::new()) })
    }

    // ---------------------------------------------------------------- carpetas

    /// Ids de las carpetas con nombre conocido (una vez por sesión).
    async fn well_known(&self) -> Result<HashMap<String, FolderRole>> {
        if let Some(r) = self.roles.lock().unwrap_or_else(PoisonError::into_inner).clone() {
            return Ok(r);
        }
        let mut out = HashMap::new();
        for (name, role) in WELL_KNOWN {
            let r = self
                .client
                .request(Method::GET, &self.client.url(&format!("/me/mailFolders/{name}?$select=id")), None, &[])
                .await?;
            if r.is_success()
                && let Some(id) = s(&r.body, "id")
            {
                out.insert(id.to_string(), *role);
            } else if r.status != 404 && !r.is_success() {
                return Err(r.error().into());
            }
        }
        *self.roles.lock().unwrap_or_else(PoisonError::into_inner) = Some(out.clone());
        Ok(out)
    }

    /// Árbol de `mailFolders` → carpetas locales (crea, renombra, borra las que ya no existen).
    async fn sync_folder_list(&self) -> Result<Vec<Folder>> {
        let ctx = &self.ctx;
        let roles = self.well_known().await?;
        let mut remote: Vec<Value> = Vec::new();
        let mut queue: Vec<(String, usize)> = vec![("/me/mailFolders?$top=100".to_string(), 0)];
        while let Some((path, depth)) = queue.pop() {
            if remote.len() >= MAX_FOLDERS {
                break;
            }
            for f in self.client.list(&path).await? {
                let Some(id) = s(&f, "id").map(str::to_string) else { continue };
                if depth < MAX_DEPTH && f.get("childFolderCount").and_then(Value::as_u64).unwrap_or(0) > 0 {
                    queue
                        .push((format!("/me/mailFolders/{}/childFolders?$top=100", enc(&id)), depth.saturating_add(1)));
                }
                remote.push(f);
                if remote.len() >= MAX_FOLDERS {
                    break;
                }
            }
        }
        let local = ctx.store.folders(ctx.acct)?;
        let mut by_path: HashMap<String, Folder> = local.into_iter().map(|f| (f.remote_path.clone(), f)).collect();
        let mut out: Vec<(Folder, Option<String>)> = Vec::new();
        for r in &remote {
            let Some(id) = s(r, "id") else { continue };
            let name: String = s(r, "displayName").unwrap_or("?").chars().take(255).collect();
            let role = roles.get(id).copied().unwrap_or(FolderRole::Custom);
            let parent_remote = s(r, "parentFolderId").map(str::to_string);
            let f = match by_path.remove(id) {
                Some(mut f) => {
                    if f.name != name || f.role != role {
                        f.name = name;
                        f.role = role;
                        ctx.store.put_folder(&f)?;
                    }
                    f
                }
                None => {
                    let mut f =
                        Folder { account: ctx.acct, name, remote_path: id.to_string(), role, ..Default::default() };
                    f.id = ctx.store.put_folder(&f)?;
                    ctx.emit(SyncEvent::FolderUpdated { account: ctx.acct, folder: f.id });
                    f
                }
            };
            out.push((f, parent_remote));
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
            for (_, gone) in by_path {
                ctx.store.delete_folder(gone.id)?;
                let _ = ctx.store.delete_sync_state(&graph_key(ctx.acct, gone.id));
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
        let mut lookups = 0usize;
        for f in folders.iter().filter(|f| !inbox_only || f.role == FolderRole::Inbox) {
            match self.sync_folder(f, &pending, &mut lookups).await {
                Ok((first, more)) => {
                    initial |= first;
                    report.backfill_pending |= more;
                }
                Err(e @ SyncError::Graph(GraphError::Throttled { .. } | GraphError::Unauthorized)) => return Err(e),
                Err(SyncError::Graph(GraphError::OAuth(o))) => return Err(SyncError::OAuth(o)),
                Err(e) => ctx.emit_error(&e),
            }
        }
        if !inbox_only {
            for f in ctx.store.folders(ctx.acct)? {
                if let Some(st) = state::load::<GraphFolderState>(&ctx.store, &graph_key(ctx.acct, f.id)) {
                    report.backfill_pending |= st.initial_done && !st.backfill_done && ctx.opts.backfill;
                }
            }
        }
        if initial {
            ctx.store.rethread_account(ctx.acct)?;
        }
        Ok(report)
    }

    /// Delta de una carpeta. Devuelve (primera vez con mensajes, queda historial).
    async fn sync_folder(&self, f: &Folder, pending: &HashSet<MessageId>, lookups: &mut usize) -> Result<(bool, bool)> {
        let ctx = &self.ctx;
        let o = &ctx.opts;
        let key = graph_key(ctx.acct, f.id);
        let mut st: GraphFolderState = state::load(&ctx.store, &key).unwrap_or_default();
        let since = (Utc::now() - chrono::Duration::days(i64::from(o.initial_days))).format("%Y-%m-%dT%H:%M:%SZ");
        let start = format!(
            "/me/mailFolders/{}/messages/delta?$select={SELECT}&$filter=receivedDateTime%20ge%20{since}",
            enc(&f.remote_path)
        );
        let page = self.client.delta(st.delta_link.as_deref(), &start).await?;
        let first = !st.initial_done || page.full;
        let mut fresh: Vec<Value> = Vec::new();
        let mut changed = false;
        for item in page.items.into_iter().take(100_000) {
            let Some(id) = s(&item, "id").map(str::to_string) else { continue };
            if item.get("@removed").is_some() {
                changed |= self.removed(f, &id, lookups).await?;
                continue;
            }
            match ctx.store.message_by_remote(f.id, &id)? {
                Some(local) => {
                    if !pending.contains(&local) {
                        changed |= self.update_from_server(local, &item)?;
                    }
                }
                None => fresh.push(item),
            }
        }
        let notify = f.role == FolderRole::Inbox && st.initial_done && !page.full;
        let inserted = self.insert(f, &fresh, notify)?;
        if !page.more {
            st.initial_done = true;
        }
        st.delta_link = page.link;
        if first && st.oldest_ms.is_none() {
            let oldest = ctx
                .store
                .list_order(f.id)?
                .last()
                .and_then(|(_, id)| ctx.store.get_envelope(MessageId(*id)).ok().flatten())
                .map(|e| e.date.timestamp_millis());
            st.oldest_ms = Some(oldest.unwrap_or_else(|| {
                (Utc::now() - chrono::Duration::days(i64::from(o.initial_days))).timestamp_millis()
            }));
        }
        state::save(&ctx.store, &key, &st)?;
        if f.role == FolderRole::Inbox && !inserted.is_empty() {
            self.prefetch(&inserted).await;
        }
        if changed || !inserted.is_empty() {
            ctx.emit(SyncEvent::FolderUpdated { account: ctx.acct, folder: f.id });
        }
        Ok((first && !inserted.is_empty(), st.initial_done && !st.backfill_done && o.backfill))
    }

    /// Un `@removed`: si el mensaje sigue en otra carpeta conocida (movido), se mueve en local
    /// (conserva cuerpo e hilo); si no, se borra.
    async fn removed(&self, f: &Folder, id: &str, lookups: &mut usize) -> Result<bool> {
        let ctx = &self.ctx;
        let Some(local) = ctx.store.message_by_remote(f.id, id)? else { return Ok(false) };
        if *lookups < MAX_REMOVED_LOOKUPS {
            *lookups = lookups.saturating_add(1);
            let url = self.client.url(&format!("/me/messages/{}?$select=id,parentFolderId", enc(id)));
            let r = self.client.request(Method::GET, &url, None, &[]).await?;
            if r.is_success()
                && let Some(parent) = s(&r.body, "parentFolderId")
                && let Some(dest) = ctx.store.folders(ctx.acct)?.into_iter().find(|x| x.remote_path == parent)
            {
                if dest.id != f.id {
                    ctx.store.move_message(local, dest.id)?;
                    ctx.store.set_remote_id(local, Some(id.to_string()))?;
                    ctx.emit(SyncEvent::FolderUpdated { account: ctx.acct, folder: dest.id });
                }
                return Ok(true);
            }
            if !r.is_success() && r.status != 404 {
                return Err(r.error().into());
            }
        }
        ctx.store.remove_from_folder(local, f.id)?;
        Ok(true)
    }

    /// Banderas y categorías del servidor (gana el servidor salvo operación local pendiente).
    fn update_from_server(&self, local: MessageId, v: &Value) -> Result<bool> {
        let store = &self.ctx.store;
        let Some(env) = store.get_envelope(local)? else { return Ok(false) };
        let mut changed = false;
        let mut server = flags_of(v);
        server.answered = env.flags.answered;
        server.forwarded = env.flags.forwarded;
        if v.get("isRead").is_some() && env.flags != server {
            store.set_flags(local, server)?;
            changed = true;
        }
        if v.get("categories").is_some() {
            changed |= store.set_keywords(local, categories(v))?;
        }
        Ok(changed)
    }

    /// Inserta mensajes nuevos de una carpeta. Un mensaje que ya existía en otra carpeta (mismo
    /// id inmutable) se mueve en vez de duplicarse.
    fn insert(&self, f: &Folder, items: &[Value], notify: bool) -> Result<Vec<(MessageId, DateTime<Utc>)>> {
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
        for v in items {
            let Some(mut env) = envelope_from_graph(v, f.id) else { continue };
            let id = env.remote_id.clone().unwrap_or_default();
            if let Some(existing) = ctx.store.message_by_provider(ctx.acct, &id)? {
                ctx.store.move_message(existing, f.id)?;
                ctx.store.set_remote_id(existing, Some(id))?;
                self.update_from_server(existing, v)?;
                continue;
            }
            if let Some(local) = env.message_id.as_ref().and_then(|m| waiting.remove(m)) {
                ctx.store.set_remote_id(local, Some(id.clone()))?;
                ctx.store.set_provider_meta(&[(local, ProviderMeta { provider_id: Some(id), thread_hint: None })])?;
                adopt.retain(|a| *a != local);
                continue;
            }
            let hint = s(v, "conversationId").filter(|c| !c.is_empty()).map(|c| format!("conv:{c}"));
            if let Some(h) = &hint {
                env.thread = ctx.store.thread_by_hint(ctx.acct, h)?;
            }
            metas.push(ProviderMeta { provider_id: Some(id), thread_hint: hint });
            batch.push(NewMessage { envelope: env, raw: None, body_text: None });
        }
        let samples: Vec<(String, String, DateTime<Utc>)> = batch
            .iter()
            .map(|n| (n.envelope.sender_display().to_string(), n.envelope.subject.clone(), n.envelope.date))
            .collect();
        if adopt.len() != adopt_before {
            state::set_adopt(&ctx.store, f.id, &adopt)?;
        }
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
        }
        Ok(out)
    }

    /// Precarga los cuerpos más recientes (los fallos no cuentan: se bajan al abrir).
    async fn prefetch(&self, inserted: &[(MessageId, DateTime<Utc>)]) {
        let o = &self.ctx.opts;
        let mut pick: Vec<(MessageId, DateTime<Utc>)> = inserted.to_vec();
        pick.sort_unstable_by_key(|b| std::cmp::Reverse(b.1));
        pick.truncate(o.prefetch_recent);
        for (id, _) in pick {
            if self.download(id).await.is_err() {
                break;
            }
        }
    }

    /// Baja el MIME (`$value`) y lo guarda.
    async fn download(&self, msg: MessageId) -> Result<()> {
        let store = &self.ctx.store;
        let mut env = store.get_envelope(msg)?.ok_or_else(|| SyncError::MessageNotFound(msg.to_string()))?;
        if env.blob.is_some() {
            return Ok(());
        }
        let id = env.remote_id.clone().ok_or_else(|| SyncError::MessageNotFound(msg.to_string()))?;
        let raw = self.client.get_bytes(&self.client.url(&format!("/me/messages/{}/$value", enc(&id)))).await?;
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
            let key = graph_key(ctx.acct, f.id);
            let Some(mut st) = state::load::<GraphFolderState>(&ctx.store, &key) else { continue };
            if !st.initial_done || st.backfill_done {
                continue;
            }
            if budget == 0 {
                more = true;
                break;
            }
            let before = st.oldest_ms.and_then(DateTime::<Utc>::from_timestamp_millis).unwrap_or_else(Utc::now);
            let top = budget.min(BACKFILL_PAGE);
            let path = format!(
                "/me/mailFolders/{}/messages?$select={SELECT}&$filter=receivedDateTime%20lt%20{}&$orderby=receivedDateTime%20desc&$top={top}",
                enc(&f.remote_path),
                before.format("%Y-%m-%dT%H:%M:%SZ")
            );
            let page = self.client.get_json(&self.client.url(&path)).await?;
            let items: Vec<Value> = page.get("value").and_then(Value::as_array).cloned().unwrap_or_default();
            let fresh: Vec<Value> = items
                .iter()
                .filter(|v| s(v, "id").is_some_and(|id| ctx.store.message_by_remote(f.id, id).ok().flatten().is_none()))
                .cloned()
                .collect();
            self.insert(&f, &fresh, false)?;
            budget = budget.saturating_sub(items.len().max(1));
            let oldest = items.iter().filter_map(|v| date(v, "receivedDateTime")).min();
            match oldest {
                Some(d) if items.len() >= top => {
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
            if !items.is_empty() {
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
                // Graph guarda lo enviado solo y no tiene «respondido» que marcar.
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
                    let throttled = matches!(e, SyncError::Graph(GraphError::Throttled { .. }));
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
                    if throttled || e.needs_reauth() {
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

    async fn call(&self, method: Method, path: &str, body: Option<&Value>) -> Result<Value> {
        let r = self.client.request(method, &self.client.url(path), body.map(Body::Json), &[]).await?;
        // Ya no existe: lo que se quería (borrar, mover) da igual.
        if r.is_success() || r.status == 404 {
            return Ok(r.body);
        }
        Err(r.error().into())
    }

    async fn apply_op(&self, op: &MailOp) -> Result<()> {
        match op {
            MailOp::RemoteFlags { remote, add, remove, .. } => {
                let has = |l: &[String], f: &str| l.iter().any(|x| x.eq_ignore_ascii_case(f));
                let mut patch = serde_json::Map::new();
                if has(add, "\\Seen") || has(remove, "\\Seen") {
                    patch.insert("isRead".into(), json!(has(add, "\\Seen")));
                }
                if has(add, "\\Flagged") || has(remove, "\\Flagged") {
                    let status = if has(add, "\\Flagged") { "flagged" } else { "notFlagged" };
                    patch.insert("flag".into(), json!({ "flagStatus": status }));
                }
                if patch.is_empty() {
                    return Ok(());
                }
                self.call(Method::PATCH, &format!("/me/messages/{}", enc(remote)), Some(&Value::Object(patch))).await?;
                Ok(())
            }
            MailOp::RemoteMove { msg, remote, to, to_folder } => {
                let r = self
                    .call(
                        Method::POST,
                        &format!("/me/messages/{}/move", enc(remote)),
                        Some(&json!({ "destinationId": to })),
                    )
                    .await?;
                // Con ids inmutables el id no cambia; si el servidor da otro, se adopta.
                if let Some(new) = s(&r, "id").filter(|n| *n != remote.as_str())
                    && let Some(env) = self.ctx.store.get_envelope(*msg)?
                    && env.folder == *to_folder
                {
                    self.ctx.store.set_remote_id(*msg, Some(new.to_string()))?;
                }
                Ok(())
            }
            MailOp::RemoteDelete { remote } => {
                let path = format!("/me/messages/{}/permanentDelete", enc(remote));
                let r = self.client.request(Method::POST, &self.client.url(&path), None, &[]).await?;
                if r.is_success() || r.status == 404 {
                    return Ok(());
                }
                // Inquilinos sin `permanentDelete`: DELETE (va a Elementos recuperables).
                self.call(Method::DELETE, &format!("/me/messages/{}", enc(remote)), None).await?;
                Ok(())
            }
            MailOp::Append { msg, folder, folder_id, .. } => {
                let Ok(raw) = self.ctx.store.get_raw(*msg) else { return Ok(()) };
                // Subir MIME crea un borrador (en Borradores); se mueve si el destino es otro.
                let b64 = base64::engine::general_purpose::STANDARD.encode(&raw);
                let body = Body::Raw { content_type: "text/plain", data: b64.as_bytes() };
                let r = self.client.request(Method::POST, &self.client.url("/me/messages"), Some(body), &[]).await?;
                if !r.is_success() {
                    return Err(r.error().into());
                }
                let Some(mut id) = s(&r.body, "id").map(str::to_string) else { return Ok(()) };
                let parent = s(&r.body, "parentFolderId").unwrap_or_default().to_string();
                if !folder.is_empty() && parent != *folder {
                    let moved = self
                        .call(
                            Method::POST,
                            &format!("/me/messages/{}/move", enc(&id)),
                            Some(&json!({ "destinationId": folder })),
                        )
                        .await?;
                    if let Some(n) = s(&moved, "id") {
                        id = n.to_string();
                    }
                }
                if let Some(env) = self.ctx.store.get_envelope(*msg)?
                    && env.folder == *folder_id
                    && env.remote_id.is_none()
                {
                    self.ctx.store.set_remote_id(*msg, Some(id.clone()))?;
                    self.ctx
                        .store
                        .set_provider_meta(&[(*msg, ProviderMeta { provider_id: Some(id), thread_hint: None })])?;
                }
                Ok(())
            }
            // Operaciones de IMAP (UIDs, etiquetas de Gmail): no aplican a Graph.
            MailOp::Flags { .. } | MailOp::Move { .. } | MailOp::Expunge { .. } | MailOp::Labels { .. } => Ok(()),
        }
    }
}

impl MailBackend for GraphAccount {
    fn ctx(&self) -> &Arc<Ctx> {
        &self.ctx
    }

    fn cycle(&self, inbox_only: bool) -> BoxFuture<'_, Result<CycleReport>> {
        Box::pin(async move {
            let started = std::time::Instant::now();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_json_to_envelope() {
        let v = json!({
            "id": "AAMkAD/x+y=",
            "subject": "Contrato",
            "from": {"emailAddress": {"name": "Ana", "address": "ana@contoso.com"}},
            "toRecipients": [{"emailAddress": {"address": "yo@contoso.com"}}],
            "receivedDateTime": "2026-10-01T10:00:00Z",
            "isRead": false,
            "flag": {"flagStatus": "flagged"},
            "categories": ["Rojo", "Cliente"],
            "internetMessageId": "<a@contoso.com>",
            "bodyPreview": "Hola   Ana",
            "hasAttachments": true
        });
        let e = envelope_from_graph(&v, FolderId(3)).unwrap();
        assert_eq!(e.remote_id.as_deref(), Some("AAMkAD/x+y="));
        assert_eq!(e.from[0].display(), "Ana");
        assert!(e.flags.flagged && !e.flags.seen);
        assert_eq!(e.keywords, vec!["Rojo".to_string(), "Cliente".to_string()]);
        assert_eq!(e.message_id.as_deref(), Some("a@contoso.com"));
        assert_eq!(e.preview, "Hola Ana");
        assert!(envelope_from_graph(&json!({"subject": "sin id"}), FolderId(3)).is_none());
    }
}
