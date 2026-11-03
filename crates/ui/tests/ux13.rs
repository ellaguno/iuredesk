//! UX-13 con egui_kittest: el fondo de «Mi día» (color, degradado, imagen e imagen del día con
//! su crédito) y su selector en Ajustes → Apariencia.

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

/// PNG pequeño hecho aquí.
fn png() -> Vec<u8> {
    let mut out = Vec::new();
    image::RgbaImage::from_pixel(64, 32, image::Rgba([30, 90, 200, 255]))
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    out
}

#[test]
fn backgrounds_paint_and_credit_is_shown() {
    use iuredesk_engine::today_bg::{Potd, TodayBackground as Bg};
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut a = app(dir.path());
    a.session.module = Module::Today;
    let mut h = harness(a);
    settle(&mut h);
    for bg in [Bg::Color { rgb: [10, 20, 30] }, Bg::Gradient { from: [0, 0, 0], to: [200, 100, 50] }] {
        h.state_mut().session.prefs.today_background = bg;
        settle(&mut h);
        assert!(
            h.query_all_by_label_contains("Buenos").next().is_some()
                || h.query_all_by_label_contains("Buenas").next().is_some()
        );
    }
    let credit = Potd {
        date: chrono::NaiveDate::from_ymd_opt(2026, 10, 21).unwrap(),
        title: "Ballesta.jpg".into(),
        artist: "Diego Delso".into(),
        license: "CC BY-SA 4.0".into(),
        page_url: "https://commons.wikimedia.org/wiki/File:Ballesta.jpg".into(),
        file: Default::default(),
    };
    let img = png();
    h.state_mut().demo_today_background(Bg::Commons, Some((&img, Some(credit))));
    settle(&mut h);
    // El crédito que piden las licencias libres, con autor y licencia.
    assert!(h.query_all_by_label_contains("Diego Delso · CC BY-SA 4.0").next().is_some(), "crédito visible");
    assert_eq!(h.state().today_background_credit().map(|p| p.license.as_str()), Some("CC BY-SA 4.0"));
}

#[test]
fn settings_choose_a_background() {
    use iuredesk_engine::today_bg::TodayBackground as Bg;
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app(dir.path()));
    settle(&mut h);
    h.state_mut().demo_settings("appearance", None);
    settle(&mut h);
    h.get_by_role_and_label(egui::accesskit::Role::ComboBox, "Fondo de «Mi día»").click();
    settle(&mut h);
    h.get_by_label("Degradado").click();
    settle(&mut h);
    assert!(matches!(h.state().session.prefs.today_background, Bg::Gradient { .. }));
    assert!(h.query_by_label("De").is_some(), "colores del degradado");
    h.get_by_role_and_label(egui::accesskit::Role::ComboBox, "Fondo de «Mi día»").click();
    settle(&mut h);
    h.get_by_label("Imagen del día de Wikimedia Commons").click();
    settle(&mut h);
    assert_eq!(h.state().session.prefs.today_background, Bg::Commons);
    assert!(h.query_all_by_label_contains("commons.wikimedia.org").next().is_some(), "explica de dónde sale");
}
