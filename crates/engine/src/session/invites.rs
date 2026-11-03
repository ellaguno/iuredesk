//! UX-11: videollamada e invitados de un evento.
//!
//! - **Videollamada**: el editor tiene «Agregar videollamada» cuando el servidor del calendario sabe
//!   crearla ([`Session::calendar_caps`]: Teams en Microsoft 365, Meet en Google). El evento se
//!   guarda con la reunión pedida (`OnlineMeeting` sin enlace) y la sincronización la crea; una
//!   que ya existe no se quita desde aquí (Outlook no deja apagarla; en Google se quita allá).
//! - **Invitados**: [`Session::invite_summary`] dice qué se va a mandar al guardar (invitación,
//!   actualización, cancelación y a cuántos) y cómo: lo manda el servidor (Graph, CalDAV con
//!   programación) o IureDesk por correo (iMIP `REQUEST`/`CANCEL` en la bandeja de salida, que se
//!   puede deshacer en su ventana). Un calendario local no manda nada, y se dice.
//! - Si cambia algo que importa a los invitados, sube `SEQUENCE` (RFC 5546).

use chrono::{DateTime, Utc};
use iuredesk_compose::{CalendarMethod, CalendarPart, Draft, OutboxId, RichBody};
use iuredesk_model::account::Identity;
use iuredesk_model::{CalendarId, Event, EventTime, Mailbox, MeetingProvider, OnlineMeeting};
use iuredesk_pim_sync::CalendarCaps;
use iuredesk_pim_sync::itip_out::{self, InvitePlan, OutMethod};

use super::Session;
use crate::calendar::draft::EventDraft;
use crate::lang::Text;

/// Cómo llegan las invitaciones de un calendario.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InviteDelivery {
    /// Las manda el servidor del calendario.
    Server,
    /// Las manda IureDesk por correo desde la cuenta.
    Email,
    /// Calendario local (o sin cuenta en marcha): no se manda nada.
    None,
}

/// Lo que el editor dice antes de guardar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InviteSummary {
    pub plan: InvitePlan,
    pub delivery: InviteDelivery,
}

impl InviteSummary {
    /// Una línea para el usuario («Se enviará la invitación a 3 personas.»).
    pub fn describe(&self) -> Text {
        let n = |v: &[Mailbox]| v.len();
        let who = |k: usize, en: &str, es: &str| -> Option<Text> {
            match k {
                0 => None,
                1 => Some(Text::new(format!("{en} to 1 person"), format!("{es} a 1 persona"))),
                k => Some(Text::new(format!("{en} to {k} people"), format!("{es} a {k} personas"))),
            }
        };
        let parts: Vec<Text> = [
            who(n(&self.plan.invite), "the invitation", "la invitación"),
            who(n(&self.plan.update), "the update", "la actualización"),
            who(n(&self.plan.cancel), "the cancellation", "la cancelación"),
        ]
        .into_iter()
        .flatten()
        .collect();
        let join = |f: fn(&Text) -> &str, and: &str| {
            let v: Vec<&str> = parts.iter().map(f).collect();
            match v.split_last() {
                Some((last, rest)) if !rest.is_empty() => format!("{} {and} {last}", rest.join(", ")),
                Some((last, _)) => (*last).to_string(),
                None => String::new(),
            }
        };
        let (en, es) = (join(|t| t.en.as_str(), "and"), join(|t| t.es.as_str(), "y"));
        match self.delivery {
            InviteDelivery::Server => Text::new(
                format!("Your calendar server will send {en}."),
                format!("Tu servidor de calendario enviará {es}."),
            ),
            InviteDelivery::Email => {
                Text::new(format!("IureDesk will email {en}."), format!("IureDesk enviará por correo {es}."))
            }
            InviteDelivery::None => Text::new(
                "This calendar is only on this computer: no invitations are sent.",
                "Este calendario es solo de este equipo: no se envían invitaciones.",
            ),
        }
    }

    /// ¿Sale algo?
    pub fn sends(&self) -> bool {
        self.delivery != InviteDelivery::None && !self.plan.is_empty()
    }
}

/// Fecha y hora para el texto del correo.
fn when_text(t: &EventTime, zone: crate::Zone) -> String {
    match t {
        EventTime::Date(d) => d.format("%d/%m/%Y").to_string(),
        EventTime::DateTime { utc, .. } => zone.local(*utc).format("%d/%m/%Y %H:%M").to_string(),
    }
}

impl Session {
    /// UX-11: lo que el servidor de un calendario hace solo (fijado a mano en pruebas y capturas).
    pub fn calendar_caps(&self, cal: CalendarId) -> Option<CalendarCaps> {
        if let Some(c) = self.cal.caps_override.get(&cal) {
            return Some(*c);
        }
        self.pim.handle.as_ref()?.calendar_caps(cal)
    }

    /// Fija las capacidades de un calendario (pruebas, capturas).
    #[doc(hidden)]
    pub fn demo_calendar_caps(&mut self, cal: CalendarId, caps: CalendarCaps) {
        self.cal.caps_override.insert(cal, caps);
    }

    /// Videollamada que se puede crear en el calendario (para «Agregar videollamada»).
    pub fn meeting_provider(&self, cal: CalendarId) -> Option<MeetingProvider> {
        self.calendar_caps(cal).and_then(|c| c.meeting)
    }

    fn invite_delivery(&self, cal: CalendarId) -> InviteDelivery {
        match self.calendar_caps(cal) {
            Some(c) if c.server_invites => InviteDelivery::Server,
            Some(_) if self.sender.is_some() => InviteDelivery::Email,
            _ => InviteDelivery::None,
        }
    }

    /// Mis direcciones (de todas las cuentas), en minúsculas, más la del organizador del calendario.
    fn me_for(&self, cal: CalendarId) -> Vec<String> {
        let mut v: Vec<String> = self.my_addresses().iter().map(|a| a.trim().to_lowercase()).collect();
        v.extend(self.organizer_for(cal).map(|m| m.addr.trim().to_lowercase()));
        v
    }

    /// El evento que resulta del formulario: organizador, videollamada pedida o quitada (si aún no
    /// existe) y `SEQUENCE` si cambió algo que importa a los invitados.
    pub(crate) fn draft_event(&self, d: &EventDraft) -> Event {
        let mut ev = d.to_event(self.zone, self.organizer_for(d.calendar));
        match (d.video_call, ev.online_meeting.as_ref()) {
            (true, None) => {
                if let Some(provider) = self.meeting_provider(d.calendar) {
                    ev.online_meeting = Some(OnlineMeeting { provider, join_url: String::new() });
                }
            }
            (false, Some(m)) if m.is_pending() => ev.online_meeting = None,
            _ => {}
        }
        if let Some(base) = &d.base
            && !ev.attendees.is_empty()
            && itip_out::significant_change(base, &ev)
        {
            ev.sequence = base.sequence.saturating_add(1);
        }
        ev
    }

    /// UX-11: qué se mandará a los invitados al guardar el formulario (`None`: nada).
    pub fn invite_summary(&self, d: &EventDraft) -> Option<InviteSummary> {
        let new = self.draft_event(d);
        let plan = itip_out::plan(d.base.as_ref(), Some(&new), &self.me_for(d.calendar));
        (!plan.is_empty()).then(|| InviteSummary { plan, delivery: self.invite_delivery(d.calendar) })
    }

    /// UX-11: qué se mandará al borrar el evento entero (`None`: nada).
    pub fn cancel_summary(&self, ev: &Event) -> Option<InviteSummary> {
        let plan = itip_out::plan(Some(ev), None, &self.me_for(ev.calendar));
        (!plan.is_empty()).then(|| InviteSummary { plan, delivery: self.invite_delivery(ev.calendar) })
    }

    /// Manda por correo lo que toque (si el servidor no lo hace). Devuelve lo puesto en la
    /// bandeja (para deshacer) y la frase para el aviso.
    pub(crate) fn deliver_invites(
        &mut self,
        cal: CalendarId,
        old: Option<&Event>,
        new: Option<&Event>,
    ) -> (Vec<OutboxId>, Option<Text>) {
        let plan = itip_out::plan(old, new, &self.me_for(cal));
        if plan.is_empty() {
            return (Vec::new(), None);
        }
        let summary = InviteSummary { plan: plan.clone(), delivery: self.invite_delivery(cal) };
        let sent_text = |s: &InviteSummary| {
            let total = s.plan.invite.len() + s.plan.update.len() + s.plan.cancel.len();
            Text::new(
                format!("notice sent to {total} {}", if total == 1 { "person" } else { "people" }),
                format!("aviso a {total} {}", if total == 1 { "persona" } else { "personas" }),
            )
        };
        if summary.delivery != InviteDelivery::Email {
            return (Vec::new(), (summary.delivery == InviteDelivery::Server).then(|| sent_text(&summary)));
        }
        let Some(account) = self.calendars.iter().find(|c| c.id == cal).map(|c| c.account) else {
            return (Vec::new(), None);
        };
        let Some(me) = self.organizer_for(cal) else { return (Vec::new(), None) };
        let now = self.now();
        let mut ids = Vec::new();
        let groups: [(&[Mailbox], OutMethod, Option<&Event>, (&str, &str)); 3] = [
            (&plan.invite, OutMethod::Request, new, ("Invitation", "Invitación")),
            (&plan.update, OutMethod::Request, new, ("Updated", "Actualizada")),
            (&plan.cancel, OutMethod::Cancel, new.or(old), ("Cancelled", "Cancelada")),
        ];
        let mut failed = false;
        for (to, method, ev, (en, es)) in groups {
            let Some(ev) = ev.filter(|_| !to.is_empty()) else { continue };
            match self.imip_request(&me, to, ev, method, if crate::lang::spanish() { es } else { en }, now) {
                Some(d) => {
                    match self.sender.as_ref().map(|s| s.outbox.queue(account, &d, &iuredesk_compose::FsResolver)) {
                        Some(Ok(id)) => ids.push(id),
                        _ => failed = true,
                    }
                }
                None => failed = true,
            }
        }
        if let Some(s) = &self.sender {
            s.wake.notify_one();
        }
        if failed {
            self.notices.push(crate::Notice::error(Text::new(
                "Some invitations couldn't be queued: check the account's outgoing server.",
                "Algunas invitaciones no se pudieron poner en la bandeja de salida: revisa el servidor de salida de la cuenta.",
            )));
        }
        let said = (!ids.is_empty()).then(|| sent_text(&summary));
        (ids, said)
    }

    /// Correo iMIP con la invitación o la cancelación.
    fn imip_request(
        &self,
        me: &Mailbox,
        to: &[Mailbox],
        ev: &Event,
        method: OutMethod,
        prefix: &str,
        now: DateTime<Utc>,
    ) -> Option<Draft> {
        let mut d = Draft::new(Identity {
            name: me.name.clone().unwrap_or_default(),
            email: me.addr.clone(),
            signature: None,
            reply_to: None,
        });
        d.to = to.to_vec();
        let title = if ev.summary.trim().is_empty() { "—" } else { ev.summary.trim() };
        let when = when_text(&ev.start, self.zone);
        d.subject = format!("{prefix}: {title} ({when})");
        let mut body = format!("{title}\n{when}");
        if !ev.location.trim().is_empty() {
            body.push_str(&format!("\n{}", ev.location.trim()));
        }
        if let Some(u) = crate::today::join_url(ev) {
            body.push_str(&format!("\n\n{}: {u}", crate::lang::pick("Join", "Unirse")));
        }
        if !ev.description.trim().is_empty() {
            body.push_str(&format!("\n\n{}", ev.description.trim()));
        }
        d.body = RichBody::from_plain(&body);
        let ics = itip_out::ics(ev, method, to, now);
        let m = match method {
            OutMethod::Request => CalendarMethod::Request,
            OutMethod::Cancel => CalendarMethod::Cancel,
        };
        d.calendar = Some(CalendarPart::new(m, ics.into_bytes()).ok()?);
        Some(d)
    }
}
