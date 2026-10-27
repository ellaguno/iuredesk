//! Destino de lo importado: cuentas y carpetas locales, lotes de mensajes con deduplicación,
//! contactos/eventos/tareas/notas por `UID`, progreso, cancelación y puntos de reanudación.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use iuredesk_model::{
    AccountId, AddressBook, AddressBookId, Calendar, CalendarId, Contact, ContactId, Event, EventId, Flags, Folder,
    FolderId, FolderRole, Note, NoteId, Task, TaskId, TaskList, TaskListId,
};
use iuredesk_store::{AccountLabel, NewMessage, Store};
use sha2::{Digest, Sha256};

use crate::ImportOptions;
use crate::checkpoint::Checkpoint;
use crate::error::{ImportError, Result};
use crate::headers;
use crate::report::{ImportEvent, ImportReport, Notice, Progress, SkipReason, Skipped};
use crate::roles::role_for_name;

/// Cada cuánto se emite progreso como mucho.
const PROGRESS_EVERY: Duration = Duration::from_millis(200);
/// Profundidad máxima de carpetas.
const MAX_FOLDER_DEPTH: usize = 32;
/// Caracteres máximos del nombre de una carpeta.
const MAX_FOLDER_NAME: usize = 255;

/// Metadatos de un mensaje que da el origen (además del mensaje crudo).
#[derive(Clone, Debug, Default)]
pub(crate) struct MsgMeta {
    /// Banderas del origen (si no, las del propio mensaje no se conocen y queda «no leído»… salvo
    /// que el llamador diga otra cosa).
    pub flags: Option<Flags>,
    /// Palabras clave / etiquetas.
    pub keywords: Vec<String>,
    /// Fecha de recepción (si falta `Date:` o es absurda).
    pub received: Option<DateTime<Utc>>,
    /// Clave de deduplicación propia (si no, [`headers::dedup_key`]).
    pub dedup: Option<String>,
}

type Ids<C, I> = (C, HashMap<String, I>);

/// Destino de la importación.
pub(crate) struct Sink<'a> {
    pub store: &'a Store,
    pub opts: &'a ImportOptions,
    events: &'a mut dyn FnMut(&ImportEvent),
    cancel: Option<&'a AtomicBool>,
    pub report: ImportReport,
    pub checkpoint: Checkpoint,
    batch: Vec<NewMessage>,
    batch_bytes: usize,
    batch_keys: HashSet<(u64, String)>,
    accounts: HashMap<String, AccountId>,
    loaded_accounts: HashSet<u64>,
    folders: HashMap<(u64, String), FolderId>,
    roles_used: HashSet<(u64, FolderRole)>,
    progress: Progress,
    last_emit: Option<Instant>,
    books: HashMap<String, Ids<AddressBookId, ContactId>>,
    cals: HashMap<String, Ids<CalendarId, EventId>>,
    lists: HashMap<String, Ids<TaskListId, TaskId>>,
    notes: Option<HashMap<String, NoteId>>,
}

impl<'a> Sink<'a> {
    pub fn new(
        store: &'a Store,
        opts: &'a ImportOptions,
        checkpoint: Checkpoint,
        cancel: Option<&'a AtomicBool>,
        events: &'a mut dyn FnMut(&ImportEvent),
    ) -> Self {
        Self {
            store,
            opts,
            events,
            cancel,
            report: ImportReport::default(),
            checkpoint,
            batch: Vec::new(),
            batch_bytes: 0,
            batch_keys: HashSet::new(),
            accounts: HashMap::new(),
            loaded_accounts: HashSet::new(),
            folders: HashMap::new(),
            roles_used: HashSet::new(),
            progress: Progress::default(),
            last_emit: None,
            books: HashMap::new(),
            cals: HashMap::new(),
            lists: HashMap::new(),
            notes: None,
        }
    }

    // ------------------------------------------------------------ control

    /// Error `Cancelled` si se pidió parar.
    pub fn check_cancel(&self) -> Result<()> {
        if self.cancel.is_some_and(|c| c.load(Ordering::Relaxed)) { Err(ImportError::Cancelled) } else { Ok(()) }
    }

    pub fn emit(&mut self, e: ImportEvent) {
        (self.events)(&e);
    }

    pub fn set_total_bytes(&mut self, total: u64) {
        self.progress.bytes_total = total;
    }

    pub fn set_current(&mut self, what: String) {
        self.progress.current = what;
        self.emit_progress(true);
    }

    /// Avanza los bytes procesados.
    pub fn advance(&mut self, bytes: u64) {
        self.progress.bytes_done = self.progress.bytes_done.saturating_add(bytes);
        self.report.bytes_read = self.report.bytes_read.saturating_add(bytes);
        self.emit_progress(false);
    }

    fn emit_progress(&mut self, force: bool) {
        let now = Instant::now();
        if !force && self.last_emit.is_some_and(|t| now.duration_since(t) < PROGRESS_EVERY) {
            return;
        }
        self.last_emit = Some(now);
        self.progress.counts = self.report.counts;
        let p = self.progress.clone();
        self.emit(ImportEvent::Progress(p));
    }

    pub fn skip(&mut self, location: String, reason: SkipReason) {
        let s = Skipped { location, reason };
        self.emit(ImportEvent::Skipped(s.clone()));
        self.report.add_skip(s);
    }

    pub fn notice(&mut self, n: Notice) {
        if self.report.add_notice(n.clone()) {
            self.emit(ImportEvent::Notice(n));
        }
    }

    // ------------------------------------------------------------ cuentas y carpetas

    /// Cuenta local con este nombre (la crea si no existe; se reutiliza al reanudar).
    pub fn account(&mut self, name: &str, email: &str) -> Result<AccountId> {
        if let Some(id) = self.accounts.get(name) {
            return Ok(*id);
        }
        let existing = self.store.account_labels()?.into_iter().find(|l| l.name == name);
        let id = match existing {
            Some(l) => l.id,
            None => {
                let id = self.store.new_account_id()?;
                self.store.put_account_label(&AccountLabel { id, name: name.to_string(), email: email.to_string() })?;
                id
            }
        };
        self.accounts.insert(name.to_string(), id);
        if !self.report.accounts.iter().any(|a| a == name) {
            self.report.accounts.push(name.to_string());
        }
        Ok(id)
    }

    fn load_account_folders(&mut self, account: AccountId) -> Result<()> {
        if !self.loaded_accounts.insert(account.get()) {
            return Ok(());
        }
        for f in self.store.folders(account)? {
            if f.role != FolderRole::Custom {
                self.roles_used.insert((account.get(), f.role));
            }
            self.folders.insert((account.get(), f.remote_path.clone()), f.id);
        }
        Ok(())
    }

    /// Carpeta `path` (segmentos) de la cuenta, creando las que falten. `role` fuerza la función
    /// de la carpeta de primer nivel; si no, se deduce del nombre.
    pub fn folder(&mut self, account: AccountId, path: &[String], role: Option<FolderRole>) -> Result<FolderId> {
        self.load_account_folders(account)?;
        let mut segs: Vec<String> = path
            .iter()
            .map(|s| {
                let mut s = s.trim().replace('/', "∕");
                iuredesk_model::truncate_chars(&mut s, MAX_FOLDER_NAME);
                s
            })
            .filter(|s| !s.is_empty())
            .collect();
        // `[Gmail]/Enviados` → `Enviados` (las carpetas de Gmail van al primer nivel).
        if segs.len() > 1 && segs.first().is_some_and(|s| s == "[Gmail]" || s == "[Google Mail]") {
            segs.remove(0);
        }
        segs.truncate(MAX_FOLDER_DEPTH);
        if segs.is_empty() {
            segs.push(self.opts.text("Importado", "Imported").to_string());
        }
        let mut parent: Option<FolderId> = None;
        let mut remote = String::new();
        let depth = segs.len();
        for (i, name) in segs.into_iter().enumerate() {
            if !remote.is_empty() {
                remote.push('/');
            }
            remote.push_str(&name);
            if let Some(id) = self.folders.get(&(account.get(), remote.clone())) {
                parent = Some(*id);
                continue;
            }
            let wanted = match (i, role) {
                (0, Some(r)) if depth == 1 => r,
                (0, _) => role_for_name(&name),
                _ => FolderRole::Custom,
            };
            let role = if wanted != FolderRole::Custom && self.roles_used.insert((account.get(), wanted)) {
                wanted
            } else {
                FolderRole::Custom
            };
            let f = Folder { account, name, remote_path: remote.clone(), role, parent, ..Default::default() };
            let id = self.store.put_folder(&f)?;
            self.report.counts.folders = self.report.counts.folders.saturating_add(1);
            self.folders.insert((account.get(), remote.clone()), id);
            parent = Some(id);
        }
        parent.ok_or_else(|| ImportError::Checkpoint("carpeta vacía".into()))
    }

    // ------------------------------------------------------------ mensajes

    /// Añade un mensaje al lote (o lo omite con motivo). `location` describe dónde estaba.
    pub fn message(
        &mut self,
        folder: FolderId,
        raw: Vec<u8>,
        meta: MsgMeta,
        location: &dyn Fn() -> String,
    ) -> Result<()> {
        self.check_cancel()?;
        let size = raw.len() as u64;
        if raw.iter().all(u8::is_ascii_whitespace) {
            self.skip(location(), SkipReason::Empty);
            return Ok(());
        }
        if size > self.opts.max_message_bytes {
            self.skip(location(), SkipReason::TooLarge { size, max: self.opts.max_message_bytes });
            return Ok(());
        }
        if !looks_like_message(&raw) {
            self.skip(location(), SkipReason::Malformed { detail: "sin cabeceras".into() });
            return Ok(());
        }
        let key = meta.dedup.unwrap_or_else(|| headers::dedup_key(&raw));
        if self.batch_keys.contains(&(folder.get(), key.clone()))
            || self.store.message_by_remote(folder, &key)?.is_some()
        {
            self.report.counts.duplicates = self.report.counts.duplicates.saturating_add(1);
            self.emit_progress(false);
            return Ok(());
        }
        let received = meta.received.unwrap_or_else(Utc::now);
        let (mut env, body) = iuredesk_sync::parse::envelope_from_raw(&raw, folder, received);
        match meta.flags {
            Some(f) => env.flags = f,
            None => env.flags.seen = self.opts.unknown_as_read,
        }
        for k in meta.keywords {
            if !env.keywords.contains(&k) {
                env.keywords.push(k);
            }
        }
        env.remote_id = Some(key.clone());
        self.batch_keys.insert((folder.get(), key));
        self.batch_bytes = self.batch_bytes.saturating_add(raw.len());
        self.batch.push(NewMessage { envelope: env, raw: Some(raw), body_text: Some(body) });
        self.report.counts.messages = self.report.counts.messages.saturating_add(1);
        if self.batch.len() >= self.opts.batch_messages || self.batch_bytes >= self.opts.batch_bytes {
            self.flush()?;
        }
        self.emit_progress(false);
        Ok(())
    }

    /// Guarda el lote y confirma el estado de reanudación.
    pub fn flush(&mut self) -> Result<()> {
        if !self.batch.is_empty() {
            let batch = std::mem::take(&mut self.batch);
            self.store.put_messages(batch)?;
            self.batch_bytes = 0;
            self.batch_keys.clear();
        }
        self.checkpoint.commit()?;
        self.emit_progress(true);
        Ok(())
    }

    // ------------------------------------------------------------ PIM

    fn named_uid(prefix: &str, parts: &[&str]) -> String {
        let mut h = Sha256::new();
        for p in parts {
            h.update(p.as_bytes());
            h.update(b"\0");
        }
        let d = h.finalize();
        let mut s = format!("{prefix}-");
        for b in d.iter().take(16) {
            s.push_str(&format!("{b:02x}"));
        }
        s
    }

    fn book(&mut self, account: AccountId, name: &str) -> Result<AddressBookId> {
        if let Some((id, _)) = self.books.get(name) {
            return Ok(*id);
        }
        let remote = format!("import:{name}");
        let existing =
            self.store.address_books()?.into_iter().find(|b| b.account == account && b.remote_path == remote);
        let (id, uids) = match existing {
            Some(b) => {
                let uids = self.store.contacts(b.id)?.into_iter().map(|c| (c.uid.clone(), c.id)).collect();
                (b.id, uids)
            }
            None => {
                let b = AddressBook { account, name: name.to_string(), remote_path: remote, ..Default::default() };
                (self.store.put_address_book(&b)?, HashMap::new())
            }
        };
        self.books.insert(name.to_string(), (id, uids));
        Ok(id)
    }

    /// Guarda un contacto en la libreta `book` (por `UID`: si ya estaba, se actualiza).
    pub fn contact(&mut self, account: AccountId, book: &str, mut c: Contact, photo: Option<Vec<u8>>) -> Result<()> {
        self.check_cancel()?;
        let book_id = self.book(account, book)?;
        if c.uid.trim().is_empty() {
            let emails: Vec<&str> = c.emails.iter().map(|e| e.value.as_str()).collect();
            c.uid = Self::named_uid("import", &[&c.full_name, &c.given_name, &c.family_name, &emails.join(",")]);
        }
        if c.full_name.trim().is_empty() {
            c.full_name = [c.given_name.as_str(), c.family_name.as_str()]
                .iter()
                .filter(|s| !s.is_empty())
                .copied()
                .collect::<Vec<_>>()
                .join(" ");
            if c.full_name.is_empty() {
                c.full_name = c.primary_email().unwrap_or("").to_string();
            }
        }
        if let Some(p) = photo.filter(|p| !p.is_empty() && p.len() <= 16 * 1024 * 1024) {
            c.photo = self.store.blobs().put(&p).ok();
        }
        c.address_book = book_id;
        let existing = self.books.get(book).and_then(|(_, m)| m.get(&c.uid).copied());
        c.id = existing.unwrap_or(ContactId::UNASSIGNED);
        let id = self.store.put_contact(&c)?;
        if let Some((_, m)) = self.books.get_mut(book) {
            m.insert(c.uid.clone(), id);
        }
        self.report.counts.contacts = self.report.counts.contacts.saturating_add(1);
        self.emit_progress(false);
        Ok(())
    }

    fn calendar(&mut self, account: AccountId, name: &str) -> Result<CalendarId> {
        if let Some((id, _)) = self.cals.get(name) {
            return Ok(*id);
        }
        let remote = format!("import:{name}");
        let existing = self.store.calendars()?.into_iter().find(|c| c.account == account && c.remote_path == remote);
        let (id, uids) = match existing {
            Some(c) => {
                let uids = self.store.events_in_calendar(c.id)?.into_iter().map(|e| (e.uid.clone(), e.id)).collect();
                (c.id, uids)
            }
            None => {
                let c = Calendar { account, name: name.to_string(), remote_path: remote, ..Default::default() };
                (self.store.put_calendar(&c)?, HashMap::new())
            }
        };
        self.cals.insert(name.to_string(), (id, uids));
        Ok(id)
    }

    /// Guarda un evento en el calendario `cal` (por `UID`).
    pub fn event(&mut self, account: AccountId, cal: &str, mut e: Event) -> Result<EventId> {
        self.check_cancel()?;
        let cal_id = self.calendar(account, cal)?;
        if e.uid.trim().is_empty() {
            let start = e.start.to_utc().to_rfc3339();
            e.uid = Self::named_uid("import", &[&e.summary, &start, &e.location]);
        }
        e.calendar = cal_id;
        e.id = self.cals.get(cal).and_then(|(_, m)| m.get(&e.uid).copied()).unwrap_or(EventId::UNASSIGNED);
        let id = self.store.put_event(&e)?;
        if let Some((_, m)) = self.cals.get_mut(cal) {
            m.insert(e.uid.clone(), id);
        }
        self.report.counts.events = self.report.counts.events.saturating_add(1);
        self.emit_progress(false);
        Ok(id)
    }

    fn task_list(&mut self, account: AccountId, name: &str) -> Result<TaskListId> {
        if let Some((id, _)) = self.lists.get(name) {
            return Ok(*id);
        }
        let remote = format!("import:{name}");
        let existing =
            self.store.task_lists()?.into_iter().find(|l| l.account == Some(account) && l.remote_path == remote);
        let (id, uids) = match existing {
            Some(l) => {
                let uids = self.store.tasks(l.id)?.into_iter().map(|t| (t.uid.clone(), t.id)).collect();
                (l.id, uids)
            }
            None => {
                let l = TaskList {
                    account: Some(account),
                    name: name.to_string(),
                    remote_path: remote,
                    ..Default::default()
                };
                (self.store.put_task_list(&l)?, HashMap::new())
            }
        };
        self.lists.insert(name.to_string(), (id, uids));
        Ok(id)
    }

    /// Guarda una tarea en la lista `list` (por `UID`).
    pub fn task(&mut self, account: AccountId, list: &str, mut t: Task) -> Result<()> {
        self.check_cancel()?;
        let list_id = self.task_list(account, list)?;
        if t.uid.trim().is_empty() {
            let due = t.due.as_ref().map(|d| d.to_utc().to_rfc3339()).unwrap_or_default();
            t.uid = Self::named_uid("import", &[&t.summary, &due, &t.description]);
        }
        t.list = list_id;
        t.id = self.lists.get(list).and_then(|(_, m)| m.get(&t.uid).copied()).unwrap_or(TaskId::UNASSIGNED);
        let id = self.store.put_task(&t)?;
        if let Some((_, m)) = self.lists.get_mut(list) {
            m.insert(t.uid.clone(), id);
        }
        self.report.counts.tasks = self.report.counts.tasks.saturating_add(1);
        self.emit_progress(false);
        Ok(())
    }

    /// Guarda una nota (deduplicada por título + cuerpo + creación).
    pub fn note(&mut self, mut n: Note) -> Result<()> {
        self.check_cancel()?;
        if self.notes.is_none() {
            let map = self
                .store
                .notes()?
                .into_iter()
                .filter_map(|n| n.remote_id.clone().filter(|r| r.starts_with("import-")).map(|r| (r, n.id)))
                .collect();
            self.notes = Some(map);
        }
        let key = Self::named_uid("import", &[&n.title, &n.body, &n.created.to_rfc3339()]);
        n.id = self.notes.as_ref().and_then(|m| m.get(&key).copied()).unwrap_or(NoteId::UNASSIGNED);
        n.remote_id = Some(key.clone());
        let id = self.store.put_note(&n)?;
        if let Some(m) = self.notes.as_mut() {
            m.insert(key, id);
        }
        self.report.counts.notes = self.report.counts.notes.saturating_add(1);
        self.emit_progress(false);
        Ok(())
    }
}

/// ¿Empieza como un mensaje (una cabecera `Nombre: valor`)?
fn looks_like_message(raw: &[u8]) -> bool {
    let start = raw.iter().position(|b| !b.is_ascii_whitespace()).unwrap_or(0);
    let line = raw.get(start..).unwrap_or(&[]);
    let line = line.split(|&b| b == b'\n').next().unwrap_or(&[]);
    let Some(colon) = line.iter().position(|&b| b == b':') else { return false };
    colon > 0 && line.get(..colon).is_some_and(|n| n.iter().all(|b| b.is_ascii_graphic() && *b != b':'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_shape() {
        assert!(looks_like_message(b"Subject: x\n\nhola"));
        assert!(looks_like_message(b"\r\nReceived: from x\r\n"));
        assert!(!looks_like_message(b"hola mundo\n"));
        assert!(!looks_like_message(b"no header: here\n"));
        assert!(!looks_like_message(b""));
    }
}
