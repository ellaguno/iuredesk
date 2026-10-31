//! Migraciones del esquema de redb (ver `MIGRATIONS` en `lib.rs`).
//!
//! postcard no admite campos nuevos al final de un registro: cuando un tipo crece, la migración
//! lee los registros con la forma vieja (copiada aquí, congelada) y los vuelve a escribir.

use chrono::{DateTime, Utc};
use iuredesk_model::{AccountId, EventTime, Note, NoteId, Task, TaskId, TaskListId, TaskStatus};
use redb::{ReadableTable, WriteTransaction};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::tables::{NOTES, TASKS};
use crate::{codec, put_rec};

/// `Task` del esquema 1 (antes de UX-6).
#[derive(Serialize, Deserialize)]
pub(crate) struct TaskV1 {
    pub id: TaskId,
    pub list: TaskListId,
    pub uid: String,
    pub summary: String,
    pub description: String,
    pub due: Option<EventTime>,
    pub priority: u8,
    pub status: TaskStatus,
    pub percent_complete: u8,
    pub completed: Option<DateTime<Utc>>,
    pub created: Option<DateTime<Utc>>,
    pub updated: Option<DateTime<Utc>>,
    pub remote_id: Option<String>,
    pub etag: Option<String>,
}

/// `Note` del esquema 1 (antes de UX-6).
#[derive(Serialize, Deserialize)]
pub(crate) struct NoteV1 {
    pub id: NoteId,
    pub account: Option<AccountId>,
    pub title: String,
    pub body: String,
    pub tags: Vec<String>,
    pub pinned: bool,
    pub created: DateTime<Utc>,
    pub updated: DateTime<Utc>,
    pub remote_id: Option<String>,
}

/// 1 → 2 (UX-6): `Task` gana inicio, recordatorio y repetición; `Note` gana su archivo `.md`.
/// Un registro ilegible se deja como está (una migración nunca borra nada).
pub(crate) fn v1_to_v2(tx: &WriteTransaction) -> Result<()> {
    {
        let mut t = tx.open_table(TASKS)?;
        let mut out = Vec::new();
        for row in t.iter()? {
            let (k, v) = row?;
            if let Ok(o) = codec::decode::<TaskV1>(v.value()) {
                let new = Task {
                    id: o.id,
                    list: o.list,
                    uid: o.uid,
                    summary: o.summary,
                    description: o.description,
                    due: o.due,
                    priority: o.priority,
                    status: o.status,
                    percent_complete: o.percent_complete,
                    completed: o.completed,
                    created: o.created,
                    updated: o.updated,
                    remote_id: o.remote_id,
                    etag: o.etag,
                    ..Default::default()
                };
                out.push((k.value(), new));
            }
        }
        for (k, v) in out {
            put_rec(&mut t, k, &v)?;
        }
    }
    {
        let mut t = tx.open_table(NOTES)?;
        let mut out = Vec::new();
        for row in t.iter()? {
            let (k, v) = row?;
            if let Ok(o) = codec::decode::<NoteV1>(v.value()) {
                let new = Note {
                    id: o.id,
                    account: o.account,
                    title: o.title,
                    body: o.body,
                    tags: o.tags,
                    pinned: o.pinned,
                    created: o.created,
                    updated: o.updated,
                    remote_id: o.remote_id,
                    file: None,
                };
                out.push((k.value(), new));
            }
        }
        for (k, v) in out {
            put_rec(&mut t, k, &v)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;
    use crate::Store;

    /// Un almacén del esquema 1 con tareas y notas se abre con todo intacto.
    #[test]
    fn migrates_tasks_and_notes_from_v1() {
        let dir = tempfile::tempdir().unwrap();
        let at = Utc.with_ymd_and_hms(2026, 10, 1, 9, 0, 0).unwrap();
        {
            let s = Store::open(dir.path()).unwrap();
            let tx = s.db.begin_write().unwrap();
            {
                let mut t = tx.open_table(TASKS).unwrap();
                let old = TaskV1 {
                    id: TaskId(7),
                    list: TaskListId(3),
                    uid: "u".into(),
                    summary: "Pagar".into(),
                    description: "renta".into(),
                    due: Some(EventTime::Date(at.date_naive())),
                    priority: 1,
                    status: TaskStatus::NeedsAction,
                    percent_complete: 0,
                    completed: None,
                    created: Some(at),
                    updated: Some(at),
                    remote_id: Some("r".into()),
                    etag: None,
                };
                put_rec(&mut t, 7, &old).unwrap();
                let mut n = tx.open_table(NOTES).unwrap();
                let old = NoteV1 {
                    id: NoteId(4),
                    account: None,
                    title: "Ideas".into(),
                    body: "# Hola".into(),
                    tags: vec!["x".into()],
                    pinned: true,
                    created: at,
                    updated: at,
                    remote_id: None,
                };
                put_rec(&mut n, 4, &old).unwrap();
                let mut meta = tx.open_table(crate::tables::META).unwrap();
                meta.insert(crate::META_SCHEMA, 1u32.to_le_bytes().as_slice()).unwrap();
            }
            tx.commit().unwrap();
        }
        let s = Store::open(dir.path()).unwrap();
        let t = s.get_task(TaskId(7)).unwrap().unwrap();
        assert_eq!((t.summary.as_str(), t.priority, t.start, t.rrule.as_str()), ("Pagar", 1, None, ""));
        let n = s.get_note(NoteId(4)).unwrap().unwrap();
        assert_eq!((n.title.as_str(), n.pinned, n.file), ("Ideas", true, None));
    }
}
