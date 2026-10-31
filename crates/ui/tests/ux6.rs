//! UX-6 con egui_kittest: editar una tarea (doble clic, Intro, F2 en la lista, desde «Mi día») y
//! la nota con su archivo Markdown y «Editar en IureEditor».

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{TimeZone, Utc};
use egui::{Key, vec2};
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

fn task_named(h: &Harness<'_, DeskApp>, name: &str) -> iuredesk_model::Task {
    let s = &h.state().session;
    s.task_lists().iter().flat_map(|l| s.tasks(l.id)).find(|t| t.summary == name).unwrap()
}

/// Elegir la tarea e Intro abre el editor; cambiar título y notas y «Guardar» deja la tarea cambiada (con
/// deshacer); Esc lo cierra; F2 cambia el título en la lista.
#[test]
fn edit_task_from_the_list() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app(dir.path()));
    settle(&mut h);
    h.state_mut().session.module = Module::Tasks;
    settle(&mut h);
    let t = task_named(&h, "Enviar propuesta a Altamira");
    // Clic en la fila (la casilla tiene su propio nombre) la elige; Intro abre el editor (el
    // doble clic es `Response::double_clicked` de egui, que el arnés no simula con su reloj).
    h.get_by_label("Tarea Enviar propuesta a Altamira").click();
    h.run();
    h.key_press(Key::Enter);
    settle(&mut h);
    assert!(h.state().task_editor_open(), "Intro abre el editor");
    let title = h.get_by_role_and_label(egui::accesskit::Role::TextInput, "Título de la tarea");
    title.focus();
    h.run();
    h.key_press_modifiers(egui::Modifiers::COMMAND, Key::A);
    h.event(egui::Event::Text("Enviar propuesta final".into()));
    h.run();
    h.query_all_by_label("Notas de la tarea").next().unwrap().focus();
    h.run();
    h.event(egui::Event::Text("Con descuento".into()));
    h.run();
    h.get_by_label("Guardar").click();
    settle(&mut h);
    assert!(!h.state().task_editor_open());
    let saved = h.state().session.task(t.id).unwrap();
    assert_eq!(saved.summary, "Enviar propuesta final");
    assert!(saved.description.contains("Con descuento"));
    assert!(h.query_all_by_label_contains("guardada").next().is_some(), "aviso con deshacer");
    h.state_mut().session.run("undo").unwrap();
    settle(&mut h);
    assert_eq!(h.state().session.task(t.id).unwrap().summary, "Enviar propuesta a Altamira");

    // Esc cierra el editor sin cambios.
    h.key_press(Key::Enter);
    settle(&mut h);
    assert!(h.state().task_editor_open());
    h.key_press(Key::Escape);
    settle(&mut h);
    assert!(!h.state().task_editor_open());
    // F2: cambiar el nombre en la lista.
    h.key_press(Key::F2);
    settle(&mut h);
    h.key_press_modifiers(egui::Modifiers::COMMAND, Key::A);
    h.event(egui::Event::Text("Propuesta Altamira".into()));
    h.key_press(Key::Enter);
    settle(&mut h);
    assert_eq!(h.state().session.task(t.id).unwrap().summary, "Propuesta Altamira");
}

/// En «Mi día», pulsar una tarea abre su editor encima.
#[test]
fn edit_task_from_today() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app(dir.path()));
    h.state_mut().session.module = Module::Today;
    settle(&mut h);
    h.get_by_label("Presentar escrito caso 241/2026").click();
    settle(&mut h);
    assert!(h.state().task_editor_open());
    assert_eq!(h.state().session.module, Module::Today);
    assert!(h.query_all_by_label("Prioridad").next().is_some());
}

/// La nota se guarda como `.md` en la carpeta de notas; la cabecera dice dónde y ofrece
/// «Editar en IureEditor».
#[test]
fn note_has_its_markdown_file() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let notes = tempfile::tempdir().unwrap();
    let mut a = app(dir.path());
    a.session.notes_fs_for_tests(notes.path().to_path_buf(), Duration::from_millis(30));
    assert!(a.session.notes_fs_flush(Duration::from_secs(10)));
    assert!(a.session.notes_fs_flush(Duration::from_secs(10)));
    a.session.module = Module::Notes;
    let mut h = harness(a);
    settle(&mut h);
    assert!(h.query_by_label("Editar en IureEditor").is_some());
    assert!(notes.path().join("Ideas para el despacho.md").is_file(), "las notas del ejemplo se exportaron");
    // Lo guardado fuera vuelve a la nota.
    let id = h.state().session.note_focus().unwrap();
    let p = h.state_mut().session.note_path(id).unwrap();
    let md = std::fs::read_to_string(&p).unwrap();
    std::fs::write(&p, format!("{md}\n- Agregado en IureEditor\n")).unwrap();
    let start = Instant::now();
    while !h.state().session.notes().iter().any(|n| n.id == id && n.body.contains("Agregado en IureEditor")) {
        assert!(start.elapsed() < Duration::from_secs(15));
        h.state_mut().session.notes_fs_flush(Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(40));
    }
    h.state_mut().set_note_preview(true);
    settle(&mut h);
    assert!(h.query_all_by_label_contains("Agregado en IureEditor").next().is_some(), "el editor muestra lo nuevo");
}
