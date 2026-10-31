//! UX-6: las notas del almacén ⇄ la carpeta de notas en Markdown (ver [`crate::notes_fs`]).
//!
//! El almacén es el índice; cada nota con texto tiene además su archivo `.md`. Lo que se escribe
//! aquí se guarda en el archivo (en el hilo de la carpeta, local primero); lo que se guarda en
//! IureEditor (u otro editor) vuelve a la nota con el aviso «Actualizada desde IureEditor». Si
//! cambió en los dos lados a la vez, se conservan las dos versiones y se avisa.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use iuredesk_model::{Note, NoteFile, NoteId};

use super::notes::{NoteUndo, note_title};
use super::{Inverse, Notice, NoticeKind, Session, store_err};
use crate::Result;
use crate::lang::{Text, pick};
use crate::notes_fs::{self, Cmd, Evt, FsEvent, Worker};

/// Estado del puente con la carpeta.
#[derive(Debug, Default)]
pub(crate) struct NotesFs {
    worker: Option<Worker>,
    /// Escrituras mandadas y sin respuesta.
    inflight: HashSet<NoteId>,
    /// Ya se hizo la primera vuelta (antes no se escribe: se enlaza lo que ya hay).
    ready: bool,
    seq: u64,
    /// Motivo por el que la carpeta no está disponible.
    unavailable: Option<String>,
    /// Copia en conflicto creada para cada nota (si se sigue escribiendo, va a la misma copia).
    copies: HashMap<NoteId, NoteId>,
    /// Pruebas: vuelta más rápida.
    poll: Option<Duration>,
}

/// Resultado de guardar desde el editor de la app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoteSaved {
    /// Guardada; su nueva fecha de modificación (la base de la siguiente).
    Saved(DateTime<Utc>),
    /// La nota cambió por debajo (IureEditor) mientras se escribía aquí: lo de aquí quedó en
    /// una copia.
    Copied(NoteId),
}

fn is_dirty(n: &Note) -> bool {
    n.file.as_ref().is_none_or(|f| f.synced < n.updated)
}

fn has_text(n: &Note) -> bool {
    !n.title.trim().is_empty() || !n.body.trim().is_empty()
}

fn render_note(n: &Note) -> String {
    notes_fs::render(&n.title, &n.body, &n.tags, n.pinned)
}

impl Session {
    /// Carpeta de notas que toca usar y si es la de omisión. Sin preferencia, el ejemplo y las
    /// sesiones sin archivo de preferencias (pruebas) no escriben archivos.
    fn wanted_notes_dir(&self) -> Option<(PathBuf, bool)> {
        if let Some(d) = self.prefs.notes_dir.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
            return Some((PathBuf::from(d), false));
        }
        if self.sample || self.prefs_path.is_none() {
            return None;
        }
        notes_fs::default_dir().map(|d| (d, true))
    }

    /// Carpeta de notas en uso (`None` = las notas solo viven en el almacén).
    pub fn notes_dir(&self) -> Option<PathBuf> {
        self.notes_fs.worker.as_ref().map(|w| w.dir().to_path_buf())
    }

    /// Por qué la carpeta de notas no está disponible ahora (unidad sin montar…).
    pub fn notes_dir_problem(&self) -> Option<&str> {
        self.notes_fs.unavailable.as_deref()
    }

    /// (Re)arranca el hilo de la carpeta si cambió.
    pub(crate) fn start_notes_fs(&mut self) {
        let wanted = self.wanted_notes_dir();
        if wanted.as_ref().map(|(d, _)| d.as_path()) == self.notes_fs.worker.as_ref().map(|w| w.dir()) {
            return;
        }
        self.notes_fs.worker = None;
        self.notes_fs.inflight.clear();
        self.notes_fs.ready = false;
        self.notes_fs.unavailable = None;
        let Some((dir, default)) = wanted else { return };
        let known: Vec<(String, String)> = self
            .store
            .notes()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|n| n.file.map(|f| (f.name, f.hash)))
            .collect();
        let poll = self.notes_fs.poll.unwrap_or(notes_fs::POLL);
        match Worker::spawn(dir, known, default, poll, Some(self.bg.waker())) {
            Ok(w) => self.notes_fs.worker = Some(w),
            Err(e) => self.notes_fs.unavailable = Some(e.to_string()),
        }
    }

    /// Cambia la carpeta de notas (Ajustes). Las notas se escriben en la nueva (la vieja se deja
    /// como está); si allí ya hay archivos, se agregan como notas o se enlazan con la igual.
    pub fn set_notes_dir(&mut self, dir: Option<String>) {
        let dir = dir.map(|d| d.trim().to_string()).filter(|d| !d.is_empty());
        if dir == self.prefs.notes_dir {
            return;
        }
        self.prefs.notes_dir = dir;
        self.save_prefs();
        for mut n in self.store.notes().unwrap_or_default() {
            if n.file.take().is_some() {
                let _ = self.store.put_note(&n);
            }
        }
        self.notes_fs.worker = None;
        self.start_notes_fs();
        self.data_gen = self.data_gen.wrapping_add(1);
    }

    /// Pruebas: vuelta corta de la carpeta y carpeta propia.
    #[doc(hidden)]
    pub fn notes_fs_for_tests(&mut self, dir: PathBuf, poll: Duration) {
        self.notes_fs.poll = Some(poll);
        self.prefs.notes_dir = Some(dir.to_string_lossy().into_owned());
        self.notes_fs.worker = None;
        self.start_notes_fs();
    }

    /// Recoge lo que hizo el hilo de la carpeta (en `poll`).
    pub(crate) fn poll_notes_fs(&mut self) {
        let Some(w) = &self.notes_fs.worker else { return };
        let evts = w.drain();
        for e in evts {
            self.apply_notes_evt(e);
        }
    }

    /// Espera a que el hilo haga una vuelta completa y aplica todo (abrir en IureEditor, pruebas).
    pub fn notes_fs_flush(&mut self, timeout: Duration) -> bool {
        self.notes_reconcile();
        let deadline = Instant::now() + timeout;
        // Las escrituras pendientes primero.
        while !self.notes_fs.inflight.is_empty() && Instant::now() < deadline {
            let Some(e) = self.notes_fs.worker.as_ref().and_then(|w| w.recv_until(deadline)) else { break };
            self.apply_notes_evt(e);
        }
        self.notes_fs.seq = self.notes_fs.seq.wrapping_add(1);
        let seq = self.notes_fs.seq;
        let Some(w) = &self.notes_fs.worker else { return false };
        w.send(Cmd::Flush(seq));
        loop {
            let Some(e) = self.notes_fs.worker.as_ref().and_then(|w| w.recv_until(deadline)) else { return false };
            if matches!(e, Evt::Flushed(s) if s == seq) {
                self.notes_reconcile();
                return true;
            }
            self.apply_notes_evt(e);
        }
    }

    fn apply_notes_evt(&mut self, e: Evt) {
        match e {
            Evt::Fs(ev) => self.apply_fs_event(ev),
            Evt::Written { id, name, hash, updated } => {
                self.notes_fs.inflight.remove(&id);
                match self.store.get_note(id).ok().flatten() {
                    Some(mut n) => {
                        n.file = Some(NoteFile { name, hash, synced: updated });
                        let _ = self.store.put_note(&n);
                        self.notes_fs.unavailable = None;
                    }
                    // Se borró mientras se escribía: el archivo sobra.
                    None => self.send_notes(Cmd::Remove { name, base: Some(hash) }),
                }
            }
            Evt::Conflict { id, name, content, hash } => {
                self.notes_fs.inflight.remove(&id);
                if let Some(n) = self.store.get_note(id).ok().flatten() {
                    if n.file.as_ref().is_some_and(|f| f.hash == hash) {
                        // Ya se resolvió al leer el cambio de fuera; se vuelve a intentar.
                        self.notes_reconcile();
                    } else {
                        self.note_conflict(n, name, &content, hash);
                    }
                }
            }
            Evt::WriteFailed { id, error } => {
                self.notes_fs.inflight.remove(&id);
                if self.notes_fs.unavailable.is_none() {
                    self.notices.push(Notice::error(Text::new(
                        format!("The note could not be saved in the notes folder ({error}). It is safe here and will be written when the folder is available."),
                        format!("La nota no se pudo guardar en la carpeta de notas ({error}). Está a salvo aquí y se escribirá cuando la carpeta esté disponible."),
                    )));
                }
                self.notes_fs.unavailable = Some(error);
            }
            Evt::Scanned { settled } => {
                if self.notes_fs.unavailable.take().is_some() {
                    self.data_gen = self.data_gen.wrapping_add(1);
                }
                if settled || self.notes_fs.ready {
                    self.notes_fs.ready = true;
                    self.notes_reconcile();
                }
            }
            Evt::Unavailable(why) => {
                self.notes_fs.unavailable = Some(why);
                self.data_gen = self.data_gen.wrapping_add(1);
            }
            Evt::Flushed(_) => {}
        }
    }

    fn send_notes(&self, c: Cmd) {
        if let Some(w) = &self.notes_fs.worker {
            w.send(c);
        }
    }

    fn apply_fs_event(&mut self, ev: FsEvent) {
        let notes = self.store.notes().unwrap_or_default();
        let by_file = |name: &str| notes.iter().find(|n| n.file.as_ref().is_some_and(|f| f.name == name)).cloned();
        let now = self.now();
        match ev {
            FsEvent::Changed { name, content, hash } => {
                if let Some(mut n) = by_file(&name) {
                    if n.file.as_ref().is_some_and(|f| f.hash == hash) {
                        return;
                    }
                    if is_dirty(&n) {
                        self.note_conflict(n, name, &content, hash);
                        return;
                    }
                    let p = notes_fs::parse(&content);
                    n.title = clip(&p.title, super::notes::MAX_NOTE_TITLE);
                    n.body = clip(&p.body, super::notes::MAX_NOTE_BODY);
                    n.tags = p.tags;
                    n.pinned = p.pinned;
                    n.updated = self.note_stamp(n.updated);
                    n.file = Some(NoteFile { name, hash, synced: n.updated });
                    if self.store.put_note(&n).is_ok() {
                        self.data_gen = self.data_gen.wrapping_add(1);
                        let t = note_title(&n);
                        self.notices.push(Notice {
                            kind: NoticeKind::Info,
                            text: Text::new(
                                format!("\"{t}\" updated from IureEditor"),
                                format!("«{t}» actualizada desde IureEditor"),
                            ),
                            undo: false,
                            secs: Some(3),
                        });
                    }
                    return;
                }
                let p = notes_fs::parse(&content);
                // ¿Ya está como nota sin archivo (primera vez, otra carpeta)? Se enlaza.
                let twin = notes.iter().find(|n| {
                    n.file.is_none() && n.title.trim() == p.title.trim() && n.body.trim_end() == p.body.trim_end()
                });
                let mut n = match twin {
                    Some(t) => t.clone(),
                    None => Note {
                        title: clip(&p.title, super::notes::MAX_NOTE_TITLE),
                        body: clip(&p.body, super::notes::MAX_NOTE_BODY),
                        tags: p.tags,
                        pinned: p.pinned,
                        created: now,
                        updated: now,
                        ..Default::default()
                    },
                };
                n.file = Some(NoteFile { name, hash, synced: n.updated });
                if self.store.put_note(&n).is_ok() {
                    self.data_gen = self.data_gen.wrapping_add(1);
                }
            }
            FsEvent::Renamed { from, to } => {
                if let Some(mut n) = by_file(&from)
                    && let Some(f) = n.file.as_mut()
                {
                    f.name = to;
                    let _ = self.store.put_note(&n);
                    self.data_gen = self.data_gen.wrapping_add(1);
                }
            }
            FsEvent::Removed { name } => {
                let Some(mut n) = by_file(&name) else { return };
                if is_dirty(&n) {
                    // Hay cambios de aquí sin escribir: se vuelve a crear el archivo.
                    n.file = None;
                    let _ = self.store.put_note(&n);
                    self.notes_reconcile();
                    return;
                }
                if self.store.delete_note(n.id).is_err() {
                    return;
                }
                if self.note_focus == Some(n.id) {
                    self.note_focus = None;
                }
                self.data_gen = self.data_gen.wrapping_add(1);
                let t = note_title(&n);
                let text = Text::new(
                    format!("\"{t}\" was deleted from the notes folder"),
                    format!("«{t}» se eliminó de la carpeta de notas"),
                );
                n.file = None;
                self.push_undo(text.clone(), vec![(Inverse::Note(Box::new(NoteUndo::Deleted(Box::new(n)))), None)]);
                self.notices.push(Notice { kind: NoticeKind::Info, text, undo: true, secs: None });
            }
        }
    }

    /// Cambió en los dos lados: la nota toma lo del archivo y lo de aquí queda en una copia.
    fn note_conflict(&mut self, mut n: Note, name: String, content: &str, hash: String) {
        let now = self.now();
        let t = note_title(&n);
        let copy_title = format!("{t} ({})", pick("conflicting copy", "copia en conflicto"));
        let copy = Note {
            title: clip(&copy_title, super::notes::MAX_NOTE_TITLE),
            body: n.body.clone(),
            tags: n.tags.clone(),
            created: now,
            updated: now,
            ..Default::default()
        };
        let Ok(copy_id) = self.store.put_note(&copy) else { return };
        self.notes_fs.copies.insert(n.id, copy_id);
        let p = notes_fs::parse(content);
        n.title = clip(&p.title, super::notes::MAX_NOTE_TITLE);
        n.body = clip(&p.body, super::notes::MAX_NOTE_BODY);
        n.tags = p.tags;
        n.pinned = p.pinned;
        n.updated = self.note_stamp(n.updated);
        n.file = Some(NoteFile { name, hash, synced: n.updated });
        let _ = self.store.put_note(&n);
        self.data_gen = self.data_gen.wrapping_add(1);
        self.notices.push(Notice {
            kind: NoticeKind::Info,
            text: Text::new(
                format!("\"{t}\" changed in IureEditor and here at the same time. Both were kept: the note has the IureEditor version and \"{copy_title}\" has yours."),
                format!("«{t}» cambió en IureEditor y aquí a la vez. Se conservaron las dos: la nota tiene la versión de IureEditor y «{copy_title}», la tuya."),
            ),
            undo: false,
            secs: Some(30),
        });
        self.notes_reconcile();
    }

    /// Escribe las notas con cambios sin escribir (o que aún no tienen archivo).
    pub(crate) fn notes_reconcile(&mut self) {
        if self.notes_fs.worker.is_none() || !self.notes_fs.ready {
            return;
        }
        for n in self.store.notes().unwrap_or_default().into_iter().take(10_000) {
            if !is_dirty(&n) || !has_text(&n) || self.notes_fs.inflight.contains(&n.id) {
                continue;
            }
            self.notes_fs.inflight.insert(n.id);
            let cmd = Cmd::Write {
                id: n.id,
                current: n.file.as_ref().map(|f| f.name.clone()),
                stem: notes_fs::file_stem(&note_title(&n)),
                content: render_note(&n),
                base: n.file.as_ref().map(|f| f.hash.clone()),
                updated: n.updated,
            };
            self.send_notes(cmd);
        }
    }

    /// Una nota se borró aquí: su archivo también (si no cambió fuera).
    pub(crate) fn note_file_removed(&mut self, n: &Note) {
        if let Some(f) = &n.file {
            self.send_notes(Cmd::Remove { name: f.name.clone(), base: Some(f.hash.clone()) });
        }
    }

    /// Guarda desde el editor de la app. `base`: la fecha de la nota cuando se cargó en el
    /// editor; si la nota cambió desde entonces (llegó lo de IureEditor), lo escrito aquí se
    /// guarda en la copia en conflicto en vez de pisar lo de IureEditor.
    pub fn save_note_from(
        &mut self,
        id: NoteId,
        title: &str,
        body: &str,
        base: Option<DateTime<Utc>>,
    ) -> Result<NoteSaved> {
        let n = self.store.get_note(id).map_err(store_err)?.ok_or(crate::EngineError::NoSelection)?;
        let (title, body) = (clip(title, super::notes::MAX_NOTE_TITLE), clip(body, super::notes::MAX_NOTE_BODY));
        if base.is_some_and(|b| b != n.updated) && (n.title != title || n.body != body) {
            let now = self.now();
            let copy_id = self.notes_fs.copies.get(&id).copied();
            let existing = copy_id.and_then(|c| self.store.get_note(c).ok().flatten());
            let copy = match existing {
                Some(mut c) => {
                    c.body = body;
                    c.updated = now;
                    c
                }
                None => {
                    let t = if title.trim().is_empty() { note_title(&n) } else { title.clone() };
                    Note {
                        title: format!("{t} ({})", pick("conflicting copy", "copia en conflicto")),
                        body,
                        created: now,
                        updated: now,
                        ..Default::default()
                    }
                }
            };
            let cid = self.store.put_note(&copy).map_err(store_err)?;
            self.notes_fs.copies.insert(id, cid);
            self.data_gen = self.data_gen.wrapping_add(1);
            self.notes_reconcile();
            return Ok(NoteSaved::Copied(cid));
        }
        self.save_note(id, &title, &body)?;
        let updated = self.store.get_note(id).ok().flatten().map_or(n.updated, |x| x.updated);
        Ok(NoteSaved::Saved(updated))
    }

    /// Ruta del archivo de una nota, ya escrito (para abrirlo en IureEditor). Espera un momento
    /// si aún no se escribió.
    pub fn note_path(&mut self, id: NoteId) -> Option<PathBuf> {
        let dir = self.notes_dir()?;
        let n = self.store.get_note(id).ok().flatten()?;
        if is_dirty(&n) {
            self.notes_fs.ready = true;
            self.notes_fs_flush(Duration::from_secs(3));
        }
        let n = self.store.get_note(id).ok().flatten()?;
        let f = n.file.filter(|f| f.synced >= n.updated)?;
        Some(dir.join(f.name))
    }
}

fn clip(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => s.get(..i).unwrap_or(s).to_string(),
        None => s.to_string(),
    }
}
