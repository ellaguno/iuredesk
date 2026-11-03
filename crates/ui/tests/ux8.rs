//! UX-8 con egui_kittest: vista previa nativa de las notas (Vista/Editar, Ctrl+Shift+E, casillas
//! con deshacer y regeneración cuando el archivo cambia desde IureEditor).

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
    // UX-12: Notas solo con IureEditor; lo que se abre queda anotado, no se lanza nada.
    app.set_suite(
        iuredesk_engine::iuredesk_integrations::Suite::empty()
            .with_app(iuredesk_engine::iuredesk_integrations::SisterApp::Editor, "/opt/iureditor/iureditor")
            .with_opener(Arc::new(Recorder::default())),
    );
    app
}

/// Anota lo que se abriría.
#[derive(Default)]
struct Recorder(std::sync::Mutex<Vec<String>>);

impl iuredesk_engine::iuredesk_integrations::Opener for Recorder {
    fn open_url(&self, url: &str) -> std::io::Result<()> {
        self.0.lock().unwrap().push(url.to_string());
        Ok(())
    }
    fn launch(&self, program: &std::path::Path, args: &[String]) -> std::io::Result<()> {
        self.0.lock().unwrap().push(format!("{} {}", program.display(), args.join(" ")));
        Ok(())
    }
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

/// UX-12: la nota solo se ve (sin editor ligero); se escribe en IureEditor.
#[test]
fn notes_are_view_only() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app(dir.path()));
    settle(&mut h);
    assert!(h.query_by_label("Texto de la nota").is_none(), "sin editor ligero");
    assert!(h.query_by_label("Vista").is_none() && h.query_by_label("Editar").is_none(), "sin conmutador");
    assert!(h.query_by_label("Editar en IureEditor").is_some());
    // El render (imagen) lleva el texto como nombre accesible.
    assert!(h.query_all_by_label_contains("Llamar al notario").next().is_some());
    assert!(iuredesk_engine::command::find("notes.toggle_preview").is_none(), "el comando ya no existe");
}

/// UX-12: «Nueva nota» la crea con título y la abre en IureEditor (donde se escribe).
#[test]
fn new_note_opens_in_iureditor() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let notes = tempfile::tempdir().unwrap();
    let mut a = app(dir.path());
    let rec = Arc::new(Recorder::default());
    a.set_suite(
        iuredesk_engine::iuredesk_integrations::Suite::empty()
            .with_app(iuredesk_engine::iuredesk_integrations::SisterApp::Editor, "/opt/iureditor/iureditor")
            .with_opener(rec.clone()),
    );
    a.session.notes_fs_for_tests(notes.path().to_path_buf(), Duration::from_millis(30));
    assert!(a.session.notes_fs_flush(Duration::from_secs(10)));
    let before = a.session.notes().len();
    let mut h = harness(a);
    settle(&mut h);
    h.get_by_label("Nueva nota").click();
    settle(&mut h);
    let start = Instant::now();
    while rec.0.lock().unwrap().is_empty() {
        assert!(start.elapsed() < Duration::from_secs(15), "no se abrió IureEditor");
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(20));
    }
    let s = &h.state().session;
    assert_eq!(s.notes().len(), before + 1);
    let id = s.note_focus().unwrap();
    let n = s.notes().into_iter().find(|n| n.id == id).unwrap();
    assert!(n.title.starts_with("Nota "), "{}", n.title);
    let opened = rec.0.lock().unwrap().join("\n");
    assert!(opened.contains(".md"), "abre su archivo: {opened}");
}

/// UX-12: sin IureEditor no hay Notas: ni en el riel ni por comando, y se sale de ellas.
#[test]
fn without_iureditor_notes_disappear() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut a = app(dir.path());
    a.set_suite(iuredesk_engine::iuredesk_integrations::Suite::empty());
    let mut h = harness(a);
    settle(&mut h);
    assert_eq!(h.state().session.module, Module::Today, "salió de Notas");
    assert!(h.query_by_label("Notas").is_none(), "sin botón en el riel");
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, "nav.notes");
    settle(&mut h);
    assert_eq!(h.state().session.module, Module::Today);
    assert!(h.query_all_by_label_contains("instálalo").next().is_some(), "dice por qué");
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
    assert!(h.query_all_by_label_contains("Desde IureEditor").next().is_some(), "la vista muestra lo nuevo");
}
