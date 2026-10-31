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
    app_at(dir, now())
}

fn app_at(dir: &std::path::Path, at: chrono::DateTime<Utc>) -> DeskApp {
    iuredesk_ui::iurefficient_ui::lang::set_lang(iuredesk_ui::iurefficient_ui::lang::Lang::Es);
    iuredesk_engine::lang::set_lang(iuredesk_engine::lang::Lang::Es);
    let store = Store::open(dir).unwrap();
    sample::populate(&store, sample::DEFAULT_SEED, now()).unwrap();
    let mut s = Session::new(SessionConfig {
        store: Arc::new(store),
        accounts: None,
        secrets: Arc::new(MemoryStore::default()),
        prefs_path: None,
        now: Some(at),
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

    // UX-6: tarea → su editor, encima de «Mi día»; «Todas» lleva al módulo de Tareas.
    h.get_by_label("Presentar escrito caso 241/2026").click();
    settle(&mut h);
    assert!(h.state().task_editor_open());
    assert_eq!(h.state().session.module, Module::Today);
    h.key_press(egui::Key::Escape);
    settle(&mut h);
    assert!(!h.state().task_editor_open());

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

/// UX-2: el botón «Unirse · Teams» de la próxima cita abre solo la reunión; antes la tarjeta
/// (que escuchaba el clic encima del botón) abría además el editor del evento y ganaba.
#[test]
fn join_button_opens_only_the_meeting() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    // Busca la reunión de Teams del ejemplo y vuelve a abrir «Mi día» poco antes de que empiece.
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app(dir.path()));
    settle(&mut h);
    let day = h.state_mut().session.day_summary().unwrap();
    let teams = day.events.iter().find(|e| e.join.as_ref().is_some_and(|j| j.provider == "Teams")).unwrap().clone();
    let url = teams.join.as_ref().unwrap().url.clone();
    drop(h);
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app_at(dir.path(), teams.occ.start - chrono::Duration::minutes(5)));
    settle(&mut h);
    let day = h.state_mut().session.day_summary().unwrap();
    let (i, _) = day.next_event(h.state().session.now()).unwrap();
    assert_eq!(day.events.get(i).map(|e| e.occ.id), Some(teams.occ.id), "la de Teams es la siguiente");

    iuredesk_ui::capture_opened_urls();
    h.get_by_label("Unirse a la reunión").click();
    settle(&mut h);
    assert_eq!(iuredesk_ui::opened_urls(), vec![url], "se abre el enlace de la reunión");
    assert_eq!(h.state().session.module, Module::Today, "y no se abre el evento");

    // Un clic en el resto de la tarjeta (el título, en el centro) sí abre el evento.
    h.query_all_by_role_and_label(egui::accesskit::Role::Button, &teams.occ.summary).next().unwrap().click();
    settle(&mut h);
    assert_eq!(h.state().session.module, Module::Calendar);
    assert_eq!(iuredesk_ui::opened_urls().len(), 1, "sin abrir la reunión otra vez");
}
