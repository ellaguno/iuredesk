//! Exchange Web Services (XML) ⇄ JSON con la forma de Microsoft Graph (F8.4).
//!
//! EWS y Graph son dos caras del mismo buzón de Exchange: los campos de un `t:CalendarItem` y los
//! de un evento de Graph se corresponden casi uno a uno (incluida la recurrencia, que en los dos
//! es `pattern` + `range`). Así que el elemento de EWS se traduce al JSON de Graph y todo lo demás
//! se reutiliza de [`super::graph`]: evento del modelo, `RRULE`, parche con solo lo cambiado,
//! series. De vuelta, el parche de Graph se traduce a campos de EWS (en el orden del esquema al
//! crear; `SetItemField`/`DeleteItemField` al cambiar).
//!
//! Horas: EWS da instantes UTC y la zona del evento por su nombre de Windows
//! (`StartTimeZone/@Id`); al escribir se manda el instante UTC y la zona de Windows equivalente
//! a la IANA del evento. Un evento de día completo empieza a medianoche en su zona.

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use chrono_tz::Tz;
use iuredesk_ews::Element;
use iuredesk_ews::soap::{T, esc};
use serde_json::{Map, Value, json};

/// Hijo `t:{name}`.
fn t<'a>(e: &'a Element, name: &str) -> Option<&'a Element> {
    e.child(T, name)
}

/// Texto recortado de `t:{name}` (acotado), si no está vacío.
fn text(e: &Element, name: &str, max: usize) -> Option<String> {
    t(e, name).map(|c| c.text_trim().chars().take(max).collect::<String>()).filter(|s| !s.is_empty())
}

fn boolean(e: &Element, name: &str) -> Option<bool> {
    t(e, name).map(|c| c.text_trim().eq_ignore_ascii_case("true"))
}

fn instant(e: &Element, name: &str) -> Option<DateTime<Utc>> {
    t(e, name).and_then(|c| DateTime::parse_from_rfc3339(c.text_trim()).ok()).map(|d| d.with_timezone(&Utc))
}

/// IANA de un nombre de zona de Windows (o IANA); `None` para UTC o desconocida.
fn zone_of(name: &str) -> Option<Tz> {
    let n = name.trim();
    if n.is_empty() || n.eq_ignore_ascii_case("UTC") {
        return None;
    }
    iuredesk_dav::tz::resolve_tzid(n, &[])
}

/// Nombre de Windows de una zona IANA. La tabla de CLDR solo trae una ciudad por zona de Windows
/// (París, no Madrid): si no está, la primera zona de Windows con los mismos desfases a lo largo
/// del año (horario de verano incluido). `"UTC"` si ninguna coincide.
pub fn windows_zone(tz: Tz) -> &'static str {
    use chrono::{Offset as _, TimeZone as _};
    if tz == chrono_tz::UTC {
        return "UTC";
    }
    let zones = iuredesk_dav::tz::WINDOWS_ZONES;
    if let Some((w, _)) = zones.iter().find(|(_, i)| *i == tz.name()) {
        return w;
    }
    let samples: Vec<DateTime<Utc>> = [(1, 15), (3, 20), (4, 15), (7, 15), (10, 20), (11, 15)]
        .iter()
        .filter_map(|(m, d)| Utc.with_ymd_and_hms(2026, *m, *d, 12, 0, 0).single())
        .collect();
    let offsets =
        |z: Tz| -> Vec<i32> { samples.iter().map(|t| t.with_timezone(&z).offset().fix().local_minus_utc()).collect() };
    let want = offsets(tz);
    zones.iter().find(|(_, i)| i.parse::<Tz>().is_ok_and(|z| offsets(z) == want)).map_or("UTC", |(w, _)| *w)
}

fn mailbox_json(m: &Element) -> Option<Value> {
    let addr = text(m, "EmailAddress", 320).unwrap_or_default();
    let name = text(m, "Name", 300).unwrap_or_default();
    // Una dirección interna sin SMTP (`/O=…`, X.500) no sirve para escribir: sin dirección.
    let addr = if addr.starts_with('/') { String::new() } else { addr };
    if addr.is_empty() && name.is_empty() {
        return None;
    }
    Some(json!({ "emailAddress": { "address": addr, "name": name } }))
}

/// `ResponseType` de EWS → `response` de Graph.
fn response(r: &str) -> &'static str {
    match r {
        "Organizer" => "organizer",
        "Accept" => "accepted",
        "Tentative" => "tentativelyAccepted",
        "Decline" => "declined",
        "NoResponseReceived" => "notResponded",
        _ => "none",
    }
}

// ------------------------------------------------------------------ recurrencia

const DAY_NAMES: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const INDEXES: [(&str, &str); 5] =
    [("First", "first"), ("Second", "second"), ("Third", "third"), ("Fourth", "fourth"), ("Last", "last")];

fn num(e: &Element, name: &str) -> Option<u64> {
    t(e, name).and_then(|c| c.text_trim().parse().ok())
}

/// `2026-11-02` de `2026-11-02`, `2026-11-02Z` o `2026-11-02-06:00`.
fn date_part(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.trim().get(..10)?, "%Y-%m-%d").ok()
}

/// `t:Recurrence` → `recurrence` de Graph.
pub fn recurrence_json(r: &Element) -> Option<Value> {
    let mut pattern = None;
    let mut range = None;
    for c in r.children.iter().take(8) {
        let days = || -> Vec<String> {
            text(c, "DaysOfWeek", 200)
                .unwrap_or_default()
                .split_whitespace()
                .filter(|d| DAY_NAMES.iter().any(|n| n.eq_ignore_ascii_case(d)))
                .map(str::to_ascii_lowercase)
                .collect()
        };
        let index = || {
            text(c, "DayOfWeekIndex", 20)
                .and_then(|i| INDEXES.iter().find(|(e, _)| e.eq_ignore_ascii_case(&i)).map(|(_, g)| *g))
                .unwrap_or("first")
        };
        let month = || {
            text(c, "Month", 20)
                .and_then(|m| MONTHS.iter().position(|n| n.eq_ignore_ascii_case(&m)))
                .map_or(1, |i| i as u64 + 1)
        };
        let interval = num(c, "Interval").unwrap_or(1).clamp(1, 1_000);
        match c.name.as_str() {
            "DailyRecurrence" => pattern = Some(json!({ "type": "daily", "interval": interval })),
            "WeeklyRecurrence" => {
                pattern = Some(json!({
                    "type": "weekly", "interval": interval, "daysOfWeek": days(),
                    "firstDayOfWeek": text(c, "FirstDayOfWeek", 20).unwrap_or_else(|| "Sunday".into()).to_ascii_lowercase(),
                }))
            }
            "AbsoluteMonthlyRecurrence" => {
                pattern = Some(json!({
                    "type": "absoluteMonthly", "interval": interval,
                    "dayOfMonth": num(c, "DayOfMonth").unwrap_or(1).clamp(1, 31),
                }))
            }
            "RelativeMonthlyRecurrence" => {
                pattern = Some(json!({
                    "type": "relativeMonthly", "interval": interval, "daysOfWeek": days(), "index": index(),
                }))
            }
            "AbsoluteYearlyRecurrence" => {
                pattern = Some(json!({
                    "type": "absoluteYearly", "interval": 1,
                    "dayOfMonth": num(c, "DayOfMonth").unwrap_or(1).clamp(1, 31), "month": month(),
                }))
            }
            "RelativeYearlyRecurrence" => {
                pattern = Some(json!({
                    "type": "relativeYearly", "interval": 1, "daysOfWeek": days(), "index": index(), "month": month(),
                }))
            }
            "NoEndRecurrence" | "EndDateRecurrence" | "NumberedRecurrence" => {
                let start = text(c, "StartDate", 40).and_then(|d| date_part(&d))?;
                let start = start.format("%Y-%m-%d").to_string();
                range = Some(match c.name.as_str() {
                    "EndDateRecurrence" => {
                        let end = text(c, "EndDate", 40).and_then(|d| date_part(&d))?;
                        json!({ "type": "endDate", "startDate": start, "endDate": end.format("%Y-%m-%d").to_string() })
                    }
                    "NumberedRecurrence" => json!({
                        "type": "numbered", "startDate": start,
                        "numberOfOccurrences": num(c, "NumberOfOccurrences").unwrap_or(1).clamp(1, 10_000),
                    }),
                    _ => json!({ "type": "noEnd", "startDate": start }),
                });
            }
            _ => {}
        }
    }
    Some(json!({ "pattern": pattern?, "range": range? }))
}

fn days_xml(p: &Value, single: bool) -> String {
    let days: Vec<&str> = p
        .get("daysOfWeek")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(|d| DAY_NAMES.iter().find(|n| n.eq_ignore_ascii_case(d)).copied())
        .collect();
    let days = if single { days.into_iter().take(1).collect::<Vec<_>>() } else { days };
    format!("<t:DaysOfWeek>{}</t:DaysOfWeek>", if days.is_empty() { "Monday".to_string() } else { days.join(" ") })
}

/// `recurrence` de Graph → `t:Recurrence` (`None` si no tiene forma conocida).
pub fn recurrence_xml(r: &Value) -> Option<String> {
    let p = r.get("pattern")?;
    let range = r.get("range")?;
    let get = |v: &Value, k: &str| v.get(k).and_then(Value::as_u64);
    let interval = format!("<t:Interval>{}</t:Interval>", get(p, "interval").unwrap_or(1).clamp(1, 1_000));
    let index = || {
        let i = p.get("index").and_then(Value::as_str).unwrap_or("first");
        let e = INDEXES.iter().find(|(_, g)| g.eq_ignore_ascii_case(i)).map_or("First", |(e, _)| *e);
        format!("<t:DayOfWeekIndex>{e}</t:DayOfWeekIndex>")
    };
    let month = || {
        let m = get(p, "month").unwrap_or(1).clamp(1, 12) as usize;
        format!("<t:Month>{}</t:Month>", MONTHS.get(m - 1).copied().unwrap_or("January"))
    };
    let day_of_month = || format!("<t:DayOfMonth>{}</t:DayOfMonth>", get(p, "dayOfMonth").unwrap_or(1).clamp(1, 31));
    let pattern = match p.get("type").and_then(Value::as_str)? {
        "daily" => format!("<t:DailyRecurrence>{interval}</t:DailyRecurrence>"),
        "weekly" => {
            let first = p
                .get("firstDayOfWeek")
                .and_then(Value::as_str)
                .and_then(|d| DAY_NAMES.iter().find(|n| n.eq_ignore_ascii_case(d)).copied())
                .unwrap_or("Sunday");
            format!(
                "<t:WeeklyRecurrence>{interval}{}<t:FirstDayOfWeek>{first}</t:FirstDayOfWeek></t:WeeklyRecurrence>",
                days_xml(p, false)
            )
        }
        "absoluteMonthly" => {
            format!("<t:AbsoluteMonthlyRecurrence>{interval}{}</t:AbsoluteMonthlyRecurrence>", day_of_month())
        }
        "relativeMonthly" => format!(
            "<t:RelativeMonthlyRecurrence>{interval}{}{}</t:RelativeMonthlyRecurrence>",
            days_xml(p, true),
            index()
        ),
        "absoluteYearly" => {
            format!("<t:AbsoluteYearlyRecurrence>{}{}</t:AbsoluteYearlyRecurrence>", day_of_month(), month())
        }
        "relativeYearly" => format!(
            "<t:RelativeYearlyRecurrence>{}{}{}</t:RelativeYearlyRecurrence>",
            days_xml(p, true),
            index(),
            month()
        ),
        _ => return None,
    };
    let start = range.get("startDate").and_then(Value::as_str).and_then(date_part)?;
    let start = format!("<t:StartDate>{}</t:StartDate>", start.format("%Y-%m-%d"));
    let range = match range.get("type").and_then(Value::as_str)? {
        "endDate" => {
            let end = range.get("endDate").and_then(Value::as_str).and_then(date_part)?;
            format!(
                "<t:EndDateRecurrence>{start}<t:EndDate>{}</t:EndDate></t:EndDateRecurrence>",
                end.format("%Y-%m-%d")
            )
        }
        "numbered" => format!(
            "<t:NumberedRecurrence>{start}<t:NumberOfOccurrences>{}</t:NumberOfOccurrences></t:NumberedRecurrence>",
            get(range, "numberOfOccurrences").unwrap_or(1).clamp(1, 999)
        ),
        _ => format!("<t:NoEndRecurrence>{start}</t:NoEndRecurrence>"),
    };
    Some(format!("<t:Recurrence>{pattern}{range}</t:Recurrence>"))
}

// ------------------------------------------------------------------ elemento → JSON

/// `t:CalendarItem` → JSON con la forma de un evento de Graph (lo lee
/// [`super::graph::event_from_graph`]). `me` son nuestras direcciones (en minúsculas): nuestra
/// respuesta es `MyResponseType`. `user_tz` ubica un día completo sin zona conocida.
pub fn item_json(e: &Element, me: &[String], user_tz: Tz) -> Option<Value> {
    let id_el = t(e, "ItemId")?;
    let id = id_el.attr("Id").filter(|i| !i.is_empty() && i.len() <= 2_000)?;
    let all_day = boolean(e, "IsAllDayEvent").unwrap_or(false);
    let zone_name =
        t(e, "StartTimeZone").and_then(|z| z.attr("Id")).unwrap_or_default().chars().take(200).collect::<String>();
    let zone = zone_of(&zone_name);
    let time = |name: &str| -> Value {
        let utc = instant(e, name).unwrap_or_default();
        if all_day {
            let d = utc.with_timezone(&zone.unwrap_or(user_tz)).date_naive();
            json!({ "dateTime": format!("{}T00:00:00", d.format("%Y-%m-%d")), "timeZone": "UTC" })
        } else {
            json!({ "dateTime": utc.format("%Y-%m-%dT%H:%M:%S").to_string(), "timeZone": "UTC" })
        }
    };
    let my_response = text(e, "MyResponseType", 40).unwrap_or_default();
    let mut attendees = Vec::new();
    for (list, kind) in
        [("RequiredAttendees", "required"), ("OptionalAttendees", "optional"), ("Resources", "resource")]
    {
        for a in t(e, list).map(|l| l.children.iter().take(1_000).collect::<Vec<_>>()).unwrap_or_default() {
            let Some(mut m) = t(a, "Mailbox").and_then(mailbox_json) else { continue };
            let addr = m["emailAddress"]["address"].as_str().unwrap_or_default().to_lowercase();
            let resp = if !addr.is_empty() && me.contains(&addr) && !my_response.is_empty() {
                response(&my_response)
            } else {
                response(&text(a, "ResponseType", 40).unwrap_or_default())
            };
            if let Value::Object(o) = &mut m {
                o.insert("type".into(), json!(kind));
                o.insert("status".into(), json!({ "response": resp }));
            }
            attendees.push(m);
        }
    }
    let show_as = match text(e, "LegacyFreeBusyStatus", 40).as_deref() {
        Some("Free") => "free",
        Some("Tentative") => "tentative",
        Some("OOF") => "oof",
        Some("WorkingElsewhere") => "workingElsewhere",
        _ => "busy",
    };
    let kind = match text(e, "CalendarItemType", 40).as_deref() {
        Some("Occurrence") => "occurrence",
        Some("Exception") => "exception",
        Some("RecurringMaster") => "seriesMaster",
        _ => "singleInstance",
    };
    let mut v = json!({
        "id": id,
        "@odata.etag": id_el.attr("ChangeKey").unwrap_or_default(),
        "type": kind,
        "subject": text(e, "Subject", 1_000).unwrap_or_default(),
        "body": { "contentType": "text", "content": t(e, "Body").map(|b| b.text.chars().take(200_000).collect::<String>()).unwrap_or_default() },
        "location": { "displayName": text(e, "Location", 1_000).unwrap_or_default() },
        "start": time("Start"),
        "end": time("End"),
        "isAllDay": all_day,
        "originalStartTimeZone": zone_name,
        "showAs": show_as,
        "isCancelled": boolean(e, "IsCancelled").unwrap_or(false),
        "isReminderOn": boolean(e, "ReminderIsSet").unwrap_or(false),
        "reminderMinutesBeforeStart": num(e, "ReminderMinutesBeforeStart").unwrap_or(15).min(40_000),
        "iCalUId": text(e, "UID", 1_000).unwrap_or_else(|| id.to_string()),
        "attendees": attendees,
        "responseRequested": boolean(e, "IsResponseRequested").unwrap_or(true),
        "responseStatus": { "response": response(&my_response) },
        "isOrganizer": my_response == "Organizer" || !boolean(e, "IsMeeting").unwrap_or(false),
    });
    if let Some(o) = v.as_object_mut() {
        if let Some(org) = t(e, "Organizer").and_then(|o| t(o, "Mailbox")).and_then(mailbox_json) {
            o.insert("organizer".into(), org);
        }
        if let Some(m) = instant(e, "LastModifiedTime") {
            o.insert("lastModifiedDateTime".into(), json!(m.to_rfc3339()));
        }
        if let Some(r) = t(e, "Recurrence").and_then(recurrence_json) {
            o.insert("recurrence".into(), r);
        }
    }
    Some(v)
}

// ------------------------------------------------------------------ parche de Graph → EWS

/// Instante UTC y zona de Windows de un `{dateTime, timeZone}` de Graph. Un día completo
/// (`all_day`) es la medianoche de esa fecha en `user_tz`.
fn when(v: &Value, all_day: bool, user_tz: Tz) -> Option<(DateTime<Utc>, &'static str)> {
    let raw = v.get("dateTime").and_then(Value::as_str)?;
    let t = raw.trim().trim_end_matches('Z');
    let local = NaiveDateTime::parse_from_str(t.split('.').next().unwrap_or(t), "%Y-%m-%dT%H:%M:%S").ok()?;
    if all_day {
        let midnight = local.date().and_hms_opt(0, 0, 0)?;
        return Some((iuredesk_dav::tz::local_to_utc(midnight, user_tz), windows_zone(user_tz)));
    }
    let tz = v.get("timeZone").and_then(Value::as_str).and_then(zone_of).unwrap_or(chrono_tz::UTC);
    Some((iuredesk_dav::tz::local_to_utc(local, tz), windows_zone(tz)))
}

fn attendees_xml(list: &[Value], kind: &str) -> String {
    list.iter()
        .filter(|a| a.get("type").and_then(Value::as_str).unwrap_or("required") == kind)
        .take(1_000)
        .filter_map(|a| {
            let e = a.get("emailAddress")?;
            let addr = e.get("address").and_then(Value::as_str).unwrap_or_default().trim();
            if addr.is_empty() {
                return None;
            }
            let name = e.get("name").and_then(Value::as_str).unwrap_or_default().trim();
            let name = if name.is_empty() { String::new() } else { format!("<t:Name>{}</t:Name>", esc(name)) };
            Some(format!(
                "<t:Attendee><t:Mailbox>{name}<t:EmailAddress>{}</t:EmailAddress></t:Mailbox></t:Attendee>",
                esc(addr)
            ))
        })
        .collect()
}

fn free_busy(show: &str) -> &'static str {
    match show {
        "free" => "Free",
        "tentative" => "Tentative",
        "oof" => "OOF",
        "workingElsewhere" => "WorkingElsewhere",
        _ => "Busy",
    }
}

/// Un campo de EWS ya traducido: su `FieldURI`, el XML dentro de `t:CalendarItem` (o `None`
/// para borrarlo) y su lugar en el orden del esquema.
struct Field {
    uri: &'static str,
    xml: Option<String>,
    order: u8,
}

/// Traduce un parche de Graph ([`super::graph::event_patch`],
/// [`super::graph::series_patch_json`]) a campos de EWS.
fn fields(patch: &Map<String, Value>, user_tz: Tz) -> Vec<Field> {
    let mut out = Vec::new();
    let mut add = |uri, xml, order| out.push(Field { uri, xml, order });
    if let Some(s) = patch.get("subject").and_then(Value::as_str) {
        add("item:Subject", Some(format!("<t:Subject>{}</t:Subject>", esc(s))), 0);
    }
    if let Some(b) = patch.get("body") {
        let c = b.get("content").and_then(Value::as_str).unwrap_or_default();
        add("item:Body", Some(format!(r#"<t:Body BodyType="Text">{}</t:Body>"#, esc(c))), 1);
    }
    if let Some(on) = patch.get("isReminderOn").and_then(Value::as_bool) {
        add("item:ReminderIsSet", Some(format!("<t:ReminderIsSet>{on}</t:ReminderIsSet>")), 2);
    }
    if let Some(m) = patch.get("reminderMinutesBeforeStart").and_then(Value::as_i64) {
        add(
            "item:ReminderMinutesBeforeStart",
            Some(format!("<t:ReminderMinutesBeforeStart>{}</t:ReminderMinutesBeforeStart>", m.clamp(0, 40_000))),
            3,
        );
    }
    let all_day = patch.get("isAllDay").and_then(Value::as_bool).unwrap_or(false);
    let start = patch.get("start").and_then(|v| when(v, all_day, user_tz));
    let end = patch.get("end").and_then(|v| when(v, all_day, user_tz));
    if let Some((s, zone)) = start {
        add("calendar:StartTimeZone", Some(format!(r#"<t:StartTimeZone Id="{}"/>"#, esc(zone))), 15);
        add("calendar:Start", Some(format!("<t:Start>{}</t:Start>", s.format("%Y-%m-%dT%H:%M:%SZ"))), 4);
    }
    if let Some((e, zone)) = end {
        add("calendar:EndTimeZone", Some(format!(r#"<t:EndTimeZone Id="{}"/>"#, esc(zone))), 16);
        add("calendar:End", Some(format!("<t:End>{}</t:End>", e.format("%Y-%m-%dT%H:%M:%SZ"))), 5);
    }
    if patch.contains_key("isAllDay") {
        add("calendar:IsAllDayEvent", Some(format!("<t:IsAllDayEvent>{all_day}</t:IsAllDayEvent>")), 6);
    }
    if let Some(show) = patch.get("showAs").and_then(Value::as_str) {
        add(
            "calendar:LegacyFreeBusyStatus",
            Some(format!("<t:LegacyFreeBusyStatus>{}</t:LegacyFreeBusyStatus>", free_busy(show))),
            7,
        );
    }
    if let Some(l) = patch.get("location") {
        let l = l.get("displayName").and_then(Value::as_str).unwrap_or_default();
        add("calendar:Location", Some(format!("<t:Location>{}</t:Location>", esc(l))), 8);
    }
    if let Some(list) = patch.get("attendees").and_then(Value::as_array) {
        for (kind, uri, tag, order) in [
            ("required", "calendar:RequiredAttendees", "RequiredAttendees", 9),
            ("optional", "calendar:OptionalAttendees", "OptionalAttendees", 10),
            ("resource", "calendar:Resources", "Resources", 11),
        ] {
            let inner = attendees_xml(list, kind);
            add(uri, (!inner.is_empty()).then(|| format!("<t:{tag}>{inner}</t:{tag}>")), order);
        }
    }
    if let Some(r) = patch.get("recurrence") {
        add("calendar:Recurrence", recurrence_xml(r), 12);
    }
    out
}

/// Contenido de `t:CalendarItem` para crear un evento (campos en el orden del esquema).
pub fn create_xml(patch: &Map<String, Value>, user_tz: Tz) -> String {
    let mut f = fields(patch, user_tz);
    f.sort_by_key(|f| f.order);
    f.into_iter().filter_map(|f| f.xml).collect()
}

/// `t:SetItemField`/`t:DeleteItemField` para cambiar un evento (vacío: nada que cambiar). La zona
/// va antes que la hora para que el servidor ubique la hora en ella.
pub fn update_xml(patch: &Map<String, Value>, user_tz: Tz) -> String {
    let mut f = fields(patch, user_tz);
    f.sort_by_key(|f| if f.uri.ends_with("TimeZone") { 0 } else { 1 + f.order });
    f.into_iter()
        .map(|f| match f.xml {
            Some(x) => format!(
                r#"<t:SetItemField><t:FieldURI FieldURI="{}"/><t:CalendarItem>{x}</t:CalendarItem></t:SetItemField>"#,
                f.uri
            ),
            None => format!(r#"<t:DeleteItemField><t:FieldURI FieldURI="{}"/></t:DeleteItemField>"#, f.uri),
        })
        .collect()
}

pub mod contact;

#[cfg(test)]
mod tests;
