//! Tareas en vivo (F7.1): las ediciones van por `pim-sync` (local primero + cola), así llegan a
//! CalDAV `VTODO`, Microsoft To Do o Google Tasks. Las listas solo locales y la de Iurefficient
//! (que tiene su propio camino por `connect`) se escriben directo en el almacén.

use iuredesk_model::{Task, TaskId, TaskListId};
use iuredesk_store::PimRef;

use super::{Inverse, Notice, NoticeKind, Outcome, Session, store_err};
use crate::lang::Text;
use crate::{EngineError, Result};

/// Largo máximo del título de una tarea nueva.
const MAX_TASK_TITLE: usize = 1_000;

impl Session {
    /// ¿La lista se sincroniza por `pim-sync`?
    fn task_list_synced(&self, list: TaskListId) -> bool {
        !self.is_iure_task(list) && self.store.get_task_list(list).ok().flatten().is_some_and(|l| l.account.is_some())
    }

    /// Guarda una tarea por el camino que le toca. Devuelve su id.
    pub(crate) fn write_task(&mut self, t: &Task) -> Result<TaskId> {
        if !self.task_list_synced(t.list) {
            return self.store.put_task(t).map_err(store_err);
        }
        let r = match &self.pim.handle {
            Some(h) => h.save_task(t),
            None => iuredesk_pim_sync::ops::save_task(&self.store, t),
        };
        r.map_err(|e| EngineError::Store(e.to_string()))
    }

    /// Deshacer: deja la tarea como estaba.
    pub(crate) fn restore_task(&mut self, t: &Task) -> Result<()> {
        self.write_task(t).map(|_| ())
    }

    /// Tarea nueva en `list` (con deshacer).
    pub fn add_task(&mut self, list: TaskListId, summary: &str) -> Result<Outcome> {
        let summary: String = summary.trim().chars().take(MAX_TASK_TITLE).collect();
        if summary.is_empty() {
            return Ok(Outcome::Done);
        }
        if self.store.get_task_list(list).map_err(store_err)?.is_none() {
            return Err(EngineError::Invalid("lista inexistente".into()));
        }
        let t = Task { list, summary, created: Some(self.now()), ..Default::default() };
        let id = self.write_task(&t)?;
        self.data_gen = self.data_gen.wrapping_add(1);
        self.push_undo(Text::new("Task added", "Tarea agregada"), vec![(Inverse::TaskAdded(id), None)]);
        Ok(Outcome::Done)
    }

    /// Borra una tarea (con deshacer).
    pub fn delete_task(&mut self, id: TaskId) -> Result<Outcome> {
        let t = self.store.get_task(id).map_err(store_err)?.ok_or(EngineError::NoSelection)?;
        self.remove_task(&t)?;
        self.data_gen = self.data_gen.wrapping_add(1);
        let text = Text::new(format!("Task \"{}\" deleted", t.summary), format!("Tarea «{}» eliminada", t.summary));
        self.push_undo(text.clone(), vec![(Inverse::TaskDeleted(Box::new(t)), None)]);
        Ok(Outcome::Notice(Notice { kind: NoticeKind::Success, text, undo: true, secs: None }))
    }

    fn remove_task(&mut self, t: &Task) -> Result<()> {
        if !self.task_list_synced(t.list) {
            return self.store.delete_task(t.id).map_err(store_err);
        }
        let r = match &self.pim.handle {
            Some(h) => h.delete_task(t.id),
            None => iuredesk_pim_sync::ops::delete_task(&self.store, t.id),
        };
        r.map_err(|e| EngineError::Store(e.to_string()))
    }

    /// Deshacer un alta.
    pub(crate) fn undo_task_added(&mut self, id: TaskId) -> Result<()> {
        if let Some(t) = self.store.get_task(id).map_err(store_err)? {
            self.remove_task(&t)?;
        }
        self.data_gen = self.data_gen.wrapping_add(1);
        Ok(())
    }

    /// Deshacer un borrado: vuelve como tarea nueva (el servidor ya pudo borrarla).
    pub(crate) fn undo_task_deleted(&mut self, mut t: Task) -> Result<()> {
        let _ = self.store.delete_raw_object(PimRef::Task(t.id));
        t.remote_id = None;
        t.etag = None;
        self.write_task(&t)?;
        self.data_gen = self.data_gen.wrapping_add(1);
        Ok(())
    }

    /// Cuenta de una lista de tareas para la insignia («Google», el correo de la cuenta…).
    pub fn task_list_badge(&self, list: TaskListId) -> Option<String> {
        if self.is_iure_task(list) {
            return Some("Iurefficient".into());
        }
        let l = self.store.get_task_list(list).ok().flatten()?;
        let acct = l.account?;
        let label = self.labels.iter().find(|x| x.id == acct)?;
        let google = l.remote_path.starts_with(iuredesk_pim_sync::GOOGLE_TASKS_LIST_PREFIX);
        Some(if google { format!("Google · {}", label.email) } else { label.email.clone() })
    }
}
