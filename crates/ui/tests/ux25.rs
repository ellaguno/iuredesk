//! UX-25: iconos al pasar sobre una fila (archivar, eliminar, leído, posponer) que actúan sobre
//! ella sin cambiar lo elegido, y «Seleccionar ▾» por criterio.

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

/// Rectángulo de la fila `n` de la lista (por su casilla y su nombre accesible).
fn row_rect(h: &Harness<'_, DeskApp>, n: usize) -> (egui::Rect, String) {
    let cb = h
        .query_all_by_role(Role::CheckBox)
        .filter(|c| c.accesskit_node().label().is_some_and(|l| l.starts_with("Seleccionar ")))
        .nth(n)
        .unwrap();
    let label = cb.accesskit_node().label().unwrap().trim_start_matches("Seleccionar ").to_string();
    let row = h.get_by_label(&label);
    (row.rect(), label)
}

#[test]
fn hover_icons_act_on_that_row_only() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app(dir.path()));
    settle(&mut h);
    let rows: Vec<MessageId> = h.state().session.rows_shared().iter().map(|r| r.id).collect();
    let open = rows[0];
    h.state_mut().session.request_focus(open);
    settle(&mut h);
    let (rect, _) = row_rect(&h, 2);
    let target = rows[2];
    h.event(egui::Event::PointerMoved(rect.center()));
    h.run_steps(2);
    let archive = h
        .get_all_by_label("Archivar")
        .find(|n| rect.contains(n.rect().center()))
        .expect("el icono Archivar aparece en la fila bajo el ratón");
    archive.click();
    settle(&mut h);
    assert!(!h.state().session.rows_shared().iter().any(|r| r.id == target), "la fila se archiva");
    assert_eq!(h.state().session.focused(), Some(open), "el abierto no cambia");

    // Posponer: el menú queda abierto aunque el ratón salga de la fila.
    let (rect, _) = row_rect(&h, 3);
    let target = h.state().session.rows_shared()[3].id;
    h.event(egui::Event::PointerMoved(rect.center()));
    h.run_steps(2);
    h.get_all_by_label("Posponer").find(|n| rect.contains(n.rect().center())).unwrap().click();
    settle(&mut h);
    h.get_all_by_label_contains("Mañana").next().expect("opciones de posponer").click();
    settle(&mut h);
    assert!(h.state().session.is_snoozed(target));
    assert_eq!(h.state().session.focused(), Some(open));
}

#[test]
fn select_menu_by_criterion() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app(dir.path()));
    settle(&mut h);
    let ids: Vec<MessageId> = h.state().session.rows_shared().iter().map(|r| r.id).collect();
    let unread = ids.iter().filter(|id| !h.state_mut().session.envelope(**id).unwrap().flags.seen).count();
    h.get_by_label("Seleccionar…").click();
    settle(&mut h);
    h.get_by_label_contains("No leídos").click();
    settle(&mut h);
    assert_eq!(h.state().session.selection().len(), unread);
    assert!(h.query_by_label(&format!("{unread} seleccionados")).is_some());
    h.get_by_label("Seleccionar…").click();
    settle(&mut h);
    h.get_by_label_contains("Ninguno").click();
    settle(&mut h);
    assert!(h.state().session.selection().is_empty());
}
