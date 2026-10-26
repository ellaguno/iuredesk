//! Sincronización de una cuenta IMAP: carpetas, mensajes nuevos, antiguos, banderas, borrados,
//! cola de operaciones, cuerpos e IDLE.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use chrono::Utc;
use iuredesk_imap::{
    FetchData, IdleOutcome, MailboxInfo, Qresync, SelectOptions, SelectResult, SeqSet, StoreMode, fetch_items,
};
use iuredesk_model::account::AccountKind;
use iuredesk_model::{Folder, FolderId, FolderRole, MessageEnvelope, MessageId};
use iuredesk_store::NewMessage;
use tokio::sync::{mpsc, watch};

use crate::engine::{Cmd, Ctx};
use crate::error::{Result, SyncError};
use crate::ops::{MailOp, QueueWork, SyncOp, classify, pending_messages};
use crate::parse::{enrich, envelope_from_fetch, flags_from_imap, parse_body};
use crate::pool::{Conn, ImapPool};
use crate::state::{self, BackfillState, FolderState, adopt_list, backfill_key, folder_key, set_adopt};
use crate::{NewMailSample, Phase, SyncEvent};

/// Carpetas máximas que se sincronizan por cuenta.
const MAX_FOLDERS: usize = 5_000;
/// Cuerpos por `UID FETCH` de precarga.
const BODY_BATCH: usize = 20;
/// Operaciones de la cola por pasada.
const OPS_PER_PASS: usize = 500;
/// Espera máxima entre reintentos de una operación.
const MAX_OP_BACKOFF: Duration = Duration::from_secs(15 * 60);

/// Resultado de una pasada.
#[derive(Debug, Default)]
pub(crate) struct CycleReport {
    /// Queda historial antiguo por bajar.
    pub backfill_pending: bool,
}

/// Cómo terminó una sesión de IDLE.
pub(crate) enum IdleExit {
    Stopped,
    Unsupported,
}

/// Una cuenta IMAP.
pub(crate) struct ImapAccount {
    pub ctx: Arc<Ctx>,
    pub pool: Arc<ImapPool>,
    backoff: Mutex<HashMap<u64, Instant>>,
}

fn uid_of(env: &MessageEnvelope) -> Option<u32> {
    env.remote_id.as_deref().and_then(|r| r.parse().ok()).filter(|u| *u > 0)
}

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

impl ImapAccount {
    pub fn new(ctx: Arc<Ctx>, pool: Arc<ImapPool>) -> Self {
        Self { ctx, pool, backoff: Mutex::new(HashMap::new()) }
    }

    fn finish(&self, conn: Conn, r: &Result<impl Sized>) {
        match r {
            Err(SyncError::Imap(e)) if e.is_fatal_for_connection() => drop(conn),
            _ => self.pool.release(conn),
        }
    }

    /// Una pasada completa (o solo la bandeja de entrada).
    pub async fn cycle(&self, inbox_only: bool) -> Result<CycleReport> {
        let started = Instant::now();
        let mut conn = self.pool.get().await?;
        let r = self.cycle_on(&mut conn, inbox_only).await;
        self.finish(conn, &r);
        if let Ok(rep) = &r {
            self.ctx.emit(SyncEvent::Synced {
                account: self.ctx.acct,
                took: started.elapsed(),
                backfill_pending: rep.backfill_pending,
            });
        }
        r
    }

    async fn cycle_on(&self, conn: &mut Conn, inbox_only: bool) -> Result<CycleReport> {
        let ctx = &self.ctx;
        let mut folders = if inbox_only {
            ctx.store.folders(ctx.acct)?.into_iter().filter(|f| f.role == FolderRole::Inbox).collect()
        } else {
            self.sync_folder_list(conn).await?
        };
        if folders.is_empty() && inbox_only {
            folders = self.sync_folder_list(conn).await?;
        }
        self.replay_on(conn).await?;
        let pending = pending_messages(&ctx.store, ctx.acct);
        folders.sort_by(|a, b| role_rank(&a.role).cmp(&role_rank(&b.role)).then_with(|| a.name.cmp(&b.name)));
        let mut report = CycleReport::default();
        let mut initial_inserted = false;
        for f in folders.iter().filter(|f| !inbox_only || f.role == FolderRole::Inbox) {
            match self.sync_folder(conn, f, &pending).await {
                Ok(o) => {
                    report.backfill_pending |= o.backfill_pending;
                    initial_inserted |= o.initial;
                }
                Err(SyncError::Imap(e)) if e.is_fatal_for_connection() => return Err(e.into()),
                Err(e) => ctx.emit_error(&e),
            }
        }
        if !inbox_only {
            for f in ctx.store.folders(ctx.acct)? {
                if let Some(bf) = state::load::<BackfillState>(&ctx.store, &backfill_key(ctx.acct, f.id)) {
                    report.backfill_pending |= !bf.done && ctx.opts.backfill;
                }
            }
        }
        if initial_inserted {
            ctx.store.rethread_account(ctx.acct)?;
        }
        Ok(report)
    }

    /// `LIST` → carpetas locales (crea, renombra, borra las que ya no existen).
    async fn sync_folder_list(&self, conn: &mut Conn) -> Result<Vec<Folder>> {
        let ctx = &self.ctx;
        let list: Vec<MailboxInfo> =
            conn.c.list().await?.into_iter().filter(|m| m.selectable() && !m.is_virtual()).take(MAX_FOLDERS).collect();
        let local = ctx.store.folders(ctx.acct)?;
        let mut by_path: HashMap<String, Folder> = local.into_iter().map(|f| (f.remote_path.clone(), f)).collect();
        let mut out: Vec<Folder> = Vec::new();
        for m in &list {
            let role = m.role();
            match by_path.remove(&m.path) {
                Some(mut f) => {
                    if f.name != m.leaf || f.role != role {
                        f.name = m.leaf.clone();
                        f.role = role;
                        ctx.store.put_folder(&f)?;
                    }
                    out.push(f);
                }
                None => {
                    let mut f = Folder {
                        account: ctx.acct,
                        name: m.leaf.clone(),
                        remote_path: m.path.clone(),
                        role,
                        ..Default::default()
                    };
                    f.id = ctx.store.put_folder(&f)?;
                    ctx.emit(SyncEvent::FolderUpdated { account: ctx.acct, folder: f.id });
                    out.push(f);
                }
            }
        }
        // Jerarquía.
        let ids: HashMap<String, FolderId> = out.iter().map(|f| (f.remote_path.clone(), f.id)).collect();
        for (f, m) in out.iter_mut().zip(list.iter()) {
            let parent = m
                .delimiter
                .and_then(|d| m.path.rsplit_once(d).map(|(p, _)| p.to_string()))
                .and_then(|p| ids.get(&p).copied());
            if f.parent != parent {
                f.parent = parent;
                ctx.store.put_folder(f)?;
            }
        }
        // Solo se borra lo local si el servidor devolvió una lista creíble (con INBOX).
        if list.iter().any(|m| m.path.eq_ignore_ascii_case("INBOX")) {
            for (_, gone) in by_path {
                ctx.store.delete_folder(gone.id)?;
                let _ = ctx.store.delete_sync_state(&folder_key(ctx.acct, gone.id));
                let _ = ctx.store.delete_sync_state(&backfill_key(ctx.acct, gone.id));
                ctx.emit(SyncEvent::FolderUpdated { account: ctx.acct, folder: gone.id });
            }
        }
        Ok(out)
    }

    /// Vacía la carpeta local conservando su id (UIDVALIDITY cambiado).
    fn reset_folder(&self, f: &Folder) -> Result<()> {
        let ctx = &self.ctx;
        ctx.store.delete_folder(f.id)?;
        ctx.store.put_folder(f)?;
        let _ = ctx.store.delete_sync_state(&folder_key(ctx.acct, f.id));
        let _ = ctx.store.delete_sync_state(&backfill_key(ctx.acct, f.id));
        set_adopt(&ctx.store, f.id, &[])?;
        ctx.emit(SyncEvent::FolderUpdated { account: ctx.acct, folder: f.id });
        Ok(())
    }

    async fn sync_folder(&self, conn: &mut Conn, f: &Folder, pending: &HashSet<MessageId>) -> Result<FolderOutcome> {
        let ctx = &self.ctx;
        let mut prev: Option<FolderState> = state::load(&ctx.store, &folder_key(ctx.acct, f.id));
        let condstore = conn.c.capabilities().has("CONDSTORE");
        let qres = prev.as_ref().and_then(|p| {
            let modseq = p.modseq?;
            (conn.c.is_enabled("QRESYNC") && p.uid_validity != 0).then_some(Qresync {
                uid_validity: p.uid_validity,
                modseq,
                known_uids: None,
            })
        });
        let used_qresync = qres.is_some();
        let opts = SelectOptions { read_only: true, condstore, qresync: qres };
        let sel = conn.c.select(&f.remote_path, &opts).await?;
        let validity = sel.mailbox.uid_validity.unwrap_or(0);
        if prev.as_ref().is_some_and(|p| p.uid_validity != validity) {
            self.reset_folder(f)?;
            prev = None;
        }
        match prev {
            None => self.initial_folder(conn, f, &sel).await,
            Some(p) => self.incremental_folder(conn, f, p, sel, used_qresync, pending).await,
        }
    }

    async fn initial_folder(&self, conn: &mut Conn, f: &Folder, sel: &SelectResult) -> Result<FolderOutcome> {
        let ctx = &self.ctx;
        let o = &ctx.opts;
        let validity = sel.mailbox.uid_validity.unwrap_or(0);
        let mut uids = Vec::new();
        if sel.mailbox.exists > 0 {
            let since = (Utc::now() - chrono::Duration::days(i64::from(o.initial_days))).format("%-d-%b-%Y");
            uids = conn.c.uid_search(&format!("SINCE {since}")).await?;
        }
        uids.sort_unstable_by(|a, b| b.cmp(a));
        uids.truncate(o.initial_max);
        let inserted = self.fetch_headers(conn, f, &uids, Phase::Headers, false).await?;
        let max_uid = uids.first().copied().unwrap_or(0);
        let uid_next = sel.mailbox.uid_next.unwrap_or(0).max(max_uid.saturating_add(1)).max(1);
        let below = uids.last().copied().unwrap_or(uid_next);
        let done = below <= 1 || u64::from(sel.mailbox.exists) <= uids.len() as u64;
        state::save(
            &ctx.store,
            &folder_key(ctx.acct, f.id),
            &FolderState {
                uid_validity: validity,
                uid_next,
                modseq: sel.mailbox.highest_modseq,
                exists: sel.mailbox.exists,
                last_full_scan: Utc::now().timestamp(),
            },
        )?;
        state::save(&ctx.store, &backfill_key(ctx.acct, f.id), &BackfillState { uid_validity: validity, below, done })?;
        if f.role == FolderRole::Inbox {
            self.prefetch(conn, f, &inserted).await?;
        }
        if !inserted.is_empty() {
            ctx.emit(SyncEvent::FolderUpdated { account: ctx.acct, folder: f.id });
        }
        Ok(FolderOutcome { initial: !inserted.is_empty(), backfill_pending: !done && o.backfill })
    }

    fn local_uids(&self, f: &Folder) -> Result<HashMap<u32, MessageId>> {
        Ok(self
            .ctx
            .store
            .remote_ids(f.id)?
            .into_iter()
            .filter_map(|(r, id)| r.parse::<u32>().ok().map(|u| (u, id)))
            .collect())
    }

    /// Aplica banderas del servidor (gana el servidor salvo operación local pendiente).
    fn apply_flags(
        &self,
        local: &HashMap<u32, MessageId>,
        fetched: &[FetchData],
        pending: &HashSet<MessageId>,
    ) -> Result<bool> {
        let mut changed = false;
        for fd in fetched {
            let (Some(uid), Some(flags)) = (fd.uid, fd.flags.as_ref()) else { continue };
            let Some(&id) = local.get(&uid) else { continue };
            if pending.contains(&id) {
                continue;
            }
            let server = flags_from_imap(flags);
            if let Some(env) = self.ctx.store.get_envelope(id)?
                && env.flags != server
            {
                self.ctx.store.set_flags(id, server)?;
                changed = true;
            }
        }
        Ok(changed)
    }

    fn delete_uids(&self, local: &HashMap<u32, MessageId>, uids: impl IntoIterator<Item = u32>) -> Result<bool> {
        let mut changed = false;
        for u in uids {
            if let Some(&id) = local.get(&u) {
                self.ctx.store.delete_message(id)?;
                changed = true;
            }
        }
        Ok(changed)
    }

    async fn incremental_folder(
        &self,
        conn: &mut Conn,
        f: &Folder,
        mut st: FolderState,
        sel: SelectResult,
        used_qresync: bool,
        pending: &HashSet<MessageId>,
    ) -> Result<FolderOutcome> {
        let ctx = &self.ctx;
        let o = &ctx.opts;
        let condstore = conn.c.capabilities().has("CONDSTORE") && !sel.mailbox.no_modseq;
        let srv_next = sel.mailbox.uid_next.unwrap_or(0);
        let mut changed = false;

        // Nuevos.
        let mut new_uids: Vec<u32> = Vec::new();
        if srv_next == 0 || srv_next > st.uid_next {
            new_uids = conn
                .c
                .uid_search(&format!("UID {}:*", st.uid_next.max(1)))
                .await?
                .into_iter()
                .filter(|u| *u >= st.uid_next)
                .collect();
            new_uids.sort_unstable_by(|a, b| b.cmp(a));
        }

        // Banderas y borrados.
        let known_top = st.uid_next.saturating_sub(1);
        if used_qresync {
            if !sel.vanished.is_empty() || !sel.changed.is_empty() {
                let local = self.local_uids(f)?;
                changed |= self.delete_uids(&local, sel.vanished.iter().copied())?;
                changed |= self.apply_flags(&local, &sel.changed, pending)?;
            }
        } else if known_top > 0 {
            let local = self.local_uids(f)?;
            let fresh_scan = Utc::now().timestamp().saturating_sub(st.last_full_scan)
                > i64::try_from(o.full_rescan_every.as_secs()).unwrap_or(i64::MAX);
            match (condstore, st.modseq, sel.mailbox.highest_modseq) {
                (true, Some(old), Some(new)) => {
                    if new != old {
                        let r = conn
                            .c
                            .uid_fetch(&SeqSet::range(1, known_top), &fetch_items::flags(true), Some(old), false)
                            .await?;
                        changed |= self.apply_flags(&local, &r.messages, pending)?;
                    }
                }
                _ => {
                    let lo = if fresh_scan {
                        st.last_full_scan = Utc::now().timestamp();
                        1
                    } else {
                        let mut ks: Vec<u32> = local.keys().copied().collect();
                        ks.sort_unstable_by(|a, b| b.cmp(a));
                        ks.get(o.flag_window.saturating_sub(1)).copied().or(ks.last().copied()).unwrap_or(1)
                    };
                    if lo <= known_top {
                        let r = conn
                            .c
                            .uid_fetch(&SeqSet::range(lo, known_top), &fetch_items::flags(false), None, false)
                            .await?;
                        changed |= self.apply_flags(&local, &r.messages, pending)?;
                    }
                }
            }
            // Borrados: si EXISTS no cuadra, se comparan los UID del tramo sincronizado.
            let expected = u64::from(st.exists).saturating_add(new_uids.len() as u64);
            if u64::from(sel.mailbox.exists) != expected {
                let bf: BackfillState = state::load(&ctx.store, &backfill_key(ctx.acct, f.id)).unwrap_or_default();
                let lo = if bf.done { 1 } else { bf.below.max(1) };
                if lo <= known_top {
                    let server: HashSet<u32> =
                        conn.c.uid_search(&format!("UID {lo}:{known_top}")).await?.into_iter().collect();
                    let gone: Vec<u32> =
                        local.keys().copied().filter(|u| *u >= lo && *u <= known_top && !server.contains(u)).collect();
                    changed |= self.delete_uids(&local, gone)?;
                }
            }
        }

        let inserted = self.fetch_headers(conn, f, &new_uids, Phase::Headers, f.role == FolderRole::Inbox).await?;
        let max_new = new_uids.first().copied().unwrap_or(0);
        st.uid_next = st.uid_next.max(srv_next).max(max_new.saturating_add(1));
        st.modseq = sel.mailbox.highest_modseq.or(st.modseq);
        st.exists = sel.mailbox.exists;
        state::save(&ctx.store, &folder_key(ctx.acct, f.id), &st)?;
        if f.role == FolderRole::Inbox && !inserted.is_empty() {
            self.prefetch(conn, f, &inserted).await?;
        }
        if changed || !inserted.is_empty() {
            ctx.emit(SyncEvent::FolderUpdated { account: ctx.acct, folder: f.id });
        }
        let bf: Option<BackfillState> = state::load(&ctx.store, &backfill_key(ctx.acct, f.id));
        Ok(FolderOutcome { initial: false, backfill_pending: bf.is_some_and(|b| !b.done) && o.backfill })
    }

    /// Baja cabeceras de `uids` (de más nuevo a más viejo) por lotes. Devuelve `(uid, id, tamaño)`
    /// de lo insertado.
    async fn fetch_headers(
        &self,
        conn: &mut Conn,
        f: &Folder,
        uids: &[u32],
        phase: Phase,
        notify: bool,
    ) -> Result<Vec<(u32, MessageId, u64)>> {
        let ctx = &self.ctx;
        let condstore = conn.c.capabilities().has("CONDSTORE");
        let items = fetch_items::headers(condstore);
        let mut adopt = adopt_list(&ctx.store, f.id);
        let adopt_before = adopt.len();
        let mut out = Vec::new();
        let mut samples = Vec::new();
        let total = uids.len() as u64;
        let mut done = 0u64;
        for chunk in uids.chunks(ctx.opts.header_batch.max(1)) {
            let wanted: HashSet<u32> = chunk.iter().copied().collect();
            let r = conn.c.uid_fetch(&SeqSet::from_numbers(chunk.iter().copied()), &items, None, false).await?;
            // Mensajes locales que esperan su UID (movidos o subidos sin COPYUID/APPENDUID).
            let mut waiting: HashMap<String, MessageEnvelope> = HashMap::new();
            if !adopt.is_empty() {
                for id in &adopt {
                    if let Some(e) = ctx.store.get_envelope(*id)?
                        && e.folder == f.id
                        && e.remote_id.is_none()
                        && let Some(mid) = e.message_id.clone()
                    {
                        waiting.insert(mid, e);
                    }
                }
            }
            let mut batch = Vec::new();
            let mut seen = HashSet::new();
            for fd in &r.messages {
                let Some(uid) = fd.uid.filter(|u| wanted.contains(u)) else { continue };
                if !seen.insert(uid) || ctx.store.message_by_remote(f.id, &uid.to_string())?.is_some() {
                    continue;
                }
                let Some(mut env) = envelope_from_fetch(fd, f.id) else { continue };
                if let Some(mid) = env.message_id.clone()
                    && let Some(local) = waiting.remove(&mid)
                {
                    env.id = local.id;
                    env.thread = local.thread;
                    env.blob = local.blob;
                    env.preview = local.preview;
                    if !local.attachments.is_empty() {
                        env.attachments = local.attachments;
                        env.has_attachments = local.has_attachments;
                    }
                    adopt.retain(|a| *a != local.id);
                }
                batch.push(NewMessage { envelope: env, raw: None, body_text: None });
            }
            let meta: Vec<(u32, u64, String, String)> = batch
                .iter()
                .map(|n| {
                    (
                        n.envelope.remote_id.as_deref().and_then(|r| r.parse().ok()).unwrap_or(0),
                        n.envelope.size,
                        n.envelope.sender_display().to_string(),
                        n.envelope.subject.clone(),
                    )
                })
                .collect();
            let ids = if batch.is_empty() { Vec::new() } else { ctx.store.put_messages(batch)? };
            for (id, (uid, size, from, subject)) in ids.into_iter().zip(meta) {
                out.push((uid, id, size));
                if notify && samples.len() < 3 {
                    samples.push(NewMailSample { message: id, from, subject });
                }
            }
            done = done.saturating_add(chunk.len() as u64);
            ctx.emit(SyncEvent::Progress { account: ctx.acct, phase, folder: Some(f.id), done, total });
        }
        if adopt.len() != adopt_before {
            set_adopt(&ctx.store, f.id, &adopt)?;
        }
        if notify && !out.is_empty() {
            ctx.emit(SyncEvent::NewMail { account: ctx.acct, count: out.len(), sample: samples });
        }
        Ok(out)
    }

    /// Precarga los cuerpos de los mensajes más recientes.
    async fn prefetch(&self, conn: &mut Conn, f: &Folder, inserted: &[(u32, MessageId, u64)]) -> Result<()> {
        let o = &self.ctx.opts;
        if o.prefetch_recent == 0 {
            return Ok(());
        }
        let mut pick: Vec<(u32, MessageId)> =
            inserted.iter().filter(|(_, _, s)| *s <= o.prefetch_max_size).map(|(u, id, _)| (*u, *id)).collect();
        pick.sort_unstable_by_key(|b| std::cmp::Reverse(b.0));
        pick.truncate(o.prefetch_recent);
        let total = pick.len() as u64;
        let mut done = 0u64;
        for chunk in pick.chunks(BODY_BATCH) {
            let ids: HashMap<u32, MessageId> = chunk.iter().copied().collect();
            self.download_bodies(conn, f, &ids).await?;
            done = done.saturating_add(chunk.len() as u64);
            self.ctx.emit(SyncEvent::Progress {
                account: self.ctx.acct,
                phase: Phase::Bodies,
                folder: Some(f.id),
                done,
                total,
            });
        }
        Ok(())
    }

    /// Baja los mensajes completos `uid → id` (la carpeta ya está seleccionada) y los guarda.
    async fn download_bodies(&self, conn: &mut Conn, _f: &Folder, ids: &HashMap<u32, MessageId>) -> Result<usize> {
        let store = &self.ctx.store;
        let r = conn.c.uid_fetch(&SeqSet::from_numbers(ids.keys().copied()), &fetch_items::full(), None, false).await?;
        let mut batch = Vec::new();
        for fd in r.messages {
            let (Some(uid), Some(raw)) = (fd.uid, fd.full_body()) else { continue };
            let Some(&id) = ids.get(&uid) else { continue };
            let Some(mut env) = store.get_envelope(id)? else { continue };
            let body = parse_body(raw);
            let text = body.body_text.clone();
            enrich(&mut env, body);
            if env.size == 0 {
                env.size = raw.len() as u64;
            }
            batch.push(NewMessage { envelope: env, raw: Some(raw.to_vec()), body_text: Some(text) });
        }
        let n = batch.len();
        if n > 0 {
            store.put_messages(batch)?;
        }
        Ok(n)
    }

    /// Cuerpo de un mensaje a petición (al abrirlo).
    pub async fn fetch_body(&self, msg: MessageId) -> Result<()> {
        let store = &self.ctx.store;
        let env = store.get_envelope(msg)?.ok_or_else(|| SyncError::MessageNotFound(msg.to_string()))?;
        if env.blob.is_some() {
            return Ok(());
        }
        let uid = uid_of(&env).ok_or_else(|| SyncError::MessageNotFound(msg.to_string()))?;
        let f = store.get_folder(env.folder)?.ok_or_else(|| SyncError::MessageNotFound(msg.to_string()))?;
        let mut conn = self.pool.get().await?;
        let r = async {
            let already = conn.c.selected().is_some_and(|s| s.name == f.remote_path);
            if !already {
                conn.c.select(&f.remote_path, &SelectOptions { read_only: true, ..Default::default() }).await?;
            }
            let ids: HashMap<u32, MessageId> = [(uid, msg)].into();
            let n = self.download_bodies(&mut conn, &f, &ids).await?;
            if n == 0 { Err(SyncError::MessageNotFound(msg.to_string())) } else { Ok(()) }
        }
        .await;
        self.finish(conn, &r);
        r
    }

    /// Una tanda de historial antiguo. Devuelve `true` si queda más.
    pub async fn backfill_slice(&self) -> Result<bool> {
        let mut conn = self.pool.get().await?;
        let r = self.backfill_on(&mut conn).await;
        self.finish(conn, &r);
        r
    }

    async fn backfill_on(&self, conn: &mut Conn) -> Result<bool> {
        let ctx = &self.ctx;
        let mut budget = ctx.opts.backfill_slice.max(1);
        let mut folders = ctx.store.folders(ctx.acct)?;
        folders.sort_by(|a, b| role_rank(&a.role).cmp(&role_rank(&b.role)).then_with(|| a.name.cmp(&b.name)));
        let mut more = false;
        let mut finished_any = false;
        for f in folders {
            let key = backfill_key(ctx.acct, f.id);
            let Some(mut bf) = state::load::<BackfillState>(&ctx.store, &key) else { continue };
            if bf.done {
                continue;
            }
            if budget == 0 {
                more = true;
                break;
            }
            let sel = conn.c.select(&f.remote_path, &SelectOptions { read_only: true, ..Default::default() }).await?;
            if sel.mailbox.uid_validity.unwrap_or(0) != bf.uid_validity {
                // Lo resolverá la próxima pasada incremental (vacía y reinicia la carpeta).
                continue;
            }
            let mut uids = if bf.below > 1 {
                conn.c.uid_search(&format!("UID 1:{}", bf.below.saturating_sub(1))).await?
            } else {
                Vec::new()
            };
            uids.retain(|u| *u < bf.below);
            uids.sort_unstable_by(|a, b| b.cmp(a));
            let all = uids.len();
            uids.truncate(budget);
            self.fetch_headers(conn, &f, &uids, Phase::Backfill, false).await?;
            budget = budget.saturating_sub(uids.len());
            if uids.len() >= all {
                bf.done = true;
                bf.below = 1;
                finished_any = true;
            } else {
                bf.below = uids.last().copied().unwrap_or(1);
                more = true;
            }
            state::save(&ctx.store, &key, &bf)?;
            if !uids.is_empty() {
                ctx.emit(SyncEvent::FolderUpdated { account: ctx.acct, folder: f.id });
            }
        }
        if finished_any || !more {
            ctx.store.rethread_account(ctx.acct)?;
        }
        Ok(more)
    }

    /// Replica la cola de operaciones (con su propia conexión).
    pub async fn replay(&self) -> Result<()> {
        let mut conn = self.pool.get().await?;
        let r = self.replay_on(&mut conn).await;
        self.finish(conn, &r);
        r
    }

    async fn replay_on(&self, conn: &mut Conn) -> Result<()> {
        let ctx = &self.ctx;
        let ops: Vec<_> = ctx.store.peek_ops(OPS_PER_PASS)?.into_iter().filter(|o| o.account == ctx.acct).collect();
        let total = ops.len() as u64;
        for (i, op) in ops.into_iter().enumerate() {
            let wait = self.backoff.lock().unwrap_or_else(PoisonError::into_inner).get(&op.seq).copied();
            if wait.is_some_and(|w| Instant::now() < w) {
                break;
            }
            let work = match classify(&op.kind, &op.payload) {
                // No es nuestra (p. ej. `outbox.send`): se deja para su dueño.
                None => continue,
                Some(None) => {
                    ctx.store.ack_op(op.seq)?;
                    continue;
                }
                Some(Some(w)) => w,
            };
            let r = match &work {
                QueueWork::Mail(m) => self.apply_op(conn, m).await,
                QueueWork::Compose(c) => self.apply_compose(conn, c).await,
            };
            match r {
                Ok(()) => {
                    ctx.store.ack_op(op.seq)?;
                    self.backoff.lock().unwrap_or_else(PoisonError::into_inner).remove(&op.seq);
                }
                Err(e) => {
                    let attempts = ctx.store.fail_op(op.seq, &e.to_string())?;
                    let fatal = matches!(&e, SyncError::Imap(x) if x.is_fatal_for_connection());
                    let permanent = !e.is_transient() && !fatal;
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
                    if fatal {
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

    async fn ensure_rw(&self, conn: &mut Conn, path: &str) -> Result<()> {
        let ok = conn.c.selected().is_some_and(|s| s.name == path && !s.read_only);
        if !ok {
            conn.c.select(path, &SelectOptions::default()).await?;
        }
        Ok(())
    }

    /// Pone el UID que asignó el servidor a un mensaje local.
    fn set_remote(&self, msg: MessageId, folder: FolderId, uid: u32) -> Result<()> {
        let store = &self.ctx.store;
        let Some(mut env) = store.get_envelope(msg)? else { return Ok(()) };
        if env.folder != folder || env.remote_id.is_some() {
            return Ok(());
        }
        env.remote_id = Some(uid.to_string());
        let body_text = store.get_raw(msg).ok().map(|raw| parse_body(&raw).body_text);
        store.put_messages(vec![NewMessage { envelope: env, raw: None, body_text }])?;
        let mut adopt = adopt_list(store, folder);
        adopt.retain(|a| *a != msg);
        set_adopt(store, folder, &adopt)?;
        Ok(())
    }

    async fn apply_op(&self, conn: &mut Conn, op: &MailOp) -> Result<()> {
        match op {
            MailOp::Flags { folder, uid, add, remove, .. } => {
                self.ensure_rw(conn, folder).await?;
                let set = SeqSet::single(*uid);
                if !add.is_empty() {
                    conn.c.uid_store(&set, StoreMode::Add, add, true, None).await?;
                }
                if !remove.is_empty() {
                    conn.c.uid_store(&set, StoreMode::Remove, remove, true, None).await?;
                }
                Ok(())
            }
            MailOp::Move { msg, from, uid, to, to_folder } => {
                self.ensure_rw(conn, from).await?;
                let map = conn.c.uid_move(&SeqSet::single(*uid), to).await?;
                if let Some(new) = map.and_then(|m| m.mapping.first().map(|p| p.1)) {
                    self.set_remote(*msg, *to_folder, new)?;
                }
                Ok(())
            }
            MailOp::Expunge { folder, uid } => {
                self.ensure_rw(conn, folder).await?;
                let set = SeqSet::single(*uid);
                conn.c.uid_store(&set, StoreMode::Add, &["\\Deleted".to_string()], true, None).await?;
                conn.c.uid_expunge(&set).await?;
                Ok(())
            }
            MailOp::Append { msg, folder, folder_id, flags } => {
                let Ok(raw) = self.ctx.store.get_raw(*msg) else {
                    // Se borró en local antes de subirlo: nada que hacer.
                    return Ok(());
                };
                let r = conn.c.append(folder, flags, None, &raw).await?;
                if let Some((_, uid)) = r {
                    self.set_remote(*msg, *folder_id, uid)?;
                }
                Ok(())
            }
        }
    }

    /// Operaciones del redactor: guardar el enviado en Enviados y marcar el original.
    async fn apply_compose(&self, conn: &mut Conn, op: &SyncOp) -> Result<()> {
        let ctx = &self.ctx;
        match op {
            SyncOp::AppendToSent { raw, message_id, .. } => {
                // Gmail y Microsoft 365 ya guardan lo enviado por SMTP: no se duplica.
                let host = ctx.account.outgoing.host.to_ascii_lowercase();
                let auto_saves = matches!(ctx.account.kind, AccountKind::Gmail | AccountKind::Microsoft365)
                    || host.ends_with("gmail.com")
                    || host.ends_with("office365.com")
                    || host.ends_with("outlook.com");
                if auto_saves {
                    return Ok(());
                }
                let Some(sent) = ctx.store.folders(ctx.acct)?.into_iter().find(|f| f.role == FolderRole::Sent) else {
                    return Ok(());
                };
                let Ok(bytes) = ctx.store.blobs().get(raw) else { return Ok(()) };
                self.ensure_rw(conn, &sent.remote_path).await?;
                let mid = message_id.trim();
                let safe = !mid.is_empty()
                    && mid.len() <= 500
                    && mid.bytes().all(|b| b.is_ascii_graphic() && b != b'"' && b != b'\\');
                if safe && !conn.c.uid_search(&format!("HEADER Message-ID \"{mid}\"")).await?.is_empty() {
                    return Ok(());
                }
                conn.c.append(&sent.remote_path, &["\\Seen".to_string()], None, &bytes).await?;
                Ok(())
            }
            SyncOp::MarkAnswered { message } | SyncOp::MarkForwarded { message } => {
                let flag = if matches!(op, SyncOp::MarkAnswered { .. }) { "\\Answered" } else { "$Forwarded" };
                let Some(env) = ctx.store.get_envelope(*message)? else { return Ok(()) };
                let Some(uid) = uid_of(&env) else { return Ok(()) };
                let Some(f) = ctx.store.get_folder(env.folder)? else { return Ok(()) };
                self.ensure_rw(conn, &f.remote_path).await?;
                conn.c.uid_store(&SeqSet::single(uid), StoreMode::Add, &[flag.to_string()], true, None).await?;
                Ok(())
            }
        }
    }

    /// IDLE en la bandeja de entrada hasta que se pida parar; cada aviso dispara una pasada
    /// rápida de la bandeja de entrada.
    pub async fn idle_session(&self, tx: &mpsc::Sender<Cmd>, shutdown: &watch::Receiver<bool>) -> Result<IdleExit> {
        let mut conn = self.pool.get().await?;
        if !conn.c.capabilities().has("IDLE") {
            self.pool.release(conn);
            return Ok(IdleExit::Unsupported);
        }
        let inbox = self
            .ctx
            .store
            .folders(self.ctx.acct)?
            .into_iter()
            .find(|f| f.role == FolderRole::Inbox)
            .map_or_else(|| "INBOX".to_string(), |f| f.remote_path);
        conn.c.select(&inbox, &SelectOptions { read_only: true, ..Default::default() }).await?;
        loop {
            let mut s = shutdown.clone();
            let stop = async move {
                let _ = s.wait_for(|v| *v).await;
            };
            match conn.c.idle(self.ctx.opts.idle_reissue, stop).await {
                Ok(IdleOutcome::Changed(_)) => {
                    let _ = tx.try_send(Cmd::Sync { inbox_only: true, reply: None });
                }
                Ok(IdleOutcome::Timeout) => {}
                Ok(IdleOutcome::Stopped) => {
                    let _ = tokio::time::timeout(Duration::from_secs(2), conn.c.unselect()).await;
                    return Ok(IdleExit::Stopped);
                }
                Err(e) => return Err(SyncError::Imap(e)),
            }
        }
    }
}

/// Resultado de una carpeta.
struct FolderOutcome {
    initial: bool,
    backfill_pending: bool,
}
