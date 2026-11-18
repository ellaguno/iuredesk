//! Tarea de EWS (`t:Task`) ⇄ JSON de una tarea de Microsoft To Do (F8.4), para reutilizar
//! [`crate::map::graph::task_from_graph`] y [`crate::map::graph::task_patch`].
//!
//! Las fechas de vencimiento e inicio de Exchange son la medianoche local del usuario guardada
//! como instante UTC: se leen como fecha en `user_tz` y se escriben igual.

use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::Tz;
use iuredesk_ews::Element;
use iuredesk_ews::soap::esc;
use serde_json::{Map, Value, json};

use super::{boolean, instant, recurrence_json, recurrence_xml, t, text};

/// `t:Task` → JSON de tarea de To Do.
pub fn task_json(e: &Element, user_tz: Tz) -> Option<Value> {
    let id_el = t(e, "ItemId")?;
    let id = id_el.attr("Id").filter(|i| !i.is_empty() && i.len() <= 2_000)?;
    let date = |name: &str| -> Value {
        match instant(e, name) {
            Some(i) => json!({
                "dateTime": format!("{}T00:00:00", i.with_timezone(&user_tz).date_naive().format("%Y-%m-%d")),
                "timeZone": "UTC",
            }),
            None => Value::Null,
        }
    };
    let utc = |name: &str| -> Value {
        instant(e, name).map_or(
            Value::Null,
            |i| json!({ "dateTime": i.format("%Y-%m-%dT%H:%M:%S").to_string(), "timeZone": "UTC" }),
        )
    };
    let status = match text(e, "Status", 40).as_deref() {
        Some("Completed") => "completed",
        Some("InProgress") => "inProgress",
        Some("WaitingOnOthers") => "waitingOnOthers",
        Some("Deferred") => "deferred",
        _ => "notStarted",
    };
    let mut v = json!({
        "id": id,
        "@odata.etag": id_el.attr("ChangeKey").unwrap_or_default(),
        "title": text(e, "Subject", 1_000).unwrap_or_default(),
        "body": { "contentType": "text", "content": t(e, "Body").map(|b| b.text.chars().take(100_000).collect::<String>()).unwrap_or_default() },
        "status": status,
        "importance": text(e, "Importance", 20).unwrap_or_else(|| "Normal".into()).to_ascii_lowercase(),
        "dueDateTime": date("DueDate"),
        "startDateTime": date("StartDate"),
        "completedDateTime": utc("CompleteDate"),
        "isReminderOn": boolean(e, "ReminderIsSet").unwrap_or(false),
        "reminderDateTime": utc("ReminderDueBy"),
    });
    if let Some(o) = v.as_object_mut() {
        for (field, el) in [("createdDateTime", "DateTimeCreated"), ("lastModifiedDateTime", "LastModifiedTime")] {
            if let Some(i) = instant(e, el) {
                o.insert(field.into(), json!(i.to_rfc3339()));
            }
        }
        if let Some(r) = t(e, "Recurrence").and_then(recurrence_json) {
            o.insert("recurrence".into(), r);
        }
    }
    Some(v)
}

/// Fecha de To Do (`{dateTime: "AAAA-MM-DDT00:00:00"}`) → medianoche local en UTC.
fn local_midnight(v: &Value, user_tz: Tz) -> Option<DateTime<Utc>> {
    let d = v.get("dateTime").and_then(Value::as_str)?.get(..10)?;
    let date = NaiveDate::parse_from_str(d, "%Y-%m-%d").ok()?;
    Some(iuredesk_dav::tz::local_to_utc(date.and_hms_opt(0, 0, 0)?, user_tz))
}

fn utc_instant(v: &Value) -> Option<DateTime<Utc>> {
    let s = v.get("dateTime").and_then(Value::as_str)?.trim().trim_end_matches('Z');
    chrono::NaiveDateTime::parse_from_str(s.split('.').next().unwrap_or(s), "%Y-%m-%dT%H:%M:%S")
        .ok()
        .map(|n| n.and_utc())
}

/// Un campo de tarea: `FieldURI`, XML dentro de `t:Task` (o `None` para borrarlo), orden.
struct Field {
    uri: &'static str,
    xml: Option<String>,
    order: u8,
}

fn fields(patch: &Map<String, Value>, user_tz: Tz) -> Vec<Field> {
    let mut out = Vec::new();
    let fmt = |d: DateTime<Utc>| d.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    if let Some(s) = patch.get("title").and_then(Value::as_str) {
        out.push(Field { uri: "item:Subject", xml: Some(format!("<t:Subject>{}</t:Subject>", esc(s))), order: 0 });
    }
    if let Some(b) = patch.get("body") {
        let c = b.get("content").and_then(Value::as_str).unwrap_or_default();
        out.push(Field {
            uri: "item:Body",
            xml: Some(format!(r#"<t:Body BodyType="Text">{}</t:Body>"#, esc(c))),
            order: 1,
        });
    }
    if let Some(i) = patch.get("importance").and_then(Value::as_str) {
        let v = match i {
            "high" => "High",
            "low" => "Low",
            _ => "Normal",
        };
        out.push(Field { uri: "item:Importance", xml: Some(format!("<t:Importance>{v}</t:Importance>")), order: 2 });
    }
    if let Some(r) = patch.get("reminderDateTime").and_then(utc_instant) {
        out.push(Field {
            uri: "item:ReminderDueBy",
            xml: Some(format!("<t:ReminderDueBy>{}</t:ReminderDueBy>", fmt(r))),
            order: 3,
        });
    }
    if let Some(on) = patch.get("isReminderOn").and_then(Value::as_bool) {
        out.push(Field {
            uri: "item:ReminderIsSet",
            xml: Some(format!("<t:ReminderIsSet>{on}</t:ReminderIsSet>")),
            order: 4,
        });
    }
    for (key, uri, tag, order) in
        [("dueDateTime", "task:DueDate", "DueDate", 5u8), ("startDateTime", "task:StartDate", "StartDate", 7)]
    {
        let Some(v) = patch.get(key) else { continue };
        let xml = local_midnight(v, user_tz).map(|d| format!("<t:{tag}>{}</t:{tag}>", fmt(d)));
        out.push(Field { uri, xml, order });
    }
    if let Some(r) = patch.get("recurrence") {
        out.push(Field { uri: "task:Recurrence", xml: recurrence_xml(r), order: 6 });
    }
    if let Some(s) = patch.get("status").and_then(Value::as_str) {
        let v = match s {
            "completed" => "Completed",
            "inProgress" => "InProgress",
            "waitingOnOthers" => "WaitingOnOthers",
            "deferred" => "Deferred",
            _ => "NotStarted",
        };
        out.push(Field { uri: "task:Status", xml: Some(format!("<t:Status>{v}</t:Status>")), order: 8 });
    }
    out
}

/// Contenido de `t:Task` para crear una tarea (en el orden del esquema).
pub fn create_xml(patch: &Map<String, Value>, user_tz: Tz) -> String {
    let mut f = fields(patch, user_tz);
    f.sort_by_key(|f| f.order);
    f.into_iter().filter_map(|f| f.xml).collect()
}

/// `t:SetItemField`/`t:DeleteItemField` para cambiar una tarea (vacío: nada que cambiar).
pub fn update_xml(patch: &Map<String, Value>, user_tz: Tz) -> String {
    fields(patch, user_tz)
        .into_iter()
        .map(|f| match f.xml {
            Some(x) => {
                format!(r#"<t:SetItemField><t:FieldURI FieldURI="{}"/><t:Task>{x}</t:Task></t:SetItemField>"#, f.uri)
            }
            None => format!(r#"<t:DeleteItemField><t:FieldURI FieldURI="{}"/></t:DeleteItemField>"#, f.uri),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::graph::{task_from_graph, task_patch};
    use iuredesk_model::{EventTime, TaskStatus};

    const TASK: &str = r#"<t:Task xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
      <t:ItemId Id="T1" ChangeKey="ck1"/><t:Subject>Enviar informe</t:Subject>
      <t:Body BodyType="Text">Con las cifras de octubre</t:Body>
      <t:DateTimeCreated>2026-11-01T10:00:00Z</t:DateTimeCreated>
      <t:Importance>High</t:Importance>
      <t:ReminderDueBy>2026-11-19T15:00:00Z</t:ReminderDueBy><t:ReminderIsSet>true</t:ReminderIsSet>
      <t:DueDate>2026-11-20T06:00:00Z</t:DueDate>
      <t:PercentComplete>50</t:PercentComplete>
      <t:StartDate>2026-11-18T06:00:00Z</t:StartDate>
      <t:Status>InProgress</t:Status>
    </t:Task>"#;

    fn mx() -> Tz {
        "America/Mexico_City".parse().unwrap()
    }

    fn task() -> iuredesk_model::Task {
        let e = iuredesk_dav::dav::xml::parse(TASK.as_bytes()).unwrap();
        task_from_graph(&task_json(&e, mx()).unwrap())
    }

    #[test]
    fn ews_task_becomes_a_task() {
        let t = task();
        assert_eq!((t.summary.as_str(), t.description.as_str()), ("Enviar informe", "Con las cifras de octubre"));
        assert_eq!(t.remote_id.as_deref(), Some("T1"));
        assert_eq!(t.priority, 1);
        assert_eq!(t.status, TaskStatus::InProcess);
        // Medianoche de México guardada en UTC → la fecha local.
        assert_eq!(t.due, Some(EventTime::Date(NaiveDate::from_ymd_opt(2026, 11, 20).unwrap())));
        assert_eq!(t.start, Some(EventTime::Date(NaiveDate::from_ymd_opt(2026, 11, 18).unwrap())));
        assert_eq!(t.reminder.map(|r| r.to_rfc3339()).as_deref(), Some("2026-11-19T15:00:00+00:00"));
        assert!(t.created.is_some());
    }

    #[test]
    fn edits_and_creation() {
        let old = task();
        let mut new = old.clone();
        new.status = TaskStatus::Completed;
        new.due = None;
        let u = update_xml(&task_patch(Some(&old), &new), mx());
        assert!(u.contains(r#"<t:FieldURI FieldURI="task:Status"/><t:Task><t:Status>Completed</t:Status>"#), "{u}");
        assert!(u.contains(r#"<t:DeleteItemField><t:FieldURI FieldURI="task:DueDate"/></t:DeleteItemField>"#), "{u}");
        assert!(!u.contains("item:Subject"));
        assert!(update_xml(&task_patch(Some(&old), &old), mx()).is_empty());

        let c = create_xml(&task_patch(None, &old), mx());
        assert!(c.contains("<t:DueDate>2026-11-20T06:00:00Z</t:DueDate>"), "{c}");
        let pos = |s: &str| c.find(s).unwrap_or_else(|| panic!("{s} en {c}"));
        assert!(pos("<t:Subject>") < pos("<t:Body") && pos("<t:Body") < pos("<t:Importance>"));
        assert!(pos("<t:Importance>") < pos("<t:ReminderDueBy>") && pos("<t:ReminderIsSet>") < pos("<t:DueDate>"));
        assert!(pos("<t:DueDate>") < pos("<t:StartDate>") && pos("<t:StartDate>") < pos("<t:Status>"));
    }

    #[test]
    fn hostile_tasks_never_panic() {
        for x in [
            r#"<t:Task xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types"/>"#,
            r#"<t:Task xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types"><t:ItemId Id="x"/><t:DueDate>mañana</t:DueDate><t:Recurrence><t:DailyRegeneration><t:Interval>2</t:Interval></t:DailyRegeneration></t:Recurrence></t:Task>"#,
        ] {
            let e = iuredesk_dav::dav::xml::parse(x.as_bytes()).unwrap();
            if let Some(v) = task_json(&e, chrono_tz::UTC) {
                let t = task_from_graph(&v);
                let _ = create_xml(&task_patch(None, &t), chrono_tz::UTC);
            }
        }
    }
}
