//! Formulario del editor de eventos ([`EventDraft`]) ⇄ [`Event`] del modelo.
//!
//! Las horas se editan como fecha + minutos del día en la zona del evento (la de su `TZID`, o la
//! del usuario); al guardar se convierten a UTC y se conserva el `TZID` (así una serie sigue a su
//! hora local tras el cambio de horario). Lo que el formulario no muestra (organizador,
//! `SEQUENCE`, `remote_id`, `ETag`, excepciones…) sale intacto del evento original.

use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use iuredesk_model::{
    Attendee, AttendeeRole, CalendarId, Event, EventId, EventTime, Mailbox, PartStat, Reminder, Transparency,
};

use super::drag::{DAY_MIN, at_minutes, minutes_of};
use super::repeat::Repeat;
use crate::lang::Text;
use crate::time::Zone;

/// Recordatorios que ofrece el editor (minutos antes).
pub const REMINDER_CHOICES: [i64; 9] = [0, 5, 10, 15, 30, 60, 120, 1_440, 10_080];

/// Texto de un recordatorio («15 minutos antes»).
pub fn reminder_label(min: i64) -> String {
    let es = crate::lang::spanish();
    match min {
        0 => crate::lang::pick("At start time", "Al empezar").into(),
        m if m % 10_080 == 0 => {
            let n = m / 10_080;
            if es {
                format!("{n} semana{} antes", if n == 1 { "" } else { "s" })
            } else {
                format!("{n} week{} before", if n == 1 { "" } else { "s" })
            }
        }
        m if m % 1_440 == 0 => {
            let n = m / 1_440;
            if es {
                format!("{n} día{} antes", if n == 1 { "" } else { "s" })
            } else {
                format!("{n} day{} before", if n == 1 { "" } else { "s" })
            }
        }
        m if m % 60 == 0 => {
            let n = m / 60;
            if es {
                format!("{n} hora{} antes", if n == 1 { "" } else { "s" })
            } else {
                format!("{n} hour{} before", if n == 1 { "" } else { "s" })
            }
        }
        m if m < 0 => crate::lang::pick("After start", "Después de empezar").into(),
        m => {
            if es {
                format!("{m} minutos antes")
            } else {
                format!("{m} minutes before")
            }
        }
    }
}

/// Zonas horarias que el selector ofrece primero (luego, cualquiera IANA escrita a mano).
pub const COMMON_ZONES: [&str; 16] = [
    "America/Mexico_City",
    "America/Bogota",
    "America/Lima",
    "America/Santiago",
    "America/Argentina/Buenos_Aires",
    "America/Caracas",
    "America/New_York",
    "America/Chicago",
    "America/Denver",
    "America/Los_Angeles",
    "Europe/Madrid",
    "Europe/London",
    "Europe/Paris",
    "Asia/Tokyo",
    "Australia/Sydney",
    "UTC",
];

/// Formulario del editor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventDraft {
    /// Evento (serie maestra) que se edita; `None` = nuevo.
    pub id: Option<EventId>,
    /// Inicio original de la aparición, si se editó desde una aparición de una serie.
    pub occurrence: Option<EventTime>,
    pub calendar: CalendarId,
    pub title: String,
    pub location: String,
    pub description: String,
    pub all_day: bool,
    pub start_date: NaiveDate,
    /// Minutos del día (0–1440) del inicio.
    pub start_min: i64,
    /// Último día (en días completos, incluido).
    pub end_date: NaiveDate,
    pub end_min: i64,
    /// Zona IANA del evento; `None` = la del usuario.
    pub tz: Option<String>,
    pub repeat: Repeat,
    /// Minutos antes del inicio.
    pub reminders: Vec<i64>,
    pub attendees: Vec<Attendee>,
    /// Ocupado (`OPAQUE`) o disponible.
    pub busy: bool,
    pub private: bool,
    /// Evento original (base de la edición).
    pub base: Option<Event>,
}

/// Zona de un nombre IANA.
pub fn parse_tz(name: &str) -> Option<Tz> {
    name.trim().parse().ok()
}

fn local_in(t: DateTime<Utc>, tz: Option<Tz>, zone: Zone) -> NaiveDateTime {
    match tz {
        Some(z) => t.with_timezone(&z).naive_local(),
        None => zone.local(t),
    }
}

fn to_utc_in(t: NaiveDateTime, tz: Option<Tz>, zone: Zone) -> DateTime<Utc> {
    match tz {
        Some(z) => z.from_local_datetime(&t).earliest().map_or_else(|| zone.to_utc(t), |d| d.with_timezone(&Utc)),
        None => zone.to_utc(t),
    }
}

impl EventDraft {
    /// Evento nuevo de `start` a `end` (horas locales del usuario).
    pub fn new_at(calendar: CalendarId, start: NaiveDateTime, end: NaiveDateTime, all_day: bool) -> EventDraft {
        let end = if end <= start { start + Duration::minutes(30) } else { end };
        let (end_date, end_min) = if all_day {
            // `end` llega exclusivo (día siguiente): el formulario muestra el último día.
            let last = if end.date() > start.date() { end.date() - Duration::days(1) } else { start.date() };
            (last, 0)
        } else if minutes_of(end) == 0 && end.date() > start.date() {
            (end.date() - Duration::days(1), DAY_MIN)
        } else {
            (end.date(), minutes_of(end))
        };
        EventDraft {
            id: None,
            occurrence: None,
            calendar,
            title: String::new(),
            location: String::new(),
            description: String::new(),
            all_day,
            start_date: start.date(),
            start_min: if all_day { 9 * 60 } else { minutes_of(start) },
            end_date,
            end_min: if all_day { 10 * 60 } else { end_min },
            tz: None,
            repeat: Repeat::none(),
            reminders: vec![15],
            attendees: Vec::new(),
            busy: !all_day,
            private: false,
            base: None,
        }
    }

    /// Formulario de un evento existente. `occurrence` = (inicio, fin) de la aparición pulsada
    /// si el evento se repite (el formulario muestra esas fechas).
    pub fn from_event(ev: &Event, occurrence: Option<(EventTime, EventTime)>, zone: Zone, private: bool) -> Self {
        // Se edita en la zona del usuario; la del evento (`TZID`) se conserva al guardar.
        let tz: Option<Tz> = None;
        let (start, end) = occurrence.clone().unwrap_or_else(|| (ev.start.clone(), ev.end.clone()));
        let mut d = match (&start, &end) {
            (EventTime::Date(a), b) => {
                let b = match b {
                    EventTime::Date(b) => *b,
                    other => other.to_utc().date_naive(),
                };
                let mut d = EventDraft::new_at(ev.calendar, at_minutes(*a, 0), at_minutes(b.max(*a), 0), true);
                if b <= *a {
                    d.end_date = *a;
                }
                d
            }
            (s, e) => {
                let s = local_in(s.to_utc(), tz, zone);
                let e = local_in(e.to_utc(), tz, zone);
                EventDraft::new_at(ev.calendar, s, e, false)
            }
        };
        d.id = Some(ev.id).filter(|i| !i.is_unassigned());
        d.occurrence = if ev.is_recurring() { occurrence.map(|(s, _)| s) } else { None };
        d.title = ev.summary.clone();
        d.location = ev.location.clone();
        d.description = ev.description.clone();
        d.tz = None;
        d.repeat = Repeat::from_rrule(ev.rrule.as_deref(), d.start_date);
        d.reminders = ev.reminders.iter().map(|r| r.minutes_before).collect();
        d.attendees = ev.attendees.clone();
        d.busy = ev.transparency == Transparency::Opaque;
        d.private = private;
        d.base = Some(ev.clone());
        d
    }

    /// Zona en que se editan las horas.
    pub fn zone_tz(&self) -> Option<Tz> {
        self.tz.as_deref().and_then(parse_tz)
    }

    /// Inicio y fin como instantes del modelo.
    pub fn times(&self, zone: Zone) -> (EventTime, EventTime) {
        if self.all_day {
            let end = self.end_date.max(self.start_date) + Duration::days(1);
            return (EventTime::Date(self.start_date), EventTime::Date(end));
        }
        let tz = self.zone_tz();
        let tzid = match (&self.tz, tz) {
            (Some(name), Some(_)) => Some(name.clone()),
            // La zona original del evento.
            _ if self.base.as_ref().is_some_and(|b| matches!(b.start, EventTime::DateTime { tzid: Some(_), .. })) => {
                match self.base.as_ref().map(|b| &b.start) {
                    Some(EventTime::DateTime { tzid, .. }) => tzid.clone(),
                    _ => None,
                }
            }
            _ => {
                // Zona del usuario: con nombre IANA si se conoce (las series siguen al horario
                // de verano); UTC no lleva `TZID`.
                let u = crate::time::user_tz(zone);
                (u != chrono_tz::UTC).then(|| u.name().to_string())
            }
        };
        let s = to_utc_in(at_minutes(self.start_date, self.start_min), tz, zone);
        let e = to_utc_in(at_minutes(self.end_date, self.end_min), tz, zone);
        (EventTime::DateTime { utc: s, tzid: tzid.clone() }, EventTime::DateTime { utc: e, tzid })
    }

    /// Duración en minutos (para mover el fin junto con el inicio).
    pub fn length_min(&self) -> i64 {
        let s = at_minutes(self.start_date, self.start_min);
        let e = at_minutes(self.end_date, self.end_min);
        (e - s).num_minutes()
    }

    /// Cambia el inicio conservando la duración.
    pub fn set_start_keep_length(&mut self, date: NaiveDate, min: i64) {
        let len = if self.all_day {
            (self.end_date - self.start_date).num_days().max(0) * DAY_MIN
        } else {
            self.length_min().max(0)
        };
        self.start_date = date;
        self.start_min = min.clamp(0, DAY_MIN);
        if self.all_day {
            self.end_date = date + Duration::days(len / DAY_MIN);
        } else {
            let e = at_minutes(date, self.start_min) + Duration::minutes(len);
            if minutes_of(e) == 0 && e.date() > date {
                self.end_date = e.date() - Duration::days(1);
                self.end_min = DAY_MIN;
            } else {
                self.end_date = e.date();
                self.end_min = minutes_of(e);
            }
        }
    }

    /// Error para mostrar si el formulario no se puede guardar.
    pub fn problem(&self, zone: Zone) -> Option<Text> {
        if self.calendar.is_unassigned() {
            return Some(Text::new("Choose a calendar.", "Elige un calendario."));
        }
        if self.all_day {
            if self.end_date < self.start_date {
                return Some(Text::new("The event ends before it starts.", "El evento termina antes de empezar."));
            }
            return None;
        }
        let (s, e) = self.times(zone);
        if e.to_utc() <= s.to_utc() {
            return Some(Text::new("The event ends before it starts.", "El evento termina antes de empezar."));
        }
        if self.tz.as_deref().is_some_and(|t| parse_tz(t).is_none()) {
            return Some(Text::new("Unknown time zone.", "Zona horaria desconocida."));
        }
        None
    }

    /// Agrega un asistente si no está ya (por correo, sin distinguir mayúsculas).
    pub fn add_attendee(&mut self, m: Mailbox, optional: bool) -> bool {
        let addr = m.addr.trim().to_lowercase();
        if addr.is_empty()
            || !addr.contains('@')
            || self.attendees.iter().any(|a| a.mailbox.addr.to_lowercase() == addr)
        {
            return false;
        }
        self.attendees.push(Attendee {
            mailbox: Mailbox { addr, ..m },
            partstat: PartStat::NeedsAction,
            role: if optional { AttendeeRole::Optional } else { AttendeeRole::Required },
            rsvp: true,
        });
        true
    }

    /// El evento resultante (sobre la base, si la hay). `organizer` se pone si hay asistentes y
    /// el evento no tenía organizador.
    pub fn to_event(&self, zone: Zone, organizer: Option<Mailbox>) -> Event {
        let mut ev = self.base.clone().unwrap_or_default();
        ev.calendar = self.calendar;
        ev.summary = self.title.trim().to_string();
        ev.location = self.location.trim().to_string();
        ev.description = self.description.clone();
        let (s, e) = self.times(zone);
        ev.start = s;
        ev.end = e;
        let first_day = match &ev.start {
            EventTime::Date(d) => *d,
            t => local_in(t.to_utc(), self.zone_tz(), zone).date(),
        };
        ev.rrule = self.repeat.to_rrule(first_day, self.all_day, zone);
        if ev.rrule.is_none() {
            ev.exdates.clear();
        }
        let mut rem: Vec<i64> = self.reminders.clone();
        rem.sort_unstable();
        rem.dedup();
        ev.reminders = rem.into_iter().take(10).map(|m| Reminder { minutes_before: m }).collect();
        ev.attendees = self.attendees.clone();
        if !ev.attendees.is_empty() && ev.organizer.is_none() {
            ev.organizer = organizer;
        }
        if ev.attendees.is_empty() && self.base.as_ref().is_none_or(|b| b.attendees.is_empty()) {
            ev.organizer = self.base.as_ref().and_then(|b| b.organizer.clone());
        }
        ev.transparency = if self.busy { Transparency::Opaque } else { Transparency::Transparent };
        ev
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn t(d: u32, h: u32, m: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 10, d).unwrap().and_hms_opt(h, m, 0).unwrap()
    }

    #[test]
    fn new_timed_and_all_day() {
        let d = EventDraft::new_at(CalendarId(1), t(21, 9, 0), t(21, 10, 30), false);
        assert_eq!((d.start_min, d.end_min, d.length_min()), (540, 630, 90));
        let ev = d.to_event(Zone::utc(), None);
        assert_eq!(
            ev.start,
            EventTime::DateTime { utc: Utc.with_ymd_and_hms(2026, 10, 21, 9, 0, 0).unwrap(), tzid: None }
        );
        let a = EventDraft::new_at(CalendarId(1), t(21, 0, 0), t(23, 0, 0), true);
        assert_eq!(a.end_date, NaiveDate::from_ymd_opt(2026, 10, 22).unwrap(), "último día incluido");
        let ev = a.to_event(Zone::utc(), None);
        assert_eq!(ev.end, EventTime::Date(NaiveDate::from_ymd_opt(2026, 10, 23).unwrap()));
        assert_eq!(ev.transparency, Transparency::Transparent);
    }

    #[test]
    fn midnight_end_is_24_00() {
        let d = EventDraft::new_at(CalendarId(1), t(21, 23, 0), t(22, 0, 0), false);
        assert_eq!((d.end_date, d.end_min), (NaiveDate::from_ymd_opt(2026, 10, 21).unwrap(), DAY_MIN));
        assert!(d.problem(Zone::utc()).is_none());
    }

    #[test]
    fn event_zone_round_trip_and_keeps_unknown_fields() {
        let start = Utc.with_ymd_and_hms(2026, 10, 21, 15, 0, 0).unwrap();
        let ev = Event {
            id: EventId(4),
            calendar: CalendarId(2),
            uid: "u@x".into(),
            summary: "Junta".into(),
            start: EventTime::DateTime { utc: start, tzid: Some("America/Mexico_City".into()) },
            end: EventTime::DateTime { utc: start + Duration::hours(1), tzid: Some("America/Mexico_City".into()) },
            sequence: 3,
            remote_id: Some("/c/u.ics".into()),
            reminders: vec![Reminder { minutes_before: 10 }],
            ..Default::default()
        };
        let d = EventDraft::from_event(&ev, None, Zone::utc(), false);
        assert_eq!(d.start_min, 15 * 60, "se edita en la zona del usuario (UTC)");
        assert_eq!(d.tz, None);
        let back = d.to_event(Zone::utc(), None);
        assert_eq!(back, ev, "sin cambios, el mismo evento (conserva el TZID)");
        let mut mx = d.clone();
        mx.tz = Some("America/Mexico_City".into());
        mx.start_min = 9 * 60;
        mx.end_min = 10 * 60;
        assert_eq!(mx.to_event(Zone::utc(), None).start, ev.start, "las 9:00 en CDMX son las 15:00 UTC");
        let mut d2 = d.clone();
        d2.set_start_keep_length(d.start_date, 10 * 60);
        assert_eq!(d2.end_min, 11 * 60);
    }

    #[test]
    fn problems() {
        let mut d = EventDraft::new_at(CalendarId(1), t(21, 9, 0), t(21, 10, 0), false);
        d.end_min = 8 * 60;
        assert!(d.problem(Zone::utc()).is_some());
        d.end_min = 11 * 60;
        d.tz = Some("Marte/Base".into());
        assert!(d.problem(Zone::utc()).is_some());
        assert!(d.add_attendee(Mailbox::new("Ana", "ANA@x.org"), true));
        assert!(!d.add_attendee(Mailbox::new("", "ana@x.org"), false), "repetido");
        assert!(!d.add_attendee(Mailbox::new("", "no-es-correo"), false));
        assert_eq!(d.attendees[0].role, AttendeeRole::Optional);
        assert_eq!(reminder_label(1_440), reminder_label(1_440));
    }
}
