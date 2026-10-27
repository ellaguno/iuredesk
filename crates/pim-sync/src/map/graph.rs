//! Microsoft Graph (JSON) ⇄ modelo: eventos (`/me/calendars/{id}/calendarView/delta`), contactos
//! (`/me/contacts/delta`) y tareas de To Do (`/me/todo/lists/{id}/tasks/delta`).
//!
//! - Las horas se piden en UTC (`Prefer: outlook.timezone="UTC"`) y la zona original sale de
//!   `originalStartTimeZone` (nombres de Windows → IANA con la tabla de `iuredesk-dav`).
//! - Escribir es un `PATCH` solo con los campos que cambiaron ([`event_patch`], [`contact_patch`],
//!   [`task_patch`]): lo que no tocamos queda en el servidor tal cual, sin pérdida.
//! - `recurrence` ⇄ `RRULE` para los patrones habituales (diario, semanal con días, mensual por
//!   día o por posición, anual; fin por fecha, por número o sin fin).

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use iuredesk_model::{
    Attendee, AttendeeRole, Contact, Event, EventStatus, EventTime, Labeled, Mailbox, PartStat, PostalAddress,
    Reminder, Task, TaskStatus, Transparency,
};
use serde_json::{Map, Value, json};

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// `@odata.etag` de un objeto.
pub fn etag(v: &Value) -> Option<String> {
    v.get("@odata.etag").and_then(Value::as_str).map(str::to_string)
}

/// `true` si el objeto de un delta es un borrado (`@removed`).
pub fn is_removed(v: &Value) -> bool {
    v.get("@removed").is_some()
}

/// Lee `2026-10-05T09:00:00.0000000` (con o sin fracción ni `Z`).
fn parse_naive(raw: &str) -> Option<NaiveDateTime> {
    let t = raw.trim().trim_end_matches('Z');
    let t = t.split('.').next().unwrap_or(t);
    NaiveDateTime::parse_from_str(t, "%Y-%m-%dT%H:%M:%S").ok()
}

/// `{dateTime, timeZone}` → instante UTC (zonas IANA o de Windows; si no se reconoce, UTC).
fn graph_instant(v: &Value) -> Option<DateTime<Utc>> {
    let local = parse_naive(s(v, "dateTime"))?;
    let tz = s(v, "timeZone");
    let zone = iuredesk_dav::tz::resolve_tzid(tz, &[]).unwrap_or(chrono_tz::UTC);
    Some(iuredesk_dav::tz::local_to_utc(local, zone))
}

fn iana_of(windows_or_iana: &str) -> Option<String> {
    let t = windows_or_iana.trim();
    if t.is_empty() || t.eq_ignore_ascii_case("UTC") || t.eq_ignore_ascii_case("tzone://Microsoft/Utc") {
        return None;
    }
    iuredesk_dav::tz::resolve_tzid(t, &[]).map(|z| z.name().to_string())
}

fn time_json(t: &EventTime) -> Value {
    match t {
        EventTime::Date(d) => json!({ "dateTime": format!("{}T00:00:00", d.format("%Y-%m-%d")), "timeZone": "UTC" }),
        EventTime::DateTime { utc, tzid } => match tzid.as_deref().and_then(|z| z.parse::<chrono_tz::Tz>().ok()) {
            Some(tz) if tz != chrono_tz::UTC => json!({
                "dateTime": utc.with_timezone(&tz).naive_local().format("%Y-%m-%dT%H:%M:%S").to_string(),
                "timeZone": tz.name(),
            }),
            _ => json!({ "dateTime": utc.format("%Y-%m-%dT%H:%M:%S").to_string(), "timeZone": "UTC" }),
        },
    }
}

fn mailbox(v: &Value) -> Option<Mailbox> {
    let e = v.get("emailAddress")?;
    let addr = s(e, "address").trim().to_lowercase();
    if addr.is_empty() {
        return None;
    }
    let name = s(e, "name").trim();
    Some(Mailbox { name: (!name.is_empty() && !name.eq_ignore_ascii_case(&addr)).then(|| name.to_string()), addr })
}

fn response(r: &str) -> PartStat {
    match r {
        "accepted" | "organizer" => PartStat::Accepted,
        "declined" => PartStat::Declined,
        "tentativelyAccepted" => PartStat::Tentative,
        _ => PartStat::NeedsAction,
    }
}

/// Texto plano de un cuerpo HTML de Graph (quita etiquetas y entidades básicas).
fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut tag = String::new();
    for c in html.chars() {
        match c {
            '<' => {
                in_tag = true;
                tag.clear();
            }
            '>' if in_tag => {
                in_tag = false;
                let t = tag.trim_start_matches('/').to_ascii_lowercase();
                if t.starts_with("br") || t.starts_with("p") || t.starts_with("div") {
                    out.push('\n');
                }
            }
            _ if in_tag => {
                if tag.len() < 16 {
                    tag.push(c);
                }
            }
            _ => out.push(c),
        }
    }
    let out = out
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&amp;", "&");
    let lines: Vec<&str> = out.lines().map(str::trim_end).collect();
    lines.join("\n").trim().to_string()
}

fn body_text(v: &Value) -> String {
    let Some(b) = v.get("body") else { return String::new() };
    let content = s(b, "content");
    if s(b, "contentType").eq_ignore_ascii_case("html") { html_to_text(content) } else { content.trim().to_string() }
}

// ------------------------------------------------------------------ recurrencia

const DAYS: [(&str, &str); 7] = [
    ("sunday", "SU"),
    ("monday", "MO"),
    ("tuesday", "TU"),
    ("wednesday", "WE"),
    ("thursday", "TH"),
    ("friday", "FR"),
    ("saturday", "SA"),
];
const INDEX: [(&str, &str); 5] = [("first", "1"), ("second", "2"), ("third", "3"), ("fourth", "4"), ("last", "-1")];

/// `recurrence` de Graph → `RRULE`.
pub fn recurrence_to_rrule(r: &Value) -> Option<String> {
    let p = r.get("pattern")?;
    let range = r.get("range");
    let interval = p.get("interval").and_then(Value::as_u64).unwrap_or(1).clamp(1, 1_000);
    let days: Vec<&str> = p
        .get("daysOfWeek")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(|d| DAYS.iter().find(|(g, _)| g.eq_ignore_ascii_case(d)).map(|(_, i)| *i))
        .collect();
    let index = INDEX.iter().find(|(g, _)| *g == s(p, "index")).map_or("1", |(_, i)| *i);
    let mut parts = match s(p, "type") {
        "daily" => vec!["FREQ=DAILY".to_string()],
        "weekly" => {
            let mut v = vec!["FREQ=WEEKLY".to_string()];
            if !days.is_empty() {
                v.push(format!("BYDAY={}", days.join(",")));
            }
            v
        }
        "absoluteMonthly" => {
            vec![
                "FREQ=MONTHLY".into(),
                format!("BYMONTHDAY={}", p.get("dayOfMonth").and_then(Value::as_u64).unwrap_or(1)),
            ]
        }
        "relativeMonthly" => vec![
            "FREQ=MONTHLY".into(),
            format!("BYDAY={}", days.iter().map(|d| format!("{index}{d}")).collect::<Vec<_>>().join(",")),
        ],
        "absoluteYearly" => vec![
            "FREQ=YEARLY".into(),
            format!("BYMONTH={}", p.get("month").and_then(Value::as_u64).unwrap_or(1)),
            format!("BYMONTHDAY={}", p.get("dayOfMonth").and_then(Value::as_u64).unwrap_or(1)),
        ],
        "relativeYearly" => vec![
            "FREQ=YEARLY".into(),
            format!("BYMONTH={}", p.get("month").and_then(Value::as_u64).unwrap_or(1)),
            format!("BYDAY={}", days.iter().map(|d| format!("{index}{d}")).collect::<Vec<_>>().join(",")),
        ],
        _ => return None,
    };
    if interval > 1 {
        parts.push(format!("INTERVAL={interval}"));
    }
    if let Some(range) = range {
        match s(range, "type") {
            "endDate" => {
                if let Ok(d) = NaiveDate::parse_from_str(s(range, "endDate"), "%Y-%m-%d") {
                    parts.push(format!("UNTIL={}", d.format("%Y%m%d")));
                }
            }
            "numbered" => {
                let n = range.get("numberOfOccurrences").and_then(Value::as_u64).unwrap_or(1).clamp(1, 10_000);
                parts.push(format!("COUNT={n}"));
            }
            _ => {}
        }
    }
    Some(parts.join(";"))
}

/// `RRULE` → `recurrence` de Graph (`None` si el patrón no tiene equivalente).
pub fn rrule_to_recurrence(rrule: &str, start: &EventTime) -> Option<Value> {
    let mut freq = "";
    let (mut interval, mut count, mut until) = (1u64, None, None);
    let (mut byday, mut bymonthday, mut bymonth) = (Vec::new(), None, None);
    for part in rrule.split(';') {
        let Some((k, v)) = part.split_once('=') else { continue };
        match k.trim().to_ascii_uppercase().as_str() {
            "FREQ" => {
                freq = if v.eq_ignore_ascii_case("DAILY") {
                    "D"
                } else if v.eq_ignore_ascii_case("WEEKLY") {
                    "W"
                } else if v.eq_ignore_ascii_case("MONTHLY") {
                    "M"
                } else if v.eq_ignore_ascii_case("YEARLY") {
                    "Y"
                } else {
                    ""
                }
            }
            "INTERVAL" => interval = v.trim().parse().unwrap_or(1),
            "COUNT" => count = v.trim().parse::<u64>().ok(),
            "UNTIL" => until = iuredesk_dav::value::parse_date_time(v, None, None).ok().map(|d| d.naive().date()),
            "BYDAY" => byday = v.split(',').map(|d| d.trim().to_ascii_uppercase()).collect(),
            "BYMONTHDAY" => bymonthday = v.trim().parse::<u64>().ok(),
            "BYMONTH" => bymonth = v.trim().parse::<u64>().ok(),
            _ => {}
        }
    }
    let day_name = |code: &str| DAYS.iter().find(|(_, i)| *i == code).map(|(g, _)| *g);
    let split_pos = |d: &str| {
        let pos: String = d.chars().take_while(|c| *c == '-' || c.is_ascii_digit()).collect();
        (pos.clone(), d.get(pos.len()..).unwrap_or_default().to_string())
    };
    let start_date = super::ical::date_of(start);
    let weekday_names: Vec<&str> = byday.iter().filter_map(|d| day_name(&split_pos(d).1)).collect();
    let rel_index = byday.first().map(|d| split_pos(d).0).filter(|p| !p.is_empty());
    let index_name = rel_index.as_deref().and_then(|p| INDEX.iter().find(|(_, i)| *i == p).map(|(g, _)| *g));
    let pattern = match freq {
        "D" => json!({ "type": "daily", "interval": interval }),
        "W" => {
            let days = if weekday_names.is_empty() {
                vec![
                    DAYS.get(chrono::Datelike::weekday(&start_date).num_days_from_sunday() as usize)
                        .map_or("monday", |d| d.0),
                ]
            } else {
                weekday_names.clone()
            };
            json!({ "type": "weekly", "interval": interval, "daysOfWeek": days, "firstDayOfWeek": "monday" })
        }
        "M" if index_name.is_some() => {
            json!({ "type": "relativeMonthly", "interval": interval, "daysOfWeek": weekday_names, "index": index_name })
        }
        "M" => json!({
            "type": "absoluteMonthly", "interval": interval,
            "dayOfMonth": bymonthday.unwrap_or(u64::from(chrono::Datelike::day(&start_date))),
        }),
        "Y" if index_name.is_some() => json!({
            "type": "relativeYearly", "interval": interval, "daysOfWeek": weekday_names, "index": index_name,
            "month": bymonth.unwrap_or(u64::from(chrono::Datelike::month(&start_date))),
        }),
        "Y" => json!({
            "type": "absoluteYearly", "interval": interval,
            "dayOfMonth": bymonthday.unwrap_or(u64::from(chrono::Datelike::day(&start_date))),
            "month": bymonth.unwrap_or(u64::from(chrono::Datelike::month(&start_date))),
        }),
        _ => return None,
    };
    let start_s = start_date.format("%Y-%m-%d").to_string();
    let range = match (count, until) {
        (Some(n), _) => json!({ "type": "numbered", "startDate": start_s, "numberOfOccurrences": n }),
        (None, Some(u)) => {
            json!({ "type": "endDate", "startDate": start_s, "endDate": u.format("%Y-%m-%d").to_string() })
        }
        (None, None) => json!({ "type": "noEnd", "startDate": start_s }),
    };
    Some(json!({ "pattern": pattern, "range": range }))
}

// ------------------------------------------------------------------ eventos

/// Evento de Graph → [`Event`]. `remote_id` = `id`; `etag` = `@odata.etag`.
pub fn event_from_graph(v: &Value) -> Event {
    let all_day = v.get("isAllDay").and_then(Value::as_bool).unwrap_or(false);
    let tzid = iana_of(s(v, "originalStartTimeZone"));
    let time = |key: &str| -> EventTime {
        let t = v.get(key).cloned().unwrap_or(Value::Null);
        if all_day {
            let d = parse_naive(s(&t, "dateTime")).map(|n| n.date()).unwrap_or_default();
            return EventTime::Date(d);
        }
        EventTime::DateTime { utc: graph_instant(&t).unwrap_or_default(), tzid: tzid.clone() }
    };
    let organizer = v.get("organizer").and_then(mailbox);
    let show_as = s(v, "showAs");
    let attendees = v
        .get("attendees")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(1_000)
        .filter_map(|a| {
            Some(Attendee {
                mailbox: mailbox(a)?,
                partstat: response(a.get("status").map(|st| s(st, "response")).unwrap_or_default()),
                role: match s(a, "type") {
                    "optional" => AttendeeRole::Optional,
                    "resource" => AttendeeRole::NonParticipant,
                    _ => AttendeeRole::Required,
                },
                rsvp: v.get("responseRequested").and_then(Value::as_bool).unwrap_or(true),
            })
        })
        .collect();
    let reminders = if v.get("isReminderOn").and_then(Value::as_bool).unwrap_or(false) {
        vec![Reminder { minutes_before: v.get("reminderMinutesBeforeStart").and_then(Value::as_i64).unwrap_or(15) }]
    } else {
        Vec::new()
    };
    Event {
        uid: v.get("iCalUId").and_then(Value::as_str).unwrap_or(s(v, "id")).to_string(),
        summary: s(v, "subject").to_string(),
        location: v.get("location").map(|l| s(l, "displayName").to_string()).unwrap_or_default(),
        description: body_text(v),
        start: time("start"),
        end: time("end"),
        rrule: v.get("recurrence").filter(|r| !r.is_null()).and_then(recurrence_to_rrule),
        organizer,
        attendees,
        reminders,
        status: if v.get("isCancelled").and_then(Value::as_bool).unwrap_or(false) {
            EventStatus::Cancelled
        } else if show_as == "tentative" {
            EventStatus::Tentative
        } else {
            EventStatus::Confirmed
        },
        transparency: if show_as == "free" { Transparency::Transparent } else { Transparency::Opaque },
        remote_id: v.get("id").and_then(Value::as_str).map(str::to_string),
        etag: etag(v),
        updated: v
            .get("lastModifiedDateTime")
            .and_then(Value::as_str)
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.with_timezone(&Utc)),
        ..Default::default()
    }
}

fn attendees_json(list: &[Attendee]) -> Value {
    Value::Array(
        list.iter()
            .map(|a| {
                json!({
                    "emailAddress": { "address": a.mailbox.addr, "name": a.mailbox.name.clone().unwrap_or_default() },
                    "type": match a.role { AttendeeRole::Optional => "optional", AttendeeRole::NonParticipant => "resource", _ => "required" },
                })
            })
            .collect(),
    )
}

/// Cuerpo de un `PATCH` con lo que cambió de `old` a `new` (vacío: nada que subir). Con
/// `old = None` es el cuerpo completo de un `POST` de creación.
pub fn event_patch(old: Option<&Event>, new: &Event) -> Map<String, Value> {
    let blank = Event::default();
    let full = old.is_none();
    let old = old.unwrap_or(&blank);
    let mut m = Map::new();
    if full || old.summary != new.summary {
        m.insert("subject".into(), json!(new.summary));
    }
    if full || old.location != new.location {
        m.insert("location".into(), json!({ "displayName": new.location }));
    }
    if full || old.description != new.description {
        m.insert("body".into(), json!({ "contentType": "text", "content": new.description }));
    }
    if full || old.start != new.start || old.end != new.end {
        m.insert("start".into(), time_json(&new.start));
        m.insert("end".into(), time_json(&new.end));
        m.insert("isAllDay".into(), json!(new.start.is_all_day()));
    }
    if full || old.status != new.status || old.transparency != new.transparency {
        let show = match (&new.status, &new.transparency) {
            (EventStatus::Tentative, _) => "tentative",
            (_, Transparency::Transparent) => "free",
            _ => "busy",
        };
        m.insert("showAs".into(), json!(show));
    }
    if full || old.reminders != new.reminders {
        match new.reminders.first() {
            Some(r) => {
                m.insert("isReminderOn".into(), json!(true));
                m.insert("reminderMinutesBeforeStart".into(), json!(r.minutes_before.max(0)));
            }
            None => {
                m.insert("isReminderOn".into(), json!(false));
            }
        }
    }
    if (full && !new.attendees.is_empty()) || (!full && old.attendees != new.attendees) {
        m.insert("attendees".into(), attendees_json(&new.attendees));
    }
    if (full && new.rrule.is_some()) || (!full && old.rrule != new.rrule) {
        let r = new.rrule.as_deref().and_then(|r| rrule_to_recurrence(r, &new.start)).unwrap_or(Value::Null);
        m.insert("recurrence".into(), r);
    }
    m
}

/// Acción de Graph para responder a una invitación.
pub fn rsvp_action(p: &PartStat) -> Option<&'static str> {
    match p {
        PartStat::Accepted => Some("accept"),
        PartStat::Declined => Some("decline"),
        PartStat::Tentative => Some("tentativelyAccept"),
        _ => None,
    }
}

// ------------------------------------------------------------------ contactos

fn address_from(v: Option<&Value>, label: &str) -> Option<PostalAddress> {
    let v = v?;
    let a = PostalAddress {
        label: label.into(),
        street: s(v, "street").into(),
        city: s(v, "city").into(),
        region: s(v, "state").into(),
        postal_code: s(v, "postalCode").into(),
        country: s(v, "countryOrRegion").into(),
    };
    let empty = PostalAddress { label: label.into(), ..Default::default() };
    (a != empty).then_some(a)
}

fn address_json(a: Option<&PostalAddress>) -> Value {
    match a {
        Some(a) => json!({
            "street": a.street, "city": a.city, "state": a.region, "postalCode": a.postal_code, "countryOrRegion": a.country,
        }),
        None => json!({}),
    }
}

/// Contacto de Graph → [`Contact`].
pub fn contact_from_graph(v: &Value) -> Contact {
    let mut phones: Vec<Labeled> = Vec::new();
    let mobile = s(v, "mobilePhone");
    if !mobile.is_empty() {
        phones.push(Labeled::new("cell", mobile));
    }
    for (key, label) in [("businessPhones", "work"), ("homePhones", "home")] {
        for p in v.get(key).and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).take(50) {
            phones.push(Labeled::new(label, p));
        }
    }
    let addresses = [("homeAddress", "home"), ("businessAddress", "work"), ("otherAddress", "other")]
        .into_iter()
        .filter_map(|(k, l)| address_from(v.get(k), l))
        .collect();
    Contact {
        uid: s(v, "id").into(),
        full_name: s(v, "displayName").into(),
        given_name: s(v, "givenName").into(),
        family_name: s(v, "surname").into(),
        nickname: s(v, "nickName").into(),
        emails: v
            .get("emailAddresses")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .take(100)
            .filter(|e| !s(e, "address").is_empty())
            .map(|e| Labeled::new("", s(e, "address")))
            .collect(),
        phones,
        org: s(v, "companyName").into(),
        title: s(v, "jobTitle").into(),
        addresses,
        notes: s(v, "personalNotes").into(),
        birthday: v
            .get("birthday")
            .and_then(Value::as_str)
            .and_then(|b| b.get(..10))
            .and_then(|b| NaiveDate::parse_from_str(b, "%Y-%m-%d").ok()),
        remote_id: v.get("id").and_then(Value::as_str).map(str::to_string),
        etag: etag(v),
        updated: v
            .get("lastModifiedDateTime")
            .and_then(Value::as_str)
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.with_timezone(&Utc)),
        ..Default::default()
    }
}

/// Cuerpo de `PATCH`/`POST` de un contacto (ver [`event_patch`]).
pub fn contact_patch(old: Option<&Contact>, new: &Contact) -> Map<String, Value> {
    let blank = Contact::default();
    let full = old.is_none();
    let old = old.unwrap_or(&blank);
    let mut m = Map::new();
    let mut put = |changed: bool, k: &str, v: Value| {
        if full || changed {
            m.insert(k.into(), v);
        }
    };
    put(old.full_name != new.full_name, "displayName", json!(new.full_name));
    put(old.given_name != new.given_name, "givenName", json!(new.given_name));
    put(old.family_name != new.family_name, "surname", json!(new.family_name));
    put(old.nickname != new.nickname, "nickName", json!(new.nickname));
    put(old.org != new.org, "companyName", json!(new.org));
    put(old.title != new.title, "jobTitle", json!(new.title));
    put(old.notes != new.notes, "personalNotes", json!(new.notes));
    put(
        old.emails != new.emails,
        "emailAddresses",
        Value::Array(new.emails.iter().map(|e| json!({ "address": e.value, "name": e.value })).collect()),
    );
    if full || old.phones != new.phones {
        let pick = |label: &str| -> Vec<&str> {
            new.phones.iter().filter(|p| p.label == label).map(|p| p.value.as_str()).collect()
        };
        let others: Vec<&str> = new
            .phones
            .iter()
            .filter(|p| !matches!(p.label.as_str(), "cell" | "home"))
            .map(|p| p.value.as_str())
            .collect();
        m.insert("mobilePhone".into(), json!(pick("cell").first().copied().unwrap_or_default()));
        m.insert("homePhones".into(), json!(pick("home")));
        m.insert("businessPhones".into(), json!(others));
    }
    if full || old.addresses != new.addresses {
        let by = |l: &str| new.addresses.iter().find(|a| a.label == l);
        m.insert("homeAddress".into(), address_json(by("home")));
        m.insert("businessAddress".into(), address_json(by("work")));
        m.insert(
            "otherAddress".into(),
            address_json(new.addresses.iter().find(|a| a.label != "home" && a.label != "work")),
        );
    }
    if full || old.birthday != new.birthday {
        m.insert(
            "birthday".into(),
            new.birthday.map_or(Value::Null, |d| json!(format!("{}T00:00:00Z", d.format("%Y-%m-%d")))),
        );
    }
    m
}

// ------------------------------------------------------------------ tareas (To Do)

/// Tarea de To Do → [`Task`].
pub fn task_from_graph(v: &Value) -> Task {
    let status = match s(v, "status") {
        "completed" => TaskStatus::Completed,
        "inProgress" | "waitingOnOthers" | "deferred" => TaskStatus::InProcess,
        _ => TaskStatus::NeedsAction,
    };
    let ts = |k: &str| {
        v.get(k)
            .and_then(Value::as_str)
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.with_timezone(&Utc))
    };
    Task {
        uid: s(v, "id").into(),
        summary: s(v, "title").into(),
        description: body_text(v),
        due: v
            .get("dueDateTime")
            .filter(|d| !d.is_null())
            .and_then(|d| parse_naive(s(d, "dateTime")))
            .map(|n| EventTime::Date(n.date())),
        priority: match s(v, "importance") {
            "high" => 1,
            "low" => 9,
            _ => 0,
        },
        percent_complete: if status == TaskStatus::Completed { 100 } else { 0 },
        completed: v.get("completedDateTime").filter(|d| !d.is_null()).and_then(graph_instant),
        status,
        created: ts("createdDateTime"),
        updated: ts("lastModifiedDateTime"),
        remote_id: v.get("id").and_then(Value::as_str).map(str::to_string),
        etag: etag(v),
        ..Default::default()
    }
}

/// Cuerpo de `PATCH`/`POST` de una tarea (ver [`event_patch`]).
pub fn task_patch(old: Option<&Task>, new: &Task) -> Map<String, Value> {
    let blank = Task::default();
    let full = old.is_none();
    let old = old.unwrap_or(&blank);
    let mut m = Map::new();
    if full || old.summary != new.summary {
        m.insert("title".into(), json!(new.summary));
    }
    if full || old.description != new.description {
        m.insert("body".into(), json!({ "contentType": "text", "content": new.description }));
    }
    if full || old.due != new.due {
        let due = new.due.as_ref().map_or(Value::Null, |d| {
            json!({ "dateTime": format!("{}T00:00:00", super::ical::date_of(d).format("%Y-%m-%d")), "timeZone": "UTC" })
        });
        m.insert("dueDateTime".into(), due);
    }
    if full || old.priority != new.priority {
        let imp = match new.priority {
            1..=4 => "high",
            6..=9 => "low",
            _ => "normal",
        };
        m.insert("importance".into(), json!(imp));
    }
    if full || old.status != new.status {
        let st = match new.status {
            TaskStatus::Completed => "completed",
            TaskStatus::InProcess => "inProgress",
            TaskStatus::NeedsAction | TaskStatus::Cancelled => "notStarted",
        };
        m.insert("status".into(), json!(st));
    }
    m
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    #[test]
    fn graph_event_round_trip() {
        let v = json!({
            "id": "AAMk1", "@odata.etag": "W/\"1\"", "iCalUId": "0400ABC",
            "subject": "Revisión", "body": {"contentType": "html", "content": "<p>Hola &amp; adiós</p>"},
            "start": {"dateTime": "2026-11-05T16:00:00.0000000", "timeZone": "UTC"},
            "end": {"dateTime": "2026-11-05T17:00:00.0000000", "timeZone": "UTC"},
            "originalStartTimeZone": "Central Standard Time (Mexico)",
            "isAllDay": false, "showAs": "tentative", "isReminderOn": true, "reminderMinutesBeforeStart": 10,
            "organizer": {"emailAddress": {"name": "Ana", "address": "Ana@Example.com"}},
            "attendees": [{"type": "optional", "status": {"response": "accepted"}, "emailAddress": {"address": "yo@example.com", "name": "Yo"}}],
            "recurrence": {"pattern": {"type": "weekly", "interval": 2, "daysOfWeek": ["monday", "thursday"]},
                           "range": {"type": "numbered", "startDate": "2026-11-05", "numberOfOccurrences": 6}},
            "lastModifiedDateTime": "2026-10-26T10:00:00Z"
        });
        let e = event_from_graph(&v);
        assert_eq!(e.uid, "0400ABC");
        assert_eq!(e.description, "Hola & adiós");
        assert_eq!(
            e.start,
            EventTime::DateTime {
                utc: Utc.with_ymd_and_hms(2026, 11, 5, 16, 0, 0).unwrap(),
                tzid: Some("America/Mexico_City".into())
            }
        );
        assert_eq!(e.status, EventStatus::Tentative);
        assert_eq!(e.reminders, vec![Reminder { minutes_before: 10 }]);
        assert_eq!(e.organizer.as_ref().unwrap().addr, "ana@example.com");
        assert_eq!(e.attendees[0].role, AttendeeRole::Optional);
        assert_eq!(e.attendees[0].partstat, PartStat::Accepted);
        assert_eq!(e.rrule.as_deref(), Some("FREQ=WEEKLY;BYDAY=MO,TH;INTERVAL=2;COUNT=6"));
        assert_eq!(e.etag.as_deref(), Some("W/\"1\""));

        assert!(event_patch(Some(&e), &e).is_empty());
        let mut n = e.clone();
        n.summary = "Otra".into();
        let p = event_patch(Some(&e), &n);
        assert_eq!(p.len(), 1);
        assert_eq!(p["subject"], "Otra");
        n.start = EventTime::DateTime {
            utc: Utc.with_ymd_and_hms(2026, 11, 5, 18, 0, 0).unwrap(),
            tzid: Some("America/Mexico_City".into()),
        };
        let p = event_patch(Some(&e), &n);
        assert_eq!(p["start"]["dateTime"], "2026-11-05T12:00:00");
        assert_eq!(p["start"]["timeZone"], "America/Mexico_City");
        let rec = rrule_to_recurrence(e.rrule.as_deref().unwrap(), &e.start).unwrap();
        assert_eq!(recurrence_to_rrule(&rec).as_deref(), e.rrule.as_deref());
        let full = event_patch(None, &e);
        assert!(full.contains_key("recurrence") && full.contains_key("attendees"));
    }

    #[test]
    fn rrule_patterns() {
        let st = EventTime::Date(NaiveDate::from_ymd_opt(2026, 3, 9).unwrap());
        for r in [
            "FREQ=DAILY",
            "FREQ=MONTHLY;BYMONTHDAY=9",
            "FREQ=MONTHLY;BYDAY=-1FR",
            "FREQ=YEARLY;BYMONTH=3;BYMONTHDAY=9",
            "FREQ=WEEKLY;BYDAY=MO;UNTIL=20261231",
        ] {
            let g = rrule_to_recurrence(r, &st).unwrap();
            assert_eq!(recurrence_to_rrule(&g).as_deref(), Some(r), "{g}");
        }
        assert!(rrule_to_recurrence("FREQ=HOURLY", &st).is_none());
    }

    #[test]
    fn contacts_and_tasks() {
        let v = json!({
            "id": "C1", "@odata.etag": "W/\"c\"", "displayName": "Ana Pérez", "givenName": "Ana", "surname": "Pérez",
            "emailAddresses": [{"name": "Ana", "address": "ana@acme.com"}], "mobilePhone": "+52 1", "businessPhones": ["+52 2"],
            "homeAddress": {"street": "Calle 1", "city": "CDMX"}, "businessAddress": {}, "birthday": "1980-05-17T00:00:00Z",
            "companyName": "ACME"
        });
        let c = contact_from_graph(&v);
        assert_eq!(c.phones, vec![Labeled::new("cell", "+52 1"), Labeled::new("work", "+52 2")]);
        assert_eq!(c.addresses.len(), 1);
        assert_eq!(c.birthday, NaiveDate::from_ymd_opt(1980, 5, 17));
        assert!(contact_patch(Some(&c), &c).is_empty());
        let mut n = c.clone();
        n.title = "CEO".into();
        assert_eq!(contact_patch(Some(&c), &n).len(), 1);

        let t = task_from_graph(&json!({
            "id": "T1", "title": "Pagar", "importance": "high", "status": "completed",
            "dueDateTime": {"dateTime": "2026-11-03T00:00:00.0000000", "timeZone": "UTC"},
            "completedDateTime": {"dateTime": "2026-11-02T10:00:00.0000000", "timeZone": "UTC"}
        }));
        assert_eq!(t.priority, 1);
        assert_eq!(t.status, TaskStatus::Completed);
        assert_eq!(t.due, Some(EventTime::Date(NaiveDate::from_ymd_opt(2026, 11, 3).unwrap())));
        assert!(task_patch(Some(&t), &t).is_empty());
        assert_eq!(task_patch(None, &t)["importance"], "high");
    }

    proptest::proptest! {
        #[test]
        fn never_panics(s in "\\PC{0,200}") {
            let v = json!({"subject": s, "start": {"dateTime": s, "timeZone": s}, "recurrence": {"pattern": {"type": "weekly", "daysOfWeek": [s]}}});
            let e = event_from_graph(&v);
            let _ = event_patch(None, &e);
            let _ = rrule_to_recurrence(&s, &e.start);
            let _ = contact_from_graph(&v);
            let _ = task_from_graph(&v);
        }
    }
}
