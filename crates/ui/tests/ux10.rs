//! UX-10 con egui_kittest: editar un contacto (botón «Editar» de la ficha, Intro en la lista),
//! guardar con deshacer, crear uno nuevo con «+» y Esc sin cambios.

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

/// El primer contacto de la lista (orden del motor).
fn first_contact(h: &mut Harness<'_, DeskApp>) -> iuredesk_model::Contact {
    h.state_mut().session.contacts().first().cloned().unwrap()
}

#[test]
fn edit_contact_from_the_card() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app(dir.path()));
    settle(&mut h);
    h.state_mut().session.module = Module::Contacts;
    settle(&mut h);
    let c = first_contact(&mut h);
    h.get_by_label("Editar").click();
    settle(&mut h);
    assert!(h.state().contact_editor_open(), "«Editar» abre el editor");
    h.get_by_role_and_label(egui::accesskit::Role::TextInput, "Empresa").focus();
    h.run();
    h.key_press_modifiers(egui::Modifiers::COMMAND, Key::A);
    h.event(egui::Event::Text("Iurefficient".into()));
    h.run();
    h.get_by_label("+ Agregar teléfono").click();
    h.run();
    h.query_all_by_role_and_label(egui::accesskit::Role::TextInput, "Teléfono").last().unwrap().focus();
    h.run();
    h.event(egui::Event::Text("+52 33 0000 1111".into()));
    h.run();
    h.get_by_label("Guardar").click();
    settle(&mut h);
    assert!(!h.state().contact_editor_open());
    let saved = h.state().session.contact(c.id).unwrap();
    assert_eq!(saved.org, "Iurefficient");
    assert_eq!(saved.phones.len(), c.phones.len() + 1);
    assert_eq!(saved.full_name, c.full_name, "el nombre no cambia si no se toca");
    assert!(h.query_all_by_label_contains("Se guardó").next().is_some(), "aviso con deshacer");
    h.state_mut().session.run("undo").unwrap();
    settle(&mut h);
    assert_eq!(h.state().session.contact(c.id).unwrap().org, c.org);

    // Esc cierra sin guardar.
    h.get_by_label("Editar").click();
    settle(&mut h);
    h.get_by_role_and_label(egui::accesskit::Role::TextInput, "Apodo").focus();
    h.run();
    h.event(egui::Event::Text("Nada".into()));
    h.key_press(Key::Escape);
    settle(&mut h);
    assert!(!h.state().contact_editor_open());
    assert_eq!(h.state().session.contact(c.id).unwrap().nickname, c.nickname);
}

#[test]
fn new_contact_with_the_plus_button() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app(dir.path()));
    settle(&mut h);
    h.state_mut().session.module = Module::Contacts;
    settle(&mut h);
    let before = h.state_mut().session.contacts().len();
    h.get_by_label("Nuevo contacto").click();
    settle(&mut h);
    assert!(h.state().contact_editor_open());
    // El nombre tiene el foco al abrir.
    h.event(egui::Event::Text("Zoe".into()));
    h.run();
    h.get_by_role_and_label(egui::accesskit::Role::TextInput, "Apellidos").focus();
    h.run();
    h.event(egui::Event::Text("Zapata".into()));
    h.run();
    // Ctrl+Intro guarda.
    h.key_press_modifiers(egui::Modifiers::COMMAND, Key::Enter);
    settle(&mut h);
    assert!(!h.state().contact_editor_open());
    let s = &mut h.state_mut().session;
    assert_eq!(s.contacts().len(), before + 1);
    assert!(s.contacts().iter().any(|c| c.full_name == "Zoe Zapata"));
    // Queda elegido y su ficha a la vista.
    assert!(h.query_all_by_label("Zoe Zapata").next().is_some());

    // Vacío no se guarda: el editor sigue abierto con el motivo.
    h.get_by_label("Nuevo contacto").click();
    settle(&mut h);
    h.get_by_label("Guardar").click();
    settle(&mut h);
    assert!(h.state().contact_editor_open());
    assert!(h.query_all_by_label_contains("al menos un nombre").next().is_some());
}
