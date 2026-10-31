//! `VEVENT`/`VTODO` ⇄ [`Event`]/[`Task`].
//!
//! **Leer** ([`events_from_ics`], [`tasks_from_ics`]) da un elemento del modelo por serie (el
//! componente sin `RECURRENCE-ID`); las excepciones se quedan en el objeto crudo y las aplica
//! [`crate::occurrences_between`].
//!
//! **Escribir** nunca regenera: [`patch_event`] y [`patch_task`] comparan el elemento tal como se
//! leyó del objeto crudo (`old`) con el editado (`new`) y solo reescriben las propiedades de los
//! campos que cambiaron. Lo que no entendemos (`X-…`, alarmas con sonido, `ATTACH`, excepciones,
//! `VTIMEZONE`) se conserva byte a byte. Solo un elemento creado aquí ([`new_event_ics`],
//! [`new_task_ics`]) se escribe desde cero.

use chrono::{DateTime, Duration, NaiveDate, Utc};
use chrono_tz::Tz;
use iuredesk_dav::calendar::{EventView, ICalendar, strip_mailto};
use iuredesk_dav::ical::{Component, Property};
use iuredesk_dav::tz::local_to_utc;
use iuredesk_dav::value::{self, DateTimeValue, format_utc};
use iuredesk_model::{
    Attendee, AttendeeRole, Event, EventStatus, EventTime, Mailbox, PartStat, Reminder, Task, TaskStatus, Transparency,
};

/// Recordatorios máximos que se leen de un evento.
const MAX_REMINDERS: usize = 50;
/// Asistentes máximos que se leen de un evento.
const MAX_ATTENDEES: usize = 1_000;

// ------------------------------------------------------------------ fechas

/// Valor de fecha del archivo → [`EventTime`]. Las horas flotantes se ubican en `user_tz`.
pub fn event_time(cal: &ICalendar, v: &DateTimeValue, user_tz: Tz) -> EventTime {
    match v {
        DateTimeValue::Date(d) => EventTime::Date(*d),
        DateTimeValue::Utc(u) => EventTime::DateTime { utc: *u, tzid: None },
        DateTimeValue::Floating(n) => EventTime::DateTime { utc: local_to_utc(*n, user_tz), tzid: None },
        DateTimeValue::Zoned { local, tzid } => {
            let tz = cal.resolve_tz(tzid);
            EventTime::DateTime {
                utc: local_to_utc(*local, tz.unwrap_or(user_tz)),
                tzid: Some(tz.map(|t| t.name().to_string()).unwrap_or_else(|| tzid.clone())),
            }
        }
    }
}

/// [`EventTime`] → propiedad (`VALUE=DATE`, `TZID=` si la zona es IANA, o UTC).
pub fn time_property(name: &str, t: &EventTime) -> Property {
    match t {
        EventTime::Date(d) => DateTimeValue::Date(*d).to_property(name),
        EventTime::DateTime { utc, tzid: Some(z) } => match z.parse::<Tz>() {
            Ok(tz) if tz != chrono_tz::UTC => {
                DateTimeValue::Zoned { local: utc.with_timezone(&tz).naive_local(), tzid: tz.name().to_string() }
                    .to_property(name)
            }
            _ => DateTimeValue::Utc(*utc).to_property(name),
        },
        EventTime::DateTime { utc, tzid: None } => DateTimeValue::Utc(*utc).to_property(name),
    }
}

/// `EXDATE` normalizado: `AAAAMMDD` para días completos y `AAAAMMDDTHHMMSSZ` para instantes,
/// ordenado y sin repetidos (así el orden no cuenta como cambio).
fn exdate_strings(cal: &ICalendar, view: &EventView<'_>, user_tz: Tz) -> Vec<String> {
    let mut out: Vec<String> = view
        .exdates()
        .unwrap_or_default()
        .iter()
        .map(|v| match v {
            DateTimeValue::Date(d) => d.format("%Y%m%d").to_string(),
            other => format_utc(cal.to_utc(other, user_tz)),
        })
        .collect();
    out.sort();
    out.dedup();
    out.truncate(1_000);
    out
}

// ------------------------------------------------------------------ lectura

fn partstat(p: &iuredesk_dav::calendar::PartStat) -> PartStat {
    use iuredesk_dav::calendar::PartStat as W;
    match p {
        W::Accepted => PartStat::Accepted,
        W::Declined => PartStat::Declined,
        W::Tentative => PartStat::Tentative,
        W::Delegated => PartStat::Delegated,
        W::NeedsAction | W::Other(_) => PartStat::NeedsAction,
    }
}

/// Valor del parámetro `PARTSTAT`.
pub fn partstat_str(p: &PartStat) -> &'static str {
    match p {
        PartStat::NeedsAction => "NEEDS-ACTION",
        PartStat::Accepted => "ACCEPTED",
        PartStat::Declined => "DECLINED",
        PartStat::Tentative => "TENTATIVE",
        PartStat::Delegated => "DELEGATED",
    }
}

/// `PartStat` del modelo → el de `iuredesk-dav`.
pub fn wire_partstat(p: &PartStat) -> iuredesk_dav::calendar::PartStat {
    iuredesk_dav::calendar::PartStat::parse(Some(partstat_str(p)))
}

fn role(r: Option<&str>) -> AttendeeRole {
    match r.map(str::to_ascii_uppercase).as_deref() {
        Some("OPT-PARTICIPANT") => AttendeeRole::Optional,
        Some("CHAIR") => AttendeeRole::Chair,
        Some("NON-PARTICIPANT") => AttendeeRole::NonParticipant,
        _ => AttendeeRole::Required,
    }
}

fn role_str(r: &AttendeeRole) -> &'static str {
    match r {
        AttendeeRole::Required => "REQ-PARTICIPANT",
        AttendeeRole::Optional => "OPT-PARTICIPANT",
        AttendeeRole::Chair => "CHAIR",
        AttendeeRole::NonParticipant => "NON-PARTICIPANT",
    }
}

fn attendee_from_prop(p: &Property) -> Attendee {
    let w = iuredesk_dav::calendar::Attendee::from_property(p);
    Attendee {
        mailbox: Mailbox { name: w.name.filter(|n| !n.trim().is_empty()), addr: w.email },
        partstat: partstat(&w.partstat),
        role: role(w.role.as_deref()),
        rsvp: w.rsvp,
    }
}

fn organizer_from(comp: &Component) -> Option<Mailbox> {
    let p = comp.prop("ORGANIZER")?;
    let addr = strip_mailto(&p.value);
    if addr.is_empty() {
        return None;
    }
    Some(Mailbox { name: p.param("CN").map(str::to_string).filter(|n| !n.trim().is_empty()), addr })
}

fn status_of(comp: &Component) -> EventStatus {
    match comp.text("STATUS").map(|s| s.trim().to_ascii_uppercase()).as_deref() {
        Some("TENTATIVE") => EventStatus::Tentative,
        Some("CANCELLED") => EventStatus::Cancelled,
        _ => EventStatus::Confirmed,
    }
}

fn transp_of(comp: &Component) -> Transparency {
    match comp.text("TRANSP").map(|s| s.trim().to_ascii_uppercase()).as_deref() {
        Some("TRANSPARENT") => Transparency::Transparent,
        _ => Transparency::Opaque,
    }
}

fn utc_prop(comp: &Component, name: &str) -> Option<DateTime<Utc>> {
    let v = DateTimeValue::from_property(comp.prop(name)?).ok()?;
    Some(match v {
        DateTimeValue::Utc(u) => u,
        other => other.naive().and_utc(),
    })
}

/// Minutos antes del inicio de un `VALARM` (`None` si no se entiende el disparador).
fn alarm_minutes(cal: &ICalendar, alarm: &Component, start: &EventTime, end: &EventTime, user_tz: Tz) -> Option<i64> {
    let trig = alarm.prop("TRIGGER")?;
    if trig.param("VALUE").is_some_and(|v| v.eq_ignore_ascii_case("DATE-TIME")) {
        let at = cal.to_utc(&DateTimeValue::from_property(trig).ok()?, user_tz);
        return Some(start.to_utc().signed_duration_since(at).num_minutes());
    }
    let d = value::parse_duration(&trig.value).ok()?;
    let from_end = trig.param("RELATED").is_some_and(|r| r.eq_ignore_ascii_case("END"));
    let offset = if from_end { d.checked_add(&end.to_utc().signed_duration_since(start.to_utc()))? } else { d };
    Some(offset.num_minutes().saturating_neg())
}

/// Fin de un evento: `DTEND`, o inicio + `DURATION`, o lo que dice RFC 5545 §3.6.1.
fn end_of(cal: &ICalendar, view: &EventView<'_>, start: &EventTime, user_tz: Tz) -> EventTime {
    if let Some(e) = view.component.prop("DTEND").and_then(|p| DateTimeValue::from_property(p).ok()) {
        return event_time(cal, &e, user_tz);
    }
    let dur = view.duration();
    match start {
        EventTime::Date(d) => {
            let days = dur.map(|x| x.num_days().max(1)).unwrap_or(1);
            EventTime::Date(d.checked_add_signed(Duration::days(days)).unwrap_or(*d))
        }
        EventTime::DateTime { utc, tzid } => EventTime::DateTime {
            utc: dur.and_then(|x| utc.checked_add_signed(x.max(Duration::zero()))).unwrap_or(*utc),
            tzid: tzid.clone(),
        },
    }
}

/// El componente maestro (sin `RECURRENCE-ID`) de un `UID`, o el primero con ese `UID`.
fn master_index(cal: &ICalendar, name: &str, uid: &str) -> Option<usize> {
    let same = |c: &Component| c.name == name && EventView::new(c).uid().as_deref() == Some(uid);
    cal.root
        .components
        .iter()
        .position(|c| same(c) && c.prop("RECURRENCE-ID").is_none())
        .or_else(|| cal.root.components.iter().position(same))
}

/// Lee un `VEVENT`. `id`, `calendar`, `remote_id` y `etag` los pone quien llama.
pub fn event_from_component(cal: &ICalendar, comp: &Component, user_tz: Tz) -> Event {
    let view = EventView::new(comp);
    let start = view
        .dtstart()
        .map(|s| event_time(cal, &s, user_tz))
        .unwrap_or_else(|| EventTime::DateTime { utc: DateTime::<Utc>::UNIX_EPOCH, tzid: None });
    let end = end_of(cal, &view, &start, user_tz);
    let mut reminders: Vec<Reminder> = comp
        .children("VALARM")
        .filter_map(|a| alarm_minutes(cal, a, &start, &end, user_tz))
        .map(|minutes_before| Reminder { minutes_before })
        .collect();
    reminders.truncate(MAX_REMINDERS);
    let mut attendees: Vec<Attendee> = comp.props("ATTENDEE").map(attendee_from_prop).collect();
    attendees.truncate(MAX_ATTENDEES);
    Event {
        uid: view.uid().unwrap_or_default(),
        summary: view.summary().unwrap_or_default(),
        location: view.location().unwrap_or_default(),
        description: view.description().unwrap_or_default(),
        rrule: view.rrules().into_iter().next().filter(|r| !r.is_empty()),
        exdates: exdate_strings(cal, &view, user_tz),
        organizer: organizer_from(comp),
        attendees,
        reminders,
        status: status_of(comp),
        transparency: transp_of(comp),
        sequence: view.sequence(),
        updated: utc_prop(comp, "LAST-MODIFIED").or_else(|| view.dtstamp()),
        start,
        end,
        ..Default::default()
    }
}

/// Un [`Event`] por serie (o por excepción huérfana: invitación a una sola repetición).
pub fn events_from_ics(cal: &ICalendar, user_tz: Tz) -> Vec<Event> {
    let mut out: Vec<Event> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for c in cal.root.children("VEVENT") {
        let uid = EventView::new(c).uid().unwrap_or_default();
        if seen.contains(&uid) {
            continue;
        }
        let Some(idx) = master_index(cal, "VEVENT", &uid) else { continue };
        if let Some(m) = cal.root.components.get(idx) {
            out.push(event_from_component(cal, m, user_tz));
            seen.push(uid);
        }
    }
    out
}

fn task_status(comp: &Component) -> TaskStatus {
    match comp.text("STATUS").map(|s| s.trim().to_ascii_uppercase()).as_deref() {
        Some("IN-PROCESS") => TaskStatus::InProcess,
        Some("COMPLETED") => TaskStatus::Completed,
        Some("CANCELLED") => TaskStatus::Cancelled,
        _ => TaskStatus::NeedsAction,
    }
}

fn task_status_str(s: &TaskStatus) -> &'static str {
    match s {
        TaskStatus::NeedsAction => "NEEDS-ACTION",
        TaskStatus::InProcess => "IN-PROCESS",
        TaskStatus::Completed => "COMPLETED",
        TaskStatus::Cancelled => "CANCELLED",
    }
}

/// Lee un `VTODO`. `id`, `list`, `remote_id` y `etag` los pone quien llama.
pub fn task_from_component(cal: &ICalendar, comp: &Component, user_tz: Tz) -> Task {
    let view = EventView::new(comp);
    let num = |name: &str| comp.prop(name).and_then(|p| p.value.trim().parse::<u32>().ok());
    Task {
        uid: view.uid().unwrap_or_default(),
        summary: view.summary().unwrap_or_default(),
        description: view.description().unwrap_or_default(),
        due: comp.prop("DUE").and_then(|p| DateTimeValue::from_property(p).ok()).map(|v| event_time(cal, &v, user_tz)),
        priority: num("PRIORITY").map_or(0, |p| u8::try_from(p.min(9)).unwrap_or(0)),
        status: task_status(comp),
        percent_complete: num("PERCENT-COMPLETE").map_or(0, |p| u8::try_from(p.min(100)).unwrap_or(100)),
        completed: utc_prop(comp, "COMPLETED"),
        created: utc_prop(comp, "CREATED"),
        updated: utc_prop(comp, "LAST-MODIFIED").or_else(|| view.dtstamp()),
        start: view.dtstart().map(|v| event_time(cal, &v, user_tz)),
        reminder: comp.children("VALARM").find_map(|a| task_alarm_at(cal, comp, a, user_tz)),
        rrule: view.rrules().into_iter().next().unwrap_or_default().chars().take(1_024).collect(),
        ..Default::default()
    }
}

/// Instante de un `VALARM` de un `VTODO` (UX-6): absoluto, o relativo a `DTSTART`
/// (`RELATED=START`, el de omisión) o a `DUE` (`RELATED=END`).
fn task_alarm_at(cal: &ICalendar, todo: &Component, alarm: &Component, user_tz: Tz) -> Option<DateTime<Utc>> {
    let trig = alarm.prop("TRIGGER")?;
    if trig.param("VALUE").is_some_and(|v| v.eq_ignore_ascii_case("DATE-TIME")) {
        return Some(cal.to_utc(&DateTimeValue::from_property(trig).ok()?, user_tz));
    }
    let d = value::parse_duration(&trig.value).ok()?;
    let from_end = trig.param("RELATED").is_some_and(|r| r.eq_ignore_ascii_case("END"));
    let anchor = todo.prop(if from_end { "DUE" } else { "DTSTART" })?;
    let at = cal.to_utc(&DateTimeValue::from_property(anchor).ok()?, user_tz);
    at.checked_add_signed(d)
}

/// `VALARM` con disparador absoluto (UX-6: el recordatorio de una tarea).
fn task_alarm(at: DateTime<Utc>) -> Component {
    let mut a = Component::new("VALARM");
    a.push(Property::new("ACTION", "DISPLAY"));
    a.push(Property::text("DESCRIPTION", "Reminder"));
    a.push(Property::new("TRIGGER", &format_utc(at)).with_param("VALUE", "DATE-TIME"));
    a
}

/// Un [`Task`] por `VTODO` maestro.
pub fn tasks_from_ics(cal: &ICalendar, user_tz: Tz) -> Vec<Task> {
    let mut out: Vec<Task> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for c in cal.root.children("VTODO") {
        let uid = EventView::new(c).uid().unwrap_or_default();
        if seen.contains(&uid) {
            continue;
        }
        let Some(idx) = master_index(cal, "VTODO", &uid) else { continue };
        if let Some(m) = cal.root.components.get(idx) {
            out.push(task_from_component(cal, m, user_tz));
            seen.push(uid);
        }
    }
    out
}

// ------------------------------------------------------------------ escritura

/// Reemplaza todas las propiedades `name` por `props`, en el lugar de la primera (o al final).
pub(crate) fn replace_all(comp: &mut Component, name: &str, props: Vec<Property>) {
    let at = comp.properties.iter().position(|p| p.name.eq_ignore_ascii_case(name));
    comp.remove(name);
    let at = at.unwrap_or(comp.properties.len()).min(comp.properties.len());
    comp.properties.splice(at..at, props);
}

fn set_text(comp: &mut Component, name: &str, text: &str) {
    if text.is_empty() {
        comp.remove(name);
        return;
    }
    match comp.prop_mut(name) {
        // Se conservan los parámetros (`LANGUAGE`, `ALTREP`…).
        Some(p) => p.value = value::escape_text(text),
        None => comp.push(Property::text(name, text)),
    }
}

/// `EXDATE` normalizados (ver `exdate_strings`) → propiedades.
fn exdate_props(exdates: &[String]) -> Vec<Property> {
    let (dates, instants): (Vec<&String>, Vec<&String>) = exdates.iter().partition(|x| x.len() == 8);
    let join = |v: &[&String]| v.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(",");
    let mut props = Vec::new();
    if !dates.is_empty() {
        props.push(Property::new("EXDATE", &join(&dates)).with_param("VALUE", "DATE"));
    }
    if !instants.is_empty() {
        props.push(Property::new("EXDATE", &join(&instants)));
    }
    props
}

fn set_time(comp: &mut Component, name: &str, t: &EventTime) {
    let mut new = time_property(name, t);
    // Parámetros ajenos (`X-…`) del original se conservan.
    if let Some(old) = comp.prop(name) {
        for p in &old.params {
            if !matches!(p.name.as_str(), "TZID" | "VALUE") {
                new.params.push(p.clone());
            }
        }
    }
    replace_all(comp, name, vec![new]);
}

fn mailbox_prop(name: &str, m: &Mailbox) -> Property {
    let mut p = Property::new(name, &format!("mailto:{}", m.addr));
    if let Some(n) = m.name.as_deref().filter(|n| !n.trim().is_empty()) {
        p.set_param("CN", n);
    }
    p
}

fn attendee_props(comp: &Component, attendees: &[Attendee]) -> Vec<Property> {
    attendees
        .iter()
        .map(|a| {
            let addr = a.mailbox.addr.trim().to_lowercase();
            match comp.props("ATTENDEE").find(|p| strip_mailto(&p.value) == addr) {
                Some(orig) => {
                    // Solo se toca lo que cambió respecto a lo que se leyó de esa línea.
                    let mut p = orig.clone();
                    let was = attendee_from_prop(orig);
                    if was.partstat != a.partstat {
                        p.set_param("PARTSTAT", partstat_str(&a.partstat));
                    }
                    if was.role != a.role {
                        p.set_param("ROLE", role_str(&a.role));
                    }
                    if was.rsvp != a.rsvp {
                        if a.rsvp {
                            p.set_param("RSVP", "TRUE");
                        } else {
                            p.remove_param("RSVP");
                        }
                    }
                    if was.mailbox.name != a.mailbox.name {
                        match a.mailbox.name.as_deref().filter(|n| !n.trim().is_empty()) {
                            Some(n) => p.set_param("CN", n),
                            None => p.remove_param("CN"),
                        }
                    }
                    p
                }
                None => {
                    let mut p = mailbox_prop("ATTENDEE", &a.mailbox);
                    p.set_param("ROLE", role_str(&a.role));
                    p.set_param("PARTSTAT", partstat_str(&a.partstat));
                    if a.rsvp {
                        p.set_param("RSVP", "TRUE");
                    }
                    p
                }
            }
        })
        .collect()
}

fn alarm_component(minutes_before: i64) -> Component {
    let mut a = Component::new("VALARM");
    a.push(Property::new("ACTION", "DISPLAY"));
    a.push(Property::text("DESCRIPTION", "Reminder"));
    a.push(Property::new("TRIGGER", &value::format_duration(Duration::minutes(minutes_before.saturating_neg()))));
    a
}

fn patch_alarms(
    cal: &ICalendar,
    comp: &mut Component,
    start: &EventTime,
    end: &EventTime,
    new: &[Reminder],
    user_tz: Tz,
) {
    let mut wanted: Vec<i64> = new.iter().map(|r| r.minutes_before).collect();
    let mut kept: Vec<Component> = Vec::new();
    let mut others: Vec<Component> = Vec::new();
    for c in std::mem::take(&mut comp.components) {
        if c.name != "VALARM" {
            others.push(c);
            continue;
        }
        // Las que ya no están en la lista nueva se quitan.
        if let Some(pos) = alarm_minutes(cal, &c, start, end, user_tz).and_then(|m| wanted.iter().position(|w| *w == m))
        {
            wanted.remove(pos);
            kept.push(c);
        }
    }
    kept.extend(wanted.into_iter().map(alarm_component));
    others.extend(kept);
    comp.components = others;
}

fn touch(comp: &mut Component, now: DateTime<Utc>) {
    comp.set(Property::new("DTSTAMP", &format_utc(now)));
    if comp.prop("LAST-MODIFIED").is_some() {
        comp.set(Property::new("LAST-MODIFIED", &format_utc(now)));
    }
}

/// Aplica a `cal` los cambios de `old` → `new` del evento `new.uid` (componente maestro). Devuelve
/// `true` si cambió algo. Si el `UID` no está en `cal`, agrega un `VEVENT` nuevo.
pub fn patch_event(cal: &mut ICalendar, old: &Event, new: &Event, user_tz: Tz, now: DateTime<Utc>) -> bool {
    let Some(idx) = master_index(cal, "VEVENT", &new.uid) else {
        cal.root.components.push(new_vevent(new, now));
        return true;
    };
    let snapshot = cal.clone();
    let Some(comp) = cal.root.components.get_mut(idx) else { return false };
    let mut changed = false;
    if old.summary != new.summary {
        set_text(comp, "SUMMARY", &new.summary);
        changed = true;
    }
    if old.location != new.location {
        set_text(comp, "LOCATION", &new.location);
        changed = true;
    }
    if old.description != new.description {
        set_text(comp, "DESCRIPTION", &new.description);
        changed = true;
    }
    if old.start != new.start {
        set_time(comp, "DTSTART", &new.start);
        changed = true;
    }
    if old.end != new.end || (old.start != new.start && comp.prop("DTEND").is_none()) {
        comp.remove("DURATION");
        set_time(comp, "DTEND", &new.end);
        changed = true;
    }
    if old.rrule != new.rrule {
        match new.rrule.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
            Some(r) => comp.set(Property::new("RRULE", r)),
            None => comp.remove("RRULE"),
        }
        changed = true;
    }
    if old.exdates != new.exdates {
        let props = exdate_props(&new.exdates);
        replace_all(comp, "EXDATE", props);
        changed = true;
    }
    if old.organizer != new.organizer {
        match &new.organizer {
            Some(m) => replace_all(comp, "ORGANIZER", vec![mailbox_prop("ORGANIZER", m)]),
            None => comp.remove("ORGANIZER"),
        }
        changed = true;
    }
    if old.attendees != new.attendees {
        let props = attendee_props(comp, &new.attendees);
        replace_all(comp, "ATTENDEE", props);
        changed = true;
    }
    if old.reminders != new.reminders {
        patch_alarms(&snapshot, comp, &new.start, &new.end, &new.reminders, user_tz);
        changed = true;
    }
    if old.status != new.status {
        let s = match new.status {
            EventStatus::Confirmed => "CONFIRMED",
            EventStatus::Tentative => "TENTATIVE",
            EventStatus::Cancelled => "CANCELLED",
        };
        comp.set(Property::new("STATUS", s));
        changed = true;
    }
    if old.transparency != new.transparency {
        let t = match new.transparency {
            Transparency::Opaque => "OPAQUE",
            Transparency::Transparent => "TRANSPARENT",
        };
        comp.set(Property::new("TRANSP", t));
        changed = true;
    }
    if old.sequence != new.sequence {
        comp.set(Property::new("SEQUENCE", &new.sequence.to_string()));
        changed = true;
    }
    if changed {
        touch(comp, now);
    }
    changed
}

fn new_vevent(ev: &Event, now: DateTime<Utc>) -> Component {
    let mut c = Component::new("VEVENT");
    c.push(Property::text("UID", &ev.uid));
    c.push(Property::new("DTSTAMP", &format_utc(now)));
    c.push(Property::new("CREATED", &format_utc(now)));
    c.push(Property::new("LAST-MODIFIED", &format_utc(now)));
    c.push(time_property("DTSTART", &ev.start));
    c.push(time_property("DTEND", &ev.end));
    if ev.sequence > 0 {
        c.push(Property::new("SEQUENCE", &ev.sequence.to_string()));
    }
    for (name, text) in [("SUMMARY", &ev.summary), ("LOCATION", &ev.location), ("DESCRIPTION", &ev.description)] {
        if !text.is_empty() {
            c.push(Property::text(name, text));
        }
    }
    if let Some(r) = ev.rrule.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
        c.push(Property::new("RRULE", r));
    }
    if let Some(o) = &ev.organizer {
        c.push(mailbox_prop("ORGANIZER", o));
    }
    if !ev.attendees.is_empty() {
        let props = attendee_props(&c, &ev.attendees);
        c.properties.extend(props);
    }
    if ev.status != EventStatus::Confirmed {
        c.push(Property::new("STATUS", if ev.status == EventStatus::Tentative { "TENTATIVE" } else { "CANCELLED" }));
    }
    if ev.transparency == Transparency::Transparent {
        c.push(Property::new("TRANSP", "TRANSPARENT"));
    }
    c.properties.extend(exdate_props(&ev.exdates));
    for r in &ev.reminders {
        c.components.push(alarm_component(r.minutes_before));
    }
    c
}

/// `VCALENDAR` nuevo con un solo `VEVENT` (para crear en el servidor).
pub fn new_event_ics(ev: &Event, now: DateTime<Utc>) -> ICalendar {
    let mut cal = ICalendar::new();
    cal.root.components.push(new_vevent(ev, now));
    cal
}

/// Aplica a `cal` los cambios `old` → `new` del `VTODO` `new.uid`. Devuelve `true` si cambió algo.
pub fn patch_task(cal: &mut ICalendar, old: &Task, new: &Task, now: DateTime<Utc>) -> bool {
    let Some(idx) = master_index(cal, "VTODO", &new.uid) else {
        cal.root.components.push(new_vtodo(new, now));
        return true;
    };
    // Instante de cada `VALARM` (para quitar solo el que era el recordatorio).
    let alarm_times: Vec<Option<DateTime<Utc>>> = cal
        .root
        .components
        .get(idx)
        .map(|c| {
            c.components
                .iter()
                .map(|a| if a.name == "VALARM" { task_alarm_at(cal, c, a, chrono_tz::UTC) } else { None })
                .collect()
        })
        .unwrap_or_default();
    let Some(comp) = cal.root.components.get_mut(idx) else { return false };
    let mut changed = false;
    if old.summary != new.summary {
        set_text(comp, "SUMMARY", &new.summary);
        changed = true;
    }
    if old.description != new.description {
        set_text(comp, "DESCRIPTION", &new.description);
        changed = true;
    }
    if old.due != new.due {
        match &new.due {
            Some(d) => set_time(comp, "DUE", d),
            None => comp.remove("DUE"),
        }
        changed = true;
    }
    if old.priority != new.priority {
        if new.priority == 0 {
            comp.remove("PRIORITY");
        } else {
            comp.set(Property::new("PRIORITY", &new.priority.min(9).to_string()));
        }
        changed = true;
    }
    if old.status != new.status {
        comp.set(Property::new("STATUS", task_status_str(&new.status)));
        changed = true;
    }
    if old.percent_complete != new.percent_complete {
        comp.set(Property::new("PERCENT-COMPLETE", &new.percent_complete.min(100).to_string()));
        changed = true;
    }
    if old.completed != new.completed {
        match new.completed {
            Some(t) => comp.set(Property::new("COMPLETED", &format_utc(t))),
            None => comp.remove("COMPLETED"),
        }
        changed = true;
    }
    if old.start != new.start {
        match &new.start {
            Some(d) => set_time(comp, "DTSTART", d),
            None => comp.remove("DTSTART"),
        }
        changed = true;
    }
    if old.rrule != new.rrule {
        match new.rrule.trim() {
            "" => comp.remove("RRULE"),
            r => comp.set(Property::new("RRULE", r)),
        }
        changed = true;
    }
    if old.reminder != new.reminder {
        // Solo se quita la alarma que era el recordatorio; las demás (con sonido, de otro
        // cliente…) se conservan.
        let mut kept = Vec::new();
        let mut removed = false;
        let subs = std::mem::take(&mut comp.components);
        for (i, c) in subs.into_iter().enumerate() {
            let at = alarm_times.get(i).copied().flatten();
            if !removed && c.name == "VALARM" && old.reminder.is_some() && at == old.reminder {
                removed = true;
                continue;
            }
            kept.push(c);
        }
        comp.components = kept;
        if let Some(at) = new.reminder {
            comp.components.push(task_alarm(at));
        }
        changed = true;
    }
    if changed {
        touch(comp, now);
    }
    changed
}

fn new_vtodo(t: &Task, now: DateTime<Utc>) -> Component {
    let mut c = Component::new("VTODO");
    c.push(Property::text("UID", &t.uid));
    c.push(Property::new("DTSTAMP", &format_utc(now)));
    c.push(Property::new("CREATED", &format_utc(t.created.unwrap_or(now))));
    c.push(Property::new("LAST-MODIFIED", &format_utc(now)));
    c.push(Property::text("SUMMARY", &t.summary));
    if !t.description.is_empty() {
        c.push(Property::text("DESCRIPTION", &t.description));
    }
    if let Some(d) = &t.due {
        c.push(time_property("DUE", d));
    }
    if t.priority > 0 {
        c.push(Property::new("PRIORITY", &t.priority.min(9).to_string()));
    }
    c.push(Property::new("STATUS", task_status_str(&t.status)));
    if t.percent_complete > 0 {
        c.push(Property::new("PERCENT-COMPLETE", &t.percent_complete.min(100).to_string()));
    }
    if let Some(done) = t.completed {
        c.push(Property::new("COMPLETED", &format_utc(done)));
    }
    if let Some(s) = &t.start {
        c.push(time_property("DTSTART", s));
    }
    if !t.rrule.trim().is_empty() {
        c.push(Property::new("RRULE", t.rrule.trim()));
    }
    if let Some(at) = t.reminder {
        c.components.push(task_alarm(at));
    }
    c
}

/// `VCALENDAR` nuevo con un solo `VTODO`.
pub fn new_task_ics(t: &Task, now: DateTime<Utc>) -> ICalendar {
    let mut cal = ICalendar::new();
    cal.root.components.push(new_vtodo(t, now));
    cal
}

/// Fecha de un día completo o la fecha UTC de un instante (para comparaciones).
pub fn date_of(t: &EventTime) -> NaiveDate {
    match t {
        EventTime::Date(d) => *d,
        EventTime::DateTime { utc, .. } => utc.date_naive(),
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    const SERIES: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Apple Inc.//macOS 15//EN\r\n\
BEGIN:VTIMEZONE\r\nTZID:Europe/Madrid\r\nX-LIC-LOCATION:Europe/Madrid\r\nEND:VTIMEZONE\r\n\
BEGIN:VEVENT\r\nUID:serie-1\r\nDTSTAMP:20261001T000000Z\r\nSEQUENCE:3\r\n\
DTSTART;TZID=Europe/Madrid:20261005T090000\r\nDTEND;TZID=Europe/Madrid:20261005T100000\r\n\
RRULE:FREQ=WEEKLY;BYDAY=MO\r\nEXDATE;TZID=Europe/Madrid:20261012T090000\r\n\
SUMMARY;LANGUAGE=es:Reunión semanal\r\nLOCATION:Sala 2\r\n\
ORGANIZER;CN=Ana:mailto:ana@example.com\r\n\
ATTENDEE;CN=Eduardo;PARTSTAT=NEEDS-ACTION;RSVP=TRUE;X-NUM-GUESTS=0:mailto:eduardo@example.com\r\n\
ATTENDEE;CN=Ana;PARTSTAT=ACCEPTED;ROLE=CHAIR:mailto:ana@example.com\r\n\
X-APPLE-TRAVEL-ADVISORY-BEHAVIOR:AUTOMATIC\r\nTRANSP:OPAQUE\r\n\
BEGIN:VALARM\r\nACTION:AUDIO\r\nTRIGGER:-PT15M\r\nX-WR-ALARMUID:abc\r\nATTACH;VALUE=URI:Chord\r\nEND:VALARM\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:serie-1\r\nRECURRENCE-ID;TZID=Europe/Madrid:20261019T090000\r\nDTSTAMP:20261001T000000Z\r\n\
DTSTART;TZID=Europe/Madrid:20261019T113000\r\nDTEND;TZID=Europe/Madrid:20261019T123000\r\n\
SUMMARY:Reunión movida\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    #[test]
    fn reads_series() {
        let cal = ICalendar::parse(SERIES).unwrap();
        let evs = events_from_ics(&cal, chrono_tz::UTC);
        assert_eq!(evs.len(), 1);
        let e = &evs[0];
        assert_eq!(e.uid, "serie-1");
        assert_eq!(e.summary, "Reunión semanal");
        assert_eq!(
            e.start,
            EventTime::DateTime {
                utc: Utc.with_ymd_and_hms(2026, 10, 5, 7, 0, 0).unwrap(),
                tzid: Some("Europe/Madrid".into())
            }
        );
        assert_eq!(e.rrule.as_deref(), Some("FREQ=WEEKLY;BYDAY=MO"));
        assert_eq!(e.exdates, vec!["20261012T070000Z".to_string()]);
        assert_eq!(e.organizer.as_ref().unwrap().addr, "ana@example.com");
        assert_eq!(e.attendees.len(), 2);
        assert_eq!(e.attendees[1].role, AttendeeRole::Chair);
        assert_eq!(e.reminders, vec![Reminder { minutes_before: 15 }]);
        assert_eq!(e.sequence, 3);
    }

    #[test]
    fn unchanged_patch_is_noop_and_edit_keeps_unknown() {
        let mut cal = ICalendar::parse(SERIES).unwrap();
        let old = events_from_ics(&cal, chrono_tz::UTC).remove(0);
        let now = Utc.with_ymd_and_hms(2026, 10, 26, 12, 0, 0).unwrap();
        let canonical = ICalendar::parse(SERIES).unwrap().to_ics();
        assert!(!patch_event(&mut cal, &old, &old.clone(), chrono_tz::UTC, now));
        assert_eq!(cal.to_ics(), canonical);

        // RSVP + título: el resto queda igual (X-…, alarma de audio, excepción, VTIMEZONE).
        let mut new = old.clone();
        new.summary = "Reunión semanal (nueva)".into();
        new.attendees[0].partstat = PartStat::Accepted;
        assert!(patch_event(&mut cal, &old, &new, chrono_tz::UTC, now));
        let out = cal.to_ics();
        assert!(out.contains("SUMMARY;LANGUAGE=es:Reunión semanal (nueva)"));
        assert!(
            out.replace("\r\n ", "").contains("PARTSTAT=ACCEPTED;RSVP=TRUE;X-NUM-GUESTS=0:mailto:eduardo@example.com")
        );
        assert!(out.contains("X-APPLE-TRAVEL-ADVISORY-BEHAVIOR:AUTOMATIC"));
        assert!(out.contains("X-WR-ALARMUID:abc"));
        assert!(out.contains("RECURRENCE-ID;TZID=Europe/Madrid:20261019T090000"));
        assert!(out.contains("DTSTART;TZID=Europe/Madrid:20261005T090000"));
        let back = events_from_ics(&ICalendar::parse(&out).unwrap(), chrono_tz::UTC).remove(0);
        assert_eq!(back.summary, new.summary);
        assert_eq!(back.attendees, new.attendees);
        assert_eq!(back.reminders, new.reminders);
    }

    #[test]
    fn edit_times_alarms_exdates() {
        let mut cal = ICalendar::parse(SERIES).unwrap();
        let old = events_from_ics(&cal, chrono_tz::UTC).remove(0);
        let mut new = old.clone();
        new.start = EventTime::DateTime {
            utc: Utc.with_ymd_and_hms(2026, 10, 5, 8, 0, 0).unwrap(),
            tzid: Some("Europe/Madrid".into()),
        };
        new.end = EventTime::DateTime {
            utc: Utc.with_ymd_and_hms(2026, 10, 5, 9, 0, 0).unwrap(),
            tzid: Some("Europe/Madrid".into()),
        };
        new.reminders = vec![Reminder { minutes_before: 15 }, Reminder { minutes_before: 60 }];
        new.exdates.push("20261026".into());
        new.exdates.sort();
        new.status = EventStatus::Tentative;
        new.transparency = Transparency::Transparent;
        let now = Utc.with_ymd_and_hms(2026, 10, 26, 12, 0, 0).unwrap();
        assert!(patch_event(&mut cal, &old, &new, chrono_tz::UTC, now));
        let out = cal.to_ics();
        assert!(out.contains("DTSTART;TZID=Europe/Madrid:20261005T100000"));
        assert!(out.contains("X-WR-ALARMUID:abc"), "la alarma que sigue se conserva");
        let back = events_from_ics(&ICalendar::parse(&out).unwrap(), chrono_tz::UTC).remove(0);
        assert_eq!(back.start, new.start);
        assert_eq!(back.end, new.end);
        let mut r = back.reminders.clone();
        r.sort_by_key(|x| x.minutes_before);
        assert_eq!(r, new.reminders);
        assert_eq!(back.exdates, new.exdates);
        assert_eq!(back.status, EventStatus::Tentative);
        assert_eq!(back.transparency, Transparency::Transparent);
    }

    #[test]
    fn new_event_round_trip() {
        let ev = Event {
            uid: "n1".into(),
            summary: "Comida, con; Juan".into(),
            start: EventTime::Date(NaiveDate::from_ymd_opt(2026, 11, 1).unwrap()),
            end: EventTime::Date(NaiveDate::from_ymd_opt(2026, 11, 2).unwrap()),
            rrule: Some("FREQ=YEARLY".into()),
            exdates: vec!["20271101".into()],
            reminders: vec![Reminder { minutes_before: 1440 }],
            organizer: Some(Mailbox::new("Yo", "yo@example.com")),
            attendees: vec![Attendee { mailbox: Mailbox::addr("juan@example.com"), rsvp: true, ..Default::default() }],
            ..Default::default()
        };
        let now = Utc.with_ymd_and_hms(2026, 10, 26, 12, 0, 0).unwrap();
        let ics = new_event_ics(&ev, now).to_ics();
        assert!(ics.contains(&format!("DTSTAMP:{}", format_utc(now))));
        let back = events_from_ics(&ICalendar::parse(&ics).unwrap(), chrono_tz::UTC).remove(0);
        assert_eq!(back.summary, ev.summary);
        assert_eq!(back.start, ev.start);
        assert_eq!(back.end, ev.end);
        assert_eq!(back.rrule, ev.rrule);
        assert_eq!(back.exdates, ev.exdates);
        assert_eq!(back.reminders, ev.reminders);
        assert_eq!(back.organizer, ev.organizer);
        assert_eq!(back.attendees, ev.attendees);
    }

    #[test]
    fn duration_and_floating() {
        let src = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:d\r\nDTSTART:20261101T100000\r\nDURATION:PT90M\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let mut cal = ICalendar::parse(src).unwrap();
        let tz: Tz = "America/Mexico_City".parse().unwrap();
        let e = events_from_ics(&cal, tz).remove(0);
        assert_eq!(e.start.to_utc(), Utc.with_ymd_and_hms(2026, 11, 1, 16, 0, 0).unwrap());
        assert_eq!(e.end.to_utc(), Utc.with_ymd_and_hms(2026, 11, 1, 17, 30, 0).unwrap());
        let mut n = e.clone();
        n.end = EventTime::DateTime { utc: Utc.with_ymd_and_hms(2026, 11, 1, 18, 0, 0).unwrap(), tzid: None };
        assert!(patch_event(&mut cal, &e, &n, tz, Utc::now()));
        let out = cal.to_ics();
        assert!(!out.contains("DURATION"));
        assert!(out.contains("DTEND:20261101T180000Z"));
        assert!(out.contains("DTSTART:20261101T100000\r\n"), "inicio flotante intacto");
    }

    #[test]
    fn tasks() {
        let src = "BEGIN:VCALENDAR\r\nBEGIN:VTODO\r\nUID:t1\r\nSUMMARY:Pagar\r\nDUE;VALUE=DATE:20261103\r\n\
PRIORITY:1\r\nSTATUS:NEEDS-ACTION\r\nX-APPLE-SORT-ORDER:5\r\nEND:VTODO\r\nEND:VCALENDAR\r\n";
        let mut cal = ICalendar::parse(src).unwrap();
        let t = tasks_from_ics(&cal, chrono_tz::UTC).remove(0);
        assert_eq!(t.priority, 1);
        assert_eq!(t.due, Some(EventTime::Date(NaiveDate::from_ymd_opt(2026, 11, 3).unwrap())));
        let mut done = t.clone();
        let now = Utc.with_ymd_and_hms(2026, 10, 26, 12, 0, 0).unwrap();
        done.status = TaskStatus::Completed;
        done.completed = Some(now);
        done.percent_complete = 100;
        assert!(patch_task(&mut cal, &t, &done, now));
        let out = cal.to_ics();
        assert!(out.contains("X-APPLE-SORT-ORDER:5"));
        let back = tasks_from_ics(&ICalendar::parse(&out).unwrap(), chrono_tz::UTC).remove(0);
        assert_eq!(back.status, TaskStatus::Completed);
        assert_eq!(back.completed, Some(now));
        assert_eq!(back.percent_complete, 100);
        let fresh = new_task_ics(&done, now).to_ics();
        let b2 = tasks_from_ics(&ICalendar::parse(&fresh).unwrap(), chrono_tz::UTC).remove(0);
        assert_eq!(b2.summary, "Pagar");
        assert_eq!(b2.due, done.due);
    }

    /// UX-6: inicio, repetición y recordatorio de un `VTODO`; al editar solo cambia lo tocado y
    /// la alarma con sonido de otro cliente se conserva.
    #[test]
    fn task_start_repeat_and_reminder() {
        let src = "BEGIN:VCALENDAR\r\nBEGIN:VTODO\r\nUID:t2\r\nSUMMARY:Revisar\r\n\
DTSTART:20261101T090000Z\r\nDUE:20261103T170000Z\r\nRRULE:FREQ=WEEKLY;BYDAY=MO\r\n\
BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER;RELATED=END:-PT1H\r\nEND:VALARM\r\n\
BEGIN:VALARM\r\nACTION:AUDIO\r\nTRIGGER;VALUE=DATE-TIME:20261102T080000Z\r\nX-OTRO:1\r\nEND:VALARM\r\n\
END:VTODO\r\nEND:VCALENDAR\r\n";
        let mut cal = ICalendar::parse(src).unwrap();
        let t = tasks_from_ics(&cal, chrono_tz::UTC).remove(0);
        let at = |d, h| Utc.with_ymd_and_hms(2026, 11, d, h, 0, 0).unwrap();
        assert_eq!(t.start, Some(EventTime::DateTime { utc: at(1, 9), tzid: None }));
        assert_eq!(t.rrule, "FREQ=WEEKLY;BYDAY=MO");
        assert_eq!(t.reminder, Some(at(3, 16)), "relativo al vencimiento");
        // Sin cambios no se toca nada.
        assert!(!patch_task(&mut cal.clone(), &t, &t, at(1, 0)));
        let mut new = t.clone();
        new.reminder = Some(at(3, 8));
        new.rrule = "FREQ=DAILY".into();
        new.start = None;
        assert!(patch_task(&mut cal, &t, &new, at(1, 0)));
        let out = cal.to_ics();
        assert!(out.contains("X-OTRO:1"), "la otra alarma sigue");
        assert!(!out.contains("RELATED=END"), "la vieja se quitó");
        assert!(out.contains("TRIGGER;VALUE=DATE-TIME:20261103T080000Z"));
        assert!(out.contains("RRULE:FREQ=DAILY") && !out.contains("DTSTART"));
        let back = tasks_from_ics(&ICalendar::parse(&out).unwrap(), chrono_tz::UTC).remove(0);
        assert_eq!((back.start, back.rrule.as_str()), (None, "FREQ=DAILY"));
        // Una tarea nueva lleva todo.
        let fresh = new_task_ics(&t, at(1, 0)).to_ics();
        let b2 = tasks_from_ics(&ICalendar::parse(&fresh).unwrap(), chrono_tz::UTC).remove(0);
        assert_eq!((b2.start, b2.reminder, b2.rrule), (t.start, t.reminder, t.rrule));
    }

    proptest::proptest! {
        #[test]
        fn mapping_never_panics(s in "\\PC{0,400}") {
            let src = format!("BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\n{s}\r\nEND:VEVENT\r\nBEGIN:VTODO\r\n{s}\r\nEND:VTODO\r\nEND:VCALENDAR\r\n");
            if let Ok(cal) = ICalendar::parse(&src) {
                let _ = events_from_ics(&cal, chrono_tz::UTC);
                let _ = tasks_from_ics(&cal, chrono_tz::UTC);
            }
        }
    }
}
