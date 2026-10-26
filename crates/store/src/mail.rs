//! Correo: cuentas (etiqueta), carpetas, mensajes, banderas, hilos y búsqueda.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, PoisonError};

use iuredesk_model::{
    AccountId, Flags, Folder, FolderId, FolderRole, MAX_REFERENCES, MessageEnvelope, MessageId, Thread, ThreadId,
    ThreadInput, normalize_subject, thread_messages,
};
use redb::{ReadableDatabase, ReadableTable, Table, WriteTransaction};
use serde::{Deserialize, Serialize};

use crate::codec::{self, clip, inv_date_key};
use crate::error::{Result, StoreError};
use crate::search::{IndexDoc, ParsedQuery, parse_query};
use crate::tables::*;
use crate::{CacheOp, FolderKeys, Pending, Store, bump_seq, get_rec, next_id, put_rec};

/// Página máxima de `list_envelopes`.
pub const MAX_PAGE: usize = 1_000;
/// Resultados máximos de una búsqueda.
pub const MAX_SEARCH_RESULTS: usize = 1_000;
/// Candidatos máximos que se piden al índice cuando hay filtros posteriores.
const MAX_SEARCH_CANDIDATES: usize = 50_000;
/// Longitud máxima de una clave de texto en índices (Message-ID, id remoto).
const MAX_KEY_LEN: usize = 998;

/// Nombre y dirección de una cuenta para la barra lateral.
///
/// Provisional: cuando `iuredesk_model::Account` esté disponible, el almacén guardará la cuenta
/// completa; esta etiqueta basta para la interfaz y el buzón de ejemplo.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountLabel {
    /// Cuenta.
    pub id: AccountId,
    /// Nombre visible.
    pub name: String,
    /// Dirección principal.
    pub email: String,
}

/// Recuento local de una carpeta.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FolderCount {
    /// Mensajes.
    pub total: u64,
    /// No leídos.
    pub unread: u64,
}

/// Mensaje a guardar.
#[derive(Clone, Debug, Default)]
pub struct NewMessage {
    /// Sobre (con `id` sin asignar para uno nuevo).
    pub envelope: MessageEnvelope,
    /// Mensaje crudo RFC 822, si ya se descargó.
    pub raw: Option<Vec<u8>>,
    /// Texto del cuerpo para el índice (lo extrae la capa que parsea MIME). Si falta se indexa el
    /// extracto.
    pub body_text: Option<String>,
}

/// Tablas de correo abiertas en una transacción de escritura.
struct MailTx<'t> {
    meta: Table<'t, &'static str, &'static [u8]>,
    folders: Table<'t, u64, &'static [u8]>,
    envelopes: Table<'t, u64, &'static [u8]>,
    folder_index: Table<'t, (u64, u64, u64), ()>,
    counts: Table<'t, u64, (u64, u64)>,
    flags: Table<'t, u64, u8>,
    remote: Table<'t, (u64, &'static str), u64>,
    msgids: Table<'t, (u64, &'static str), u64>,
    orphans: Table<'t, (u64, &'static str), u64>,
    threads: Table<'t, u64, &'static [u8]>,
    members: Table<'t, (u64, u64), ()>,
}

impl<'t> MailTx<'t> {
    fn open(tx: &'t WriteTransaction) -> Result<Self> {
        Ok(Self {
            meta: tx.open_table(META)?,
            folders: tx.open_table(FOLDERS)?,
            envelopes: tx.open_table(ENVELOPES)?,
            folder_index: tx.open_table(FOLDER_INDEX)?,
            counts: tx.open_table(FOLDER_COUNTS)?,
            flags: tx.open_table(FLAGS)?,
            remote: tx.open_table(REMOTE_INDEX)?,
            msgids: tx.open_table(MSGID_INDEX)?,
            orphans: tx.open_table(ORPHAN_REFS)?,
            threads: tx.open_table(THREADS)?,
            members: tx.open_table(THREAD_MEMBERS)?,
        })
    }

    fn bump_counts(&mut self, folder: u64, dtotal: i64, dunread: i64) -> Result<()> {
        let (t, u) = self.counts.get(folder)?.map(|g| g.value()).unwrap_or((0, 0));
        let t = t.saturating_add_signed(dtotal);
        let u = u.saturating_add_signed(dunread).min(t);
        self.counts.insert(folder, (t, u))?;
        Ok(())
    }

    /// Quita un mensaje de los índices de carpeta, banderas, remoto y de su hilo.
    fn detach(&mut self, env: &MessageEnvelope, pending: &mut Pending) -> Result<()> {
        let id = env.id.get();
        let key = (inv_date_key(env.date), id);
        self.folder_index.remove((env.folder.get(), key.0, key.1))?;
        pending.cache.push(CacheOp::Remove(env.folder.get(), key));
        self.bump_counts(env.folder.get(), -1, if env.flags.seen { 0 } else { -1 })?;
        if let Some(r) = env.remote_id.as_deref() {
            let r = clip(r, MAX_KEY_LEN);
            if self.remote.get((env.folder.get(), r))?.map(|g| g.value()) == Some(id) {
                self.remote.remove((env.folder.get(), r))?;
            }
        }
        if let Some(t) = env.thread {
            self.members.remove((t.get(), id))?;
            if let Some(mut th) = get_rec::<Thread>(&self.threads, t.get())? {
                th.message_count = th.message_count.saturating_sub(1);
                if !env.flags.seen {
                    th.unread_count = th.unread_count.saturating_sub(1);
                }
                if th.message_count == 0 {
                    self.threads.remove(t.get())?;
                } else {
                    put_rec(&mut self.threads, t.get(), &th)?;
                }
            }
        }
        Ok(())
    }

    /// Añade un mensaje (ya con id e hilo) a los índices.
    fn attach(&mut self, env: &MessageEnvelope, pending: &mut Pending) -> Result<()> {
        let id = env.id.get();
        let key = (inv_date_key(env.date), id);
        self.folder_index.insert((env.folder.get(), key.0, key.1), ())?;
        pending.cache.push(CacheOp::Insert(env.folder.get(), key));
        self.bump_counts(env.folder.get(), 1, if env.flags.seen { 0 } else { 1 })?;
        self.flags.insert(id, env.flags.bits())?;
        if let Some(r) = env.remote_id.as_deref() {
            self.remote.insert((env.folder.get(), clip(r, MAX_KEY_LEN)), id)?;
        }
        if let Some(t) = env.thread {
            self.members.insert((t.get(), id), ())?;
            let mut th = get_rec::<Thread>(&self.threads, t.get())?.unwrap_or_else(|| Thread {
                id: t,
                account: env.account,
                subject: normalize_subject(&env.subject).0,
                last_date: env.date,
                message_count: 0,
                unread_count: 0,
            });
            th.message_count = th.message_count.saturating_add(1);
            if !env.flags.seen {
                th.unread_count = th.unread_count.saturating_add(1);
            }
            if env.date > th.last_date {
                th.last_date = env.date;
            }
            put_rec(&mut self.threads, t.get(), &th)?;
        }
        Ok(())
    }

    /// Hilo para un mensaje nuevo: el de un mensaje citado, el de un hijo que lo esperaba, o uno
    /// nuevo. (La agrupación por asunto la hace [`Store::rethread_account`].)
    fn assign_thread(&mut self, env: &MessageEnvelope) -> Result<ThreadId> {
        let acct = env.account.get();
        let refs = env.in_reply_to.iter().chain(env.references.iter().rev()).take(MAX_REFERENCES.saturating_add(1));
        for r in refs {
            let Some(r) = clean_msgid(r) else { continue };
            let Some(m) = self.msgids.get((acct, r))?.map(|g| g.value()) else { continue };
            if let Some(e) = get_rec::<MessageEnvelope>(&self.envelopes, m)?
                && let Some(t) = e.thread
                && self.threads.get(t.get())?.is_some()
            {
                return Ok(t);
            }
        }
        if let Some(own) = env.message_id.as_deref().and_then(clean_msgid)
            && let Some(t) = self.orphans.get((acct, own))?.map(|g| g.value())
            && self.threads.get(t)?.is_some()
        {
            return Ok(ThreadId(t));
        }
        Ok(ThreadId(next_id(&mut self.meta, "thread")?))
    }

    /// Registra el Message-ID propio y las referencias aún no vistas.
    fn record_ids(&mut self, env: &MessageEnvelope) -> Result<()> {
        let acct = env.account.get();
        if let Some(own) = env.message_id.as_deref().and_then(clean_msgid) {
            if self.msgids.get((acct, own))?.is_none() {
                self.msgids.insert((acct, own), env.id.get())?;
            }
            self.orphans.remove((acct, own))?;
        }
        if let Some(t) = env.thread {
            let start = env.references.len().saturating_sub(MAX_REFERENCES);
            let refs = env.references.iter().skip(start).chain(env.in_reply_to.iter());
            for r in refs {
                let Some(r) = clean_msgid(r) else { continue };
                if self.msgids.get((acct, r))?.is_none() && self.orphans.get((acct, r))?.is_none() {
                    self.orphans.insert((acct, r), t.get())?;
                }
            }
        }
        Ok(())
    }

    fn upsert(&mut self, mut env: MessageEnvelope, body: Option<&str>, pending: &mut Pending) -> Result<MessageId> {
        env.clamp();
        if env.folder.is_unassigned() {
            return Err(StoreError::Invalid("mensaje sin carpeta".into()));
        }
        let folder: Folder =
            get_rec(&self.folders, env.folder.get())?.ok_or_else(|| StoreError::not_found("carpeta", env.folder))?;
        env.account = folder.account;
        if env.id.is_unassigned() {
            let existing = match env.remote_id.as_deref() {
                Some(r) => self.remote.get((env.folder.get(), clip(r, MAX_KEY_LEN)))?.map(|g| g.value()),
                None => None,
            };
            env.id = MessageId(match existing {
                Some(id) => id,
                None => next_id(&mut self.meta, "message")?,
            });
        } else {
            bump_seq(&mut self.meta, "message", env.id.get())?;
        }
        let old: Option<MessageEnvelope> = get_rec(&self.envelopes, env.id.get())?;
        if let Some(old) = &old {
            self.detach(old, pending)?;
            if env.thread.is_none() {
                env.thread = old.thread.filter(|_| old.account == env.account);
            }
        }
        if env.thread.is_none() {
            env.thread = Some(self.assign_thread(&env)?);
        }
        self.attach(&env, pending)?;
        self.record_ids(&env)?;
        put_rec(&mut self.envelopes, env.id.get(), &env)?;
        pending.index.push(index_doc(&env, body));
        Ok(env.id)
    }
}

/// `Message-ID` limpio (sin `<>` ni espacios) para usar como clave.
fn clean_msgid(s: &str) -> Option<&str> {
    let t = s.trim().trim_start_matches('<').trim_end_matches('>').trim();
    if t.is_empty() { None } else { Some(clip(t, MAX_KEY_LEN)) }
}

fn join_mailboxes<'a>(lists: impl IntoIterator<Item = &'a Vec<iuredesk_model::Mailbox>>) -> String {
    let mut s = String::new();
    for list in lists {
        for m in list {
            if let Some(n) = &m.name {
                s.push_str(n);
                s.push(' ');
            }
            s.push_str(&m.addr);
            s.push(' ');
        }
    }
    s
}

pub(crate) fn index_doc(env: &MessageEnvelope, body: Option<&str>) -> IndexDoc {
    IndexDoc {
        msg: env.id.get(),
        account: env.account.get(),
        folder: env.folder.get(),
        subject: env.subject.clone(),
        from: join_mailboxes([&env.from, &env.reply_to]),
        to: join_mailboxes([&env.to, &env.cc, &env.bcc]),
        body: body.unwrap_or(&env.preview).to_string(),
        attachments: env.attachments.iter().map(|a| a.filename.as_str()).collect::<Vec<_>>().join(" "),
        date_ms: env.date.timestamp_millis(),
        has_attachments: env.has_attachments,
    }
}

/// Alias en español (y comunes) de las carpetas especiales para `in:`.
fn role_alias(s: &str) -> Option<FolderRole> {
    Some(match s {
        "inbox" | "entrada" | "bandeja" | "recibidos" => FolderRole::Inbox,
        "sent" | "enviados" | "enviado" => FolderRole::Sent,
        "drafts" | "draft" | "borradores" | "borrador" => FolderRole::Drafts,
        "trash" | "papelera" | "eliminados" | "deleted" => FolderRole::Trash,
        "junk" | "spam" | "nodeseado" | "no-deseado" | "basura" => FolderRole::Junk,
        "archive" | "archivo" | "archivados" => FolderRole::Archive,
        _ => return None,
    })
}

impl Store {
    // ---------------------------------------------------------------- cuentas

    /// Guarda (o reemplaza) la etiqueta de una cuenta.
    pub fn put_account_label(&self, label: &AccountLabel) -> Result<()> {
        if label.id.is_unassigned() {
            return Err(StoreError::Invalid("cuenta sin id".into()));
        }
        self.write(|tx, _| {
            let mut t = tx.open_table(ACCOUNTS)?;
            put_rec(&mut t, label.id.get(), label)?;
            let mut meta = tx.open_table(META)?;
            bump_seq(&mut meta, "account", label.id.get())
        })
    }

    /// Reserva un id de cuenta nuevo.
    pub fn new_account_id(&self) -> Result<AccountId> {
        self.write(|tx, _| {
            let mut meta = tx.open_table(META)?;
            Ok(AccountId(next_id(&mut meta, "account")?))
        })
    }

    /// Etiquetas de todas las cuentas, por id.
    pub fn account_labels(&self) -> Result<Vec<AccountLabel>> {
        self.read_all(ACCOUNTS)
    }

    // ---------------------------------------------------------------- carpetas

    /// Guarda una carpeta (la crea si su id está sin asignar) y devuelve su id.
    pub fn put_folder(&self, folder: &Folder) -> Result<FolderId> {
        let mut f = folder.clone();
        self.write(|tx, _| {
            let mut meta = tx.open_table(META)?;
            if f.id.is_unassigned() {
                f.id = FolderId(next_id(&mut meta, "folder")?);
            } else {
                bump_seq(&mut meta, "folder", f.id.get())?;
            }
            let mut t = tx.open_table(FOLDERS)?;
            if let Some(old) = get_rec::<Folder>(&t, f.id.get())?
                && old.account != f.account
            {
                tx.open_table(ACCOUNT_FOLDERS)?.remove((old.account.get(), f.id.get()))?;
            }
            put_rec(&mut t, f.id.get(), &f)?;
            tx.open_table(ACCOUNT_FOLDERS)?.insert((f.account.get(), f.id.get()), ())?;
            Ok(f.id)
        })
    }

    /// Una carpeta.
    pub fn get_folder(&self, id: FolderId) -> Result<Option<Folder>> {
        self.read_rec(FOLDERS, id.get())
    }

    /// Carpetas de una cuenta (bandeja de entrada primero, luego por nombre).
    pub fn folders(&self, account: AccountId) -> Result<Vec<Folder>> {
        let tx = self.db.begin_read()?;
        let idx = tx.open_table(ACCOUNT_FOLDERS)?;
        let t = tx.open_table(FOLDERS)?;
        let mut out = Vec::new();
        for item in idx.range((account.get(), 0)..=(account.get(), u64::MAX))? {
            let (k, _) = item?;
            if let Some(f) = get_rec::<Folder>(&t, k.value().1)? {
                out.push(f);
            }
        }
        out.sort_by(|a, b| role_rank(&a.role).cmp(&role_rank(&b.role)).then_with(|| a.name.cmp(&b.name)));
        Ok(out)
    }

    /// Todas las carpetas.
    pub fn all_folders(&self) -> Result<Vec<Folder>> {
        self.read_all(FOLDERS)
    }

    /// Recuento local (total y no leídos) de una carpeta.
    pub fn count(&self, folder: FolderId) -> Result<FolderCount> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(FOLDER_COUNTS)?;
        let (total, unread) = t.get(folder.get())?.map(|g| g.value()).unwrap_or((0, 0));
        Ok(FolderCount { total, unread })
    }

    /// Borra una carpeta y todos sus mensajes.
    pub fn delete_folder(&self, id: FolderId) -> Result<()> {
        self.write(|tx, pending| {
            {
                let mut m = MailTx::open(tx)?;
                let f = id.get();
                let keys: Vec<u64> = m
                    .folder_index
                    .range((f, 0, 0)..=(f, u64::MAX, u64::MAX))?
                    .map(|r| r.map(|(k, _)| k.value().2))
                    .collect::<Result<_, _>>()?;
                for msg in keys {
                    if let Some(env) = get_rec::<MessageEnvelope>(&m.envelopes, msg)? {
                        m.detach(&env, pending)?;
                        m.envelopes.remove(msg)?;
                        m.flags.remove(msg)?;
                        pending.unindex.push(msg);
                    }
                }
                m.counts.remove(id.get())?;
                if let Some(f) = get_rec::<Folder>(&m.folders, id.get())? {
                    tx.open_table(ACCOUNT_FOLDERS)?.remove((f.account.get(), id.get()))?;
                }
                m.folders.remove(id.get())?;
            }
            pending.cache.push(CacheOp::Invalidate(id.get()));
            Ok(())
        })
    }

    // ---------------------------------------------------------------- mensajes

    /// Guarda un mensaje (sobre + crudo). Si el id está sin asignar y ya existe uno con el mismo
    /// id remoto en la carpeta, lo reemplaza. Indexa el extracto como cuerpo; para indexar el
    /// texto completo use [`Store::put_messages`] con `body_text`.
    pub fn put_message(&self, envelope: MessageEnvelope, raw: &[u8]) -> Result<MessageId> {
        let ids = self.put_messages(vec![NewMessage { envelope, raw: Some(raw.to_vec()), body_text: None }])?;
        ids.into_iter().next().ok_or_else(|| StoreError::Invalid("lote vacío".into()))
    }

    /// Guarda un lote de mensajes en una sola transacción (lo que debe usar la sincronización:
    /// una transacción con `fsync` por mensaje sería lenta).
    pub fn put_messages(&self, batch: Vec<NewMessage>) -> Result<Vec<MessageId>> {
        // Los blobs van primero y fuera de la transacción: son idempotentes. Comprimir y crear
        // archivos domina el coste de insertar, así que se reparte entre varios hilos.
        let mut prepared = batch;
        let workers = std::thread::available_parallelism().map_or(1, usize::from).clamp(1, 8);
        let chunk = prepared.len().div_ceil(workers).max(16);
        let blobs = &self.blobs;
        let results: Vec<Result<()>> = std::thread::scope(|scope| {
            let handles: Vec<_> = prepared
                .chunks_mut(chunk)
                .map(|part| {
                    scope.spawn(move || -> Result<()> {
                        for m in part {
                            if let Some(raw) = m.raw.take() {
                                m.envelope.blob = Some(blobs.put(&raw)?);
                                if m.envelope.size == 0 {
                                    m.envelope.size = raw.len() as u64;
                                }
                            }
                        }
                        Ok(())
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap_or_else(|_| Err(StoreError::Invalid("fallo al guardar blobs".into()))))
                .collect()
        });
        for r in results {
            r?;
        }
        self.write(|tx, pending| {
            let mut m = MailTx::open(tx)?;
            let mut ids = Vec::with_capacity(prepared.len());
            for p in prepared {
                ids.push(m.upsert(p.envelope, p.body_text.as_deref(), pending)?);
            }
            Ok(ids)
        })
    }

    /// Un sobre.
    pub fn get_envelope(&self, id: MessageId) -> Result<Option<MessageEnvelope>> {
        self.read_rec(ENVELOPES, id.get())
    }

    /// Mensaje crudo (RFC 822) desde el almacén de blobs.
    pub fn get_raw(&self, id: MessageId) -> Result<Vec<u8>> {
        let env = self.get_envelope(id)?.ok_or_else(|| StoreError::not_found("mensaje", id))?;
        let h = env.blob.ok_or_else(|| StoreError::not_found("cuerpo descargado de", id))?;
        self.blobs.get(&h)
    }

    /// Página de sobres de una carpeta por fecha descendente.
    pub fn list_envelopes(&self, folder: FolderId, offset: usize, limit: usize) -> Result<Vec<MessageEnvelope>> {
        let keys = self.folder_keys(folder)?;
        let limit = limit.min(MAX_PAGE);
        let end = offset.saturating_add(limit).min(keys.len());
        let page = keys.get(offset.min(end)..end).unwrap_or(&[]);
        let tx = self.db.begin_read()?;
        let t = tx.open_table(ENVELOPES)?;
        let mut out = Vec::with_capacity(page.len());
        for &(_, id) in page {
            if let Some(e) = get_rec::<MessageEnvelope>(&t, id)? {
                out.push(e);
            }
        }
        Ok(out)
    }

    /// Ids de una carpeta por fecha descendente (todo el orden; barato, sale de la caché).
    pub fn list_ids(&self, folder: FolderId) -> Result<Vec<MessageId>> {
        Ok(self.folder_keys(folder)?.iter().map(|&(_, id)| MessageId(id)).collect())
    }

    /// Orden de la carpeta desde la caché (construyéndola si hace falta).
    pub(crate) fn folder_keys(&self, folder: FolderId) -> Result<FolderKeys> {
        if let Some(v) = self.folder_cache.read().unwrap_or_else(PoisonError::into_inner).get(&folder.get()) {
            return Ok(Arc::clone(v));
        }
        // Se construye con la caché bloqueada: las escrituras que confirmen mientras tanto
        // aplicarán sus cambios después (son idempotentes), así que no se pierde ninguno.
        let mut cache = self.folder_cache.write().unwrap_or_else(PoisonError::into_inner);
        if let Some(v) = cache.get(&folder.get()) {
            return Ok(Arc::clone(v));
        }
        let tx = self.db.begin_read()?;
        let t = tx.open_table(FOLDER_INDEX)?;
        let f = folder.get();
        let mut v = Vec::new();
        for item in t.range((f, 0, 0)..=(f, u64::MAX, u64::MAX))? {
            let (k, _) = item?;
            let (_, d, m) = k.value();
            v.push((d, m));
        }
        let v = Arc::new(v);
        cache.insert(f, Arc::clone(&v));
        Ok(v)
    }

    /// Cambia las banderas de un mensaje.
    pub fn set_flags(&self, id: MessageId, flags: Flags) -> Result<()> {
        self.write(|tx, _| {
            let mut m = MailTx::open(tx)?;
            let mut env: MessageEnvelope =
                get_rec(&m.envelopes, id.get())?.ok_or_else(|| StoreError::not_found("mensaje", id))?;
            if env.flags == flags {
                return Ok(());
            }
            let delta: i64 = match (env.flags.seen, flags.seen) {
                (true, false) => 1,
                (false, true) => -1,
                _ => 0,
            };
            env.flags = flags;
            m.flags.insert(id.get(), flags.bits())?;
            if delta != 0 {
                m.bump_counts(env.folder.get(), 0, delta)?;
                if let Some(t) = env.thread
                    && let Some(mut th) = get_rec::<Thread>(&m.threads, t.get())?
                {
                    th.unread_count = if delta > 0 {
                        th.unread_count.saturating_add(1).min(th.message_count)
                    } else {
                        th.unread_count.saturating_sub(1)
                    };
                    put_rec(&mut m.threads, t.get(), &th)?;
                }
            }
            put_rec(&mut m.envelopes, id.get(), &env)
        })
    }

    /// Mueve un mensaje a otra carpeta de la misma cuenta. El id remoto se borra: la
    /// sincronización asignará el nuevo.
    pub fn move_message(&self, id: MessageId, to: FolderId) -> Result<()> {
        self.write(|tx, pending| {
            let mut m = MailTx::open(tx)?;
            let mut env: MessageEnvelope =
                get_rec(&m.envelopes, id.get())?.ok_or_else(|| StoreError::not_found("mensaje", id))?;
            let dest: Folder = get_rec(&m.folders, to.get())?.ok_or_else(|| StoreError::not_found("carpeta", to))?;
            if dest.account != env.account {
                return Err(StoreError::Invalid("no se puede mover entre cuentas".into()));
            }
            if env.folder == to {
                return Ok(());
            }
            m.detach(&env, pending)?;
            env.folder = to;
            env.remote_id = None;
            m.attach(&env, pending)?;
            put_rec(&mut m.envelopes, id.get(), &env)
        })
    }

    /// Borra un mensaje del almacén local (el blob se conserva: puede compartirse; la
    /// recolección de blobs huérfanos es tarea aparte).
    pub fn delete_message(&self, id: MessageId) -> Result<()> {
        self.write(|tx, pending| {
            let mut m = MailTx::open(tx)?;
            let Some(env) = get_rec::<MessageEnvelope>(&m.envelopes, id.get())? else { return Ok(()) };
            m.detach(&env, pending)?;
            if let Some(own) = env.message_id.as_deref().and_then(clean_msgid)
                && m.msgids.get((env.account.get(), own))?.map(|g| g.value()) == Some(id.get())
            {
                m.msgids.remove((env.account.get(), own))?;
            }
            m.flags.remove(id.get())?;
            m.envelopes.remove(id.get())?;
            pending.unindex.push(id.get());
            Ok(())
        })
    }

    /// Número total de mensajes.
    pub fn message_count(&self) -> Result<u64> {
        use redb::ReadableTableMetadata;
        let tx = self.db.begin_read()?;
        Ok(tx.open_table(ENVELOPES)?.len()?)
    }

    // ---------------------------------------------------------------- hilos

    /// Un hilo.
    pub fn get_thread(&self, id: ThreadId) -> Result<Option<Thread>> {
        self.read_rec(THREADS, id.get())
    }

    /// Mensajes de un hilo, por fecha ascendente.
    pub fn thread_envelopes(&self, id: ThreadId) -> Result<Vec<MessageEnvelope>> {
        let tx = self.db.begin_read()?;
        let mem = tx.open_table(THREAD_MEMBERS)?;
        let envs = tx.open_table(ENVELOPES)?;
        let mut out = Vec::new();
        for item in mem.range((id.get(), 0)..=(id.get(), u64::MAX))?.take(100_000) {
            let (k, _) = item?;
            if let Some(e) = get_rec::<MessageEnvelope>(&envs, k.value().1)? {
                out.push(e);
            }
        }
        out.sort_by_key(|e| (e.date, e.id));
        Ok(out)
    }

    /// Recalcula todos los hilos de una cuenta con el algoritmo JWZ completo (incluye la
    /// agrupación por asunto, que la asignación incremental de `put_messages` no hace). Devuelve
    /// el número de hilos.
    pub fn rethread_account(&self, account: AccountId) -> Result<usize> {
        let mut envs: Vec<MessageEnvelope> = Vec::new();
        for f in self.folders(account)? {
            for id in self.list_ids(f.id)? {
                if let Some(e) = self.get_envelope(id)? {
                    envs.push(e);
                }
            }
        }
        let groups = {
            let inputs: Vec<ThreadInput<'_>> = envs.iter().map(ThreadInput::from).collect();
            thread_messages(&inputs)
        };
        let mut by_id: HashMap<MessageId, MessageEnvelope> = envs.into_iter().map(|e| (e.id, e)).collect();
        self.write(|tx, _| {
            let mut m = MailTx::open(tx)?;
            // Quita los hilos viejos.
            let old: HashSet<u64> = by_id.values().filter_map(|e| e.thread.map(ThreadId::get)).collect();
            for t in old {
                m.threads.remove(t)?;
                let members: Vec<u64> =
                    m.members.range((t, 0)..=(t, u64::MAX))?.filter_map(|r| r.ok().map(|(k, _)| k.value().1)).collect();
                for msg in members {
                    m.members.remove((t, msg))?;
                }
            }
            let n = groups.len();
            for g in groups {
                let tid = ThreadId(next_id(&mut m.meta, "thread")?);
                let mut th = Thread {
                    id: tid,
                    account,
                    subject: g.subject.clone(),
                    last_date: g.last_date,
                    ..Default::default()
                };
                for mem in &g.members {
                    let Some(e) = by_id.get_mut(&mem.id) else { continue };
                    e.thread = Some(tid);
                    th.message_count = th.message_count.saturating_add(1);
                    if !e.flags.seen {
                        th.unread_count = th.unread_count.saturating_add(1);
                    }
                    m.members.insert((tid.get(), e.id.get()), ())?;
                    put_rec(&mut m.envelopes, e.id.get(), &*e)?;
                    m.record_ids(e)?;
                }
                put_rec(&mut m.threads, tid.get(), &th)?;
            }
            Ok(n)
        })
    }

    // ---------------------------------------------------------------- búsqueda

    /// Busca mensajes con el lenguaje de [`parse_query`]; resultados por fecha descendente.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<MessageEnvelope>> {
        self.search_parsed(&parse_query(query), limit)
    }

    /// Como [`Store::search`], con la consulta ya interpretada.
    pub fn search_parsed(&self, q: &ParsedQuery, limit: usize) -> Result<Vec<MessageEnvelope>> {
        let limit = limit.clamp(1, MAX_SEARCH_RESULTS);
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let folders = if q.folders.is_empty() { None } else { Some(self.resolve_folders(&q.folders)?) };
        if folders.as_ref().is_some_and(HashSet::is_empty) {
            return Ok(Vec::new());
        }
        let keep = |e: &MessageEnvelope| -> bool {
            folders.as_ref().is_none_or(|fs| fs.contains(&e.folder))
                && q.unread.is_none_or(|u| u != e.flags.seen)
                && q.flagged.is_none_or(|f| f == e.flags.flagged)
        };
        if q.needs_index() {
            let post = folders.is_some() || q.unread.is_some() || q.flagged.is_some();
            // Con filtros posteriores se piden más candidatos: recoger el top-k cuesta casi lo
            // mismo con k = 1000 que con k = 100 (domina recorrer las coincidencias).
            let mut k = if post { limit.saturating_mul(10) } else { limit };
            loop {
                let ids = self.search.run(q, k)?;
                let exhausted = ids.len() < k;
                let tx = self.db.begin_read()?;
                let t = tx.open_table(ENVELOPES)?;
                let mut out = Vec::with_capacity(limit);
                for id in ids {
                    // Un documento huérfano (mensaje ya borrado) simplemente no aparece.
                    if let Some(e) = get_rec::<MessageEnvelope>(&t, id)?
                        && keep(&e)
                    {
                        out.push(e);
                        if out.len() >= limit {
                            break;
                        }
                    }
                }
                if out.len() >= limit || exhausted || k >= MAX_SEARCH_CANDIDATES {
                    return Ok(out);
                }
                k = k.saturating_mul(8).min(MAX_SEARCH_CANDIDATES);
            }
        }
        // Solo filtros estructurales (carpeta / banderas): recorrido de las carpetas por fecha.
        let fids: Vec<FolderId> = match &folders {
            Some(fs) => fs.iter().copied().collect(),
            None => self.all_folders()?.into_iter().map(|f| f.id).collect(),
        };
        let mut cands: Vec<(u64, u64)> = Vec::new();
        {
            let tx = self.db.begin_read()?;
            let flags = tx.open_table(FLAGS)?;
            for f in fids {
                let keys = self.folder_keys(f)?;
                let mut found = 0usize;
                for &(d, id) in keys.iter() {
                    let fl = Flags::from_bits(flags.get(id)?.map(|g| g.value()).unwrap_or(0));
                    if q.unread.is_none_or(|u| u != fl.seen) && q.flagged.is_none_or(|x| x == fl.flagged) {
                        cands.push((d, id));
                        found = found.saturating_add(1);
                        if found >= limit {
                            break;
                        }
                    }
                }
            }
        }
        cands.sort_unstable();
        cands.truncate(limit);
        let tx = self.db.begin_read()?;
        let t = tx.open_table(ENVELOPES)?;
        let mut out = Vec::with_capacity(cands.len());
        for (_, id) in cands {
            if let Some(e) = get_rec::<MessageEnvelope>(&t, id)? {
                out.push(e);
            }
        }
        Ok(out)
    }

    fn resolve_folders(&self, names: &[String]) -> Result<HashSet<FolderId>> {
        let all = self.all_folders()?;
        let mut out = HashSet::new();
        for n in names {
            let n = n.to_lowercase();
            let role = role_alias(&n);
            for f in &all {
                if role.as_ref() == Some(&f.role) || f.name.to_lowercase() == n || f.remote_path.to_lowercase() == n {
                    out.insert(f.id);
                }
            }
        }
        Ok(out)
    }

    /// Reconstruye el índice de búsqueda desde redb. `extract` obtiene el texto del cuerpo a
    /// partir del mensaje crudo (lo aporta la capa que parsea MIME); sin él se indexa el extracto.
    /// Devuelve el número de mensajes indexados.
    pub fn reindex_all(&self, extract: Option<&dyn Fn(&[u8]) -> Option<String>>) -> Result<u64> {
        let _g = self.lock_writes();
        self.search.clear()?;
        let mut last = 0u64;
        let mut total = 0u64;
        loop {
            let mut docs = Vec::with_capacity(1_000);
            {
                let tx = self.db.begin_read()?;
                let t = tx.open_table(ENVELOPES)?;
                for item in t.range(last.saturating_add(1)..)?.take(1_000) {
                    let (k, v) = item?;
                    last = k.value();
                    let env: MessageEnvelope = codec::decode(v.value())?;
                    let body = match (extract, env.blob) {
                        (Some(f), Some(h)) => self.blobs.get(&h).ok().and_then(|raw| f(&raw)),
                        _ => None,
                    };
                    docs.push(index_doc(&env, body.as_deref()));
                }
            }
            if docs.is_empty() {
                break;
            }
            total = total.saturating_add(docs.len() as u64);
            self.search.upsert(&docs)?;
            if last == u64::MAX {
                break;
            }
        }
        if extract.is_some() {
            let tx = self.db.begin_write()?;
            tx.open_table(META)?.remove(crate::META_NEEDS_BODY_REINDEX)?;
            tx.commit()?;
        }
        Ok(total)
    }
}

fn role_rank(r: &FolderRole) -> u8 {
    match r {
        FolderRole::Inbox => 0,
        FolderRole::Drafts => 1,
        FolderRole::Sent => 2,
        FolderRole::Archive => 3,
        FolderRole::Junk => 4,
        FolderRole::Trash => 5,
        FolderRole::Custom => 6,
    }
}
