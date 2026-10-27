//! Ediciones locales primero (offline-first) y su contrato con la cola de operaciones.
//!
//! Cada función de aquí aplica el cambio al almacén **al instante** (la interfaz lo ve ya) y
//! encola una operación que la sincronización replica cuando hay red.
//!
//! # Contrato de la cola
//!
//! | `kind` | Carga (JSON) | Quién la consume |
//! |---|---|---|
//! | `pim.event.put`, `pim.event.delete`, `pim.event.rsvp`, `pim.contact.put`, `pim.contact.delete`, `pim.task.put`, `pim.task.delete` | [`PimOp`] | este crate (`PimSyncEngine`) |
//! | `mail.imip_reply` ([`OP_IMIP_REPLY`]) | [`ImipReply`] | la bandeja de salida (`compose`) |
//!
//! La sincronización de correo (`iuredesk-sync`) salta los `kind` que no son suyos, y esta salta
//! los que no empiezan por `pim.`.
//!
//! ## `mail.imip_reply`
//! Al responder a una invitación (aceptar, rechazar, provisional) en una cuenta CalDAV sin
//! programación en el servidor (RFC 6638), o en un evento solo local, el organizador debe recibir
//! un correo iMIP con `METHOD:REPLY`. [`rsvp`] encola un `mail.imip_reply` en la **cuenta del
//! calendario**. La bandeja de salida lo convierte en un borrador:
//! - `From` = [`ImipReply::from`], `To` = [`ImipReply::to`], `Subject` = [`ImipReply::subject`];
//! - cuerpo de texto = [`ImipReply::body`];
//! - parte `text/calendar; method=REPLY` = [`ImipReply::ics`] (ya es un `VCALENDAR` completo con
//!   `METHOD:REPLY`, hecho por `iuredesk_dav::itip::build_reply`): con
//!   `compose::CalendarPart::new(CalendarMethod::Reply, ics.into_bytes())`.
//!
//! y la confirma (`ack_op`) cuando quede en la bandeja de salida. En Microsoft 365 la respuesta
//! la manda el servidor (`/accept`, `/decline`…) y no se encola correo.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use iuredesk_dav::itip::{ItipMessage, build_reply};
use iuredesk_model::{
    AccountId, CalendarId, Contact, ContactId, Event, EventId, Mailbox, PartStat, Task, TaskId, TaskListId,
};
use iuredesk_store::{PimRef, Store};
use serde::{Deserialize, Serialize};

use crate::error::{PimError, Result};
use crate::map::ical::{new_event_ics, wire_partstat};

/// Prefijo de las operaciones de este crate.
pub const OP_PREFIX: &str = "pim.";
/// Respuesta iMIP para la bandeja de salida.
pub const OP_IMIP_REPLY: &str = "mail.imip_reply";

/// Operación PIM pendiente de replicar.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum PimOp {
    /// Crear o actualizar un evento (se sube su estado actual).
    PutEvent {
        /// Evento local.
        event: EventId,
    },
    /// Borrar un evento del servidor (ya no existe en local).
    DeleteEvent {
        /// Calendario donde estaba.
        calendar: CalendarId,
        /// `href` (DAV) o id (Graph).
        href: String,
        /// Versión que se borró.
        etag: Option<String>,
    },
    /// Responder a una invitación (el `PARTSTAT` ya está en el evento local).
    Rsvp {
        /// Evento local.
        event: EventId,
        /// Respuesta.
        partstat: PartStat,
        /// Comentario para el organizador.
        comment: Option<String>,
    },
    /// Crear o actualizar un contacto.
    PutContact {
        /// Contacto local.
        contact: ContactId,
    },
    /// Borrar un contacto del servidor.
    DeleteContact {
        /// Libreta donde estaba.
        book: iuredesk_model::AddressBookId,
        /// `href` o id remoto.
        href: String,
        /// Versión que se borró.
        etag: Option<String>,
    },
    /// Crear o actualizar una tarea.
    PutTask {
        /// Tarea local.
        task: TaskId,
    },
    /// Borrar una tarea del servidor.
    DeleteTask {
        /// Lista donde estaba.
        list: TaskListId,
        /// `href` o id remoto.
        href: String,
        /// Versión que se borró.
        etag: Option<String>,
    },
}

impl PimOp {
    /// `kind` en la cola.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::PutEvent { .. } => "pim.event.put",
            Self::DeleteEvent { .. } => "pim.event.delete",
            Self::Rsvp { .. } => "pim.event.rsvp",
            Self::PutContact { .. } => "pim.contact.put",
            Self::DeleteContact { .. } => "pim.contact.delete",
            Self::PutTask { .. } => "pim.task.put",
            Self::DeleteTask { .. } => "pim.task.delete",
        }
    }

    /// JSON para la cola.
    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// Lee una operación (`None` si no es nuestra o está dañada).
    pub fn decode(kind: &str, payload: &[u8]) -> Option<Self> {
        if !kind.starts_with(OP_PREFIX) {
            return None;
        }
        serde_json::from_slice(payload).ok()
    }

    /// Elemento local que mantiene «sucio» (no se pisa al bajar cambios del servidor).
    pub fn dirty_ref(&self) -> Option<PimRef> {
        match self {
            Self::PutEvent { event } | Self::Rsvp { event, .. } => Some(PimRef::Event(*event)),
            Self::PutContact { contact } => Some(PimRef::Contact(*contact)),
            Self::PutTask { task } => Some(PimRef::Task(*task)),
            _ => None,
        }
    }

    /// `href` borrado en local y aún no en el servidor.
    pub fn deleted_href(&self) -> Option<&str> {
        match self {
            Self::DeleteEvent { href, .. } | Self::DeleteContact { href, .. } | Self::DeleteTask { href, .. } => {
                Some(href)
            }
            _ => None,
        }
    }
}

/// Respuesta iMIP para la bandeja de salida (`mail.imip_reply`). Ver el contrato arriba.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImipReply {
    /// Evento local al que responde.
    pub event: EventId,
    /// `UID` de la invitación.
    pub uid: String,
    /// Remitente: el asistente que responde (nosotros).
    pub from: Mailbox,
    /// Destinatario: el organizador.
    pub to: Mailbox,
    /// Asunto listo («Aceptado: …» / «Accepted: …»).
    pub subject: String,
    /// Texto del cuerpo (comentario o una línea con la respuesta).
    pub body: String,
    /// Respuesta.
    pub partstat: PartStat,
    /// `VCALENDAR` con `METHOD:REPLY`.
    pub ics: String,
}

impl ImipReply {
    /// JSON para la cola.
    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// Lee la carga de un `mail.imip_reply`.
    pub fn decode(payload: &[u8]) -> Option<Self> {
        serde_json::from_slice(payload).ok()
    }
}

/// Elementos con cambios locales pendientes y `href` borrados pendientes de una cuenta.
#[derive(Debug, Default)]
pub(crate) struct Pending {
    pub dirty: BTreeSet<PimRef>,
    pub deleted: BTreeSet<String>,
}

pub(crate) fn pending(store: &Store, acct: AccountId) -> Result<Pending> {
    let mut p = Pending::default();
    for op in store.peek_ops(10_000)?.into_iter().filter(|o| o.account == acct) {
        let Some(op) = PimOp::decode(&op.kind, &op.payload) else { continue };
        if let Some(r) = op.dirty_ref() {
            p.dirty.insert(r);
        }
        if let Some(h) = op.deleted_href() {
            p.deleted.insert(h.to_string());
        }
    }
    Ok(p)
}

fn enqueue(store: &Store, acct: AccountId, op: &PimOp) -> Result<()> {
    // Un `put` pendiente del mismo elemento ya sube el estado actual: no se duplica.
    if matches!(op, PimOp::PutEvent { .. } | PimOp::PutContact { .. } | PimOp::PutTask { .. }) {
        let payload = op.encode();
        let dup =
            store.peek_ops(10_000)?.iter().any(|o| o.account == acct && o.kind == op.kind() && o.payload == payload);
        if dup {
            return Ok(());
        }
    }
    store.enqueue_op(acct, op.kind(), &op.encode())?;
    Ok(())
}

/// `UID` nuevo (128 bits al azar) con el dominio dado.
pub fn new_uid(domain: &str) -> String {
    let mut b = [0u8; 16];
    if getrandom::getrandom(&mut b).is_err() {
        let n = Utc::now().timestamp_nanos_opt().unwrap_or_default().to_le_bytes();
        for (dst, src) in b.iter_mut().zip(n.iter()) {
            *dst = *src;
        }
    }
    let mut out = String::with_capacity(48);
    for (i, x) in b.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            out.push('-');
        }
        out.push_str(&format!("{x:02x}"));
    }
    format!("{out}@{domain}")
}

fn calendar_account(store: &Store, cal: CalendarId) -> Result<(AccountId, bool)> {
    let c = store.get_calendar(cal)?.ok_or_else(|| PimError::NotFound(cal.to_string()))?;
    Ok((c.account, c.read_only))
}

/// Guarda un evento (nuevo si `id` está sin asignar) y encola su subida. Asigna `UID` si falta.
/// Si cambian las fechas o la regla de un evento sin asistentes o que organizamos, sube
/// `SEQUENCE` (RFC 5546).
pub fn save_event(store: &Store, event: &Event) -> Result<EventId> {
    let (acct, read_only) = calendar_account(store, event.calendar)?;
    if read_only {
        return Err(PimError::ReadOnly);
    }
    let mut ev = event.clone();
    if ev.uid.trim().is_empty() {
        ev.uid = new_uid("iuredesk");
    }
    // Mismo orden que al leer un .ics: el orden de EXDATE no es un cambio.
    ev.exdates.sort();
    ev.exdates.dedup();
    if !ev.id.is_unassigned()
        && let Some(old) = store.get_event(ev.id)?
    {
        let timing = old.start != ev.start || old.end != ev.end || old.rrule != ev.rrule || old.exdates != ev.exdates;
        if timing && ev.sequence == old.sequence {
            ev.sequence = ev.sequence.saturating_add(1);
        }
        if old.calendar != ev.calendar && old.remote_id.is_some() {
            // Mover de calendario = borrar allá y crear aquí.
            store.delete_raw_object(PimRef::Event(ev.id))?;
            if let Some(href) = old.remote_id.clone() {
                let (old_acct, _) = calendar_account(store, old.calendar)?;
                enqueue(store, old_acct, &PimOp::DeleteEvent { calendar: old.calendar, href, etag: old.etag.clone() })?;
            }
            ev.remote_id = None;
            ev.etag = None;
        }
    }
    ev.updated = Some(Utc::now());
    let id = store.put_event(&ev)?;
    enqueue(store, acct, &PimOp::PutEvent { event: id })?;
    Ok(id)
}

/// Borra un evento en local y encola el borrado remoto.
pub fn delete_event(store: &Store, id: EventId) -> Result<()> {
    let Some(ev) = store.get_event(id)? else { return Ok(()) };
    let (acct, read_only) = calendar_account(store, ev.calendar)?;
    if read_only {
        return Err(PimError::ReadOnly);
    }
    let href = store.raw_object(PimRef::Event(id))?.map(|r| r.href).or(ev.remote_id.clone());
    store.delete_event(id)?;
    if let Some(href) = href {
        enqueue(store, acct, &PimOp::DeleteEvent { calendar: ev.calendar, href, etag: ev.etag })?;
    }
    Ok(())
}

/// Responde a una invitación: pone nuestro `PARTSTAT` en el evento local, encola la réplica y,
/// si `send_imip`, encola el `mail.imip_reply` para el organizador. `me` son nuestras direcciones
/// (la primera que esté entre los asistentes es la que responde). Devuelve la respuesta iMIP
/// encolada, si hubo. `spanish` elige el idioma del asunto y del texto.
pub fn rsvp(
    store: &Store,
    id: EventId,
    partstat: PartStat,
    comment: Option<&str>,
    me: &[String],
    send_imip: bool,
    spanish: bool,
    now: DateTime<Utc>,
) -> Result<Option<ImipReply>> {
    let mut ev = store.get_event(id)?.ok_or_else(|| PimError::NotFound(id.to_string()))?;
    let (acct, _) = calendar_account(store, ev.calendar)?;
    let mine: Vec<String> = me.iter().map(|m| m.trim().to_lowercase()).filter(|m| !m.is_empty()).collect();
    let pos = ev
        .attendees
        .iter()
        .position(|a| mine.contains(&a.mailbox.addr.trim().to_lowercase()))
        .ok_or_else(|| PimError::Invalid("no estás en la lista de asistentes".into()))?;
    let me_box = ev.attendees.get(pos).map(|a| a.mailbox.clone()).unwrap_or_default();
    if let Some(a) = ev.attendees.get_mut(pos) {
        a.partstat = partstat.clone();
        a.rsvp = false;
    }
    store.put_event(&ev)?;
    enqueue(store, acct, &PimOp::Rsvp { event: id, partstat: partstat.clone(), comment: comment.map(str::to_string) })?;

    let Some(organizer) = ev.organizer.clone().filter(|o| !mine.contains(&o.addr.to_lowercase())) else {
        return Ok(None);
    };
    if !send_imip {
        return Ok(None);
    }
    // La invitación original (con excepciones) si la hay; si no, la armamos.
    let request_bytes = match store.raw_object_data(PimRef::Event(id))? {
        Some((_, data)) => data,
        None => new_event_ics(&ev, now).to_ics().into_bytes(),
    };
    let request = ItipMessage::parse(&request_bytes, Some("text/calendar; method=REQUEST"))?;
    let ics = build_reply(&request, &me_box.addr, wire_partstat(&partstat), comment, now)?;
    let (verb_es, verb_en) = match partstat {
        PartStat::Accepted => ("Aceptado", "Accepted"),
        PartStat::Declined => ("Rechazado", "Declined"),
        PartStat::Tentative => ("Provisional", "Tentative"),
        _ => ("Respuesta", "Response"),
    };
    let verb = if spanish { verb_es } else { verb_en };
    let who = me_box.display().to_string();
    let reply = ImipReply {
        event: id,
        uid: ev.uid.clone(),
        from: me_box,
        to: organizer,
        subject: format!("{verb}: {}", ev.summary),
        body: comment.map(str::to_string).filter(|c| !c.trim().is_empty()).unwrap_or_else(|| format!("{who} — {verb}")),
        partstat,
        ics,
    };
    store.enqueue_op(acct, OP_IMIP_REPLY, &reply.encode())?;
    Ok(Some(reply))
}

fn book_account(store: &Store, book: iuredesk_model::AddressBookId) -> Result<(AccountId, bool)> {
    let b = store.get_address_book(book)?.ok_or_else(|| PimError::NotFound(book.to_string()))?;
    Ok((b.account, b.read_only))
}

/// Guarda un contacto (y su foto nueva, si se da) y encola su subida.
pub fn save_contact(store: &Store, contact: &Contact, photo: Option<&[u8]>) -> Result<ContactId> {
    let (acct, read_only) = book_account(store, contact.address_book)?;
    if read_only {
        return Err(PimError::ReadOnly);
    }
    let mut c = contact.clone();
    if c.uid.trim().is_empty() {
        c.uid = new_uid("iuredesk").split('@').next().unwrap_or_default().to_string();
    }
    if let Some(bytes) = photo {
        c.photo = if bytes.is_empty() { None } else { Some(store.blobs().put(bytes)?) };
    }
    c.updated = Some(Utc::now());
    let id = store.put_contact(&c)?;
    enqueue(store, acct, &PimOp::PutContact { contact: id })?;
    Ok(id)
}

/// Borra un contacto en local y encola el borrado remoto.
pub fn delete_contact(store: &Store, id: ContactId) -> Result<()> {
    let Some(c) = store.get_contact(id)? else { return Ok(()) };
    let (acct, read_only) = book_account(store, c.address_book)?;
    if read_only {
        return Err(PimError::ReadOnly);
    }
    let href = store.raw_object(PimRef::Contact(id))?.map(|r| r.href).or(c.remote_id.clone());
    store.delete_contact(id)?;
    if let Some(href) = href {
        enqueue(store, acct, &PimOp::DeleteContact { book: c.address_book, href, etag: c.etag })?;
    }
    Ok(())
}

fn list_account(store: &Store, list: TaskListId) -> Result<Option<AccountId>> {
    let l = store.get_task_list(list)?.ok_or_else(|| PimError::NotFound(list.to_string()))?;
    Ok(l.account)
}

/// Guarda una tarea y, si la lista es de una cuenta, encola su subida.
pub fn save_task(store: &Store, task: &Task) -> Result<TaskId> {
    let acct = list_account(store, task.list)?;
    let mut t = task.clone();
    if t.uid.trim().is_empty() {
        t.uid = new_uid("iuredesk");
    }
    t.updated = Some(Utc::now());
    if t.created.is_none() {
        t.created = t.updated;
    }
    let id = store.put_task(&t)?;
    if let Some(acct) = acct {
        enqueue(store, acct, &PimOp::PutTask { task: id })?;
    }
    Ok(id)
}

/// Borra una tarea en local y encola el borrado remoto.
pub fn delete_task(store: &Store, id: TaskId) -> Result<()> {
    let Some(t) = store.get_task(id)? else { return Ok(()) };
    let acct = list_account(store, t.list)?;
    let href = store.raw_object(PimRef::Task(id))?.map(|r| r.href).or(t.remote_id.clone());
    store.delete_task(id)?;
    if let (Some(acct), Some(href)) = (acct, href) {
        enqueue(store, acct, &PimOp::DeleteTask { list: t.list, href, etag: t.etag })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use iuredesk_model::{Attendee, Calendar, EventTime};

    use super::*;

    #[test]
    fn op_round_trip_and_prefix() {
        let op = PimOp::Rsvp { event: EventId(3), partstat: PartStat::Accepted, comment: None };
        assert_eq!(PimOp::decode(op.kind(), &op.encode()), Some(op));
        assert_eq!(PimOp::decode("flags", b"{}"), None);
        assert!(new_uid("x").ends_with("@x"));
        assert_ne!(new_uid("x"), new_uid("x"));
    }

    #[test]
    fn local_first_and_dedupe() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let cal = s.put_calendar(&Calendar { account: AccountId(7), ..Default::default() }).unwrap();
        let id = save_event(&s, &Event { calendar: cal, summary: "a".into(), ..Default::default() }).unwrap();
        let mut ev = s.get_event(id).unwrap().unwrap();
        assert!(!ev.uid.is_empty());
        ev.summary = "b".into();
        save_event(&s, &ev).unwrap();
        let ops = s.peek_ops(10).unwrap();
        assert_eq!(ops.len(), 1, "dos ediciones = una subida");
        assert_eq!(ops[0].account, AccountId(7));
        let p = pending(&s, AccountId(7)).unwrap();
        assert!(p.dirty.contains(&PimRef::Event(id)));
        // Cambiar la hora sube SEQUENCE.
        ev = s.get_event(id).unwrap().unwrap();
        ev.start = EventTime::Date(chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap());
        save_event(&s, &ev).unwrap();
        assert_eq!(s.get_event(id).unwrap().unwrap().sequence, 1);
        // Borrar algo nunca subido no encola nada.
        delete_event(&s, id).unwrap();
        assert!(s.get_event(id).unwrap().is_none());
        assert_eq!(s.peek_ops(10).unwrap().len(), 1);
    }

    #[test]
    fn rsvp_needs_attendee() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let cal = s.put_calendar(&Calendar { account: AccountId(1), ..Default::default() }).unwrap();
        let id = s
            .put_event(&Event {
                calendar: cal,
                uid: "u".into(),
                summary: "Junta".into(),
                organizer: Some(Mailbox::addr("jefa@example.com")),
                attendees: vec![Attendee {
                    mailbox: Mailbox::addr("yo@example.com"),
                    rsvp: true,
                    ..Default::default()
                }],
                ..Default::default()
            })
            .unwrap();
        assert!(rsvp(&s, id, PartStat::Accepted, None, &["otro@example.com".into()], true, true, Utc::now()).is_err());
        let r =
            rsvp(&s, id, PartStat::Accepted, Some("Ahí estaré"), &["YO@example.com".into()], true, true, Utc::now())
                .unwrap()
                .unwrap();
        assert_eq!(r.to.addr, "jefa@example.com");
        assert_eq!(r.subject, "Aceptado: Junta");
        assert!(r.ics.contains("METHOD:REPLY"));
        assert!(r.ics.contains("PARTSTAT=ACCEPTED"));
        let kinds: Vec<String> = s.peek_ops(10).unwrap().into_iter().map(|o| o.kind).collect();
        assert_eq!(kinds, vec!["pim.event.rsvp".to_string(), OP_IMIP_REPLY.to_string()]);
        assert_eq!(s.get_event(id).unwrap().unwrap().attendees[0].partstat, PartStat::Accepted);
    }
}
