//! UX-27: colores propios de la barra lateral y de la barra de título (Ajustes → Apariencia), con
//! el texto de encima claro u oscuro según el color.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{TimeZone, Utc};
use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
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
fn bar_colors_from_settings() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut app = app(dir.path());
    app.demo_settings("appearance", None);
    let mut h = harness(app);
    settle(&mut h);
    assert_eq!(h.state().session.prefs.sidebar_color, None);
    h.get_by_label("Barra lateral izquierda #1f2937").click();
    settle(&mut h);
    assert_eq!(h.state().session.prefs.sidebar_color, Some([0x1f, 0x29, 0x37]));
    h.get_by_label("Barra de título (la de la búsqueda) #f5f0e6").click();
    settle(&mut h);
    assert_eq!(h.state().session.prefs.titlebar_color, Some([0xf5, 0xf0, 0xe6]));
    // «Del tema» lo devuelve (el primero es el de la barra lateral).
    h.get_all_by_label("Del tema").next().unwrap().click();
    settle(&mut h);
    assert_eq!(h.state().session.prefs.sidebar_color, None);
    assert_eq!(h.state().session.prefs.titlebar_color, Some([0xf5, 0xf0, 0xe6]));
}
