//! UX-11: invitaciones del organizador por correo (iMIP, RFC 6047) cuando el servidor no las
//! manda solo (CalDAV sin programación RFC 6638).
//!
//! [`plan`] compara el evento antes y después de guardar y dice a quién va qué: invitación a los
//! asistentes nuevos, actualización a los que siguen si cambió algo que les importa (título,
//! lugar, descripción, horas, repetición, estado o la videollamada) y cancelación a los que se
//! quitaron (o a todos si se borró). [`ics`] arma el `VCALENDAR` con `METHOD:REQUEST` o
//! `METHOD:CANCEL`. El asunto, el texto y el envío los pone el motor (idioma, identidad,
//! bandeja de salida).

use chrono::{DateTime, Utc};
use iuredesk_dav::ical::Property;
use iuredesk_model::{Attendee, Event, EventStatus, Mailbox};

use crate::map::ical::new_event_ics;

/// Destinatarios por mensaje, como mucho (el resto no se invita: una lista enorme es un error).
const MAX_RECIPIENTS: usize = 500;

/// Lo que hay que mandar al guardar o borrar un evento del que somos organizadores.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InvitePlan {
    /// Asistentes nuevos: invitación.
    pub invite: Vec<Mailbox>,
    /// Asistentes que siguen: actualización (solo si cambió algo que les importa).
    pub update: Vec<Mailbox>,
    /// Asistentes quitados (o todos, si se borró): cancelación.
    pub cancel: Vec<Mailbox>,
}

impl InvitePlan {
    /// ¿No hay nada que mandar?
    pub fn is_empty(&self) -> bool {
        self.invite.is_empty() && self.update.is_empty() && self.cancel.is_empty()
    }
}

/// Método del mensaje.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutMethod {
    /// Invitación o actualización.
    Request,
    /// Cancelación.
    Cancel,
}

fn key(m: &Mailbox) -> String {
    m.addr.trim().to_lowercase()
}

/// ¿Es mío el evento? Sin organizador también (lo creo yo al poner asistentes).
pub fn i_organize(ev: &Event, me: &[String]) -> bool {
    ev.organizer.as_ref().is_none_or(|o| me.iter().any(|m| m.eq_ignore_ascii_case(o.addr.trim())))
}

/// ¿Cambió algo que los asistentes deben saber?
pub fn significant_change(old: &Event, new: &Event) -> bool {
    old.summary != new.summary
        || old.location != new.location
        || old.description != new.description
        || old.start != new.start
        || old.end != new.end
        || old.rrule != new.rrule
        || old.exdates != new.exdates
        || old.status != new.status
        || old.online_meeting.as_ref().map(|m| m.join_url.as_str())
            != new.online_meeting.as_ref().map(|m| m.join_url.as_str())
}

/// A quién va qué. `old = None`: evento nuevo; `new = None`: se borró. Vacío si no somos
/// organizadores. Nunca nos incluye a nosotros.
pub fn plan(old: Option<&Event>, new: Option<&Event>, me: &[String]) -> InvitePlan {
    let mine = |e: &Event| i_organize(e, me);
    if old.is_some_and(|o| !mine(o)) || new.is_some_and(|n| !mine(n)) {
        return InvitePlan::default();
    }
    let people = |e: Option<&Event>| -> Vec<Mailbox> {
        let mut seen = Vec::new();
        let mut out = Vec::new();
        for a in e.map(|e| e.attendees.as_slice()).unwrap_or_default() {
            let k = key(&a.mailbox);
            if k.contains('@') && !me.iter().any(|m| m.eq_ignore_ascii_case(&k)) && !seen.contains(&k) {
                seen.push(k);
                out.push(a.mailbox.clone());
            }
        }
        out.truncate(MAX_RECIPIENTS);
        out
    };
    let before = people(old);
    let after = people(new);
    let has = |list: &[Mailbox], m: &Mailbox| list.iter().any(|x| key(x) == key(m));
    match (old, new) {
        (_, None) => InvitePlan { cancel: before, ..Default::default() },
        (None, Some(_)) => InvitePlan { invite: after, ..Default::default() },
        (Some(o), Some(n)) => {
            let cancelled = n.status == EventStatus::Cancelled && o.status != EventStatus::Cancelled;
            if cancelled {
                return InvitePlan { cancel: before, ..Default::default() };
            }
            let changed = significant_change(o, n);
            InvitePlan {
                invite: after.iter().filter(|m| !has(&before, m)).cloned().collect(),
                update: if changed { after.iter().filter(|m| has(&before, m)).cloned().collect() } else { Vec::new() },
                cancel: before.iter().filter(|m| !has(&after, m)).cloned().collect(),
            }
        }
    }
}

/// `VCALENDAR` iMIP para `to`. En la invitación van todos los asistentes del evento (cada uno
/// ve con quién va); en la cancelación, solo los quitados, con `STATUS:CANCELLED`.
pub fn ics(ev: &Event, method: OutMethod, to: &[Mailbox], now: DateTime<Utc>) -> String {
    let mut e = ev.clone();
    if method == OutMethod::Cancel {
        e.status = EventStatus::Cancelled;
        let keep: Vec<String> = to.iter().map(key).collect();
        e.attendees.retain(|a: &Attendee| keep.contains(&key(&a.mailbox)));
        e.reminders.clear();
    }
    let mut cal = new_event_ics(&e, now);
    cal.root.set(Property::new(
        "METHOD",
        match method {
            OutMethod::Request => "REQUEST",
            OutMethod::Cancel => "CANCEL",
        },
    ));
    cal.to_ics()
}

#[cfg(test)]
mod tests {
    use iuredesk_model::{AttendeeRole, EventTime, PartStat};

    use super::*;

    fn mb(a: &str) -> Mailbox {
        Mailbox { name: None, addr: a.into() }
    }

    fn att(a: &str) -> Attendee {
        Attendee { mailbox: mb(a), partstat: PartStat::NeedsAction, role: AttendeeRole::Required, rsvp: true }
    }

    fn ev(people: &[&str]) -> Event {
        let at = DateTime::<Utc>::UNIX_EPOCH + chrono::Duration::days(20_400);
        Event {
            uid: "u1".into(),
            summary: "Junta".into(),
            organizer: Some(mb("yo@x.com")),
            attendees: people.iter().map(|p| att(p)).collect(),
            start: EventTime::DateTime { utc: at, tzid: None },
            end: EventTime::DateTime { utc: at + chrono::Duration::hours(1), tzid: None },
            ..Default::default()
        }
    }

    const ME: &[&str] = &["yo@x.com"];

    fn me() -> Vec<String> {
        ME.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn new_event_invites_everyone_but_me() {
        let n = ev(&["ana@x.com", "YO@x.com", "beto@x.com", "ana@X.com"]);
        let p = plan(None, Some(&n), &me());
        assert_eq!(p.invite, vec![mb("ana@x.com"), mb("beto@x.com")]);
        assert!(p.update.is_empty() && p.cancel.is_empty());
    }

    #[test]
    fn edits_split_into_invite_update_cancel() {
        let o = ev(&["ana@x.com", "beto@x.com"]);
        let mut n = ev(&["ana@x.com", "caro@x.com"]);
        // Solo cambian los asistentes: Ana no recibe nada.
        let p = plan(Some(&o), Some(&n), &me());
        assert_eq!((p.invite, p.update, p.cancel), (vec![mb("caro@x.com")], vec![], vec![mb("beto@x.com")]));
        // Cambia la hora: Ana recibe la actualización.
        n.start = EventTime::DateTime { utc: DateTime::<Utc>::UNIX_EPOCH, tzid: None };
        let p = plan(Some(&o), Some(&n), &me());
        assert_eq!(p.update, vec![mb("ana@x.com")]);
        // Borrar o cancelar: cancelación a todos.
        assert_eq!(plan(Some(&o), None, &me()).cancel.len(), 2);
        let mut c = o.clone();
        c.status = EventStatus::Cancelled;
        assert_eq!(plan(Some(&o), Some(&c), &me()).cancel.len(), 2);
    }

    #[test]
    fn nothing_when_someone_else_organizes() {
        let mut o = ev(&["ana@x.com"]);
        o.organizer = Some(mb("jefa@y.com"));
        assert!(plan(Some(&o), Some(&o.clone()), &me()).is_empty());
        assert!(plan(Some(&o), None, &me()).is_empty());
    }

    #[test]
    fn ics_carries_the_method_and_only_the_removed_on_cancel() {
        let e = ev(&["ana@x.com", "beto@x.com"]);
        let unfold = |s: String| s.replace("\r\n ", "").replace("\n ", "");
        let req = unfold(ics(&e, OutMethod::Request, &[mb("ana@x.com")], Utc::now()));
        assert!(req.contains("METHOD:REQUEST") && req.contains("ana@x.com") && req.contains("beto@x.com"), "{req}");
        let can = unfold(ics(&e, OutMethod::Cancel, &[mb("beto@x.com")], Utc::now()));
        assert!(can.contains("METHOD:CANCEL") && can.contains("STATUS:CANCELLED"), "{can}");
        assert!(!can.contains("ana@x.com") && can.contains("beto@x.com"), "{can}");
        // Lo que se manda se vuelve a leer como iTIP.
        let parsed = iuredesk_dav::itip::ItipMessage::parse(req.as_bytes(), None).unwrap();
        assert_eq!(parsed.method, iuredesk_dav::itip::Method::Request);
    }

    #[test]
    fn huge_lists_are_capped() {
        let many: Vec<String> = (0..2_000).map(|i| format!("p{i}@x.com")).collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        assert_eq!(plan(None, Some(&ev(&refs)), &me()).invite.len(), MAX_RECIPIENTS);
    }
}
