//! Acciones con varios correos a la vez: casillas en la lista y barra de acciones en lote (marcar
//! leídos o no leídos, eliminar, seleccionar todo, quitar la selección).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{TimeZone, Utc};
use egui::accesskit::Role;
use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use iuredesk_accounts::MemoryStore;
use iuredesk_engine::{OfflineTransport, Session, SessionConfig, ThemePref, Zone};
use iuredesk_model::MessageId;
use iuredesk_store::{Store, sample};
use iuredesk_ui::{DeskApp, Options};

static GPU: Mutex<()> = Mutex::new(());

fn app(dir: &std::path::Path) -> DeskApp {
    iuredesk_ui::iurefficient_ui::lang::set_lang(iuredesk_ui::iurefficient_ui::lang::Lang::Es);
    iuredesk_engine::lang::set_lang(iuredesk_engine::lang::Lang::Es);
    let now = Utc.with_ymd_and_hms(2026, 10, 21, 11, 20, 0).unwrap();
    let store = Store::open(dir).unwrap();
    sample::populate(&store, sample::DEFAULT_SEED, now).unwrap();
    let mut s = Session::new(SessionConfig {
        store: Arc::new(store),
        accounts: None,
        secrets: Arc::new(MemoryStore::default()),
        prefs_path: None,
        now: Some(now),
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
}

/// Marca la casilla de la fila `n` (entre las visibles).
fn check(h: &mut Harness<'_, DeskApp>, n: usize) {
    h.query_all_by_role(Role::CheckBox)
        .filter(|c| c.accesskit_node().label().is_some_and(|l| l.starts_with("Seleccionar")))
        .nth(n)
        .unwrap()
        .click();
    h.run_steps(2);
}

fn seen(h: &mut Harness<'_, DeskApp>, ids: &[MessageId]) -> Vec<bool> {
    ids.iter().map(|&id| h.state_mut().session.envelope(id).is_some_and(|e| e.flags.seen)).collect()
}

#[test]
fn checkboxes_select_several_and_the_bar_acts_on_all() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app(dir.path()));
    settle(&mut h);
    // (La lista anuncia «… seleccionados» a los lectores de pantalla: la barra se reconoce por su botón.)
    assert!(h.query_by_label("Quitar selección (Esc)").is_none(), "sin selección múltiple, sin barra");
    // Dos casillas, sin teclas.
    check(&mut h, 0);
    check(&mut h, 1);
    let sel: Vec<MessageId> = h.state().session.selection().to_vec();
    assert_eq!(sel.len(), 2, "las casillas suman a la selección");
    assert!(h.query_by_label("2 seleccionados").is_some());
    // No leídos y leídos, los dos a la vez.
    h.get_by_label("Marcar como no leídos").click();
    settle(&mut h);
    assert_eq!(seen(&mut h, &sel), [false, false]);
    h.get_by_label("Marcar como leídos").click();
    settle(&mut h);
    assert_eq!(seen(&mut h, &sel), [true, true]);
    // Eliminar los dos.
    let before = h.state().session.rows_shared().len();
    h.get_all_by_label("Eliminar").next().unwrap().click();
    settle(&mut h);
    assert_eq!(h.state().session.rows_shared().len(), before - 2);
}

#[test]
fn select_all_and_escape() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app(dir.path()));
    settle(&mut h);
    let total = h.state().session.rows_shared().len();
    check(&mut h, 0);
    check(&mut h, 2);
    // En la columna angosta «Seleccionar todos» vive en «Más acciones».
    if h.query_all_by_label_contains("Seleccionar todos").next().is_none() {
        h.get_by_label("Más acciones para la selección").click();
        settle(&mut h);
    }
    h.get_all_by_label_contains("Seleccionar todos").next().unwrap().click();
    settle(&mut h);
    assert_eq!(h.state().session.selection().len(), total);
    assert!(h.query_by_label(&format!("{total} seleccionados")).is_some());
    h.key_press(egui::Key::Escape);
    settle(&mut h);
    assert!(h.state().session.selection().len() <= 1, "Esc quita la selección");
    assert!(h.query_by_label("Quitar selección (Esc)").is_none(), "sin barra");
}
