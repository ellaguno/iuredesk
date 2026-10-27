//! F4.2 en la interfaz (egui_kittest): el diálogo «Se enviará…» aparece antes de llamar a un
//! proveedor externo, el triage de hoy muestra la lista ordenada, el asistente de importación
//! trae un mbox pequeño y una app hermana que falta invita a instalarla. El arnés verifica que
//! todo control tenga nombre accesible.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{TimeZone, Utc};
use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use iuredesk_accounts::MemoryStore;
use iuredesk_engine::assist::{AiKey, AiState, AiValue, CannedProvider, ProviderFactory, sample_answer};
use iuredesk_engine::iuredesk_assistant::AiProvider;
use iuredesk_engine::migrate::MigrateState;
use iuredesk_engine::{Module, OfflineTransport, Session, SessionConfig, ThemePref, Zone};
use iuredesk_store::{Store, sample};
use iuredesk_ui::{DeskApp, Options};

static GPU: Mutex<()> = Mutex::new(());

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 21, 11, 20, 0).unwrap()
}

fn app(dir: &std::path::Path) -> DeskApp {
    iuredesk_ui::iurefficient_ui::lang::set_lang(iuredesk_ui::iurefficient_ui::lang::Lang::Es);
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
    s.prefs.theme = ThemePref::Light;
    s.prefs.mark_read_delay_ms = 0;
    let mut app = DeskApp::new(s, Options { system_fonts: false, no_theme_watch: true, ..Default::default() });
    app.dismiss_reminders();
    app
}

fn harness(app: DeskApp) -> Harness<'static, DeskApp> {
    Harness::builder().with_size(vec2(1440.0, 900.0)).build_ui_state(
        |ui, app: &mut DeskApp| {
            let full = ui.ctx().content_rect();
            ui.scope_builder(egui::UiBuilder::new().max_rect(full), |ui| {
                ui.set_clip_rect(full);
                app.ui(ui);
            });
        },
        app,
    )
}

fn settle(h: &mut Harness<'_, DeskApp>) {
    let start = Instant::now();
    let mut quiet = 0;
    while start.elapsed() < Duration::from_secs(60) {
        h.run_steps(1);
        if h.state().is_settled() {
            quiet += 1;
            if quiet > 4 {
                break;
            }
        } else {
            quiet = 0;
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    h.run();
}

fn focus_first(h: &mut Harness<'_, DeskApp>) {
    let id = h.state().session.rows()[0].id;
    h.state_mut().session.request_focus(id);
    settle(h);
    assert_eq!(h.state().session.focused(), Some(id));
}

#[test]
fn datascope_is_shown_before_any_external_call() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let c2 = Arc::clone(&calls);
    let factory: ProviderFactory = Arc::new(move |kind| {
        let c = Arc::clone(&c2);
        Arc::new(CannedProvider::new("Anthropic · prueba", true, move |req| {
            c.fetch_add(1, Ordering::SeqCst);
            sample_answer(kind, req)
        })) as Arc<dyn AiProvider>
    });
    let mut a = app(dir.path());
    a.set_ai_factory(factory);
    let mut h = harness(a);
    settle(&mut h);
    focus_first(&mut h);
    let id = h.state().session.focused().unwrap();

    h.get_by_label("Analizar").click();
    settle(&mut h);
    assert!(h.state().ai_consent_open(), "primero se muestra qué se enviará");
    assert!(h.query_all_by_label_contains("Se enviará:").next().is_some());
    assert!(h.query_by_label("No volver a preguntar para este proveedor").is_some());
    assert_eq!(calls.load(Ordering::SeqCst), 0, "nada salió todavía");

    h.get_by_label("Enviar").click();
    settle(&mut h);
    assert!(!h.state().ai_consent_open());
    assert!(calls.load(Ordering::SeqCst) >= 1);
    assert!(matches!(h.state().ai_state(AiKey::Analysis(id)), Some(AiState::Done(AiValue::Analysis(_)))));
    // La propuesta se ofrece, no se aplica sola.
    assert!(h.query_all_by_label("Crear evento").next().is_some());
    assert!(h.query_all_by_label("Crear tarea").count() >= 2, "la del análisis además de la del panel");
}

#[test]
fn triage_shows_a_ranked_list() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app(dir.path()));
    settle(&mut h);
    h.state_mut().session.module = Module::Assistant;
    settle(&mut h);
    h.get_by_label("¿Qué necesito atender hoy?").click();
    settle(&mut h);
    let rows = match h.state().ai_state(AiKey::Triage) {
        Some(AiState::Done(AiValue::Triage(r))) => r,
        other => panic!("{other:?}"),
    };
    assert!(!rows.is_empty());
    assert!(rows.windows(2).all(|w| w[0].importance.rank() <= w[1].importance.rank()));
    assert!(h.query_all_by_label("Baja").next().is_some(), "importancia visible");
    let first = rows[0].clone();
    assert!(h.query_all_by_label_contains(&first.subject).next().is_some());
    // «Abrir» lleva al correo.
    h.query_all_by_label("Abrir").next().unwrap().click();
    settle(&mut h);
    assert_eq!(h.state().session.module, Module::Mail);
    let focused = h.state().session.focused().expect("el correo queda abierto");
    let thread = |h: &mut Harness<'_, DeskApp>, id| h.state_mut().session.envelope(id).and_then(|e| e.thread);
    assert!(focused == first.id || thread(&mut h, focused) == thread(&mut h, first.id));
}

#[test]
fn import_wizard_brings_a_small_mbox() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    let mbox = src.path().join("Viejo.mbox");
    std::fs::write(
        &mbox,
        "From a@example.com Mon Oct 20 10:00:00 2026\nFrom: Ana <a@example.com>\nSubject: Uno\nMessage-ID: <uno@example.com>\nDate: Mon, 20 Oct 2026 10:00:00 +0000\n\nHola.\n\nFrom b@example.com Mon Oct 20 11:00:00 2026\nFrom: Beto <b@example.com>\nSubject: Dos\nMessage-ID: <dos@example.com>\nDate: Mon, 20 Oct 2026 11:00:00 +0000\n\nAdiós.\n",
    )
    .unwrap();
    let mut a = app(dir.path());
    a.demo_import(Vec::new(), vec![mbox]);
    let mut h = harness(a);
    settle(&mut h);
    assert!(h.query_all_by_label_contains("Viejo.mbox").next().is_some(), "origen elegido");
    h.get_by_label("Importar (1)").click();
    settle(&mut h);
    match h.state().import_state() {
        MigrateState::Finished(r) => assert_eq!(r.counts.messages, 2),
        other => panic!("{other:?}"),
    }
    assert!(h.query_by_label("Importación terminada").is_some());
    h.get_by_label("Listo").click();
    settle(&mut h);
    assert!(h.query_by_label("Importación terminada").is_none());
}

#[test]
fn missing_sister_app_shows_install_hint() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut a = app(dir.path());
    a.set_suite(iuredesk_engine::iuredesk_integrations::Suite::empty());
    let mut h = harness(a);
    settle(&mut h);
    focus_first(&mut h);
    h.get_by_label("Leer en voz alta").click();
    settle(&mut h);
    assert!(h.query_all_by_label_contains("IureVoz no está instalada").next().is_some(), "invitación, no error");
    assert!(h.query_by_label("Descargar IureVoz").is_some());
    h.get_by_label("Ahora no").click();
    settle(&mut h);
    assert!(h.query_all_by_label_contains("IureVoz no está instalada").next().is_none());
}
