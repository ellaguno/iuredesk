//! Vistas tipadas sobre un `VCALENDAR`: eventos, tareas, asistentes y zonas.
//!
//! Las vistas solo leen; para editar se modifica el [`Component`] y se reescribe con
//! [`ICalendar::to_ics`], que conserva todo lo que no se tocó.

use chrono::{DateTime, Duration, Utc};
use chrono_tz::Tz;

use crate::ical::{self, Component, Property};
use crate::value::{self, DateTimeValue};
use crate::{Error, Result};

/// Número máximo de fechas en una sola propiedad `EXDATE`/`RDATE`.
pub const MAX_DATES_PER_PROPERTY: usize = 10_000;

/// Un objeto iCalendar (un `VCALENDAR`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ICalendar {
    /// El componente raíz `VCALENDAR`.
    pub root: Component,
}

impl ICalendar {
    /// Lee el primer `VCALENDAR` del texto.
    pub fn parse(input: &str) -> Result<Self> {
        Self::parse_all(input)?.into_iter().next().ok_or(Error::Missing("VCALENDAR"))
    }

    /// Lee todos los `VCALENDAR` (un `.ics` exportado puede traer varios).
    pub fn parse_all(input: &str) -> Result<Vec<Self>> {
        Ok(ical::parse(input)?.into_iter().filter(|c| c.name == "VCALENDAR").map(|root| Self { root }).collect())
    }

    /// Lee bytes (UTF-8 con pérdida).
    pub fn parse_bytes(input: &[u8]) -> Result<Self> {
        ical::parse_bytes(input)?
            .into_iter()
            .find(|c| c.name == "VCALENDAR")
            .map(|root| Self { root })
            .ok_or(Error::Missing("VCALENDAR"))
    }

    /// Calendario vacío con `VERSION` y `PRODID`.
    pub fn new() -> Self {
        let mut root = Component::new("VCALENDAR");
        root.push(Property::new("VERSION", "2.0"));
        root.push(Property::new("PRODID", PRODID));
        Self { root }
    }

    /// Texto `.ics` (CRLF, líneas plegadas).
    pub fn to_ics(&self) -> String {
        self.root.to_ical_string()
    }

    /// `METHOD` (iTIP), si lo hay.
    pub fn method(&self) -> Option<String> {
        self.root.text("METHOD").map(|m| m.trim().to_ascii_uppercase())
    }

    /// Los `VEVENT`.
    pub fn events(&self) -> impl Iterator<Item = EventView<'_>> {
        self.root.children("VEVENT").map(EventView::new)
    }

    /// Los `VTODO`.
    pub fn todos(&self) -> impl Iterator<Item = EventView<'_>> {
        self.root.children("VTODO").map(EventView::new)
    }

    /// Los `VTIMEZONE`.
    pub fn vtimezones(&self) -> Vec<&Component> {
        self.root.children("VTIMEZONE").collect()
    }

    /// Resuelve un `TZID` usando también los `VTIMEZONE` del archivo.
    pub fn resolve_tz(&self, tzid: &str) -> Option<Tz> {
        crate::tz::resolve_tzid(tzid, &self.vtimezones())
    }

    /// Convierte un valor a instante UTC. Las horas flotantes, las fechas y las zonas
    /// desconocidas se interpretan en `fallback` (la zona del usuario).
    pub fn to_utc(&self, v: &DateTimeValue, fallback: Tz) -> DateTime<Utc> {
        match v {
            DateTimeValue::Utc(u) => *u,
            DateTimeValue::Date(_) | DateTimeValue::Floating(_) => crate::tz::local_to_utc(v.naive(), fallback),
            DateTimeValue::Zoned { local, tzid } => {
                crate::tz::local_to_utc(*local, self.resolve_tz(tzid).unwrap_or(fallback))
            }
        }
    }
}

impl Default for ICalendar {
    fn default() -> Self {
        Self::new()
    }
}

/// `PRODID` de IureDesk.
pub const PRODID: &str = "-//Iurefficient//IureDesk//ES";

/// Estado de participación de un asistente (RFC 5545 §3.2.12).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PartStat {
    /// Sin respuesta.
    NeedsAction,
    /// Acepta.
    Accepted,
    /// Rechaza.
    Declined,
    /// Provisional.
    Tentative,
    /// Delegó.
    Delegated,
    /// Otro valor (se conserva).
    Other(String),
}

impl PartStat {
    /// Lee el parámetro `PARTSTAT` (por omisión `NEEDS-ACTION`).
    pub fn parse(s: Option<&str>) -> Self {
        match s.map(str::to_ascii_uppercase).as_deref() {
            None | Some("NEEDS-ACTION") => Self::NeedsAction,
            Some("ACCEPTED") => Self::Accepted,
            Some("DECLINED") => Self::Declined,
            Some("TENTATIVE") => Self::Tentative,
            Some("DELEGATED") => Self::Delegated,
            Some(other) => Self::Other(other.to_string()),
        }
    }

    /// Valor para el parámetro.
    pub fn as_str(&self) -> &str {
        match self {
            Self::NeedsAction => "NEEDS-ACTION",
            Self::Accepted => "ACCEPTED",
            Self::Declined => "DECLINED",
            Self::Tentative => "TENTATIVE",
            Self::Delegated => "DELEGATED",
            Self::Other(s) => s,
        }
    }
}

/// Asistente de un evento.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attendee {
    /// Correo (sin `mailto:`), en minúsculas.
    pub email: String,
    /// Nombre (`CN`).
    pub name: Option<String>,
    /// Estado de participación.
    pub partstat: PartStat,
    /// Rol (`REQ-PARTICIPANT`, `OPT-PARTICIPANT`, `CHAIR`…).
    pub role: Option<String>,
    /// ¿Se pide respuesta?
    pub rsvp: bool,
}

impl Attendee {
    /// Lee una propiedad `ATTENDEE`.
    pub fn from_property(p: &Property) -> Self {
        Self {
            email: strip_mailto(&p.value),
            name: p.param("CN").map(str::to_string),
            partstat: PartStat::parse(p.param("PARTSTAT")),
            role: p.param("ROLE").map(str::to_string),
            rsvp: p.param("RSVP").is_some_and(|v| v.eq_ignore_ascii_case("TRUE")),
        }
    }
}

/// Quita `mailto:` (sin distinguir mayúsculas) y pasa a minúsculas.
pub fn strip_mailto(uri: &str) -> String {
    let t = uri.trim();
    let rest = match t.get(..7) {
        Some(prefix) if prefix.eq_ignore_ascii_case("mailto:") => t.get(7..).unwrap_or_default(),
        _ => t,
    };
    rest.trim().to_lowercase()
}

/// Vista de solo lectura sobre un `VEVENT` o `VTODO`.
#[derive(Debug, Clone, Copy)]
pub struct EventView<'a> {
    /// El componente.
    pub component: &'a Component,
}

impl<'a> EventView<'a> {
    /// Envuelve un componente.
    pub fn new(component: &'a Component) -> Self {
        Self { component }
    }

    /// `UID`.
    pub fn uid(&self) -> Option<String> {
        self.component.text("UID").map(|u| u.trim().to_string()).filter(|u| !u.is_empty())
    }

    /// `SEQUENCE` (0 si falta o no es número).
    pub fn sequence(&self) -> u32 {
        self.component.prop("SEQUENCE").and_then(|p| p.value.trim().parse().ok()).unwrap_or(0)
    }

    /// `SUMMARY`.
    pub fn summary(&self) -> Option<String> {
        self.component.text("SUMMARY")
    }

    /// `DESCRIPTION`.
    pub fn description(&self) -> Option<String> {
        self.component.text("DESCRIPTION")
    }

    /// `LOCATION`.
    pub fn location(&self) -> Option<String> {
        self.component.text("LOCATION")
    }

    /// `STATUS` en mayúsculas.
    pub fn status(&self) -> Option<String> {
        self.component.text("STATUS").map(|s| s.trim().to_ascii_uppercase())
    }

    /// `DTSTART`.
    pub fn dtstart(&self) -> Option<DateTimeValue> {
        self.date_prop("DTSTART")
    }

    /// `DTEND` (eventos) o `DUE` (tareas).
    pub fn dtend(&self) -> Option<DateTimeValue> {
        self.date_prop("DTEND").or_else(|| self.date_prop("DUE"))
    }

    /// `DURATION`.
    pub fn duration(&self) -> Option<Duration> {
        self.component.prop("DURATION").and_then(|p| value::parse_duration(&p.value).ok())
    }

    /// `RECURRENCE-ID` (solo en excepciones de una serie).
    pub fn recurrence_id(&self) -> Option<DateTimeValue> {
        self.date_prop("RECURRENCE-ID")
    }

    /// `DTSTAMP` en UTC.
    pub fn dtstamp(&self) -> Option<DateTime<Utc>> {
        match self.date_prop("DTSTAMP")? {
            DateTimeValue::Utc(u) => Some(u),
            other => Some(chrono::TimeZone::from_utc_datetime(&Utc, &other.naive())),
        }
    }

    /// ¿Es de día completo? (`DTSTART;VALUE=DATE`).
    pub fn is_all_day(&self) -> bool {
        self.dtstart().is_some_and(|d| d.is_date())
    }

    /// Valores crudos de `RRULE`.
    pub fn rrules(&self) -> Vec<String> {
        self.component.props("RRULE").map(|p| p.value.trim().to_string()).collect()
    }

    /// ¿Es una serie (tiene `RRULE` o `RDATE`)?
    pub fn is_recurring(&self) -> bool {
        self.component.prop("RRULE").is_some() || self.component.prop("RDATE").is_some()
    }

    /// Fechas de `RDATE` (todas las propiedades).
    pub fn rdates(&self) -> Result<Vec<DateTimeValue>> {
        self.date_list("RDATE")
    }

    /// Fechas de `EXDATE` (todas las propiedades).
    pub fn exdates(&self) -> Result<Vec<DateTimeValue>> {
        self.date_list("EXDATE")
    }

    /// Organizador (correo sin `mailto:`).
    pub fn organizer(&self) -> Option<String> {
        self.component.prop("ORGANIZER").map(|p| strip_mailto(&p.value)).filter(|e| !e.is_empty())
    }

    /// Asistentes.
    pub fn attendees(&self) -> Vec<Attendee> {
        self.component.props("ATTENDEE").map(Attendee::from_property).collect()
    }

    fn date_prop(&self, name: &str) -> Option<DateTimeValue> {
        self.component.prop(name).and_then(|p| DateTimeValue::from_property(p).ok())
    }

    fn date_list(&self, name: &str) -> Result<Vec<DateTimeValue>> {
        let mut out = Vec::new();
        for p in self.component.props(name) {
            let remaining = MAX_DATES_PER_PROPERTY.saturating_sub(out.len());
            out.extend(DateTimeValue::list_from_property(p, remaining)?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INVITE: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Microsoft Corporation//Outlook 16.0//EN\r\n\
METHOD:REQUEST\r\nBEGIN:VTIMEZONE\r\nTZID:Central Standard Time (Mexico)\r\nBEGIN:STANDARD\r\n\
DTSTART:16010101T000000\r\nTZOFFSETFROM:-0600\r\nTZOFFSETTO:-0600\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\n\
BEGIN:VEVENT\r\nUID:040000008200E00074C5B7101A82E008\r\nSEQUENCE:2\r\nSUMMARY;LANGUAGE=es-MX:Revisión\r\n\
DTSTART;TZID=Central Standard Time (Mexico):20261105T100000\r\n\
DTEND;TZID=Central Standard Time (Mexico):20261105T110000\r\n\
ORGANIZER;CN=Ana López:mailto:Ana@Example.com\r\n\
ATTENDEE;ROLE=REQ-PARTICIPANT;PARTSTAT=NEEDS-ACTION;RSVP=TRUE;CN=Eduardo:MAILTO:eduardo@example.com\r\n\
X-MICROSOFT-CDO-BUSYSTATUS:TENTATIVE\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    #[test]
    fn outlook_invite_view() {
        let cal = ICalendar::parse(INVITE).unwrap();
        assert_eq!(cal.method().as_deref(), Some("REQUEST"));
        let ev = cal.events().next().unwrap();
        assert_eq!(ev.sequence(), 2);
        assert_eq!(ev.summary().as_deref(), Some("Revisión"));
        assert_eq!(ev.organizer().as_deref(), Some("ana@example.com"));
        let att = ev.attendees();
        assert_eq!(att[0].email, "eduardo@example.com");
        assert!(att[0].rsvp);
        assert_eq!(att[0].partstat, PartStat::NeedsAction);
        let start = cal.to_utc(&ev.dtstart().unwrap(), chrono_tz::UTC);
        assert_eq!(start.to_rfc3339(), "2026-11-05T16:00:00+00:00");
        // Sin cambios, se reescribe sin perder nada (la línea larga de ATTENDEE se pliega).
        assert_eq!(ICalendar::parse(&cal.to_ics()).unwrap(), cal);
        assert!(cal.to_ics().contains("X-MICROSOFT-CDO-BUSYSTATUS:TENTATIVE"));
    }

    #[test]
    fn mailto() {
        assert_eq!(strip_mailto("MAILTO:A@B.com"), "a@b.com");
        assert_eq!(strip_mailto("a@b.com"), "a@b.com");
        assert_eq!(strip_mailto("mai"), "mai");
    }
}
