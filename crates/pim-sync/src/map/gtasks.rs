//! Google Tasks API (`tasks/v1`) ⇄ [`Task`].
//!
//! Google guarda el vencimiento como fecha (la hora se descarta: `due` llega como
//! `AAAA-MM-DDT00:00:00.000Z`), no tiene prioridad ni porcentaje y solo dos estados
//! (`needsAction`, `completed`).

use chrono::{DateTime, NaiveDate, Utc};
use iuredesk_model::{EventTime, Task, TaskStatus};
use serde_json::{Map, Value, json};

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or_default()
}

fn instant(v: &Value, k: &str) -> Option<DateTime<Utc>> {
    v.get(k).and_then(Value::as_str).and_then(|t| DateTime::parse_from_rfc3339(t.trim()).ok()).map(|t| t.to_utc())
}

/// `ETag` de un objeto.
pub fn etag(v: &Value) -> Option<String> {
    v.get("etag").and_then(Value::as_str).map(str::to_string)
}

/// `true` si el objeto llega borrado (`deleted: true`, con `showDeleted`).
pub fn is_deleted(v: &Value) -> bool {
    v.get("deleted").and_then(Value::as_bool).unwrap_or(false)
}

/// Fecha `AAAA-MM-DD` del vencimiento (Google solo guarda la fecha).
fn due_date(v: &Value) -> Option<NaiveDate> {
    let raw = v.get("due").and_then(Value::as_str)?.trim();
    if let Ok(t) = DateTime::parse_from_rfc3339(raw) {
        return Some(t.to_utc().date_naive());
    }
    NaiveDate::parse_from_str(raw.get(..10)?, "%Y-%m-%d").ok()
}

/// Tarea de Google → modelo (sin `id` ni `list` locales).
pub fn task_from_google(v: &Value) -> Task {
    let done = s(v, "status") == "completed";
    let status = if done { TaskStatus::Completed } else { TaskStatus::NeedsAction };
    Task {
        uid: s(v, "id").chars().take(1_024).collect(),
        summary: s(v, "title").chars().take(4_096).collect(),
        description: s(v, "notes").chars().take(256 * 1024).collect(),
        due: due_date(v).map(EventTime::Date),
        priority: 0,
        status,
        percent_complete: if done { 100 } else { 0 },
        completed: if done { instant(v, "completed") } else { None },
        created: None,
        updated: instant(v, "updated"),
        remote_id: v.get("id").and_then(Value::as_str).map(str::to_string),
        etag: etag(v),
        ..Default::default()
    }
}

fn due_json(t: &Task) -> Value {
    t.due
        .as_ref()
        .map_or(Value::Null, |d| json!(format!("{}T00:00:00.000Z", super::ical::date_of(d).format("%Y-%m-%d"))))
}

/// Cuerpo de `PATCH` (solo lo que cambió respecto de `old`) o de `POST` (`old = None`).
pub fn task_patch(old: Option<&Task>, new: &Task) -> Map<String, Value> {
    let blank = Task::default();
    let full = old.is_none();
    let old = old.unwrap_or(&blank);
    let mut m = Map::new();
    if full || old.summary != new.summary {
        m.insert("title".into(), json!(new.summary));
    }
    if full || old.description != new.description {
        m.insert("notes".into(), json!(new.description));
    }
    if full || old.due.as_ref().map(super::ical::date_of) != new.due.as_ref().map(super::ical::date_of) {
        m.insert("due".into(), due_json(new));
    }
    if full || old.is_done() != new.is_done() {
        if new.is_done() {
            m.insert("status".into(), json!("completed"));
            let at = new.completed.unwrap_or_else(Utc::now);
            m.insert("completed".into(), json!(at.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()));
        } else {
            m.insert("status".into(), json!("needsAction"));
            m.insert("completed".into(), Value::Null);
        }
    }
    m
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    #[test]
    fn round_trip_and_patch() {
        let v = json!({
            "id": "t1", "etag": "\"e1\"", "title": "Pagar renta", "notes": "Antes del 5",
            "status": "needsAction", "due": "2026-11-03T00:00:00.000Z", "updated": "2026-10-28T10:00:00.000Z"
        });
        let t = task_from_google(&v);
        assert_eq!(t.summary, "Pagar renta");
        assert_eq!(t.due, Some(EventTime::Date(NaiveDate::from_ymd_opt(2026, 11, 3).unwrap())));
        assert_eq!((t.remote_id.as_deref(), t.etag.as_deref()), (Some("t1"), Some("\"e1\"")));
        assert!(!t.is_done());
        assert!(task_patch(Some(&t), &t).is_empty());
        let mut done = t.clone();
        done.status = TaskStatus::Completed;
        done.completed = Some(Utc.with_ymd_and_hms(2026, 10, 29, 9, 0, 0).unwrap());
        let p = task_patch(Some(&t), &done);
        assert_eq!(p.get("status"), Some(&json!("completed")));
        assert_eq!(p.get("completed"), Some(&json!("2026-10-29T09:00:00.000Z")));
        assert!(!p.contains_key("title"));
        let back = task_patch(Some(&done), &t);
        assert_eq!(back.get("completed"), Some(&Value::Null));
        let full = task_patch(None, &t);
        assert_eq!(full.get("due"), Some(&json!("2026-11-03T00:00:00.000Z")));
    }

    #[test]
    fn hostile_input_does_not_panic() {
        for v in [json!(null), json!({"due": "x"}), json!({"due": 5, "status": []}), json!({"deleted": "yes"})] {
            let t = task_from_google(&v);
            assert!(t.due.is_none());
            assert!(!is_deleted(&v));
        }
        assert!(is_deleted(&json!({"deleted": true})));
        let t = task_from_google(&json!({"status": "completed", "completed": "2026-10-01T00:00:00Z"}));
        assert!(t.is_done() && t.completed.is_some());
    }
}
