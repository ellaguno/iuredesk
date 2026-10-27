//! Invitaciones dentro de un correo: busca la parte `text/calendar` (iMIP, RFC 6047), la lee con
//! el parser iTIP de `iuredesk-dav` y la resume para la tarjeta del panel de lectura (cuándo,
//! dónde, organizador, asistentes, mi respuesta, si es nueva o una actualización).
//!
//! El correo es hostil: partes acotadas en número y tamaño, y un `.ics` roto da `None` (el
//! mensaje se lee igual, sin tarjeta).

use iuredesk_model::{Attendee, Event, EventId, EventTime, Mailbox, PartStat};
use iuredesk_pim_sync::map::ical::events_from_ics;
use mail_parser::{MessageParser, MimeHeaders};

/// Tamaño máximo de una parte `text/calendar`.
const MAX_ICS_BYTES: usize = 1024 * 1024;
/// Partes que se revisan como mucho.
const MAX_PARTS: usize = 200;

/// Qué pide el mensaje.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InviteKind {
    /// Invitación o actualización (`REQUEST`).
    Request,
    /// Cancelación (`CANCEL`).
    Cancel,
    /// Un asistente propone otro horario (`COUNTER`).
    Counter,
    /// Respuesta de un asistente a un evento nuestro (`REPLY`).
    Reply,
    /// Publicación sin respuesta (`PUBLISH`, `ADD`…).
    Publish,
}

/// Novedad respecto a la copia de mi calendario.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Freshness {
    New,
    Update,
    Same,
    /// Más vieja que la copia guardada (llegó después de una actualización).
    Outdated,
}

/// Una invitación leída de un correo.
#[derive(Clone, Debug, PartialEq)]
pub struct Invitation {
    pub kind: InviteKind,
    pub uid: String,
    /// Evento tal como lo describe la invitación (serie maestra o la repetición).
    pub event: Event,
    pub organizer: Option<Mailbox>,
    pub attendees: Vec<Attendee>,
    /// Mi dirección entre los asistentes, si estoy.
    pub me: Option<String>,
    /// Mi respuesta actual (la del calendario si ya lo tengo; si no, la de la invitación).
    pub my_status: Option<PartStat>,
    /// Soy el organizador (`REPLY`/`COUNTER` a un evento mío).
    pub i_organize: bool,
    /// El evento ya en mi calendario.
    pub existing: Option<EventId>,
    pub freshness: Freshness,
    /// La invitación es de una sola repetición de una serie.
    pub single_occurrence: bool,
    /// `REPLY`: quién respondió y qué.
    pub reply: Option<(Mailbox, PartStat)>,
    /// `COUNTER`: horario propuesto.
    pub proposed: Option<(EventTime, EventTime)>,
    /// Comentario del remitente (`COMMENT`).
    pub comment: Option<String>,
    /// El `.ics` original.
    pub ics: Vec<u8>,
}

/// Partes `text/calendar` de un mensaje: (encabezado `Content-Type` reconstruido, bytes).
pub fn calendar_parts(raw: &[u8]) -> Vec<(String, Vec<u8>)> {
    let Some(msg) = MessageParser::default().parse(raw) else { return Vec::new() };
    let mut out = Vec::new();
    for part in msg.parts.iter().take(MAX_PARTS) {
        let Some(ct) = part.content_type() else { continue };
        let is_cal = ct.ctype().eq_ignore_ascii_case("text") && ct.subtype().is_some_and(|s| s.eq_ignore_ascii_case("calendar"))
            || ct.ctype().eq_ignore_ascii_case("application") && ct.subtype().is_some_and(|s| s.eq_ignore_ascii_case("ics"));
        if !is_cal {
            continue;
        }
        let bytes = part.contents();
        if bytes.is_empty() || bytes.len() > MAX_ICS_BYTES {
            continue;
        }
        let header = match ct.attribute("method") {
            Some(m) => format!("text/calendar; method={m}"),
            None => "text/calendar".to_string(),
        };
        out.push((header, bytes.to_vec()));
        if out.len() >= 4 {
            break;
        }
    }
    out
}

/// Lee la invitación de un correo crudo. `mine` son mis direcciones (en cualquier caja).
pub fn parse_invitation(raw: &[u8], mine: &[String], tz: chrono_tz::Tz) -> Option<Invitation> {
    calendar_parts(raw).into_iter().find_map(|(ct, ics)| parse_ics(&ics, Some(&ct), mine, tz))
}

/// Lee una invitación de un `.ics` (con el `Content-Type` de su parte, para el `method=`).
pub fn parse_ics(ics: &[u8], content_type: Option<&str>, mine: &[String], tz: chrono_tz::Tz) -> Option<Invitation> {
    use iuredesk_dav::itip::{ItipMessage, Method};
    let msg = ItipMessage::parse(ics, content_type).ok()?;
    let kind = match msg.method {
        Method::Request => InviteKind::Request,
        Method::Cancel => InviteKind::Cancel,
        Method::Counter => InviteKind::Counter,
        Method::Reply => InviteKind::Reply,
        Method::Publish | Method::Add => InviteKind::Publish,
        _ => return None,
    };
    let mine: Vec<String> = mine.iter().map(|m| m.trim().to_lowercase()).filter(|m| !m.is_empty()).collect();
    let event = events_from_ics(&msg.calendar, tz).into_iter().find(|e| e.uid == msg.uid)?;
    let organizer = event.organizer.clone();
    let i_organize = organizer.as_ref().is_some_and(|o| mine.contains(&o.addr.trim().to_lowercase()));
    let me_att = event.attendees.iter().find(|a| mine.contains(&a.mailbox.addr.trim().to_lowercase()));
    let me = me_att.map(|a| a.mailbox.addr.trim().to_lowercase());
    let my_status = me_att.map(|a| a.partstat.clone());
    let reply = (kind == InviteKind::Reply)
        .then(|| event.attendees.first().map(|a| (a.mailbox.clone(), a.partstat.clone())))
        .flatten();
    let proposed = (kind == InviteKind::Counter).then(|| (event.start.clone(), event.end.clone()));
    let comment = msg
        .calendar
        .root
        .children("VEVENT")
        .find_map(|c| c.text("COMMENT"))
        .map(|c| c.trim().chars().take(2_000).collect::<String>())
        .filter(|c| !c.is_empty());
    Some(Invitation {
        kind,
        uid: msg.uid.clone(),
        organizer,
        attendees: event.attendees.clone(),
        me,
        my_status,
        i_organize,
        existing: None,
        freshness: Freshness::New,
        single_occurrence: msg.recurrence_id.is_some(),
        reply,
        proposed,
        comment,
        event,
        ics: ics.to_vec(),
    })
}

/// Novedad de una invitación (`SEQUENCE`, y a igualdad la fecha de cambio) frente a mi copia.
pub fn freshness(stored: Option<&Event>, incoming: &Event) -> Freshness {
    let Some(s) = stored else { return Freshness::New };
    match incoming.sequence.cmp(&s.sequence) {
        std::cmp::Ordering::Greater => Freshness::Update,
        std::cmp::Ordering::Less => Freshness::Outdated,
        std::cmp::Ordering::Equal => {
            let differs = incoming.start != s.start
                || incoming.end != s.end
                || incoming.summary != s.summary
                || incoming.location != s.location
                || incoming.rrule != s.rrule;
            match (incoming.updated, s.updated) {
                (Some(a), Some(b)) if a > b && differs => Freshness::Update,
                _ if differs && s.remote_id.is_none() => Freshness::Update,
                _ => Freshness::Same,
            }
        }
    }
}

/// Texto de una respuesta.
pub fn partstat_label(p: &PartStat) -> &'static str {
    use crate::lang::pick;
    match p {
        PartStat::Accepted => pick("Accepted", "Aceptado"),
        PartStat::Declined => pick("Declined", "Rechazado"),
        PartStat::Tentative => pick("Maybe", "Tal vez"),
        PartStat::Delegated => pick("Delegated", "Delegado"),
        PartStat::NeedsAction => pick("No response", "Sin respuesta"),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Un correo con una invitación `REQUEST` (también lo usan las pruebas de la sesión).
    pub(crate) fn invite_mail(method: &str, seq: u32, to: &str) -> Vec<u8> {
        let ics = format!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Prueba//ES\r\nMETHOD:{method}\r\nBEGIN:VEVENT\r\n\
UID:junta-42@example.org\r\nSEQUENCE:{seq}\r\nDTSTAMP:20261020T100000Z\r\n\
DTSTART:20261022T160000Z\r\nDTEND:20261022T170000Z\r\nSUMMARY:Revisión trimestral\r\n\
LOCATION:Sala 3\r\nORGANIZER;CN=Carmen Brown:mailto:carmen@example.org\r\n\
ATTENDEE;CN=Carmen Brown;PARTSTAT=ACCEPTED:mailto:carmen@example.org\r\n\
ATTENDEE;CN=Yo;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:{to}\r\n\
END:VEVENT\r\nEND:VCALENDAR\r\n"
        );
        format!(
            "From: Carmen Brown <carmen@example.org>\r\nTo: {to}\r\nSubject: Invitación: Revisión trimestral\r\n\
Date: Tue, 20 Oct 2026 10:00:00 +0000\r\nMessage-ID: <inv-42@example.org>\r\nMIME-Version: 1.0\r\n\
Content-Type: multipart/alternative; boundary=\"b\"\r\n\r\n--b\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n\
Te invito a la revisión.\r\n--b\r\nContent-Type: text/calendar; charset=utf-8; method={method}\r\n\r\n{ics}--b--\r\n"
        )
        .into_bytes()
    }

    #[test]
    fn reads_request_from_mail() {
        let raw = invite_mail("REQUEST", 0, "laura@despacho.example.org");
        let inv = parse_invitation(&raw, &["Laura@Despacho.example.org".into()], chrono_tz::UTC).unwrap();
        assert_eq!(inv.kind, InviteKind::Request);
        assert_eq!(inv.event.summary, "Revisión trimestral");
        assert_eq!(inv.me.as_deref(), Some("laura@despacho.example.org"));
        assert_eq!(inv.my_status, Some(PartStat::NeedsAction));
        assert!(!inv.i_organize);
        assert_eq!(inv.attendees.len(), 2);
    }

    #[test]
    fn cancel_and_garbage() {
        let raw = invite_mail("CANCEL", 1, "laura@despacho.example.org");
        assert_eq!(parse_invitation(&raw, &[], chrono_tz::UTC).unwrap().kind, InviteKind::Cancel);
        assert!(parse_invitation(b"From: x\r\n\r\nhola", &[], chrono_tz::UTC).is_none());
        let broken = String::from_utf8_lossy(&raw).replace("BEGIN:VEVENT", "BEGIN:VXXX").into_bytes();
        assert!(parse_invitation(&broken, &[], chrono_tz::UTC).is_none());
    }

    #[test]
    fn freshness_by_sequence() {
        let raw = invite_mail("REQUEST", 2, "a@b.c");
        let inv = parse_invitation(&raw, &[], chrono_tz::UTC).unwrap();
        let mut stored = inv.event.clone();
        assert_eq!(freshness(None, &inv.event), Freshness::New);
        assert_eq!(freshness(Some(&stored), &inv.event), Freshness::Same);
        stored.sequence = 1;
        assert_eq!(freshness(Some(&stored), &inv.event), Freshness::Update);
        stored.sequence = 5;
        assert_eq!(freshness(Some(&stored), &inv.event), Freshness::Outdated);
    }
}
