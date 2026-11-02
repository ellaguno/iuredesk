//! UX-8 con egui_kittest: vista previa nativa de las notas (Vista/Editar, Ctrl+Shift+E, casillas
//! con deshacer y regeneración cuando el archivo cambia desde IureEditor).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{TimeZone, Utc};
use egui::{Key, Modifiers, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use iuredesk_accounts::MemoryStore;
use iuredesk_engine::{Module, OfflineTransport, Session, SessionConfig, ThemePref, Zone};
use iuredesk_store::{Store, sample};
use iuredesk_ui::{DeskApp, Options};

static GPU: Mutex<()> = Mutex::new(());

const BODY: &str =
    "Intro con **negritas**.\n\n- [ ] Llamar al notario\n- [x] Enviar propuesta\n\n| a | b |\n|---|---|\n| 1 | 2 |\n";

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
    s.new_note().unwrap();
    let id = s.note_focus().unwrap();
    s.save_note(id, "Pendientes UX-8", BODY).unwrap();
    s.module = Module::Notes;
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

fn body(h: &Harness<'_, DeskApp>) -> String {
    let s = &h.state().session;
    let id = s.note_focus().unwrap();
    s.notes().into_iter().find(|n| n.id == id).unwrap().body
}

/// Una nota con texto se abre en la vista; Ctrl+Shift+E y «Editar»/«Vista» alternan.
#[test]
fn preview_is_default_and_toggles() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app(dir.path()));
    settle(&mut h);
    assert!(h.state().note_preview_on(), "vista por defecto con texto");
    assert!(h.query_by_label("Texto de la nota").is_none(), "sin editor en la vista");
    // El render (imagen) lleva el texto como nombre accesible.
    assert!(h.query_all_by_label_contains("Llamar al notario").next().is_some());
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::E);
    settle(&mut h);
    assert!(!h.state().note_preview_on());
    assert!(h.query_by_label("Texto de la nota").is_some(), "Ctrl+Shift+E abre el editor");
    h.get_by_label("Vista").click();
    settle(&mut h);
    assert!(h.state().note_preview_on());
    h.get_by_label("Editar").click();
    settle(&mut h);
    assert!(!h.state().note_preview_on());
    // Por id, como desde la paleta o MCP.
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, "notes.toggle_preview");
    settle(&mut h);
    assert!(h.state().note_preview_on());
}

/// Marcar una casilla en la vista reescribe el Markdown y se deshace.
#[test]
fn task_checkbox_rewrites_markdown_with_undo() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app(dir.path()));
    settle(&mut h);
    h.state_mut().toggle_note_task(0);
    settle(&mut h);
    assert!(body(&h).contains("- [x] Llamar al notario"), "{}", body(&h));
    h.state_mut().toggle_note_task(1);
    settle(&mut h);
    assert!(body(&h).contains("- [ ] Enviar propuesta"));
    // Una casilla que no existe no cambia nada.
    h.state_mut().toggle_note_task(9);
    settle(&mut h);
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, "undo");
    h.state_mut().run(&ctx, "undo");
    settle(&mut h);
    assert_eq!(body(&h), BODY);
}

/// Lo guardado en IureEditor (el archivo) regenera la vista sin bloquear.
#[test]
fn preview_follows_the_file() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let notes = tempfile::tempdir().unwrap();
    let mut a = app(dir.path());
    a.session.notes_fs_for_tests(notes.path().to_path_buf(), Duration::from_millis(30));
    assert!(a.session.notes_fs_flush(Duration::from_secs(10)));
    assert!(a.session.notes_fs_flush(Duration::from_secs(10)));
    let mut h = harness(a);
    settle(&mut h);
    let id = h.state().session.note_focus().unwrap();
    let p = h.state_mut().session.note_path(id).unwrap();
    let md = std::fs::read_to_string(&p).unwrap();
    std::fs::write(&p, format!("{md}\n## Desde IureEditor\n")).unwrap();
    let start = Instant::now();
    while !body(&h).contains("Desde IureEditor") {
        assert!(start.elapsed() < Duration::from_secs(15));
        h.state_mut().session.notes_fs_flush(Duration::from_secs(5));
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(40));
    }
    settle(&mut h);
    assert!(h.state().note_preview_on());
    assert!(h.query_all_by_label_contains("Desde IureEditor").next().is_some(), "la vista muestra lo nuevo");
}
