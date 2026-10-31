//! Calendario en la sesión: navegación (`cal.*`), calendarios visibles, crear/editar/mover/borrar
//! eventos (con «solo este / este y los siguientes / toda la serie» y deshacer), invitaciones de
//! correo (aceptar, tal vez, rechazar, cancelaciones, contrapropuestas, respuestas), asistente de
//! programación (disponibilidad) y recordatorios.
//!
//! Las escrituras van por un [`PimBackend`]: con sincronización, el `PimSyncHandle` (sube en
//! segundo plano); sin ella, las funciones locales-primero de `pim_sync::ops`; en pruebas, uno
//! falso.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, Timelike, Utc};
use iuredesk_model::{CalendarId, Event, EventId, EventTime, Mailbox, MessageId, PartStat, SharedCalendars};
use iuredesk_pim_sync::ops::PimOp;
use iuredesk_pim_sync::series::{detach_occurrence, exclude_occurrence, split_series, truncate_before};
use iuredesk_pim_sync::{ImipReply, PimError, PimSyncHandle, SeriesScope};
use iuredesk_store::{PimRef, Store};

use super::{Inverse, Module, Notice, Outcome, Session};
use crate::calendar::Occurrence;
use crate::calendar::draft::EventDraft;
use crate::calendar::invite::{Freshness, Invitation, InviteKind, freshness, parse_invitation};
use crate::calendar::nav::{CalView, shift};
use crate::calendar::reminders::{Candidate, DueReminder, ReminderKey, ReminderState};
use crate::calendar::schedule::Availability;
use crate::lang::Text;
use crate::{EngineError, Result};

/// Invitaciones en caché.
const INVITE_CACHE: usize = 256;

/// Escrituras de calendario (ver el módulo).
pub trait PimBackend: Send + Sync {
    /// Guarda (crea si el id está sin asignar).
    fn save_event(&self, store: &Store, ev: &Event) -> std::result::Result<EventId, PimError>;
    /// Borra.
    fn delete_event(&self, store: &Store, id: EventId) -> std::result::Result<(), PimError>;
    /// Responde a una invitación; `me` son mis direcciones.
    fn rsvp(
        &self,
        store: &Store,
        id: EventId,
        partstat: PartStat,
        comment: Option<&str>,
        me: &[String],
    ) -> std::result::Result<Option<ImipReply>, PimError>;
}

/// Sin sincronización: local primero con la cola (se subirá cuando haya un motor).
struct LocalBackend {
    spanish: bool,
}

impl PimBackend for LocalBackend {
    fn save_event(&self, store: &Store, ev: &Event) -> std::result::Result<EventId, PimError> {
        iuredesk_pim_sync::ops::save_event(store, ev)
    }
    fn delete_event(&self, store: &Store, id: EventId) -> std::result::Result<(), PimError> {
        iuredesk_pim_sync::ops::delete_event(store, id)
    }
    fn rsvp(
        &self,
        store: &Store,
        id: EventId,
        partstat: PartStat,
        comment: Option<&str>,
        me: &[String],
    ) -> std::result::Result<Option<ImipReply>, PimError> {
        iuredesk_pim_sync::ops::rsvp(store, id, partstat, comment, me, true, self.spanish, Utc::now())
    }
}

/// Con la sincronización PIM en marcha.
struct LiveBackend {
    handle: PimSyncHandle,
    spanish: bool,
}

impl PimBackend for LiveBackend {
    fn save_event(&self, _store: &Store, ev: &Event) -> std::result::Result<EventId, PimError> {
        self.handle.save_event(ev)
    }
    fn delete_event(&self, _store: &Store, id: EventId) -> std::result::Result<(), PimError> {
        self.handle.delete_event(id)
    }
    fn rsvp(
        &self,
        store: &Store,
        id: EventId,
        partstat: PartStat,
        comment: Option<&str>,
        me: &[String],
    ) -> std::result::Result<Option<ImipReply>, PimError> {
        match self.handle.rsvp(id, partstat.clone(), comment) {
            // Calendario de una cuenta sin sincronización PIM (local): por correo.
            Err(PimError::UnknownAccount(_)) => {
                iuredesk_pim_sync::ops::rsvp(store, id, partstat, comment, me, true, self.spanish, Utc::now())
            }
            other => other,
        }
    }
}

/// Una versión anterior de un evento (para deshacer).
#[derive(Clone, Debug)]
pub(crate) struct Snapshot {
    event: Event,
    /// Objeto crudo del servidor: datos, `href`, `ETag`.
    raw: Option<(Vec<u8>, String, Option<String>)>,
}

/// Deshacer una edición de calendario.
#[derive(Clone, Debug, Default)]
pub(crate) struct CalUndo {
    /// Eventos a dejar como estaban.
    restore: Vec<Snapshot>,
    /// Eventos creados por la edición (se borran).
    remove: Vec<EventId>,
}

/// Invitación con lo que depende de mi calendario (se recalcula cuando cambian los datos).
#[derive(Clone, Debug)]
struct CachedInvite {
    parsed: Option<Invitation>,
    data_gen: u64,
    enriched: Option<Invitation>,
    conflicts: Vec<Occurrence>,
}

/// Estado del calendario en la sesión.
#[derive(Default)]
pub struct CalState {
    /// Día de referencia (`None` = hoy).
    pub anchor: Option<NaiveDate>,
    backend: Option<Arc<dyn PimBackend>>,
    invitations: HashMap<MessageId, CachedInvite>,
    reminders: ReminderState,
    candidates: Vec<Candidate>,
    candidates_key: Option<(i64, u64)>,
    free_busy: Arc<Mutex<HashMap<(String, NaiveDate), Availability>>>,
}

/// Inicio de una aparición como en el modelo (fecha en días completos).
pub fn occurrence_start(o: &Occurrence) -> EventTime {
    match o.days {
        Some((d, _)) if o.all_day => EventTime::Date(d),
        _ => EventTime::DateTime { utc: o.recurrence_id.unwrap_or(o.start), tzid: None },
    }
}

fn pim_err(e: PimError) -> EngineError {
    match e {
        PimError::ReadOnly => EngineError::Invalid("calendario de solo lectura".into()),
        other => EngineError::Store(other.to_string()),
    }
}

/// Lleva `t` el mismo desfase que hubo de `from` a `to` (mismo tipo de hora).
fn shift_time(t: &EventTime, from: &EventTime, to: &EventTime) -> Option<EventTime> {
    match (t, from, to) {
        (EventTime::DateTime { utc, tzid }, EventTime::DateTime { utc: a, .. }, EventTime::DateTime { utc: b, .. }) => {
            Some(EventTime::DateTime { utc: *utc + (*b - *a), tzid: tzid.clone() })
        }
        (EventTime::Date(d), EventTime::Date(a), EventTime::Date(b)) => Some(EventTime::Date(*d + (*b - *a))),
        _ => None,
    }
}

impl Session {
    // ------------------------------------------------------------------ escritura

    /// Cambia cómo se guardan los eventos (pruebas: un motor falso).
    #[doc(hidden)]
    pub fn set_pim_backend(&mut self, b: Arc<dyn PimBackend>) {
        self.cal.backend = Some(b);
    }

    fn backend(&self) -> Arc<dyn PimBackend> {
        if let Some(b) = &self.cal.backend {
            return Arc::clone(b);
        }
        let spanish = crate::lang::spanish();
        match &self.pim.handle {
            Some(h) => Arc::new(LiveBackend { handle: h.clone(), spanish }),
            None => Arc::new(LocalBackend { spanish }),
        }
    }

    fn calendar_changed(&mut self) {
        self.events_cache = None;
        self.data_gen = self.data_gen.wrapping_add(1);
    }

    fn snapshot(&self, id: EventId) -> Option<Snapshot> {
        let event = self.store.get_event(id).ok().flatten()?;
        let raw = self.store.raw_object_data(PimRef::Event(id)).ok().flatten().map(|(r, d)| (d, r.href, r.etag));
        Some(Snapshot { event, raw })
    }

    // ------------------------------------------------------------------ navegación

    /// Vista actual.
    pub fn cal_view(&self) -> CalView {
        self.prefs.cal_view
    }

    /// Cambia la vista (se recuerda).
    pub fn set_cal_view(&mut self, v: CalView) {
        if self.prefs.cal_view != v {
            self.prefs.cal_view = v;
            self.save_prefs();
        }
    }

    /// Día de referencia.
    pub fn cal_anchor(&self) -> NaiveDate {
        self.cal.anchor.unwrap_or_else(|| self.today())
    }

    /// Comandos `cal.*`.
    pub(crate) fn run_calendar(&mut self, base: &str) -> Result<Outcome> {
        self.module = Module::Calendar;
        let anchor = self.cal_anchor();
        match base {
            "cal.today" => self.cal.anchor = None,
            "cal.prev" => self.cal.anchor = Some(shift(self.prefs.cal_view, anchor, false)),
            "cal.next" => self.cal.anchor = Some(shift(self.prefs.cal_view, anchor, true)),
            "cal.new_event" => {
                let today = self.today();
                let start = if anchor == today {
                    // La próxima media hora.
                    let now = self.zone.local(self.now());
                    let m = i64::from(now.hour()) * 60 + i64::from(now.minute());
                    let next = ((m / 30) + 1) * 30;
                    crate::calendar::drag::at_minutes(today, next.min(23 * 60))
                } else {
                    crate::calendar::drag::at_minutes(anchor, 9 * 60)
                };
                return match self.new_event_draft(start, start + Duration::minutes(30), false) {
                    Some(d) => Ok(Outcome::EditEvent(Box::new(d))),
                    None => Ok(Outcome::Notice(Notice::info(
                        "Add an account with a calendar first.",
                        "Primero agrega una cuenta con calendario.",
                    ))),
                };
            }
            b => {
                if let Some(v) = b.strip_prefix("cal.view.").and_then(CalView::from_key) {
                    self.set_cal_view(v);
                }
            }
        }
        Ok(Outcome::Done)
    }

    // ------------------------------------------------------------------ calendarios

    /// Calendarios que son la misma colección remota vista desde varias cuentas (BUG-DUP).
    pub fn shared_calendars(&self) -> SharedCalendars {
        SharedCalendars::detect(&self.calendars, |a| self.labels.iter().find(|l| l.id == a).map(|l| l.email.clone()))
    }

    /// ¿Está oculto el calendario (casilla de la barra lateral)? Un duplicado («También en …»)
    /// está oculto salvo que el usuario lo muestre a propósito.
    pub fn calendar_hidden(&self, id: CalendarId) -> bool {
        if self.shared_calendars().is_duplicate(id) {
            return !self.prefs.shown_duplicate_calendars.contains(&id.get());
        }
        self.prefs.hidden_calendars.contains(&id.get())
    }

    /// Muestra u oculta un calendario (se recuerda).
    pub fn set_calendar_hidden(&mut self, id: CalendarId, hidden: bool) {
        if self.shared_calendars().is_duplicate(id) {
            let list = &mut self.prefs.shown_duplicate_calendars;
            list.retain(|c| *c != id.get());
            if !hidden {
                list.push(id.get());
            }
        } else {
            let list = &mut self.prefs.hidden_calendars;
            list.retain(|c| *c != id.get());
            if hidden {
                list.push(id.get());
            }
        }
        self.save_prefs();
    }

    /// Calendarios cuyas apariciones no se muestran ni avisan (vistas, «Mi día», recordatorios,
    /// disponibilidad): los ocultos y los duplicados no mostrados. Las apariciones de un duplicado
    /// se funden en su primario (`occurrences_between`), así que mostrar el duplicado a propósito
    /// muestra el primario aunque esté oculto.
    pub fn hidden_calendar_ids(&self) -> Vec<u64> {
        let shared = self.shared_calendars();
        let shown = |id: CalendarId| self.prefs.shown_duplicate_calendars.contains(&id.get());
        let mut out: Vec<u64> = self
            .prefs
            .hidden_calendars
            .iter()
            .copied()
            .filter(|h| !shared.duplicates_of(CalendarId(*h)).into_iter().any(shown))
            .collect();
        for c in self.calendars.iter().filter(|c| shared.is_duplicate(c.id) && !shown(c.id)) {
            out.push(c.id.get());
        }
        out
    }

    /// Calendarios donde se puede escribir (sin duplicados: se edita por la cuenta del primario).
    pub fn writable_calendars(&self) -> Vec<iuredesk_model::Calendar> {
        let shared = self.shared_calendars();
        self.calendars.iter().filter(|c| !c.read_only && !shared.is_duplicate(c.id)).cloned().collect()
    }

    /// Calendario para eventos nuevos: el primero visible y escribible.
    pub fn default_calendar(&self) -> Option<CalendarId> {
        let w = self.writable_calendars();
        w.iter().find(|c| !self.calendar_hidden(c.id)).or(w.first()).map(|c| c.id)
    }

    /// Apariciones de los calendarios visibles en `[from, to)`.
    pub fn visible_occurrences(&mut self, from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<Occurrence> {
        let hidden = self.hidden_calendar_ids();
        self.occurrences(from, to).iter().filter(|o| !hidden.contains(&o.calendar.get())).cloned().collect()
    }

    /// Un evento del almacén.
    pub fn calendar_event(&self, id: EventId) -> Option<Event> {
        self.store.get_event(id).ok().flatten()
    }

    fn private_key(uid: &str) -> String {
        format!("cal-private/{}", uid.chars().take(300).collect::<String>())
    }

    /// ¿Marcado como privado? (Por ahora solo en este equipo: el modelo aún no tiene `CLASS`.)
    pub fn event_private(&self, uid: &str) -> bool {
        !uid.is_empty() && self.store.sync_state(&Self::private_key(uid)).ok().flatten().is_some_and(|v| v == b"1")
    }

    fn set_event_private(&self, uid: &str, private: bool) {
        if !uid.is_empty() {
            let _ = self.store.put_sync_state(&Self::private_key(uid), if private { b"1" } else { b"0" });
        }
    }

    // ------------------------------------------------------------------ formularios

    /// Formulario para un evento nuevo de `start` a `end` (horas locales).
    pub fn new_event_draft(&self, start: NaiveDateTime, end: NaiveDateTime, all_day: bool) -> Option<EventDraft> {
        let cal = self.default_calendar()?;
        Some(EventDraft::new_at(cal, start, end, all_day))
    }

    /// Formulario de una aparición del calendario.
    pub fn draft_for(&self, o: &Occurrence) -> Option<EventDraft> {
        let ev = self.calendar_event(o.id)?;
        let occ = if ev.is_recurring() {
            let start = occurrence_start(o);
            let (s, e) = match (o.days, o.all_day) {
                (Some((a, b)), true) => (EventTime::Date(a), EventTime::Date(b)),
                _ => (EventTime::DateTime { utc: o.start, tzid: None }, EventTime::DateTime { utc: o.end, tzid: None }),
            };
            let _ = start;
            Some((s, e))
        } else {
            None
        };
        let mut d = EventDraft::from_event(&ev, occ, self.zone, self.event_private(&ev.uid));
        if ev.is_recurring() {
            d.occurrence = Some(occurrence_start(o));
        }
        Some(d)
    }

    /// ¿Lo puedo editar? (No en calendarios de solo lectura.)
    pub fn can_edit(&self, cal: CalendarId) -> bool {
        self.calendars.iter().any(|c| c.id == cal && !c.read_only)
    }

    fn organizer_for(&self, cal: CalendarId) -> Option<Mailbox> {
        let acct = self.calendars.iter().find(|c| c.id == cal)?.account;
        let l = self.labels.iter().find(|l| l.id == acct)?;
        Some(Mailbox { name: Some(l.name.clone()).filter(|n| !n.is_empty()), addr: l.email.clone() })
    }

    /// Guarda el formulario. `scope` solo cuenta si se editó una aparición de una serie.
    pub fn save_event_draft(&mut self, d: &EventDraft, scope: SeriesScope) -> Result<Outcome> {
        if let Some(p) = d.problem(self.zone) {
            return Ok(Outcome::Notice(Notice::error(p)));
        }
        let edited = d.to_event(self.zone, self.organizer_for(d.calendar));
        let new = d.id.is_none();
        let out = self.apply_edit(d.id, d.occurrence.clone(), edited.clone(), scope)?;
        // `UID` final (los nuevos lo reciben al guardar).
        let uid = match d.id {
            Some(id) => self.calendar_event(id).map(|e| e.uid).unwrap_or(edited.uid),
            None => out.created.first().and_then(|id| self.calendar_event(*id)).map(|e| e.uid).unwrap_or_default(),
        };
        for id in &out.created {
            if let Some(e) = self.calendar_event(*id) {
                self.set_event_private(&e.uid, d.private);
            }
        }
        self.set_event_private(&uid, d.private);
        let title = if edited.summary.is_empty() { "—".to_string() } else { edited.summary.clone() };
        let text = if new {
            Text::new(format!("Event created: {title}"), format!("Evento creado: {title}"))
        } else {
            Text::new(format!("Event saved: {title}"), format!("Evento guardado: {title}"))
        };
        Ok(self.finish(text, out))
    }

    /// Mueve o estira una aparición (arrastre en la rejilla).
    pub fn reschedule(
        &mut self,
        o: &Occurrence,
        start: EventTime,
        end: EventTime,
        scope: SeriesScope,
    ) -> Result<Outcome> {
        let Some(ev) = self.calendar_event(o.id) else { return Err(EngineError::NoSelection) };
        let mut edited = ev.clone();
        edited.start = start;
        edited.end = end;
        // Las horas conservan la zona del evento.
        for t in [&mut edited.start, &mut edited.end] {
            if let (EventTime::DateTime { tzid, .. }, EventTime::DateTime { tzid: orig, .. }) = (&mut *t, &ev.start) {
                *tzid = orig.clone();
            }
        }
        let occ = ev.is_recurring().then(|| occurrence_start(o));
        let out = self.apply_edit(Some(o.id), occ, edited, scope)?;
        let text = Text::new(format!("Moved: {}", o.summary), format!("Movido: {}", o.summary));
        Ok(self.finish(text, out))
    }

    /// Borra una aparición (o la serie según `scope`).
    pub fn delete_occurrence(&mut self, o: &Occurrence, scope: SeriesScope) -> Result<Outcome> {
        let Some(ev) = self.calendar_event(o.id) else { return Err(EngineError::NoSelection) };
        if !self.can_edit(ev.calendar) {
            return Err(EngineError::Invalid("calendario de solo lectura".into()));
        }
        let backend = self.backend();
        let mut out = EditResult::default();
        let at = occurrence_start(o);
        let effective = if ev.is_recurring() { scope } else { SeriesScope::All };
        let replacement = match effective {
            SeriesScope::All => None,
            SeriesScope::This => Some(exclude_occurrence(&ev, &at)),
            SeriesScope::ThisAndFollowing => truncate_before(&ev, &at),
        };
        if let Some(s) = self.snapshot(ev.id) {
            out.undo.restore.push(s);
        }
        match replacement {
            Some(m) => {
                backend.save_event(&self.store, &m).map_err(pim_err)?;
            }
            None => backend.delete_event(&self.store, ev.id).map_err(pim_err)?,
        }
        let text = Text::new(format!("Deleted: {}", o.summary), format!("Eliminado: {}", o.summary));
        Ok(self.finish(text, out))
    }

    fn finish(&mut self, text: Text, out: EditResult) -> Outcome {
        self.calendar_changed();
        if out.undo.restore.is_empty() && out.undo.remove.is_empty() {
            return Outcome::Notice(Notice { kind: super::NoticeKind::Success, text, undo: false, secs: None });
        }
        let notice_text = text.clone();
        self.push_undo(text, vec![(Inverse::Calendar(Box::new(out.undo)), None)]);
        Outcome::Notice(Notice { kind: super::NoticeKind::Success, text: notice_text, undo: true, secs: None })
    }

    /// Aplica una edición (ver [`SeriesScope`]).
    fn apply_edit(
        &mut self,
        id: Option<EventId>,
        occurrence: Option<EventTime>,
        edited: Event,
        scope: SeriesScope,
    ) -> Result<EditResult> {
        let backend = self.backend();
        let mut out = EditResult::default();
        if !self.can_edit(edited.calendar) {
            return Err(EngineError::Invalid("calendario de solo lectura".into()));
        }
        let Some(id) = id else {
            let new_id = backend.save_event(&self.store, &edited).map_err(pim_err)?;
            out.undo.remove.push(new_id);
            out.created.push(new_id);
            return Ok(out);
        };
        let master = self.calendar_event(id).ok_or(EngineError::NoSelection)?;
        if let Some(s) = self.snapshot(id) {
            out.undo.restore.push(s);
        }
        let tz = crate::time::user_tz(self.zone);
        let at = occurrence.filter(|_| master.is_recurring());
        match (at, scope) {
            (Some(at), SeriesScope::This) => {
                let (series, single) = detach_occurrence(&master, &at, &edited);
                backend.save_event(&self.store, &series).map_err(pim_err)?;
                let nid = backend.save_event(&self.store, &single).map_err(pim_err)?;
                out.undo.remove.push(nid);
                out.created.push(nid);
            }
            (Some(at), SeriesScope::ThisAndFollowing) => {
                let (head, tail) = split_series(&master, &at, &edited, tz).map_err(pim_err)?;
                match head {
                    Some(head) => {
                        backend.save_event(&self.store, &head).map_err(pim_err)?;
                        let nid = backend.save_event(&self.store, &tail).map_err(pim_err)?;
                        out.undo.remove.push(nid);
                        out.created.push(nid);
                    }
                    // Desde la primera: es toda la serie.
                    None => {
                        let whole = whole_series(&master, &at, &edited);
                        backend.save_event(&self.store, &whole).map_err(pim_err)?;
                    }
                }
            }
            (Some(at), SeriesScope::All) => {
                let whole = whole_series(&master, &at, &edited);
                backend.save_event(&self.store, &whole).map_err(pim_err)?;
            }
            (None, _) => {
                let mut ev = edited;
                ev.id = id;
                backend.save_event(&self.store, &ev).map_err(pim_err)?;
            }
        }
        Ok(out)
    }

    /// Deshace una edición de calendario (desde `undo_last`).
    pub(crate) fn undo_calendar(&mut self, u: CalUndo) -> Result<()> {
        let backend = self.backend();
        for id in u.remove {
            let _ = backend.delete_event(&self.store, id);
        }
        for snap in u.restore {
            let exists = self.store.get_event(snap.event.id).ok().flatten().is_some();
            if exists {
                backend.save_event(&self.store, &snap.event).map_err(pim_err)?;
                continue;
            }
            // Borrado: si el borrado aún no salió, se cancela y se devuelve tal cual.
            let href = snap.raw.as_ref().map(|r| r.1.clone()).or(snap.event.remote_id.clone());
            let mut cancelled = false;
            if let Some(href) = &href {
                for op in self.store.peek_ops(10_000).unwrap_or_default() {
                    if let Some(PimOp::DeleteEvent { href: h, .. }) = PimOp::decode(&op.kind, &op.payload)
                        && h == *href
                    {
                        let _ = self.store.ack_op(op.seq);
                        cancelled = true;
                    }
                }
            }
            if cancelled || href.is_none() {
                self.store.put_event(&snap.event).map_err(super::store_err)?;
                if let Some((data, href, etag)) = &snap.raw {
                    let _ = self.store.put_raw_object(PimRef::Event(snap.event.id), data, href, etag.as_deref());
                }
            } else {
                // Ya se borró en el servidor: se vuelve a crear.
                let mut ev = snap.event.clone();
                ev.remote_id = None;
                ev.etag = None;
                backend.save_event(&self.store, &ev).map_err(pim_err)?;
            }
        }
        self.calendar_changed();
        Ok(())
    }

    // ------------------------------------------------------------------ invitaciones

    /// Busca en mi calendario el evento con ese `UID` cerca de `near`.
    fn event_by_uid(&self, uid: &str, near: &Event) -> Option<Event> {
        let s = near.start.to_utc();
        let e = near.end.to_utc().max(s);
        self.store
            .events_between(&[], s - Duration::days(2), e + Duration::days(2))
            .unwrap_or_default()
            .into_iter()
            .find(|x| x.uid == uid)
    }

    /// La invitación de un mensaje (si trae una), con mi respuesta y los choques con mi
    /// calendario. `None` si no hay o el cuerpo aún no se descargó.
    pub fn invitation(&mut self, msg: MessageId) -> Option<(Invitation, Vec<Occurrence>)> {
        let gen_now = self.data_gen;
        if !self.cal.invitations.contains_key(&msg) {
            // Solo con el mensaje ya en el almacén (si no, se reintenta cuando llegue).
            if !self.bodies.contains_key(&msg) {
                return None;
            }
            let raw = self.store.get_raw(msg).ok()?;
            let tz = crate::time::user_tz(self.zone);
            let parsed = parse_invitation(&raw, &self.my_addresses(), tz);
            if self.cal.invitations.len() >= INVITE_CACHE {
                self.cal.invitations.clear();
            }
            self.cal.invitations.insert(
                msg,
                CachedInvite { parsed, data_gen: gen_now.wrapping_sub(1), enriched: None, conflicts: Vec::new() },
            );
        }
        let cached = self.cal.invitations.get(&msg)?.clone();
        let parsed = cached.parsed?;
        if cached.data_gen == gen_now
            && let Some(e) = cached.enriched
        {
            return Some((e, cached.conflicts));
        }
        let mut inv = parsed;
        let stored = self.event_by_uid(&inv.uid, &inv.event);
        inv.existing = stored.as_ref().map(|e| e.id);
        inv.freshness = freshness(stored.as_ref(), &inv.event);
        if let (Some(s), Some(me)) = (&stored, &inv.me)
            && let Some(a) = s.attendees.iter().find(|a| a.mailbox.addr.trim().eq_ignore_ascii_case(me))
            && inv.freshness != Freshness::Update
        {
            inv.my_status = Some(a.partstat.clone());
        }
        let conflicts = if inv.kind == InviteKind::Request || inv.kind == InviteKind::Counter {
            let (s, e) = inv.proposed.clone().unwrap_or((inv.event.start.clone(), inv.event.end.clone()));
            let (from, to) = (s.to_utc(), e.to_utc().max(s.to_utc() + Duration::minutes(1)));
            let hidden = self.hidden_calendar_ids();
            self.occurrences_uncached(from - Duration::days(1), to + Duration::days(1))
                .into_iter()
                .filter(|o| {
                    !o.all_day
                        && o.busy
                        && !o.cancelled
                        && o.start < to
                        && from < o.end
                        && Some(o.id) != inv.existing
                        && !hidden.contains(&o.calendar.get())
                })
                .take(5)
                .collect()
        } else {
            Vec::new()
        };
        if let Some(c) = self.cal.invitations.get_mut(&msg) {
            c.data_gen = gen_now;
            c.enriched = Some(inv.clone());
            c.conflicts = conflicts.clone();
        }
        Some((inv, conflicts))
    }

    /// Calendario donde guardar una invitación que llegó a la cuenta del mensaje.
    fn calendar_for_message(&mut self, msg: MessageId) -> Option<CalendarId> {
        let acct = self.envelope(msg).map(|e| e.account);
        let w = self.writable_calendars();
        acct.and_then(|a| w.iter().find(|c| c.account == a && !self.calendar_hidden(c.id)).map(|c| c.id))
            .or_else(|| self.default_calendar())
    }

    /// Responde a la invitación del mensaje (`Accepted`, `Tentative`, `Declined`): la guarda o
    /// actualiza en mi calendario y responde al organizador (iMIP por la bandeja de salida o el
    /// servidor).
    pub fn respond_invitation(&mut self, msg: MessageId, partstat: PartStat, comment: Option<&str>) -> Result<Outcome> {
        let (inv, _) = self.invitation(msg).ok_or(EngineError::NoSelection)?;
        let backend = self.backend();
        let id = self.import_invitation(Some(msg), &inv)?;
        let comment = comment.map(str::trim).filter(|c| !c.is_empty());
        let mine = self.my_addresses();
        let reply = backend.rsvp(&self.store, id, partstat.clone(), comment, &mine).map_err(pim_err)?;
        self.calendar_changed();
        let what = crate::calendar::invite::partstat_label(&partstat);
        let to = inv.organizer.as_ref().map(|o| o.display().to_string()).unwrap_or_default();
        Ok(Outcome::Notice(match reply {
            Some(_) => Notice::success(
                format!("{what} — reply to {to} is in the outbox."),
                format!("{what} — la respuesta a {to} está en la bandeja de salida."),
            ),
            None => Notice::success(format!("{what}: {}", inv.event.summary), format!("{what}: {}", inv.event.summary)),
        }))
    }

    /// Guarda (o actualiza) en mi calendario el evento de la invitación; devuelve su id.
    /// `msg` elige el calendario de la cuenta que la recibió; sin mensaje (un `.ics` abierto), el
    /// calendario por omisión.
    fn import_invitation(&mut self, msg: Option<MessageId>, inv: &Invitation) -> Result<EventId> {
        let backend = self.backend();
        match (inv.existing.and_then(|id| self.calendar_event(id)), inv.freshness) {
            (Some(mut ev), Freshness::Update) => {
                let i = &inv.event;
                ev.summary = i.summary.clone();
                ev.location = i.location.clone();
                ev.description = i.description.clone();
                ev.start = i.start.clone();
                ev.end = i.end.clone();
                ev.rrule = i.rrule.clone();
                ev.exdates = i.exdates.clone();
                ev.organizer = i.organizer.clone();
                // Mi respuesta anterior se conserva si las fechas no cambiaron.
                let mine: Vec<String> = self.my_addresses().iter().map(|m| m.to_lowercase()).collect();
                let keep: Option<PartStat> = ev
                    .attendees
                    .iter()
                    .find(|a| mine.contains(&a.mailbox.addr.to_lowercase()))
                    .map(|a| a.partstat.clone());
                ev.attendees = i.attendees.clone();
                if let Some(k) = keep
                    && let Some(a) = ev.attendees.iter_mut().find(|a| mine.contains(&a.mailbox.addr.to_lowercase()))
                {
                    a.partstat = k;
                }
                ev.sequence = i.sequence.max(ev.sequence);
                ev.status = i.status.clone();
                backend.save_event(&self.store, &ev).map_err(pim_err)
            }
            (Some(ev), _) => Ok(ev.id),
            (None, _) => {
                let cal = msg
                    .and_then(|m| self.calendar_for_message(m))
                    .or_else(|| self.default_calendar())
                    .ok_or(EngineError::NoAccount)?;
                let mut ev = inv.event.clone();
                ev.id = EventId::default();
                ev.calendar = cal;
                ev.remote_id = None;
                ev.etag = None;
                if ev.reminders.is_empty() {
                    ev.reminders = vec![iuredesk_model::Reminder { minutes_before: 15 }];
                }
                backend.save_event(&self.store, &ev).map_err(pim_err)
            }
        }
    }

    // ------------------------------------------------------------------ R1.2: `.ics` abiertos

    /// Completa una invitación leída de un archivo con mi calendario (si ya la tengo y si es más
    /// nueva), como hace [`Session::invitation`] con las de un correo.
    pub fn enrich_file_invitation(&self, inv: &mut Invitation) {
        let stored = self.event_by_uid(&inv.uid, &inv.event);
        inv.existing = stored.as_ref().map(|e| e.id);
        inv.freshness = freshness(stored.as_ref(), &inv.event);
    }

    /// Responde a una invitación abierta desde un `.ics` (como [`Session::respond_invitation`]).
    pub fn respond_file_invitation(&mut self, inv: &Invitation, partstat: PartStat) -> Result<Outcome> {
        let mut inv = inv.clone();
        self.enrich_file_invitation(&mut inv);
        let backend = self.backend();
        let id = self.import_invitation(None, &inv)?;
        let mine = self.my_addresses();
        let reply = backend.rsvp(&self.store, id, partstat.clone(), None, &mine).map_err(pim_err)?;
        self.calendar_changed();
        let what = crate::calendar::invite::partstat_label(&partstat);
        let to = inv.organizer.as_ref().map(|o| o.display().to_string()).unwrap_or_default();
        Ok(Outcome::Notice(match reply {
            Some(_) => Notice::success(
                format!("{what} — reply to {to} is in the outbox."),
                format!("{what} — la respuesta a {to} está en la bandeja de salida."),
            ),
            None => Notice::success(format!("{what}: {}", inv.event.summary), format!("{what}: {}", inv.event.summary)),
        }))
    }

    /// Agrega a mi calendario por omisión los eventos de un `.ics` abierto (los que ya tengo, por
    /// `UID`, se saltan). Con deshacer.
    pub fn add_file_events(&mut self, events: &[Event]) -> Result<Outcome> {
        let cal = self.default_calendar().ok_or(EngineError::NoAccount)?;
        let backend = self.backend();
        let mut undo = CalUndo::default();
        let mut created = Vec::new();
        let mut skipped = 0usize;
        for e in events.iter().take(crate::open::MAX_ITEMS) {
            if self.event_by_uid(&e.uid, e).is_some() {
                skipped += 1;
                continue;
            }
            let mut ev = e.clone();
            ev.id = EventId::default();
            ev.calendar = cal;
            ev.remote_id = None;
            ev.etag = None;
            let id = backend.save_event(&self.store, &ev).map_err(pim_err)?;
            undo.remove.push(id);
            created.push(id);
        }
        let n = created.len();
        let text = match (n, skipped) {
            (0, _) => {
                Text::new("Those events are already in your calendar.", "Esos eventos ya están en tu calendario.")
            }
            (1, 0) => {
                let s = events.first().map(|e| e.summary.clone()).unwrap_or_default();
                Text::new(format!("Added to your calendar: {s}"), format!("Agregado a tu calendario: {s}"))
            }
            (n, 0) => Text::new(
                format!("{n} events added to your calendar."),
                format!("{n} eventos agregados a tu calendario."),
            ),
            (n, k) => Text::new(
                format!("{n} events added; {k} were already in your calendar."),
                format!("{n} eventos agregados; {k} ya estaban en tu calendario."),
            ),
        };
        Ok(self.finish(text, EditResult { undo, created }))
    }

    /// Una aparición del evento con ese `UID` (la próxima si se repite), para abrirlo desde un
    /// enlace `iuredesk://event/…`. Busca dos años hacia atrás y uno hacia delante.
    pub fn occurrence_by_uid(&mut self, uid: &str) -> Option<Occurrence> {
        let now = self.now();
        let ev = self
            .store
            .events_between(&[], now - Duration::days(730), now + Duration::days(366))
            .unwrap_or_default()
            .into_iter()
            .find(|e| e.uid == uid)?;
        let start = ev.start.to_utc();
        let from = if ev.rrule.is_some() { now.max(start) } else { start } - Duration::days(1);
        self.occurrences_uncached(from, from + Duration::days(370)).into_iter().find(|o| o.id == ev.id)
    }

    /// Agrega al calendario una invitación sin responder (o una publicación).
    pub fn add_invitation_to_calendar(&mut self, msg: MessageId) -> Result<Outcome> {
        let (inv, _) = self.invitation(msg).ok_or(EngineError::NoSelection)?;
        let existed = inv.existing.is_some();
        let id = self.import_invitation(Some(msg), &inv)?;
        let mut undo = CalUndo::default();
        if !existed {
            undo.remove.push(id);
        }
        let s = &inv.event.summary;
        Ok(self.finish(
            Text::new(format!("Added to your calendar: {s}"), format!("Agregado a tu calendario: {s}")),
            EditResult { undo, created: vec![id] },
        ))
    }

    /// Cancelación recibida: quita el evento (o la repetición) de mi calendario, con deshacer.
    pub fn apply_invitation_cancel(&mut self, msg: MessageId) -> Result<Outcome> {
        let (inv, _) = self.invitation(msg).ok_or(EngineError::NoSelection)?;
        let Some(ev) = inv.existing.and_then(|id| self.calendar_event(id)) else {
            return Ok(Outcome::Notice(Notice::info("It is not in your calendar.", "No está en tu calendario.")));
        };
        let backend = self.backend();
        let mut out = EditResult::default();
        if let Some(s) = self.snapshot(ev.id) {
            out.undo.restore.push(s);
        }
        if inv.single_occurrence && ev.is_recurring() {
            let m = exclude_occurrence(&ev, &inv.event.start);
            backend.save_event(&self.store, &m).map_err(pim_err)?;
        } else {
            backend.delete_event(&self.store, ev.id).map_err(pim_err)?;
        }
        let s = &ev.summary;
        Ok(self.finish(
            Text::new(format!("Removed from your calendar: {s}"), format!("Quitado de tu calendario: {s}")),
            out,
        ))
    }

    /// Contrapropuesta (soy el organizador): acepta el horario propuesto.
    pub fn accept_counter(&mut self, msg: MessageId) -> Result<Outcome> {
        let (inv, _) = self.invitation(msg).ok_or(EngineError::NoSelection)?;
        let (Some(ev), Some((s, e))) = (inv.existing.and_then(|id| self.calendar_event(id)), inv.proposed.clone())
        else {
            return Ok(Outcome::Notice(Notice::info("It is not in your calendar.", "No está en tu calendario.")));
        };
        let mut edited = ev.clone();
        edited.start = s;
        edited.end = e;
        let out = self.apply_edit(Some(ev.id), None, edited, SeriesScope::All)?;
        let t = &ev.summary;
        Ok(self.finish(Text::new(format!("New time saved: {t}"), format!("Nuevo horario guardado: {t}")), out))
    }

    /// Respuesta de un asistente (soy el organizador): la anota en mi evento.
    pub fn apply_invitation_reply(&mut self, msg: MessageId) -> Result<Outcome> {
        let (inv, _) = self.invitation(msg).ok_or(EngineError::NoSelection)?;
        let (Some(mut ev), Some((who, ps))) = (inv.existing.and_then(|id| self.calendar_event(id)), inv.reply.clone())
        else {
            return Ok(Outcome::Done);
        };
        match ev.attendees.iter_mut().find(|a| a.mailbox.addr.eq_ignore_ascii_case(&who.addr)) {
            Some(a) => a.partstat = ps,
            None => return Ok(Outcome::Done),
        }
        self.backend().save_event(&self.store, &ev).map_err(pim_err)?;
        self.calendar_changed();
        Ok(Outcome::Done)
    }

    // ------------------------------------------------------------------ disponibilidad

    /// Pide la disponibilidad de `emails` el día `day` (al servidor del calendario `cal`).
    pub fn request_free_busy(&mut self, cal: CalendarId, emails: &[String], day: NaiveDate) {
        let missing: Vec<String> = {
            let map = self.cal.free_busy.lock().unwrap_or_else(PoisonError::into_inner);
            emails
                .iter()
                .map(|e| e.trim().to_lowercase())
                .filter(|e| !e.is_empty() && !map.contains_key(&(e.clone(), day)))
                .collect()
        };
        if missing.is_empty() {
            return;
        }
        let Some(handle) = self.pim.handle.clone() else {
            let mut map = self.cal.free_busy.lock().unwrap_or_else(PoisonError::into_inner);
            for e in missing {
                map.insert((e, day), Availability::Unknown);
            }
            return;
        };
        {
            let mut map = self.cal.free_busy.lock().unwrap_or_else(PoisonError::into_inner);
            for e in &missing {
                map.insert((e.clone(), day), Availability::Loading);
            }
        }
        let from = self.zone.day_start(day);
        let to = self.zone.day_start(day + Duration::days(1));
        let shared = Arc::clone(&self.cal.free_busy);
        self.bg.spawn(async move {
            let res = tokio::time::timeout(
                std::time::Duration::from_secs(30),
                handle.free_busy(cal, missing.clone(), from, to),
            )
            .await;
            let mut map = shared.lock().unwrap_or_else(PoisonError::into_inner);
            match res {
                Ok(Ok(list)) => {
                    for e in &missing {
                        let a = list
                            .iter()
                            .find(|f| f.email == *e)
                            .and_then(|f| f.busy.as_ref())
                            .map(|b| Availability::Busy(b.iter().map(|s| (s.start, s.end, s.tentative)).collect()))
                            .unwrap_or(Availability::Unknown);
                        map.insert((e.clone(), day), a);
                    }
                }
                _ => {
                    for e in &missing {
                        map.insert((e.clone(), day), Availability::Unknown);
                    }
                }
            }
            crate::JobOutput::Custom(Box::new(()))
        });
    }

    /// Disponibilidad conocida de una persona ese día (`Loading` si no se pidió).
    pub fn availability(&self, email: &str, day: NaiveDate) -> Availability {
        let map = self.cal.free_busy.lock().unwrap_or_else(PoisonError::into_inner);
        map.get(&(email.trim().to_lowercase(), day)).cloned().unwrap_or(Availability::Loading)
    }

    /// Mi disponibilidad ese día, de mis calendarios (sin `skip`, el evento que se edita).
    pub fn my_availability(&mut self, day: NaiveDate, skip: Option<EventId>) -> Availability {
        let from = self.zone.day_start(day);
        let to = self.zone.day_start(day + Duration::days(1));
        let hidden = self.hidden_calendar_ids();
        let busy = self
            .occurrences_uncached(from, to)
            .into_iter()
            .filter(|o| {
                o.busy && !o.cancelled && !o.all_day && Some(o.id) != skip && !hidden.contains(&o.calendar.get())
            })
            .map(|o| (o.start, o.end, o.tentative))
            .collect();
        Availability::Busy(busy)
    }

    // ------------------------------------------------------------------ recordatorios

    fn refresh_candidates(&mut self) {
        let now = self.now();
        // Se recalcula cuando cambian los datos o cada 10 minutos.
        let key = (now.timestamp() / 600, self.data_gen);
        if self.cal.candidates_key == Some(key) {
            return;
        }
        self.cal.candidates_key = Some(key);
        let occ = self.occurrences_uncached(now - Duration::days(1), now + Duration::days(2));
        let hidden = self.hidden_calendar_ids();
        let mut reminders: HashMap<EventId, Vec<iuredesk_model::Reminder>> = HashMap::new();
        let mut out = Vec::new();
        for o in occ.into_iter().filter(|o| !o.cancelled && !hidden.contains(&o.calendar.get())).take(2_000) {
            let r = reminders
                .entry(o.id)
                .or_insert_with(|| self.store.get_event(o.id).ok().flatten().map(|e| e.reminders).unwrap_or_default())
                .clone();
            if r.is_empty() {
                continue;
            }
            out.push(Candidate {
                event: o.id,
                summary: o.summary,
                location: o.location,
                start: o.start,
                end: o.end,
                all_day: o.all_day,
                reminders: r,
            });
        }
        self.cal.candidates = out;
    }

    /// Recordatorios que tocan ahora.
    pub fn due_reminders(&mut self) -> Vec<DueReminder> {
        self.refresh_candidates();
        self.cal.reminders.due(&self.cal.candidates, self.now())
    }

    /// Recordatorios que empiezan a tocar (para la notificación del escritorio; una sola vez).
    pub fn take_new_reminders(&mut self) -> Vec<DueReminder> {
        let due = self.due_reminders();
        self.cal.reminders.take_new(&due)
    }

    /// Pospone un recordatorio.
    pub fn snooze_reminder(&mut self, key: ReminderKey, minutes: i64) {
        let now = self.now();
        self.cal.reminders.snooze(key, minutes, now);
    }

    /// Descarta un recordatorio.
    pub fn dismiss_reminder(&mut self, key: ReminderKey) {
        self.cal.reminders.dismiss(key);
    }

    /// Cuánto falta para el próximo recordatorio (para despertar la interfaz).
    pub fn next_reminder_in(&mut self) -> Option<std::time::Duration> {
        self.refresh_candidates();
        let now = self.now();
        let next = self.cal.reminders.next_fire(&self.cal.candidates, now)?;
        (next - now).to_std().ok()
    }
}

/// Resultado de una edición.
#[derive(Debug, Default)]
struct EditResult {
    undo: CalUndo,
    created: Vec<EventId>,
}

/// «Toda la serie» desde una aparición: los cambios de la aparición se aplican a la serie y, si
/// se movió, la serie se corre lo mismo.
fn whole_series(master: &Event, at: &EventTime, edited: &Event) -> Event {
    let mut ev = edited.clone();
    ev.id = master.id;
    ev.uid = master.uid.clone();
    ev.exdates = master.exdates.clone();
    if edited.rrule.is_none() {
        ev.rrule = master.rrule.clone();
    }
    let len_utc = edited.end.to_utc() - edited.start.to_utc();
    // Si cambió de «todo el día» a «con hora» (o al revés), la serie empieza en esta aparición.
    if let Some(s) = shift_time(&master.start, at, &edited.start) {
        {
            ev.end = match (&s, &edited.end) {
                (EventTime::DateTime { utc, tzid }, EventTime::DateTime { .. }) => {
                    EventTime::DateTime { utc: *utc + len_utc, tzid: tzid.clone() }
                }
                (EventTime::Date(d), EventTime::Date(_)) => {
                    EventTime::Date(*d + Duration::days(len_utc.num_days().max(1)))
                }
                _ => edited.end.clone(),
            };
            ev.start = s;
        }
    }
    ev
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn whole_series_shifts_master() {
        let s = Utc.with_ymd_and_hms(2026, 3, 2, 9, 0, 0).unwrap();
        let master = Event {
            id: EventId(3),
            uid: "s".into(),
            start: EventTime::DateTime { utc: s, tzid: Some("Europe/Madrid".into()) },
            end: EventTime::DateTime { utc: s + Duration::hours(1), tzid: Some("Europe/Madrid".into()) },
            rrule: Some("FREQ=WEEKLY".into()),
            ..Default::default()
        };
        let at = EventTime::DateTime { utc: s + Duration::weeks(2), tzid: None };
        let mut edited = master.clone();
        edited.start = EventTime::DateTime { utc: s + Duration::weeks(2) + Duration::minutes(30), tzid: None };
        edited.end = EventTime::DateTime { utc: s + Duration::weeks(2) + Duration::minutes(120), tzid: None };
        edited.summary = "Nuevo".into();
        let w = whole_series(&master, &at, &edited);
        assert_eq!(w.start, EventTime::DateTime { utc: s + Duration::minutes(30), tzid: Some("Europe/Madrid".into()) });
        assert_eq!(w.end.to_utc() - w.start.to_utc(), Duration::minutes(90));
        assert_eq!(w.summary, "Nuevo");
        assert_eq!(w.id, master.id);
    }
}
