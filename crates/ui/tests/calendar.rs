//! Calendario en la interfaz (F2.4, egui_kittest): crear rápido con un clic en la rejilla,
//! guardar desde el editor y aceptar una invitación desde el panel de lectura.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{TimeZone, Utc};
use egui::accesskit::Role;
use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use iuredesk_accounts::MemoryStore;
use iuredesk_engine::{Module, OfflineTransport, Session, SessionConfig, ThemePref, Zone};
use iuredesk_model::PartStat;
use iuredesk_store::{Store, sample};
use iuredesk_ui::{DeskApp, Options};

/// Las pruebas que renderizan se turnan.
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
    let mut a = DeskApp::new(s, Options { system_fonts: false, no_theme_watch: true, ..Default::default() });
    a.dismiss_reminders();
    a
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
    h.run_steps(2);
}

fn titles(app: &mut DeskApp) -> Vec<String> {
    let z = app.session.zone();
    let d = app.session.today();
    app.session
        .visible_occurrences(z.day_start(d - chrono::Duration::days(3)), z.day_start(d + chrono::Duration::days(7)))
        .into_iter()
        .map(|o| o.summary)
        .collect()
}

#[test]
fn quick_create_from_a_click_on_the_grid() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut a = app(dir.path());
    a.session.module = Module::Calendar;
    a.set_calendar_view("day");
    let mut h = harness(a);
    settle(&mut h);
    h.get_by_label("Rejilla del calendario").click();
    settle(&mut h);
    assert!(h.state().quick_create_open(), "un clic abre el aviso de crear rápido");
    h.event(egui::Event::Text("Llamada con Ana".into()));
    h.run_steps(2);
    h.get_by_label("Guardar").click();
    settle(&mut h);
    assert!(!h.state().quick_create_open());
    assert!(titles(h.state_mut()).contains(&"Llamada con Ana".to_string()));
    assert!(h.query_all_by_label_contains("Evento creado").next().is_some(), "aviso con deshacer");
}

#[test]
fn editor_saves_changes() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut a = app(dir.path());
    a.session.module = Module::Calendar;
    assert!(a.demo_event_editor());
    let mut h = harness(a);
    settle(&mut h);
    assert!(h.state().event_editor_open());
    h.get_by_role_and_label(Role::TextInput, "Título").focus();
    h.run();
    h.key_press(egui::Key::End);
    h.event(egui::Event::Text(" (revisado)".into()));
    h.run_steps(2);
    h.get_by_label("Guardar").click();
    settle(&mut h);
    assert!(!h.state().event_editor_open(), "se cierra al guardar");
    assert!(titles(h.state_mut()).iter().any(|t| t.ends_with("(revisado)")));
}

#[test]
fn invitation_card_accepts() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut a = app(dir.path());
    let start = Instant::now();
    while !a.session.is_idle() && start.elapsed() < Duration::from_secs(30) {
        a.session.poll();
        std::thread::sleep(Duration::from_millis(5));
    }
    a.session.poll();
    let rows: Vec<_> = a.session.rows().iter().map(|r| r.id).collect();
    let msg = rows
        .into_iter()
        .find(|id| a.session.envelope(*id).is_some_and(|e| e.subject.starts_with("Invitación: Revisión")))
        .unwrap();
    a.session.request_focus(msg);
    let mut h = harness(a);
    settle(&mut h);
    assert!(
        h.query_all_by_label_contains("Sin choques").next().is_some()
            || h.query_all_by_label_contains("Choca con").next().is_some(),
        "la tarjeta muestra los choques"
    );
    h.get_by_label("Aceptar").click();
    settle(&mut h);
    let (inv, _) = h.state_mut().session.invitation(msg).unwrap();
    assert_eq!(inv.my_status, Some(PartStat::Accepted));
    assert!(inv.existing.is_some(), "quedó en el calendario");
}
