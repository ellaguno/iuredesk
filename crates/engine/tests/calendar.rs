//! Calendario en la sesión (F2.4): crear/mover/borrar con deshacer, editar series («solo este»,
//! «este y los siguientes», «toda la serie»), comandos `cal.*` y el flujo de invitaciones con un
//! motor PIM falso que registra las llamadas.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{NaiveDate, TimeZone, Utc};
use iuredesk_accounts::MemoryStore;
use iuredesk_engine::calendar::draft::EventDraft;
use iuredesk_engine::calendar::invite::InviteKind;
use iuredesk_engine::calendar::nav::CalView;
use iuredesk_engine::{Module, OfflineTransport, Outcome, PimBackend, Session, SessionConfig, Zone};
use iuredesk_model::{Event, EventId, EventTime, PartStat};
use iuredesk_pim_sync::{ImipReply, PimError, SeriesScope};
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

fn week_titles(s: &mut Session) -> Vec<String> {
    let from = Zone::utc().day_start(day(19));
    let to = Zone::utc().day_start(day(26));
    s.visible_occurrences(from, to).into_iter().map(|o| o.summary).collect()
}

#[test]
fn create_then_undo_removes_it() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, fake) = session(dir.path());
    let start = day(22).and_hms_opt(7, 0, 0).unwrap();
    let mut d = s.new_event_draft(start, start + chrono::Duration::minutes(45), false).unwrap();
    d.title = "Café con Ana".into();
    let out = s.save_event_draft(&d, SeriesScope::All).unwrap();
    assert!(matches!(out, Outcome::Notice(ref n) if n.undo));
    assert!(week_titles(&mut s).contains(&"Café con Ana".to_string()));
    s.run("undo").unwrap();
    assert!(!week_titles(&mut s).contains(&"Café con Ana".to_string()));
    let calls = fake.calls.lock().unwrap().clone();
    assert_eq!(calls.first().map(String::as_str), Some("save:Café con Ana"));
    assert!(calls.iter().any(|c| c.starts_with("delete:")));
}

#[test]
fn drag_move_and_delete_with_undo() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, _) = session(dir.path());
    let from = Zone::utc().day_start(day(19));
    let to = Zone::utc().day_start(day(26));
    let o = s.visible_occurrences(from, to).into_iter().find(|o| !o.recurring && !o.all_day).unwrap();
    let new_start = o.start + chrono::Duration::days(1) + chrono::Duration::minutes(30);
    let len = o.end - o.start;
    s.reschedule(
        &o,
        EventTime::DateTime { utc: new_start, tzid: None },
        EventTime::DateTime { utc: new_start + len, tzid: None },
        SeriesScope::All,
    )
    .unwrap();
    let ev = s.calendar_event(o.id).unwrap();
    assert_eq!(ev.start.to_utc(), new_start);
    assert!(ev.start.tz().is_some(), "conserva la zona del evento");
    s.run("undo").unwrap();
    assert_eq!(s.calendar_event(o.id).unwrap().start.to_utc(), o.start);

    s.delete_occurrence(&o, SeriesScope::All).unwrap();
    assert!(s.calendar_event(o.id).is_none());
    s.run("undo").unwrap();
    assert_eq!(s.calendar_event(o.id).unwrap().summary, o.summary, "deshacer devuelve el evento");
}

#[test]
fn recurring_edit_scopes() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, _) = session(dir.path());
    let range = |s: &mut Session, a: u32, b: u32| {
        s.visible_occurrences(Zone::utc().day_start(day(a)), Zone::utc().day_start(day(b)))
            .into_iter()
            .filter(|o| o.summary.starts_with("Junta semanal") || o.summary.starts_with("Junta movida"))
            .collect::<Vec<_>>()
    };
    // La junta semanal de los lunes: 19 y 26 de octubre, 2 de noviembre.
    let occ = range(&mut s, 19, 31);
    assert_eq!(occ.len(), 2);
    let second = occ[1].clone();

    // Solo este: la serie sigue y la del 26 queda como evento suelto.
    let mut d: EventDraft = s.draft_for(&second).unwrap();
    d.title = "Junta movida".into();
    s.save_event_draft(&d, SeriesScope::This).unwrap();
    let after = range(&mut s, 19, 31);
    assert_eq!(after.iter().filter(|o| o.summary == "Junta movida").count(), 1);
    assert!(after.iter().any(|o| o.summary.starts_with("Junta semanal") && o.recurring));
    s.run("undo").unwrap();
    assert_eq!(range(&mut s, 19, 31).iter().filter(|o| o.summary == "Junta movida").count(), 0);

    // Este y los siguientes: dos series, la nueva desde el 26.
    let mut d = s.draft_for(&second).unwrap();
    d.title = "Junta movida".into();
    s.save_event_draft(&d, SeriesScope::ThisAndFollowing).unwrap();
    let later = s.visible_occurrences(Zone::utc().day_start(day(19)), Zone::utc().day_start(day(19)) + chrono::Duration::days(21));
    let titles: Vec<_> = later.iter().filter(|o| o.recurring).map(|o| o.summary.clone()).collect();
    assert_eq!(titles.iter().filter(|t| t.starts_with("Junta semanal")).count(), 1, "solo el 19");
    assert_eq!(titles.iter().filter(|t| *t == "Junta movida").count(), 2, "26 y 2 de noviembre");
    s.run("undo").unwrap();

    // Toda la serie desde una aparición.
    let mut d = s.draft_for(&second).unwrap();
    d.title = "Junta movida".into();
    s.save_event_draft(&d, SeriesScope::All).unwrap();
    assert!(range(&mut s, 19, 31).iter().all(|o| o.summary == "Junta movida"));

    // Borrar solo esta aparición.
    let o = range(&mut s, 26, 27).remove(0);
    s.delete_occurrence(&o, SeriesScope::This).unwrap();
    assert_eq!(range(&mut s, 19, 31).len(), 1);
    s.run("undo").unwrap();
    assert_eq!(range(&mut s, 19, 31).len(), 2);
}

#[test]
fn calendar_commands_navigate_and_switch_views() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, _) = session(dir.path());
    s.run("nav.calendar").unwrap();
    s.run("cal.view.day").unwrap();
    assert_eq!(s.cal_view(), CalView::Day);
    s.run("cal.next").unwrap();
    assert_eq!(s.cal_anchor(), day(22));
    s.run("cal.today").unwrap();
    assert_eq!(s.cal_anchor(), day(21));
    // «Nuevo» en el calendario abre el editor de eventos.
    assert_eq!(s.module, Module::Calendar);
    match s.run("mail.new").unwrap() {
        Outcome::EditEvent(d) => assert_eq!(d.start_date, day(21)),
        other => panic!("se esperaba el editor de eventos: {other:?}"),
    }
}

#[test]
fn invitation_accept_imports_and_queues_the_reply() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, fake) = session(dir.path());
    let rows: Vec<_> = s.rows().iter().map(|r| r.id).collect();
    let msg = rows
        .into_iter()
        .find(|id| s.envelope(*id).is_some_and(|e| e.subject.starts_with("Invitación: Revisión")))
        .expect("el ejemplo trae una invitación");
    s.request_body(msg);
    settle(&mut s);
    assert!(s.body(msg).is_some());
    let (inv, conflicts) = s.invitation(msg).unwrap();
    assert_eq!(inv.kind, InviteKind::Request);
    assert!(inv.existing.is_none());
    assert_eq!(inv.my_status, Some(PartStat::NeedsAction));
    let _ = conflicts;

    s.respond_invitation(msg, PartStat::Accepted, Some("Ahí estaré")).unwrap();
    let (inv, _) = s.invitation(msg).unwrap();
    assert!(inv.existing.is_some(), "quedó en mi calendario");
    assert_eq!(inv.my_status, Some(PartStat::Accepted));
    let calls = fake.calls.lock().unwrap().clone();
    assert!(calls.iter().any(|c| c == "rsvp:Accepted"));
    let replies = s
        .store()
        .peek_ops(1000)
        .unwrap()
        .into_iter()
        .filter(|o| o.kind == iuredesk_pim_sync::OP_IMIP_REPLY)
        .count();
    assert_eq!(replies, 1, "respuesta iMIP para la bandeja de salida");
}
