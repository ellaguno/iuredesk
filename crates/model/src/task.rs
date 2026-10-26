//! Tareas (modelo cercano a `VTODO`, RFC 5545).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::calendar::EventTime;
use crate::ids::{AccountId, TaskId, TaskListId};

/// Lista de tareas (colección CalDAV con VTODO, lista de Microsoft To Do / Google Tasks, o local).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskList {
    /// Id local.
    pub id: TaskListId,
    /// Cuenta; `None` = solo local.
    pub account: Option<AccountId>,
    /// Nombre visible.
    pub name: String,
    /// Ruta o id remoto, opaco.
    pub remote_path: String,
    /// Estado de sincronización opaco.
    pub sync_token: Option<String>,
}

/// Estado de una tarea (`STATUS` de VTODO).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TaskStatus {
    /// Pendiente.
    #[default]
    NeedsAction,
    /// En curso.
    InProcess,
    /// Terminada.
    Completed,
    /// Cancelada.
    Cancelled,
}

/// Tarea.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    /// Id local.
    pub id: TaskId,
    /// Lista.
    pub list: TaskListId,
    /// `UID` de iCalendar.
    pub uid: String,
    /// Título.
    pub summary: String,
    /// Descripción.
    pub description: String,
    /// Vencimiento.
    pub due: Option<EventTime>,
    /// Prioridad iCalendar: 0 = sin definir, 1 = máxima … 9 = mínima.
    pub priority: u8,
    /// Estado.
    pub status: TaskStatus,
    /// Porcentaje completado (0–100).
    pub percent_complete: u8,
    /// Momento en que se completó.
    pub completed: Option<DateTime<Utc>>,
    /// Creación.
    pub created: Option<DateTime<Utc>>,
    /// Última modificación.
    pub updated: Option<DateTime<Utc>>,
    /// Id o `href` remoto, opaco.
    pub remote_id: Option<String>,
    /// `ETag` remoto, opaco.
    pub etag: Option<String>,
}

impl Task {
    /// `true` si está terminada.
    pub fn is_done(&self) -> bool {
        self.status == TaskStatus::Completed || self.completed.is_some()
    }
}
