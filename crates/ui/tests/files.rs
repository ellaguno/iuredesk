//! S1.5 con egui_kittest: «Archivos», las unidades de IureDav dentro de IureDesk.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{TimeZone, Utc};
use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use iuredesk_accounts::MemoryStore;
use iuredesk_engine::iuredesk_integrations::{DavMount, SisterApp, Suite};
use iuredesk_engine::{Module, OfflineTransport, Session, SessionConfig, ThemePref, Zone};
use iuredesk_store::{Store, sample};
use iuredesk_ui::{DeskApp, Options};

static GPU: Mutex<()> = Mutex::new(());

fn app(dir: &std::path::Path, suite: Suite) -> DeskApp {
    iuredesk_ui::iurefficient_ui::lang::set_lang(iuredesk_ui::iurefficient_ui::lang::Lang::Es);
    iuredesk_engine::lang::set_lang(iuredesk_engine::lang::Lang::Es);
    let store = Store::open(dir).unwrap();
    let now = Utc.with_ymd_and_hms(2026, 10, 21, 11, 20, 0).unwrap();
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
    let mut a = DeskApp::new(s, Options { system_fonts: false, no_theme_watch: true, ..Default::default() });
    a.dismiss_reminders();
    a.set_suite(suite);
    a
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

/// Corre cuadros hasta que aparezca `label` (lo leen hilos de fondo).
fn wait_for(h: &mut Harness<'_, DeskApp>, label: &str) {
    let start = Instant::now();
    while h.query_by_label(label).is_none() {
        assert!(start.elapsed() < Duration::from_secs(15), "no apareció «{label}»");
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn drive(mount: &std::path::Path, mounted: bool) -> DavMount {
    DavMount {
        id: "despacho".into(),
        name: "Despacho".into(),
        url: "https://dav.example.com/".into(),
        user: "ana".into(),
        mount_point: mount.to_string_lossy().into_owned(),
        mounted,
        writable: true,
    }
}

#[test]
fn files_browses_the_drive_and_mails_a_file() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mount = tempfile::tempdir().unwrap();
    std::fs::create_dir(mount.path().join("casos")).unwrap();
    std::fs::write(mount.path().join("contrato.pdf"), b"%PDF").unwrap();
    std::fs::write(mount.path().join("casos").join("demanda.docx"), b"docx").unwrap();
    let m = drive(mount.path(), true);
    let suite =
        Suite::empty().with_app(SisterApp::Dav, "/opt/iuredav/iuredav-app").with_mounts(move || vec![m.clone()]);
    let mut h = harness(app(dir.path(), suite));
    h.run_steps(4);
    assert!(h.query_by_label("Archivos").is_some(), "en el riel");
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, "nav.files");
    h.run_steps(2);
    assert_eq!(h.state().session.module, Module::Files);
    wait_for(&mut h, "contrato.pdf");
    assert!(h.query_by_label("casos").is_some(), "la carpeta");
    // Dentro de la carpeta.
    h.state_mut().files_enter("casos");
    wait_for(&mut h, "demanda.docx");
    assert!(h.query_by_label("contrato.pdf").is_none());
    // Elegir el archivo y enviarlo por correo: un correo nuevo con el adjunto.
    h.get_by_label("demanda.docx").click();
    h.run_steps(2);
    h.get_by_label("Enviar por correo").click();
    h.run_steps(4);
    assert_eq!(h.state().composer_count(), 1);
}

#[test]
fn unmounted_drive_invites_to_open_iuredav_and_no_iuredav_no_files() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mount = tempfile::tempdir().unwrap();
    let m = drive(mount.path(), false);
    let suite =
        Suite::empty().with_app(SisterApp::Dav, "/opt/iuredav/iuredav-app").with_mounts(move || vec![m.clone()]);
    let mut h = harness(app(dir.path(), suite));
    h.run_steps(4);
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, "nav.files");
    h.run_steps(4);
    assert!(h.query_all_by_label_contains("no está montada").next().is_some());
    // Sin IureDav: no hay Archivos ni en el riel ni por comando.
    let dir2 = tempfile::tempdir().unwrap();
    let mut h = harness(app(dir2.path(), Suite::empty()));
    h.run_steps(4);
    assert!(h.query_by_label("Archivos").is_none());
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, "nav.files");
    h.run_steps(2);
    assert_ne!(h.state().session.module, Module::Files);
}
