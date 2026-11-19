//! UX-17 y UX-18: el ojo de cada cuenta y las tres columnas del correo que se pliegan.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{TimeZone, Utc};

use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use iuredesk_accounts::MemoryStore;
use iuredesk_engine::{OfflineTransport, Session, SessionConfig, ThemePref, Zone};
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

#[test]
fn columns_fold_and_unfold() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app(dir.path()));
    settle(&mut h);
    let prefs = |h: &Harness<'_, DeskApp>| {
        let p = &h.state().session.prefs;
        (p.folders_collapsed, p.list_collapsed, p.reading_collapsed)
    };
    // Plegar cuentas y carpetas: queda el riel; su botón la despliega.
    h.get_by_label("Ocultar cuentas y carpetas").click();
    settle(&mut h);
    assert_eq!(prefs(&h), (true, false, false));
    assert!(h.query_by_label("Nuevo mensaje").is_none(), "la columna ya no se ve");
    h.get_all_by_label("Mostrar cuentas y carpetas").next().unwrap().click();
    settle(&mut h);
    assert_eq!(prefs(&h), (false, false, false));
    // Plegar la lista: la lectura ocupa su lugar.
    h.get_by_label("Ocultar la lista de correos").click();
    settle(&mut h);
    assert_eq!(prefs(&h), (false, true, false));
    // Plegar la lectura despliega la lista (nunca las dos plegadas).
    h.get_by_label("Ocultar el panel de lectura").click();
    settle(&mut h);
    assert_eq!(prefs(&h), (false, false, true));
    h.get_all_by_label("Mostrar el panel de lectura").next().unwrap().click();
    settle(&mut h);
    assert_eq!(prefs(&h), (false, false, false));
}

#[test]
fn the_eye_hides_an_account() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app(dir.path()));
    settle(&mut h);
    let total = h.state().session.rows_shared().len();
    // El ojo de la primera cuenta (accesible como «Visible: …»).
    let eye = h
        .query_all_by_label_contains("Visible:")
        .next()
        .and_then(|n| n.accesskit_node().label().map(|l| l.to_string()));
    let eye = eye.expect("el ojo de la cuenta");
    h.get_by_label(&eye).click();
    settle(&mut h);
    assert_eq!(h.state().session.prefs.hidden_accounts.len(), 1);
    assert!(h.state().session.rows_shared().len() < total, "sus correos salen de la bandeja");
    h.get_by_label(&eye).click();
    settle(&mut h);
    assert!(h.state().session.prefs.hidden_accounts.is_empty());
    assert_eq!(h.state().session.rows_shared().len(), total);
}
