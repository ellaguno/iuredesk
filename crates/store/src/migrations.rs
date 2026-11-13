//! Migraciones del esquema de redb (ver `MIGRATIONS` en `lib.rs`).
//!
//! postcard no admite campos nuevos al final de un registro: cuando un tipo crece, la migración
//! lee los registros con la forma vieja (copiada aquí, congelada) y los vuelve a escribir.

use chrono::{DateTime, Utc};
use iuredesk_model::{
    AccountId, Attendee, CalendarId, Event, EventId, EventStatus, EventTime, Mailbox, Note, NoteId, OnlineMeeting,
    Reminder, Task, TaskId, TaskListId, TaskStatus, Transparency,
};
use redb::{ReadableTable, WriteTransaction};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::tables::{EVENTS, NOTES, SYNC_STATE, TASKS};
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

/// `Event` de los esquemas 1 y 2 (antes de UX-11).
#[derive(Serialize, Deserialize)]
pub(crate) struct EventV2 {
    pub id: EventId,
    pub calendar: CalendarId,
    pub uid: String,
    pub summary: String,
    pub location: String,
    pub description: String,
    pub start: EventTime,
    pub end: EventTime,
    pub rrule: Option<String>,
    pub exdates: Vec<String>,
    pub organizer: Option<Mailbox>,
    pub attendees: Vec<Attendee>,
    pub reminders: Vec<Reminder>,
    pub status: EventStatus,
    pub transparency: Transparency,
    pub sequence: u32,
    pub remote_id: Option<String>,
    pub etag: Option<String>,
    pub updated: Option<DateTime<Utc>>,
}

/// `Event` del esquema 3 (antes de R4.5).
#[derive(Serialize, Deserialize)]
pub(crate) struct EventV3 {
    pub id: EventId,
    pub calendar: CalendarId,
    pub uid: String,
    pub summary: String,
    pub location: String,
    pub description: String,
    pub start: EventTime,
    pub end: EventTime,
    pub rrule: Option<String>,
    pub exdates: Vec<String>,
    pub organizer: Option<Mailbox>,
    pub attendees: Vec<Attendee>,
    pub reminders: Vec<Reminder>,
    pub status: EventStatus,
    pub transparency: Transparency,
    pub sequence: u32,
    pub remote_id: Option<String>,
    pub etag: Option<String>,
    pub updated: Option<DateTime<Utc>>,
    pub online_meeting: Option<OnlineMeeting>,
}

/// Marca en `sync_state`: hay eventos migrados al esquema 4 cuyas excepciones siguen solo en el
/// objeto crudo. `iuredesk-pim-sync` las lee una vez al arrancar y borra la marca (la capa del
/// almacén no sabe leer iCalendar).
pub const OVERRIDES_BACKFILL_KEY: &str = "pim/overrides-backfill-v4";

/// 3 → 4 (R4.5): `Event` gana sus apariciones cambiadas (`overrides`, vacías al migrar; ver
/// [`OVERRIDES_BACKFILL_KEY`]).
pub(crate) fn v3_to_v4(tx: &WriteTransaction) -> Result<()> {
    let mut t = tx.open_table(EVENTS)?;
    let mut out = Vec::new();
    for row in t.iter()? {
        let (k, v) = row?;
        if let Ok(o) = codec::decode::<EventV3>(v.value()) {
            let new = Event {
                id: o.id,
                calendar: o.calendar,
                uid: o.uid,
                summary: o.summary,
                location: o.location,
                description: o.description,
                start: o.start,
                end: o.end,
                rrule: o.rrule,
                exdates: o.exdates,
                organizer: o.organizer,
                attendees: o.attendees,
                reminders: o.reminders,
                status: o.status,
                transparency: o.transparency,
                sequence: o.sequence,
                remote_id: o.remote_id,
                etag: o.etag,
                updated: o.updated,
                online_meeting: o.online_meeting,
                overrides: Vec::new(),
            };
            out.push((k.value(), new));
        }
    }
    let any = !out.is_empty();
    for (k, v) in out {
        put_rec(&mut t, k, &v)?;
    }
    if any {
        tx.open_table(SYNC_STATE)?.insert(OVERRIDES_BACKFILL_KEY, [1u8].as_slice())?;
    }
    Ok(())
}

/// 2 → 3 (UX-11): `Event` gana su reunión en línea (`online_meeting`, vacía al migrar: la próxima
/// sincronización la trae del servidor).
pub(crate) fn v2_to_v3(tx: &WriteTransaction) -> Result<()> {
    let mut t = tx.open_table(EVENTS)?;
    let mut out = Vec::new();
    for row in t.iter()? {
        let (k, v) = row?;
        if let Ok(o) = codec::decode::<EventV2>(v.value()) {
            let new = EventV3 {
                id: o.id,
                calendar: o.calendar,
                uid: o.uid,
                summary: o.summary,
                location: o.location,
                description: o.description,
                start: o.start,
                end: o.end,
                rrule: o.rrule,
                exdates: o.exdates,
                organizer: o.organizer,
                attendees: o.attendees,
                reminders: o.reminders,
                status: o.status,
                transparency: o.transparency,
                sequence: o.sequence,
                remote_id: o.remote_id,
                etag: o.etag,
                updated: o.updated,
                online_meeting: None,
            };
            out.push((k.value(), new));
        }
    }
    for (k, v) in out {
        put_rec(&mut t, k, &v)?;
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

    /// Un almacén del esquema 2 con eventos se abre con todo intacto y sin reunión en línea.
    #[test]
    fn migrates_events_from_v2() {
        let dir = tempfile::tempdir().unwrap();
        let at = Utc.with_ymd_and_hms(2026, 11, 3, 16, 0, 0).unwrap();
        {
            let s = Store::open(dir.path()).unwrap();
            let tx = s.db.begin_write().unwrap();
            {
                let mut t = tx.open_table(EVENTS).unwrap();
                let old = EventV2 {
                    id: EventId(9),
                    calendar: CalendarId(2),
                    uid: "ev-9".into(),
                    summary: "Revisión".into(),
                    location: "Sala 1".into(),
                    description: String::new(),
                    start: EventTime::DateTime { utc: at, tzid: None },
                    end: EventTime::DateTime { utc: at + chrono::Duration::hours(1), tzid: None },
                    rrule: Some("FREQ=WEEKLY".into()),
                    exdates: vec![],
                    organizer: Some(Mailbox { name: None, addr: "yo@x.com".into() }),
                    attendees: vec![],
                    reminders: vec![Reminder { minutes_before: 10 }],
                    status: EventStatus::Confirmed,
                    transparency: Transparency::Opaque,
                    sequence: 3,
                    remote_id: Some("r".into()),
                    etag: Some("e".into()),
                    updated: Some(at),
                };
                put_rec(&mut t, 9, &old).unwrap();
                let mut meta = tx.open_table(crate::tables::META).unwrap();
                meta.insert(crate::META_SCHEMA, 2u32.to_le_bytes().as_slice()).unwrap();
            }
            tx.commit().unwrap();
        }
        let s = Store::open(dir.path()).unwrap();
        let e = s.get_event(EventId(9)).unwrap().unwrap();
        assert_eq!((e.summary.as_str(), e.sequence, e.rrule.as_deref()), ("Revisión", 3, Some("FREQ=WEEKLY")));
        assert_eq!(e.reminders, vec![Reminder { minutes_before: 10 }]);
        assert_eq!(e.online_meeting, None);
    }

    /// R4.5: un evento del esquema 3 con reunión en línea se abre igual y sin apariciones cambiadas.
    #[test]
    fn migrates_events_from_v3() {
        let dir = tempfile::tempdir().unwrap();
        let at = Utc.with_ymd_and_hms(2026, 11, 2, 16, 0, 0).unwrap();
        {
            let s = Store::open(dir.path()).unwrap();
            let tx = s.db.begin_write().unwrap();
            {
                let mut t = tx.open_table(EVENTS).unwrap();
                let old = EventV3 {
                    id: EventId(5),
                    calendar: CalendarId(1),
                    uid: "serie".into(),
                    summary: "Diario".into(),
                    location: String::new(),
                    description: String::new(),
                    start: EventTime::DateTime { utc: at, tzid: Some("America/Mexico_City".into()) },
                    end: EventTime::DateTime { utc: at + chrono::Duration::hours(1), tzid: None },
                    rrule: Some("FREQ=DAILY".into()),
                    exdates: vec!["20261103T160000Z".into()],
                    organizer: None,
                    attendees: vec![],
                    reminders: vec![],
                    status: EventStatus::Confirmed,
                    transparency: Transparency::Opaque,
                    sequence: 1,
                    remote_id: None,
                    etag: None,
                    updated: Some(at),
                    online_meeting: Some(OnlineMeeting {
                        provider: iuredesk_model::MeetingProvider::Teams,
                        join_url: "https://teams.example/x".into(),
                    }),
                };
                put_rec(&mut t, 5, &old).unwrap();
                let mut meta = tx.open_table(crate::tables::META).unwrap();
                meta.insert(crate::META_SCHEMA, 3u32.to_le_bytes().as_slice()).unwrap();
            }
            tx.commit().unwrap();
        }
        let s = Store::open(dir.path()).unwrap();
        let e = s.get_event(EventId(5)).unwrap().unwrap();
        assert_eq!((e.summary.as_str(), e.exdates.len()), ("Diario", 1));
        assert_eq!(e.online_meeting.map(|m| m.join_url).as_deref(), Some("https://teams.example/x"));
        assert!(e.overrides.is_empty());
        assert_eq!(s.sync_state(OVERRIDES_BACKFILL_KEY).unwrap(), Some(vec![1]), "marca para leerlas del crudo");
    }
}
