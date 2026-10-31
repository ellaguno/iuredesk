//! Tareas en vivo (F7.1): las ediciones van por `pim-sync` (local primero + cola), así llegan a
//! CalDAV `VTODO`, Microsoft To Do o Google Tasks. Las listas solo locales y la de Iurefficient
//! (que tiene su propio camino por `connect`) se escriben directo en el almacén.

use iuredesk_model::{EventTime, Task, TaskId, TaskList, TaskListId, TaskStatus};
use iuredesk_store::PimRef;

use super::{Inverse, Notice, NoticeKind, Outcome, Session, store_err};
use crate::lang::Text;
use crate::{EngineError, Result};

/// Largo máximo del título de una tarea nueva.
const MAX_TASK_TITLE: usize = 1_000;
/// Largo máximo de las notas de una tarea.
const MAX_TASK_NOTES: usize = 64 * 1024;

/// De dónde es una lista de tareas (UX-6: qué se puede editar).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TaskSource {
    /// Solo en este equipo.
    Local,
    /// CalDAV `VTODO` (iCloud, Fastmail, Nextcloud…).
    CalDav,
    /// Microsoft To Do (Graph).
    MicrosoftToDo,
    /// Google Tasks (`tasks/v1`).
    GoogleTasks,
    /// Tareas de Iurefficient (por `connect`).
    Iurefficient,
}

impl TaskSource {
    /// Nombre para los avisos («Google Tasks no guarda…»).
    pub fn label(self) -> &'static str {
        match self {
            TaskSource::Local => crate::lang::pick("This computer", "Este equipo"),
            TaskSource::CalDav => "CalDAV",
            TaskSource::MicrosoftToDo => "Microsoft To Do",
            TaskSource::GoogleTasks => "Google Tasks",
            TaskSource::Iurefficient => "Iurefficient",
        }
    }
}

/// Campos que la fuente de una lista sabe guardar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TaskCaps {
    pub source: TaskSource,
    /// Se puede cambiar desde aquí (Iurefficient: con sesión).
    pub editable: bool,
    /// Vencimiento con hora (si no, solo la fecha).
    pub due_time: bool,
    pub start: bool,
    pub priority: bool,
    pub reminder: bool,
    pub recurrence: bool,
    /// Mover a otra lista de la misma cuenta.
    pub move_list: bool,
}

impl Session {
    /// ¿La lista se sincroniza por `pim-sync`?
    fn task_list_synced(&self, list: TaskListId) -> bool {
        !self.is_iure_task(list) && self.store.get_task_list(list).ok().flatten().is_some_and(|l| l.account.is_some())
    }

    /// Guarda una tarea por el camino que le toca. Devuelve su id.
    pub(crate) fn write_task(&mut self, t: &Task) -> Result<TaskId> {
        if self.is_iure_task(t.list) {
            let old = self.store.get_task(t.id).map_err(store_err)?;
            let id = self.store.put_task(t).map_err(store_err)?;
            if let Some(old) = old {
                self.iure_task_changed(&old, t);
            }
            return Ok(id);
        }
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

    /// Una tarea (UX-6: el editor la abre por id).
    pub fn task(&self, id: TaskId) -> Option<Task> {
        self.store.get_task(id).ok().flatten()
    }

    /// De dónde es una lista de tareas.
    pub fn task_source(&self, list: TaskListId) -> TaskSource {
        if self.is_iure_task(list) {
            return TaskSource::Iurefficient;
        }
        let Some(l) = self.store.get_task_list(list).ok().flatten() else { return TaskSource::Local };
        if l.account.is_none() || l.remote_path.is_empty() {
            return TaskSource::Local;
        }
        let p = l.remote_path.as_str();
        if p.starts_with(iuredesk_pim_sync::GOOGLE_TASKS_LIST_PREFIX) {
            TaskSource::GoogleTasks
        } else if p.starts_with('/') || p.contains("://") {
            TaskSource::CalDav
        } else {
            TaskSource::MicrosoftToDo
        }
    }

    /// Qué campos puede guardar la fuente de una lista (lo demás se ve deshabilitado, con el
    /// motivo).
    pub fn task_caps(&self, list: TaskListId) -> TaskCaps {
        let source = self.task_source(list);
        let all = TaskCaps {
            source,
            editable: true,
            due_time: true,
            start: true,
            priority: true,
            reminder: true,
            recurrence: true,
            move_list: true,
        };
        match source {
            TaskSource::Local => all,
            TaskSource::CalDav => all,
            TaskSource::MicrosoftToDo => TaskCaps { due_time: false, ..all },
            TaskSource::GoogleTasks => {
                TaskCaps { due_time: false, start: false, priority: false, reminder: false, recurrence: false, ..all }
            }
            TaskSource::Iurefficient => TaskCaps {
                editable: self.iure_can_write(),
                due_time: false,
                reminder: false,
                recurrence: false,
                move_list: false,
                ..all
            },
        }
    }

    /// Listas a las que se puede mover una tarea: las de la misma cuenta y el mismo tipo.
    pub fn task_move_targets(&self, list: TaskListId) -> Vec<TaskList> {
        let caps = self.task_caps(list);
        let Some(cur) = self.store.get_task_list(list).ok().flatten() else { return Vec::new() };
        if !caps.move_list {
            return vec![cur];
        }
        self.task_lists()
            .into_iter()
            .filter(|l| l.id == list || (l.account == cur.account && self.task_source(l.id) == caps.source))
            .collect()
    }

    /// Guarda lo editado en el editor de tareas (con deshacer). Lo que la fuente no admite se deja
    /// como estaba; cambiar de lista mueve la tarea (borrar + crear en la otra).
    pub fn save_task_edit(&mut self, edited: Task) -> Result<Outcome> {
        let old = self.store.get_task(edited.id).map_err(store_err)?.ok_or(EngineError::NoSelection)?;
        let caps = self.task_caps(old.list);
        if !caps.editable {
            return Ok(Outcome::Notice(Notice::error(Text::new(
                "Iurefficient is not connected: change this task there.",
                "Iurefficient no está conectado: cambia esta tarea allá.",
            ))));
        }
        let mut new = edited;
        new.summary = new.summary.trim().chars().take(MAX_TASK_TITLE).collect();
        if new.summary.is_empty() {
            return Ok(Outcome::Notice(Notice::error(Text::new(
                "A task needs a title.",
                "Una tarea necesita título.",
            ))));
        }
        new.description = new.description.chars().take(MAX_TASK_NOTES).collect();
        new.rrule = new.rrule.trim().trim_start_matches("RRULE:").chars().take(1_024).collect();
        let day = |d: &Option<EventTime>| d.as_ref().map(iuredesk_pim_sync::map::ical::date_of);
        if !caps.due_time {
            new.due = day(&new.due).map(EventTime::Date);
            if day(&old.due) == day(&new.due) {
                new.due = old.due.clone();
            }
        }
        if !caps.start {
            new.start = old.start.clone();
        }
        if !caps.priority {
            new.priority = old.priority;
        }
        if !caps.reminder {
            new.reminder = old.reminder;
        }
        if !caps.recurrence {
            new.rrule = old.rrule.clone();
        }
        let targets = self.task_move_targets(old.list);
        if !targets.iter().any(|l| l.id == new.list) {
            new.list = old.list;
        }
        // Terminada / pendiente.
        let done = new.status == TaskStatus::Completed || new.completed.is_some();
        if done && !old.is_done() {
            new.status = TaskStatus::Completed;
            new.completed = Some(self.now());
            new.percent_complete = 100;
        } else if !done && old.is_done() {
            new.status = TaskStatus::NeedsAction;
            new.completed = None;
            new.percent_complete = 0;
        }
        // Lo que no se edita aquí no cambia.
        new.uid = old.uid.clone();
        new.remote_id = old.remote_id.clone();
        new.etag = old.etag.clone();
        new.created = old.created;
        new.updated = old.updated;
        if new == old {
            return Ok(Outcome::Done);
        }
        if new.list != old.list {
            self.remove_task(&old)?;
            new.id = TaskId::UNASSIGNED;
            new.remote_id = None;
            new.etag = None;
            if matches!(caps.source, TaskSource::GoogleTasks | TaskSource::MicrosoftToDo) {
                // El `UID` de esas fuentes es el id remoto: la copia recibe el suyo.
                new.uid = String::new();
            }
            let id = self.write_task(&new)?;
            self.data_gen = self.data_gen.wrapping_add(1);
            let moved = Text::new(format!("Task \"{}\" moved", new.summary), format!("Tarea «{}» movida", new.summary));
            self.push_undo(
                moved.clone(),
                vec![(Inverse::TaskAdded(id), None), (Inverse::TaskDeleted(Box::new(old)), None)],
            );
            return Ok(Outcome::Notice(Notice { kind: NoticeKind::Success, text: moved, undo: true, secs: None }));
        }
        let text = Text::new(format!("Task \"{}\" saved", new.summary), format!("Tarea «{}» guardada", new.summary));
        self.write_task(&new)?;
        self.data_gen = self.data_gen.wrapping_add(1);
        self.push_undo(text.clone(), vec![(Inverse::Task(Box::new(old)), None)]);
        Ok(Outcome::Notice(Notice { kind: NoticeKind::Success, text, undo: true, secs: None }))
    }

    /// Cambia solo el título (edición rápida en la lista, F2).
    pub fn rename_task(&mut self, id: TaskId, title: &str) -> Result<Outcome> {
        let Some(mut t) = self.store.get_task(id).map_err(store_err)? else { return Ok(Outcome::Done) };
        t.summary = title.to_string();
        self.save_task_edit(t)
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
