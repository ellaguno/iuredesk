//! Invitaciones por correo: iTIP (RFC 5546) sobre iMIP (RFC 6047).
//!
//! - [`ItipMessage::parse`]: lee la parte `text/calendar` de un correo (el `METHOD` puede venir en
//!   el `VCALENDAR` o en el parámetro `method=` del `Content-Type`).
//! - [`build_reply`]: arma la respuesta `METHOD:REPLY` de un asistente (aceptar, rechazar,
//!   provisional), lista para enviarse por correo al organizador.
//! - [`classify`]: decide si una invitación es nueva, una actualización, repetida o vieja,
//!   comparando `UID`, `SEQUENCE` y `DTSTAMP` (RFC 5546 §2.1.5).
//! - [`apply_reply`]: aplica la respuesta de un asistente a nuestra copia (somos organizadores).

use chrono::{DateTime, Utc};

use crate::calendar::{Attendee, EventView, ICalendar, PartStat, strip_mailto};
use crate::ical::{Component, Property};
use crate::value::{DateTimeValue, format_utc};
use crate::{Error, Result};

/// Método iTIP.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Method {
    /// Publicación sin respuesta.
    Publish,
    /// Invitación o actualización.
    Request,
    /// Respuesta de un asistente.
    Reply,
    /// Agregar repeticiones a una serie.
    Add,
    /// Cancelación.
    Cancel,
    /// Un asistente pide la versión actual.
    Refresh,
    /// Un asistente propone otro horario.
    Counter,
    /// El organizador rechaza la contrapropuesta.
    DeclineCounter,
    /// Otro valor.
    Other(String),
}

impl Method {
    /// Lee el nombre del método.
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_uppercase().as_str() {
            "PUBLISH" => Self::Publish,
            "REQUEST" => Self::Request,
            "REPLY" => Self::Reply,
            "ADD" => Self::Add,
            "CANCEL" => Self::Cancel,
            "REFRESH" => Self::Refresh,
            "COUNTER" => Self::Counter,
            "DECLINECOUNTER" => Self::DeclineCounter,
            other => Self::Other(other.to_string()),
        }
    }

    /// Nombre del método.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Publish => "PUBLISH",
            Self::Request => "REQUEST",
            Self::Reply => "REPLY",
            Self::Add => "ADD",
            Self::Cancel => "CANCEL",
            Self::Refresh => "REFRESH",
            Self::Counter => "COUNTER",
            Self::DeclineCounter => "DECLINECOUNTER",
            Self::Other(s) => s,
        }
    }
}

/// `method=` del encabezado `Content-Type: text/calendar; method=REQUEST; charset=UTF-8`.
pub fn method_from_content_type(content_type: &str) -> Option<Method> {
    content_type.split(';').skip(1).find_map(|p| {
        let (k, v) = p.split_once('=')?;
        k.trim().eq_ignore_ascii_case("method").then(|| Method::parse(v.trim().trim_matches('"')))
    })
}

/// Un mensaje iTIP ya leído.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItipMessage {
    /// Método.
    pub method: Method,
    /// El `VCALENDAR` completo.
    pub calendar: ICalendar,
    /// `UID` del objeto.
    pub uid: String,
    /// `SEQUENCE` mayor entre los componentes.
    pub sequence: u32,
    /// `DTSTAMP` más reciente.
    pub dtstamp: Option<DateTime<Utc>>,
    /// `RECURRENCE-ID` si el mensaje es solo de una repetición.
    pub recurrence_id: Option<DateTimeValue>,
    /// Organizador (correo).
    pub organizer: Option<String>,
    /// Asistentes del primer componente.
    pub attendees: Vec<Attendee>,
}

impl ItipMessage {
    /// Lee la parte `text/calendar` de un correo. `content_type` es el encabezado de esa parte
    /// (para el `method=` si el `VCALENDAR` no trae `METHOD`).
    pub fn parse(body: &[u8], content_type: Option<&str>) -> Result<Self> {
        let calendar = ICalendar::parse_bytes(body)?;
        let method = calendar
            .method()
            .map(|m| Method::parse(&m))
            .or_else(|| content_type.and_then(method_from_content_type))
            .ok_or(Error::Missing("METHOD"))?;
        let comps: Vec<&Component> = calendar
            .root
            .components
            .iter()
            .filter(|c| matches!(c.name.as_str(), "VEVENT" | "VTODO" | "VJOURNAL" | "VFREEBUSY"))
            .collect();
        let first = comps.first().map(|c| EventView::new(c)).ok_or(Error::Missing("VEVENT"))?;
        let uid = first.uid().ok_or(Error::Missing("UID"))?;
        let sequence = comps.iter().map(|c| EventView::new(c).sequence()).max().unwrap_or(0);
        let dtstamp = comps.iter().filter_map(|c| EventView::new(c).dtstamp()).max();
        // Si todos los componentes son excepciones, el mensaje es de una sola repetición.
        let recurrence_id =
            if comps.iter().all(|c| c.prop("RECURRENCE-ID").is_some()) { first.recurrence_id() } else { None };
        Ok(Self {
            method,
            uid,
            sequence,
            dtstamp,
            recurrence_id,
            organizer: first.organizer(),
            attendees: first.attendees(),
            calendar,
        })
    }

    /// El asistente con ese correo, si está en la lista.
    pub fn attendee(&self, email: &str) -> Option<&Attendee> {
        let e = strip_mailto(email);
        self.attendees.iter().find(|a| a.email == e)
    }

    /// Para `REPLY`: el primer asistente y su respuesta.
    pub fn reply_partstat(&self) -> Option<(&str, &PartStat)> {
        self.attendees.first().map(|a| (a.email.as_str(), &a.partstat))
    }
}

/// Lo que hay que hacer con una invitación respecto a la copia guardada.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateKind {
    /// No la tenemos: es nueva.
    New,
    /// Más reciente que la guardada: hay que aplicarla.
    Update,
    /// Igual a la guardada (mismo `SEQUENCE` y `DTSTAMP`).
    Duplicate,
    /// Más vieja que la guardada: se ignora.
    Outdated,
}

/// Versión guardada de un objeto, para [`classify`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoredVersion {
    /// `SEQUENCE` guardado.
    pub sequence: u32,
    /// `DTSTAMP` guardado.
    pub dtstamp: Option<DateTime<Utc>>,
}

impl StoredVersion {
    /// Versión de un calendario guardado (máximo `SEQUENCE` y `DTSTAMP` de sus componentes con
    /// ese `UID`).
    pub fn of(cal: &ICalendar, uid: &str) -> Option<Self> {
        let comps: Vec<EventView<'_>> =
            cal.root.components.iter().map(EventView::new).filter(|e| e.uid().as_deref() == Some(uid)).collect();
        if comps.is_empty() {
            return None;
        }
        Some(Self {
            sequence: comps.iter().map(EventView::sequence).max().unwrap_or(0),
            dtstamp: comps.iter().filter_map(EventView::dtstamp).max(),
        })
    }
}

/// Nueva, actualización, repetida o vieja (RFC 5546 §2.1.5): manda `SEQUENCE`; a igualdad,
/// `DTSTAMP`.
pub fn classify(stored: Option<StoredVersion>, incoming: &ItipMessage) -> UpdateKind {
    let Some(s) = stored else { return UpdateKind::New };
    match incoming.sequence.cmp(&s.sequence) {
        std::cmp::Ordering::Greater => UpdateKind::Update,
        std::cmp::Ordering::Less => UpdateKind::Outdated,
        std::cmp::Ordering::Equal => match (incoming.dtstamp, s.dtstamp) {
            (Some(a), Some(b)) if a > b => UpdateKind::Update,
            (Some(a), Some(b)) if a < b => UpdateKind::Outdated,
            _ => UpdateKind::Duplicate,
        },
    }
}

/// Propiedades que se copian de la invitación a la respuesta (RFC 5546 §3.2.3).
const REPLY_COPY: &[&str] =
    &["UID", "SEQUENCE", "RECURRENCE-ID", "DTSTART", "DTEND", "DURATION", "DUE", "SUMMARY", "ORGANIZER"];

/// Arma la respuesta `METHOD:REPLY` de `attendee_email` a una invitación `REQUEST`.
///
/// Contiene un componente por cada componente de la invitación (serie y excepciones), solo con
/// nuestro `ATTENDEE` y su `PARTSTAT`, y los `VTIMEZONE` necesarios. Si no estábamos en la
/// lista (invitación a una lista de distribución), nos agrega.
pub fn build_reply(
    request: &ItipMessage,
    attendee_email: &str,
    partstat: PartStat,
    comment: Option<&str>,
    now: DateTime<Utc>,
) -> Result<String> {
    let me = strip_mailto(attendee_email);
    if me.is_empty() {
        return Err(Error::Missing("correo del asistente"));
    }
    let mut out = ICalendar::new();
    out.root.push(Property::new("METHOD", "REPLY"));
    let mut tzids: Vec<String> = Vec::new();
    for comp in request.calendar.root.components.iter().filter(|c| matches!(c.name.as_str(), "VEVENT" | "VTODO")) {
        let mut reply = Component::new(&comp.name);
        for name in REPLY_COPY {
            for p in comp.props(name) {
                if let Some(t) = p.param("TZID")
                    && !tzids.iter().any(|x| x == t)
                {
                    tzids.push(t.to_string());
                }
                reply.push(p.clone());
            }
        }
        reply.push(Property::new("DTSTAMP", &format_utc(now)));
        let mut att = comp
            .props("ATTENDEE")
            .find(|p| strip_mailto(&p.value) == me)
            .cloned()
            .unwrap_or_else(|| Property::new("ATTENDEE", &format!("mailto:{me}")));
        att.set_param("PARTSTAT", partstat.as_str());
        att.remove_param("RSVP");
        reply.push(att);
        if let Some(c) = comment.filter(|c| !c.trim().is_empty()) {
            reply.push(Property::text("COMMENT", c));
        }
        out.root.components.push(reply);
    }
    if out.root.components.is_empty() {
        return Err(Error::Missing("VEVENT"));
    }
    // Las zonas van antes de los eventos.
    let zones: Vec<Component> = request
        .calendar
        .root
        .children("VTIMEZONE")
        .filter(|z| z.text("TZID").is_some_and(|id| tzids.iter().any(|t| *t == id.trim())))
        .cloned()
        .collect();
    out.root.components.splice(0..0, zones);
    Ok(out.to_ics())
}

/// Aplica un `REPLY` a nuestra copia (somos el organizador): actualiza el `PARTSTAT` del
/// asistente en los componentes con el mismo `UID` (y `RECURRENCE-ID`, si la respuesta es de una
/// repetición). Devuelve `true` si cambió algo.
pub fn apply_reply(stored: &mut ICalendar, reply: &ItipMessage) -> Result<bool> {
    if reply.method != Method::Reply {
        return Err(Error::Parse("el mensaje no es una respuesta (REPLY)".into()));
    }
    let Some((email, partstat)) = reply.reply_partstat() else { return Err(Error::Missing("ATTENDEE")) };
    let (email, partstat) = (email.to_string(), partstat.clone());
    let mut changed = false;
    for comp in stored.root.components.iter_mut() {
        let view = EventView::new(comp);
        if view.uid().as_deref() != Some(reply.uid.as_str()) {
            continue;
        }
        if let Some(rid) = &reply.recurrence_id
            && view.recurrence_id().as_ref() != Some(rid)
        {
            continue;
        }
        for p in comp.properties.iter_mut().filter(|p| p.name == "ATTENDEE") {
            if strip_mailto(&p.value) == email && p.param("PARTSTAT") != Some(partstat.as_str()) {
                p.set_param("PARTSTAT", partstat.as_str());
                p.remove_param("RSVP");
                changed = true;
            }
        }
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn request(seq: u32) -> String {
        format!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Google Inc//Google Calendar 70.9054//EN\r\nMETHOD:REQUEST\r\n\
BEGIN:VTIMEZONE\r\nTZID:America/Mexico_City\r\nBEGIN:STANDARD\r\nDTSTART:19700101T000000\r\nTZOFFSETFROM:-0600\r\n\
TZOFFSETTO:-0600\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\nBEGIN:VTIMEZONE\r\nTZID:Unused\r\nEND:VTIMEZONE\r\n\
BEGIN:VEVENT\r\nDTSTART;TZID=America/Mexico_City:20261110T100000\r\nDTEND;TZID=America/Mexico_City:20261110T110000\r\n\
DTSTAMP:20261026T120000Z\r\nORGANIZER;CN=Ana:mailto:ana@example.com\r\nUID:abc123@google.com\r\n\
ATTENDEE;CUTYPE=INDIVIDUAL;ROLE=REQ-PARTICIPANT;PARTSTAT=NEEDS-ACTION;RSVP=TRUE;CN=Eduardo:mailto:eduardo@example.com\r\n\
ATTENDEE;PARTSTAT=ACCEPTED:mailto:ana@example.com\r\nSEQUENCE:{seq}\r\nSUMMARY:Junta de revisión\r\n\
DESCRIPTION:Larga descripción que no va en la respuesta\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
        )
    }

    #[test]
    fn parse_request() {
        let m = ItipMessage::parse(request(0).as_bytes(), None).unwrap();
        assert_eq!(m.method, Method::Request);
        assert_eq!(m.uid, "abc123@google.com");
        assert_eq!(m.organizer.as_deref(), Some("ana@example.com"));
        assert!(m.attendee("MAILTO:Eduardo@example.com").is_some());
    }

    #[test]
    fn method_from_header() {
        let body = request(0).replace("METHOD:REQUEST\r\n", "");
        assert!(ItipMessage::parse(body.as_bytes(), None).is_err());
        let m = ItipMessage::parse(body.as_bytes(), Some("text/calendar; charset=UTF-8; method=CANCEL")).unwrap();
        assert_eq!(m.method, Method::Cancel);
    }

    #[test]
    fn reply_round_trip() {
        let req = ItipMessage::parse(request(2).as_bytes(), None).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 10, 27, 9, 0, 0).unwrap();
        let text = build_reply(&req, "eduardo@example.com", PartStat::Accepted, Some("Ahí estaré"), now).unwrap();
        let reply = ItipMessage::parse(text.as_bytes(), None).unwrap();
        assert_eq!(reply.method, Method::Reply);
        assert_eq!(reply.sequence, 2);
        assert_eq!(reply.attendees.len(), 1);
        assert_eq!(reply.attendees[0].partstat, PartStat::Accepted);
        assert!(!reply.attendees[0].rsvp);
        assert!(!text.contains("DESCRIPTION"));
        assert!(text.contains("TZID:America/Mexico_City"));
        assert!(!text.contains("TZID:Unused"));
        assert!(text.contains("DTSTAMP:20261027T090000Z"));

        // El organizador aplica la respuesta.
        let mut stored = req.calendar.clone();
        assert!(apply_reply(&mut stored, &reply).unwrap());
        let ev = stored.events().next().unwrap();
        assert_eq!(ev.attendees()[0].partstat, PartStat::Accepted);
        assert!(!apply_reply(&mut stored, &reply).unwrap());
    }

    #[test]
    fn reply_when_not_listed() {
        let req = ItipMessage::parse(request(0).as_bytes(), None).unwrap();
        let text = build_reply(&req, "lista@example.com", PartStat::Declined, None, Utc::now()).unwrap();
        assert!(text.contains("ATTENDEE;PARTSTAT=DECLINED:mailto:lista@example.com"));
    }

    #[test]
    fn classify_by_sequence_and_dtstamp() {
        let m = ItipMessage::parse(request(1).as_bytes(), None).unwrap();
        assert_eq!(classify(None, &m), UpdateKind::New);
        let stored = StoredVersion::of(&m.calendar, &m.uid).unwrap();
        assert_eq!(classify(Some(stored), &m), UpdateKind::Duplicate);
        let older = ItipMessage::parse(request(0).as_bytes(), None).unwrap();
        assert_eq!(classify(Some(stored), &older), UpdateKind::Outdated);
        let newer = ItipMessage::parse(request(2).as_bytes(), None).unwrap();
        assert_eq!(classify(Some(stored), &newer), UpdateKind::Update);
        let later_stamp = ItipMessage::parse(
            request(1).replace("DTSTAMP:20261026T120000Z", "DTSTAMP:20261026T130000Z").as_bytes(),
            None,
        )
        .unwrap();
        assert_eq!(classify(Some(stored), &later_stamp), UpdateKind::Update);
    }

    #[test]
    fn counter_and_cancel() {
        let counter = request(1).replace("METHOD:REQUEST", "METHOD:COUNTER").replace("T100000", "T120000");
        let m = ItipMessage::parse(counter.as_bytes(), None).unwrap();
        assert_eq!(m.method, Method::Counter);
        let proposed = m.calendar.events().next().unwrap().dtstart().unwrap();
        assert_eq!(proposed.naive().format("%H").to_string(), "12");
        let cancel = request(3).replace("METHOD:REQUEST", "METHOD:CANCEL");
        assert_eq!(ItipMessage::parse(cancel.as_bytes(), None).unwrap().method, Method::Cancel);
    }
}
