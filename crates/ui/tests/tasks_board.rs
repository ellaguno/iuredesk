//! S1.34 con egui_kittest: las tareas como tablero (arrastrar entre columnas, con deshacer; Alt+flecha
//! con el teclado; canceladas plegadas) y agrupadas por caso.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{TimeZone, Utc};
use egui::{Key, Modifiers, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use iuredesk_accounts::MemoryStore;
use iuredesk_engine::{Module, OfflineTransport, Session, SessionConfig, ThemePref, Zone};
use iuredesk_model::TaskStatus;
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

/// Un tablero de una lista local con tareas pendientes: abre Tareas, elige la lista y la vista.
fn board_on_a_local_list(dir: &std::path::Path) -> (Harness<'static, DeskApp>, iuredesk_model::Task) {
    let mut h = harness(app(dir));
    settle(&mut h);
    h.state_mut().session.module = Module::Tasks;
    h.run_steps(3);
    let (list, task) = {
        let s = &h.state().session;
        s.task_lists()
            .into_iter()
            .filter(|l| !s.is_iure_task(l.id) && s.task_caps(l.id).in_progress)
            .find_map(|l| s.tasks(l.id).into_iter().find(|t| !t.is_done()).map(|t| (l, t)))
            .unwrap()
    };
    h.get_all_by_label(&list.name).next().unwrap().click();
    h.run_steps(3);
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, "tasks.view.board");
    h.run_steps(4);
    (h, task)
}

fn card_label(t: &iuredesk_model::Task, column: &str) -> String {
    format!("Tarea {} · {column}", t.summary)
}

/// Arrastrar una tarjeta a «En curso» cambia su estado; deshacer la devuelve.
#[test]
fn dragging_a_card_changes_its_status() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let (mut h, t) = board_on_a_local_list(dir.path());
    let from = h.get_by_label(&card_label(&t, "Por hacer")).rect().center();
    let col = h.get_all_by_label("En curso").next().unwrap().rect();
    let to = col.center() + vec2(0.0, 80.0);
    h.event(egui::Event::PointerMoved(from));
    h.event(egui::Event::PointerButton {
        pos: from,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    h.run_steps(2);
    for k in 1..=8 {
        h.event(egui::Event::PointerMoved(from + (to - from) * (k as f32 / 8.0)));
        h.run();
    }
    h.event(egui::Event::PointerButton {
        pos: to,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    h.run_steps(4);
    assert_eq!(h.state().session.task(t.id).unwrap().status, TaskStatus::InProcess);
    assert!(h.query_by_label(&card_label(&t, "En curso")).is_some(), "la tarjeta ya está en su columna");
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, "undo");
    h.run_steps(3);
    assert_eq!(h.state().session.task(t.id).unwrap().status, TaskStatus::NeedsAction);
}

/// Con el teclado: elegir la tarjeta y Alt+→ la pasa a la siguiente columna.
#[test]
fn alt_arrow_moves_the_selected_card() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let (mut h, t) = board_on_a_local_list(dir.path());
    h.get_by_label(&card_label(&t, "Por hacer")).click();
    h.run_steps(3);
    h.key_press_modifiers(Modifiers::ALT, Key::ArrowRight);
    h.run_steps(3);
    assert_eq!(h.state().session.task(t.id).unwrap().status, TaskStatus::InProcess);
    h.key_press_modifiers(Modifiers::ALT, Key::ArrowRight);
    h.run_steps(3);
    assert!(h.state().session.task(t.id).unwrap().is_done());
}

/// Las canceladas van plegadas debajo del tablero, como las terminadas en la lista.
#[test]
fn cancelled_tasks_are_folded() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let (mut h, t) = board_on_a_local_list(dir.path());
    h.state_mut().session.set_task_status(t.id, TaskStatus::Cancelled).unwrap();
    h.run_steps(3);
    assert!(h.query_by_label_contains(&t.summary).is_none(), "plegada");
    h.get_by_label("Canceladas (1)").click();
    h.run_steps(3);
    assert!(h.query_by_label(&card_label(&t, "Cancelada")).is_some());
}

/// «Por caso»: las tareas de Iurefficient agrupadas por su caso; las demás, en «Sin caso».
#[test]
fn tasks_by_case() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app(dir.path()));
    settle(&mut h);
    h.state_mut().session.module = Module::Tasks;
    h.run_steps(3);
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, "tasks.view.case");
    h.run_steps(4);
    for case in ["Amparo Ruiz Mendoza", "Arrendamiento Altamira", "Sin caso"] {
        assert!(h.query_by_label(case).is_some(), "falta el grupo {case}");
    }
    // Las dos tareas del caso «Amparo Ruiz Mendoza» están debajo de su encabezado y antes del siguiente grupo.
    let y = |l: &str| h.get_by_label(l).rect().top();
    let (a, b) = (y("Amparo Ruiz Mendoza"), y("Arrendamiento Altamira"));
    for name in ["Revisar escrito de contestación", "Llamar a la contraparte"] {
        let t = y(&format!("Tarea {name}"));
        assert!(t > a && t < b, "{name} no está en Amparo Ruiz Mendoza");
    }
    // Una tarea local que nombra el caso va con él.
    let local = y("Tarea Presentar escrito Arrendamiento Altamira");
    assert!(local > b && local < y("Sin caso"), "la tarea local del caso no está en su grupo");
    // La vista se recuerda.
    assert_eq!(h.state().session.task_view(), iuredesk_engine::tasks_view::TaskView::ByCase);
}

/// S1.35: una tarea de Iurefficient se abre en su página (clic derecho → «Abrir en Iurefficient»,
/// o el comando de la paleta con la tarea elegida).
#[test]
fn an_iurefficient_task_opens_in_its_page() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    iuredesk_ui::capture_opened_urls();
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app(dir.path()));
    settle(&mut h);
    h.state_mut().session.module = Module::Tasks;
    h.run_steps(3);
    // En «Por caso» están las tareas de todas las listas.
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, "tasks.view.case");
    h.run_steps(3);
    let row = h.get_by_label("Tarea Revisar escrito de contestación");
    row.click_secondary();
    h.run_steps(3);
    h.get_by_label("Abrir en Iurefficient").click();
    h.run_steps(3);
    assert_eq!(
        iuredesk_ui::opened_urls().last().map(String::as_str),
        Some("https://demo.iurefficient.com/dashboard/tasks?taskId=it-1&view=list")
    );
    // Sin un plan de IureProject que se siga para su caso, el comando lo explica.
    h.key_press(Key::Escape);
    h.run_steps(2);
    h.get_by_label("Tarea Revisar escrito de contestación").click();
    h.run_steps(2);
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, "tasks.open_project");
    h.run_steps(3);
    assert!(h.query_all_by_label_contains("no tiene un plan de IureProject").next().is_some());
}
