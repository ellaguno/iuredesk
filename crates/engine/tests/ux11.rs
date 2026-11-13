//! UX-11 en la sesión: «Agregar videollamada» (Meet/Teams según el calendario) y lo que se
//! manda a los invitados: lo dice antes de guardar, por correo (iMIP en la bandeja de salida,
//! con deshacer) si el servidor no lo hace, actualización con `SEQUENCE` nueva, cancelación a
//! los quitados y al borrar.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{NaiveDate, TimeZone, Utc};
use iuredesk_accounts::MemoryStore;
use iuredesk_engine::session::InviteDelivery;
use iuredesk_engine::{OfflineTransport, Outcome, PimBackend, Session, SessionConfig, Zone};
use iuredesk_model::{Event, EventId, Mailbox, MeetingProvider, PartStat};
use iuredesk_pim_sync::{CalendarCaps, ImipReply, PimError, SeriesScope};
use iuredesk_store::{Store, sample};

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 21, 11, 20, 0).unwrap()
}

/// Motor falso: aplica en el almacén con `pim_sync::ops` y anota cada llamada.
#[derive(Default)]
struct Fake {
    calls: Mutex<Vec<String>>,
}

impl PimBackend for Fake {
    fn save_event(&self, store: &Store, ev: &Event) -> Result<EventId, PimError> {
        self.calls.lock().unwrap().push(format!("save:{}", ev.summary));
        iuredesk_pim_sync::ops::save_event(store, ev)
    }
    fn delete_event(&self, store: &Store, id: EventId) -> Result<(), PimError> {
        self.calls.lock().unwrap().push(format!("delete:{id}"));
        iuredesk_pim_sync::ops::delete_event(store, id)
    }
    fn rsvp(
        &self,
        store: &Store,
        id: EventId,
        partstat: PartStat,
        comment: Option<&str>,
        me: &[String],
    ) -> Result<Option<ImipReply>, PimError> {
        self.calls.lock().unwrap().push(format!("rsvp:{partstat:?}"));
        iuredesk_pim_sync::ops::rsvp(store, id, partstat, comment, me, true, true, now())
    }
}

fn session(dir: &std::path::Path) -> (Session, Arc<Fake>) {
    iuredesk_engine::lang::set_lang(iuredesk_engine::lang::Lang::Es);
    let store = Store::open(dir).unwrap();
    sample::populate(&store, sample::DEFAULT_SEED, now()).unwrap();
    let mut s = Session::new(SessionConfig {
        store: Arc::new(store),
        accounts: None,
        secrets: Arc::new(MemoryStore::default()),
        prefs_path: None,
        now: Some(now()),
        zone: Zone::utc(),
        sample: true,
        sync: None,
        transport: Some(Arc::new(OfflineTransport)),
    })
    .unwrap();
    let fake = Arc::new(Fake::default());
    s.set_pim_backend(fake.clone());
    settle(&mut s);
    (s, fake)
}

fn settle(s: &mut Session) {
    let start = Instant::now();
    loop {
        s.poll();
        if s.is_idle() {
            s.poll();
            return;
        }
        assert!(start.elapsed() < Duration::from_secs(30), "los trabajos de fondo no terminan");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn day(d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 10, d).unwrap()
}

fn new_meeting(s: &Session, people: &[&str]) -> iuredesk_engine::calendar::draft::EventDraft {
    let start = day(23).and_hms_opt(16, 0, 0).unwrap();
    let mut d = s.new_event_draft(start, start + chrono::Duration::hours(1), false).unwrap();
    d.title = "Revisión del contrato".into();
    for p in people {
        d.add_attendee(Mailbox::new("", *p), false);
    }
    d
}

fn saved(s: &Session, title: &str) -> Event {
    let cal = s.calendars().iter().map(|c| c.id).collect::<Vec<_>>();
    s.store()
        .events_between(
            &cal,
            Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap(),
        )
        .unwrap()
        .into_iter()
        .find(|e| e.summary == title)
        .unwrap()
}

fn email_caps(s: &mut Session, cal: iuredesk_model::CalendarId) {
    s.demo_calendar_caps(cal, CalendarCaps { server_invites: false, meeting: None, exceptions: true });
}

#[test]
fn invitations_by_email_with_undo() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, _) = session(dir.path());
    let d = new_meeting(&s, &["ana@contoso.com", "beto@contoso.com"]);
    email_caps(&mut s, d.calendar);
    let sum = s.invite_summary(&d).unwrap();
    assert_eq!(sum.delivery, InviteDelivery::Email);
    assert_eq!(sum.describe().es, "IureDesk enviará por correo la invitación a 2 personas.");
    let Outcome::Notice(n) = s.save_event_draft(&d, SeriesScope::All).unwrap() else { panic!() };
    assert!(n.undo && n.text.es.contains("aviso a 2 personas"), "{:?}", n.text);
    let out = s.outbox_entries();
    assert_eq!(out.len(), 1, "un correo para los dos");
    assert!(out[0].subject.starts_with("Invitación: Revisión del contrato"), "{}", out[0].subject);
    assert!(out[0].to.contains("ana@contoso.com") && out[0].to.contains("beto@contoso.com"));
    // Deshacer quita la cita y la invitación que aún no salió.
    s.run("undo").unwrap();
    assert!(s.outbox_entries().is_empty());
}

#[test]
fn edits_send_updates_and_cancellations() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, _) = session(dir.path());
    let d = new_meeting(&s, &["ana@contoso.com", "beto@contoso.com"]);
    email_caps(&mut s, d.calendar);
    s.save_event_draft(&d, SeriesScope::All).unwrap();
    let ev = saved(&s, "Revisión del contrato");
    let before = s.outbox_entries().len();

    // Solo se mueve: actualización a los dos y `SEQUENCE` nueva.
    let mut e = iuredesk_engine::calendar::draft::EventDraft::from_event(&ev, None, Zone::utc(), false);
    e.start_min += 60;
    e.end_min += 60;
    assert_eq!(
        s.invite_summary(&e).unwrap().describe().es,
        "IureDesk enviará por correo la actualización a 2 personas."
    );
    s.save_event_draft(&e, SeriesScope::All).unwrap();
    let moved = saved(&s, "Revisión del contrato");
    assert_eq!(moved.sequence, ev.sequence + 1);
    let out = s.outbox_entries();
    assert_eq!(out.len(), before + 1);
    assert!(out.iter().any(|o| o.subject.starts_with("Actualizada:")));

    // Quitar a Beto sin cambiar nada más: solo su cancelación.
    let mut e = iuredesk_engine::calendar::draft::EventDraft::from_event(&moved, None, Zone::utc(), false);
    e.attendees.retain(|a| a.mailbox.addr != "beto@contoso.com");
    let sum = s.invite_summary(&e).unwrap();
    assert_eq!((sum.plan.invite.len(), sum.plan.update.len(), sum.plan.cancel.len()), (0, 0, 1));
    s.save_event_draft(&e, SeriesScope::All).unwrap();
    assert!(s.outbox_entries().iter().any(|o| o.subject.starts_with("Cancelada:") && o.to.contains("beto@")));

    // Borrar la cita: cancelación a quien queda.
    let left = saved(&s, "Revisión del contrato");
    assert_eq!(s.cancel_summary(&left).unwrap().plan.cancel.len(), 1);
    let from = Zone::utc().day_start(day(19));
    let to = Zone::utc().day_start(day(30));
    let o = s.visible_occurrences(from, to).into_iter().find(|o| o.summary == "Revisión del contrato").unwrap();
    let Outcome::Notice(n) = s.delete_occurrence(&o, SeriesScope::All).unwrap() else { panic!() };
    assert!(n.text.es.contains("aviso a 1 persona"), "{:?}", n.text);
    assert!(s.outbox_entries().iter().filter(|o| o.subject.starts_with("Cancelada:")).count() >= 2);
}

#[test]
fn server_scheduling_sends_nothing_from_here_and_meet_is_requested() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, _) = session(dir.path());
    let mut d = new_meeting(&s, &["ana@contoso.com"]);
    s.demo_calendar_caps(
        d.calendar,
        CalendarCaps { server_invites: true, meeting: Some(MeetingProvider::GoogleMeet), exceptions: true },
    );
    assert_eq!(s.meeting_provider(d.calendar), Some(MeetingProvider::GoogleMeet));
    d.video_call = true;
    let sum = s.invite_summary(&d).unwrap();
    assert_eq!(sum.delivery, InviteDelivery::Server);
    assert_eq!(sum.describe().es, "Tu servidor de calendario enviará la invitación a 1 persona.");
    s.save_event_draft(&d, SeriesScope::All).unwrap();
    assert!(s.outbox_entries().is_empty(), "lo manda el servidor");
    let ev = saved(&s, "Revisión del contrato");
    let m = ev.online_meeting.clone().unwrap();
    assert_eq!(m.provider, MeetingProvider::GoogleMeet);
    assert!(m.is_pending(), "la crea la sincronización");
    // Apagarla antes de que exista la quita; una ya creada se conserva.
    let mut e = iuredesk_engine::calendar::draft::EventDraft::from_event(&ev, None, Zone::utc(), false);
    assert!(e.video_call);
    e.video_call = false;
    s.save_event_draft(&e, SeriesScope::All).unwrap();
    assert_eq!(saved(&s, "Revisión del contrato").online_meeting, None);
}

#[test]
fn local_calendars_say_nothing_is_sent_and_no_video_option() {
    let dir = tempfile::tempdir().unwrap();
    let (s, _) = session(dir.path());
    let mut d = new_meeting(&s, &["ana@contoso.com"]);
    assert_eq!(s.meeting_provider(d.calendar), None);
    d.video_call = true;
    let sum = s.invite_summary(&d).unwrap();
    assert_eq!(sum.delivery, InviteDelivery::None);
    assert!(!sum.sends());
    assert!(sum.describe().es.contains("no se envían invitaciones"));
    // Sin asistentes no hay nada que decir.
    assert!(s.invite_summary(&new_meeting(&s, &[])).is_none());
}

#[test]
fn google_html_descriptions_open_as_markdown_and_save_back_as_html() {
    // Regresión (dueño 2026-11-03): la descripción de una cita de Google se veía como HTML crudo.
    let dir = tempfile::tempdir().unwrap();
    let (mut s, _) = session(dir.path());
    let d = new_meeting(&s, &[]);
    s.save_event_draft(&d, SeriesScope::All).unwrap();
    let mut ev = saved(&s, "Revisión del contrato");
    ev.description = "<p><strong>Agenda</strong><br>📅 <strong>Fecha:</strong> 3 nov</p>\n\n-::~:~::~:~:~:~::~:~::-\nÚnase con Google Meet: https://meet.google.com/abc\n-::~:~::~:~:~:~::~:~::-".into();
    s.store().put_event(&ev).unwrap();
    let ev = saved(&s, "Revisión del contrato");
    let mut e = iuredesk_engine::calendar::draft::EventDraft::from_event(&ev, None, Zone::utc(), false);
    assert!(e.description.contains("**Agenda**") && !e.description.contains("<p>"), "{}", e.description);
    assert!(e.description_shown.conference.contains("meet.google.com/abc"));
    // Guardar sin tocarla: idéntica.
    s.save_event_draft(&e, SeriesScope::All).unwrap();
    assert_eq!(saved(&s, "Revisión del contrato").description, ev.description);
    // Editada: vuelve como HTML y con el bloque de Meet.
    e.description.push_str("\n\nTraer el **contrato**");
    s.save_event_draft(&e, SeriesScope::All).unwrap();
    let out = saved(&s, "Revisión del contrato").description;
    assert!(out.contains("<strong>contrato</strong>") && out.contains("meet.google.com/abc"), "{out}");
}
