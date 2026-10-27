//! «Mi día» (F6.1) con egui_kittest: la pantalla se pinta con el buzón de ejemplo, el riel la
//! ofrece primero, y pulsar un elemento lleva a él (tarea → Tareas, correo → Correo enfocado,
//! cita → Calendario).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{TimeZone, Utc};
use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use iuredesk_accounts::MemoryStore;
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
    s.open_at_start();
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
    h.run_steps(2);
}

#[test]
fn today_renders_and_items_navigate() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app(dir.path()));
    settle(&mut h);
    assert_eq!(h.state().session.module, Module::Today, "usuario nuevo: abre en «Mi día»");
    assert!(h.query_all_by_label_contains("Buenos días, Laura").next().is_some(), "saludo");
    assert!(h.query_all_by_label_contains("Día despejado").next().is_none(), "el ejemplo tiene citas hoy");

    // Tarea → módulo de Tareas con su lista.
    h.get_by_label("Presentar escrito caso 241/2026").click();
    settle(&mut h);
    assert_eq!(h.state().session.module, Module::Tasks);

    // Volver con el riel y abrir un correo que espera respuesta.
    h.state_mut().session.run("nav.today").unwrap();
    settle(&mut h);
    assert_eq!(h.state().session.module, Module::Today);
    let day = h.state_mut().session.day_summary().unwrap();
    let m = day.needs_reply.first().unwrap().clone();
    h.get_by_label(&format!("{} — {}", m.from, m.subject)).click();
    settle(&mut h);
    assert_eq!(h.state().session.module, Module::Mail);
    assert_eq!(h.state().session.focused(), Some(m.id), "el mensaje queda abierto");
}

#[test]
fn completing_a_task_from_today() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app(dir.path()));
    settle(&mut h);
    h.get_by_label("Terminar Presentar escrito caso 241/2026").click();
    settle(&mut h);
    assert!(h.query_all_by_label_contains("Tarea terminada").next().is_some(), "aviso con deshacer");
    let day = h.state_mut().session.day_summary().unwrap();
    assert!(!day.tasks.today.iter().any(|t| t.task.summary == "Presentar escrito caso 241/2026"));
    assert_eq!(h.state().session.module, Module::Today, "terminar no navega");
}
