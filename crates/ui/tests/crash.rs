//! R3.3: un informe de fallo sin ver se ofrece al arrancar («Reportar en GitHub» abre el issue con
//! el resumen y copia el informe); al cerrar el aviso queda como visto y no vuelve a salir.

use std::sync::Arc;

use chrono::{TimeZone, Utc};
use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use iuredesk_accounts::MemoryStore;
use iuredesk_engine::{OfflineTransport, Session, SessionConfig, ThemePref, Zone, crash};
use iuredesk_store::{Store, sample};
use iuredesk_ui::{DeskApp, Options};

fn app(dir: &std::path::Path, crash_dir: &std::path::Path) -> DeskApp {
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
    let options = Options {
        system_fonts: false,
        no_theme_watch: true,
        crash_dir: Some(crash_dir.to_path_buf()),
        ..Default::default()
    };
    DeskApp::new(s, options)
}

fn harness(app: DeskApp) -> Harness<'static, DeskApp> {
    Harness::builder().with_size(vec2(1280.0, 800.0)).build_ui_state(
        |ui, app: &mut DeskApp| {
            let full = ui.ctx().content_rect();
            ui.scope_builder(egui::UiBuilder::new().max_rect(full), |ui| app.ui(ui));
        },
        app,
    )
}

#[test]
fn pending_crash_report_is_offered_once() {
    let dir = tempfile::tempdir().unwrap();
    let crashes = tempfile::tempdir().unwrap();
    let facts = crash::PanicFacts {
        message: "attempt to subtract with overflow".into(),
        location: Some("blitz/src/layout.rs:42:7".into()),
        thread: Some("main".into()),
        backtrace: "0: blitz::layout".into(),
    };
    crash::write_report(crashes.path(), &crash::render(&facts, Utc::now())).unwrap();

    iuredesk_ui::capture_opened_urls();
    let mut h = harness(app(dir.path(), crashes.path()));
    h.run_steps(3);
    assert_eq!(h.state().crash_reports_pending(), 1);
    h.get_by_label("IureDesk se cerró inesperadamente");
    h.get_by_label("Reportar en GitHub").click();
    h.run_steps(2);
    let urls = iuredesk_ui::opened_urls();
    let url = urls.first().expect("se abrió el issue");
    assert!(url.starts_with(crash::ISSUES_URL), "{url}");
    assert!(url.contains("subtract"), "{url}");

    h.get_by_label("Cerrar").click();
    h.run_steps(2);
    assert_eq!(h.state().crash_reports_pending(), 0);
    assert!(crash::pending(crashes.path()).is_empty());

    // La siguiente vez ya no sale.
    let dir2 = tempfile::tempdir().unwrap();
    let mut h = harness(app(dir2.path(), crashes.path()));
    h.run_steps(3);
    assert_eq!(h.state().crash_reports_pending(), 0);
}
