//! F4.2: el asistente, la suite y la importación unidos a la sesión (sin red: proveedores de
//! respuestas fijas y una suite vacía).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use chrono::{TimeZone, Utc};
use iuredesk_accounts::MemoryStore;
use iuredesk_engine::assist::{
    AiKey, AiState, AiTask, AiValue, Assist, CannedProvider, ProviderFactory, sample_answer,
};
use iuredesk_engine::iuredesk_assistant::schema::Importance;
use iuredesk_engine::iuredesk_assistant::{AiProvider, DataField};
use iuredesk_engine::migrate::{Migrate, MigrateState};
use iuredesk_engine::suite::{SuiteAction, SuiteEvent, SuiteHub};
use iuredesk_engine::{OfflineTransport, Outcome, Session, SessionConfig, Zone};
use iuredesk_model::MessageId;
use iuredesk_store::{Store, sample};

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 21, 15, 0, 0).unwrap()
}

fn session(dir: &std::path::Path) -> Session {
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
    s.prefs.mark_read_delay_ms = 0;
    wait(|| {
        s.poll();
        s.is_idle()
    });
    s
}

fn wait(mut done: impl FnMut() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(start.elapsed() < Duration::from_secs(30), "no termina");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Proveedor externo que cuenta las llamadas.
fn external(calls: Arc<AtomicUsize>) -> ProviderFactory {
    Arc::new(move |kind| {
        let calls = Arc::clone(&calls);
        Arc::new(CannedProvider::new("Anthropic · prueba", true, move |req| {
            calls.fetch_add(1, Ordering::SeqCst);
            sample_answer(kind, req)
        })) as Arc<dyn AiProvider>
    })
}

fn first_message(s: &Session) -> MessageId {
    s.rows().first().map(|r| r.id).unwrap()
}

fn finish(a: &mut Assist, key: AiKey) -> AiState {
    wait(|| {
        a.poll();
        a.job(key).is_some_and(|j| j.state != AiState::Running)
    });
    a.job(key).map(|j| j.state.clone()).unwrap()
}

#[test]
fn external_provider_waits_for_consent_and_can_remember_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(dir.path());
    let calls = Arc::new(AtomicUsize::new(0));
    let mut a = Assist::new(&s, None).with_factory(external(Arc::clone(&calls)));
    let id = first_message(&s);

    a.request(&mut s, AiTask::Summarize(id)).unwrap();
    let c = a.consent().expect("se pide consentimiento antes de enviar");
    assert!(c.scope.external && c.scope.redacted);
    assert!(c.scope.fields.contains_key(&DataField::Subject));
    assert!(c.scope.fields.contains_key(&DataField::Body));
    assert_eq!(c.provider, "Anthropic · prueba");
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(calls.load(Ordering::SeqCst), 0, "nada sale sin confirmar");

    // Rechazar: nada se envía.
    a.decline();
    assert!(a.consent().is_none() && a.job(AiKey::Summary(id)).is_none());

    // Confirmar y recordar.
    a.request(&mut s, AiTask::Summarize(id)).unwrap();
    a.confirm(true);
    assert!(matches!(finish(&mut a, AiKey::Summary(id)), AiState::Done(AiValue::Summary(_))));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(a.prefs().no_ask.contains("Anthropic · prueba"));

    // Ya no pregunta para este proveedor.
    a.request(&mut s, AiTask::Replies(id)).unwrap();
    assert!(a.consent().is_none());
    match finish(&mut a, AiKey::Replies(id)) {
        AiState::Done(AiValue::Replies(r)) => assert_eq!(r.suggestions.len(), 3),
        other => panic!("{other:?}"),
    }
}

#[test]
fn local_provider_runs_without_asking_and_streams() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(dir.path());
    // Buzón de ejemplo: proveedor local de respuestas fijas.
    let mut a = Assist::new(&s, None);
    let id = first_message(&s);
    a.request(&mut s, AiTask::Analyze(id)).unwrap();
    assert!(a.consent().is_none());
    match finish(&mut a, AiKey::Analysis(id)) {
        AiState::Done(AiValue::Analysis(an)) => {
            assert_eq!(an.extraction.events.len(), 1);
            assert_eq!(an.extraction.tasks.len(), 1);
            assert!(an.classification.and_then(|c| c.lead).is_some());
        }
        other => panic!("{other:?}"),
    }
    assert!(!a.streamed(AiKey::Analysis(id)).is_empty(), "el texto llegó por partes");
}

#[test]
fn triage_ranks_unread_mail() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(dir.path());
    let mut a = Assist::new(&s, None);
    let unread = s.ai_unread();
    assert!(!unread.is_empty(), "el buzón de ejemplo tiene no leídos");
    a.request(&mut s, AiTask::Triage).unwrap();
    match finish(&mut a, AiKey::Triage) {
        AiState::Done(AiValue::Triage(rows)) => {
            assert_eq!(rows.len(), unread.len());
            assert!(rows.iter().any(|r| r.importance == Importance::Low), "los boletines van al final");
            assert_ne!(rows.first().map(|r| r.importance), Some(Importance::Low));
            assert!(rows.windows(2).all(|w| w[0].importance.rank() <= w[1].importance.rank()));
            assert!(rows.iter().all(|r| unread.iter().any(|(id, _)| *id == r.id)));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn cancel_stops_a_running_job() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(dir.path());
    let slow: ProviderFactory = Arc::new(|kind| {
        Arc::new(CannedProvider::new("Lento", false, move |req| sample_answer(kind, req)).delay(Duration::from_secs(2)))
            as Arc<dyn AiProvider>
    });
    let mut a = Assist::new(&s, None).with_factory(slow);
    a.request(&mut s, AiTask::Chat("hola".into())).unwrap();
    assert_eq!(a.job(AiKey::Chat).map(|j| j.state.clone()), Some(AiState::Running));
    a.cancel(AiKey::Chat);
    std::thread::sleep(Duration::from_millis(100));
    a.poll();
    assert_eq!(a.job(AiKey::Chat).map(|j| j.state.clone()), Some(AiState::Cancelled));
    assert!(a.is_idle());
}

#[test]
fn chat_and_errors_are_human() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(dir.path());
    let mut a = Assist::new(&s, None);
    assert!(a.request(&mut s, AiTask::Chat("   ".into())).is_err());
    assert!(a.request(&mut s, AiTask::Minutes { transcript: String::new(), title: None }).is_err());
    a.request(&mut s, AiTask::Chat("¿Qué tengo hoy?".into())).unwrap();
    finish(&mut a, AiKey::Chat);
    assert_eq!(a.chat().len(), 2);
    assert!(a.chat()[0].mine && !a.chat()[1].mine);
}

#[test]
fn commands_route_to_the_ui_and_proposals_create_local_items() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(dir.path());
    assert_eq!(s.run("ai.triage").unwrap(), Outcome::Assist("ai.triage".into()));
    assert_eq!(s.module, iuredesk_engine::Module::Assistant);
    assert_eq!(s.run("app.import").unwrap(), Outcome::Assist("app.import".into()));
    // Sin mensaje enfocado, «Resumir» lo explica.
    assert!(matches!(s.run("ai.summarize").unwrap(), Outcome::Notice(_)));

    let before: usize = s.task_lists().iter().map(|l| s.tasks(l.id).len()).sum();
    let due = chrono::NaiveDate::from_ymd_opt(2026, 10, 30);
    assert!(matches!(s.ai_create_task("Enviar la propuesta", due, "").unwrap(), Outcome::Notice(_)));
    let after: usize = s.task_lists().iter().map(|l| s.tasks(l.id).len()).sum();
    assert_eq!(after, before + 1);
    assert!(s.ai_create_task("  ", None, "").is_err());

    let notes = s.notes().len();
    s.ai_save_note("Minuta", "# Minuta\n\n- acuerdo").unwrap();
    assert_eq!(s.notes().len(), notes + 1);
}

#[test]
fn missing_sister_app_is_an_invitation_not_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let s = session(dir.path());
    let mut h = SuiteHub::new(&s).with_suite(iuredesk_engine::iuredesk_integrations::Suite::empty());
    h.run(SuiteAction::Speak("hola".into()));
    h.run(SuiteAction::OpenAttachment { msg: first_message(&s), name: "a.pdf".into() });
    h.poll();
    let ev = h.take_events();
    assert_eq!(ev.len(), 2);
    assert!(ev.iter().all(|e| matches!(e, SuiteEvent::NotInstalled(_))));
}

#[test]
fn import_wizard_imports_a_small_mbox_and_reports() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(dir.path());
    let src = tempfile::tempdir().unwrap();
    let mbox = src.path().join("Archivo.mbox");
    std::fs::write(
        &mbox,
        "From a@example.com Mon Oct 20 10:00:00 2026\nFrom: Ana <a@example.com>\nTo: yo@example.com\nSubject: Uno\nMessage-ID: <uno@example.com>\nDate: Mon, 20 Oct 2026 10:00:00 +0000\n\nHola uno.\n\nFrom b@example.com Mon Oct 20 11:00:00 2026\nFrom: Beto <b@example.com>\nTo: yo@example.com\nSubject: Dos\nMessage-ID: <dos@example.com>\nDate: Mon, 20 Oct 2026 11:00:00 +0000\n\nHola dos.\n",
    )
    .unwrap();
    let accounts = s.account_labels().len();
    let mut m = Migrate::new(&s);
    m.start(vec![mbox.clone()], Some("Migrado".into()), true).unwrap();
    assert!(m.start(vec![mbox], None, true).is_err(), "una a la vez");
    wait(|| {
        if m.poll() {
            s.reload_external();
        }
        m.is_idle()
    });
    match m.state() {
        MigrateState::Finished(rep) => {
            assert_eq!(rep.counts.messages, 2, "{rep:?}");
            assert!(!rep.cancelled);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(s.account_labels().len(), accounts + 1, "cuenta local nueva");
}
