//! Notas (F7.1): crear, editar (Markdown, guardado automático), buscar y borrar con deshacer.
//! UX-6: cada nota es también un archivo Markdown en la carpeta de notas (ver `notes_files`).

use iuredesk_model::{Note, NoteId};

use super::{Inverse, Notice, NoticeKind, Outcome, Session, store_err};
use crate::lang::Text;
use crate::{EngineError, Result};

/// Largo máximo del título y del cuerpo de una nota.
pub const MAX_NOTE_TITLE: usize = 500;
pub const MAX_NOTE_BODY: usize = 2 * 1024 * 1024;

/// Deshacer de notas.
#[derive(Clone, Debug)]
pub(crate) enum NoteUndo {
    /// Se creó: se borra.
    Created(NoteId),
    /// Se borró: se devuelve tal cual.
    Deleted(Box<Note>),
}

fn clip(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => s.get(..i).unwrap_or(s).to_string(),
        None => s.to_string(),
    }
}

/// Título visible: el propio o, si está vacío, la primera línea con texto del cuerpo.
pub fn note_title(n: &Note) -> String {
    let t = n.title.trim();
    if !t.is_empty() {
        return t.to_string();
    }
    n.body
        .lines()
        .map(|l| l.trim().trim_start_matches(['#', '-', '*', '>', ' ']).trim())
        .find(|l| !l.is_empty())
        .map(|l| clip(l, 120))
        .unwrap_or_else(|| crate::lang::pick("Untitled note", "Nota sin título").to_string())
}

/// ¿La nota coincide con la búsqueda? Todas las palabras, sin distinguir mayúsculas ni acentos,
/// en el título, el cuerpo o las etiquetas.
pub fn note_matches(n: &Note, query: &str) -> bool {
    let words: Vec<String> = query.split_whitespace().take(16).map(crate::sort_key).collect();
    if words.is_empty() {
        return true;
    }
    let hay = crate::sort_key(&format!("{}\n{}\n{}", n.title, n.body, n.tags.join(" ")));
    words.iter().all(|w| hay.contains(w.as_str()))
}

impl Session {
    /// Nota enfocada en el módulo de notas.
    pub fn note_focus(&self) -> Option<NoteId> {
        self.note_focus
    }

    /// Enfoca una nota (o ninguna).
    pub fn set_note_focus(&mut self, id: Option<NoteId>) {
        self.note_focus = id;
    }

    /// Notas que coinciden con `query` (fijadas primero, luego las más recientes).
    pub fn notes_matching(&self, query: &str) -> Vec<Note> {
        self.notes().into_iter().filter(|n| note_matches(n, query)).collect()
    }

    /// Nota nueva vacía (con deshacer) y enfocada.
    pub fn new_note(&mut self) -> Result<Outcome> {
        let now = self.now();
        let note = Note { created: now, updated: now, ..Default::default() };
        let id = self.store.put_note(&note).map_err(store_err)?;
        self.note_focus = Some(id);
        self.module = super::Module::Notes;
        self.data_gen = self.data_gen.wrapping_add(1);
        self.push_undo(
            Text::new("Note created", "Nota creada"),
            vec![(Inverse::Note(Box::new(NoteUndo::Created(id))), None)],
        );
        Ok(Outcome::Done)
    }

    /// Guarda el título y el cuerpo (guardado automático del editor). No hace nada si no
    /// cambió.
    pub fn save_note(&mut self, id: NoteId, title: &str, body: &str) -> Result<()> {
        let mut n = self.store.get_note(id).map_err(store_err)?.ok_or(EngineError::NoSelection)?;
        let (title, body) = (clip(title, MAX_NOTE_TITLE), clip(body, MAX_NOTE_BODY));
        if n.title == title && n.body == body {
            return Ok(());
        }
        n.title = title;
        n.body = body;
        n.updated = self.note_stamp(n.updated);
        self.store.put_note(&n).map_err(store_err)?;
        self.data_gen = self.data_gen.wrapping_add(1);
        self.notes_reconcile();
        Ok(())
    }

    /// Fecha de modificación nueva, siempre posterior a la anterior (la carpeta de notas compara
    /// fechas para saber qué falta escribir).
    pub(crate) fn note_stamp(&self, prev: chrono::DateTime<chrono::Utc>) -> chrono::DateTime<chrono::Utc> {
        let now = self.now();
        if now > prev { now } else { prev + chrono::Duration::milliseconds(1) }
    }

    /// Fija o suelta una nota arriba de la lista.
    pub fn toggle_note_pin(&mut self, id: NoteId) -> Result<()> {
        let mut n = self.store.get_note(id).map_err(store_err)?.ok_or(EngineError::NoSelection)?;
        n.pinned = !n.pinned;
        // Va en el archivo (bloque YAML): hay que reescribirlo.
        n.updated = self.note_stamp(n.updated);
        self.store.put_note(&n).map_err(store_err)?;
        self.data_gen = self.data_gen.wrapping_add(1);
        self.notes_reconcile();
        Ok(())
    }

    /// Borra una nota (con deshacer).
    pub fn delete_note(&mut self, id: NoteId) -> Result<Outcome> {
        let n = self.store.get_note(id).map_err(store_err)?.ok_or(EngineError::NoSelection)?;
        self.store.delete_note(id).map_err(store_err)?;
        self.note_file_removed(&n);
        if self.note_focus == Some(id) {
            self.note_focus = None;
        }
        self.data_gen = self.data_gen.wrapping_add(1);
        let title = note_title(&n);
        let text = Text::new(format!("Note \"{title}\" deleted"), format!("Nota «{title}» eliminada"));
        self.push_undo(text.clone(), vec![(Inverse::Note(Box::new(NoteUndo::Deleted(Box::new(n)))), None)]);
        Ok(Outcome::Notice(Notice { kind: NoticeKind::Success, text, undo: true, secs: None }))
    }

    /// `notes.delete[:<id>]`.
    pub(crate) fn run_delete_note(&mut self, arg: Option<&str>) -> Result<Outcome> {
        let id = arg.and_then(|a| a.trim().parse::<u64>().ok()).map(NoteId).or(self.note_focus);
        match id {
            Some(id) => self.delete_note(id),
            None => Ok(Outcome::Notice(Notice::info("Select a note first.", "Primero selecciona una nota."))),
        }
    }

    pub(crate) fn undo_note(&mut self, u: NoteUndo) -> Result<()> {
        match u {
            NoteUndo::Created(id) => {
                if let Some(n) = self.store.get_note(id).map_err(store_err)? {
                    self.note_file_removed(&n);
                }
                self.store.delete_note(id).map_err(store_err)?;
                if self.note_focus == Some(id) {
                    self.note_focus = None;
                }
            }
            NoteUndo::Deleted(mut n) => {
                // El archivo se borró con la nota: se vuelve a escribir.
                n.file = None;
                let id = self.store.put_note(&n).map_err(store_err)?;
                self.note_focus = Some(id);
            }
        }
        self.data_gen = self.data_gen.wrapping_add(1);
        self.notes_reconcile();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_and_search() {
        let n = Note { body: "\n# Minuta con el **cliente**\n- acuerdos".into(), ..Default::default() };
        assert_eq!(note_title(&n), "Minuta con el **cliente**");
        let n2 = Note { title: "Ideas".into(), body: "Revisar la demanda de Ramírez".into(), ..Default::default() };
        assert_eq!(note_title(&n2), "Ideas");
        assert!(note_matches(&n2, "ramirez DEMANDA"));
        assert!(note_matches(&n2, "ideas"));
        assert!(!note_matches(&n2, "ramirez contrato"));
        assert!(note_matches(&n2, "   "));
        assert_eq!(clip("añoñoño", 3), "año");
    }
}
