//! R1.2: archivos que abre el sistema (`iuredesk tarjeta.vcf`, `cita.ics`, `mensaje.eml`): cada
//! uno en su ventana, con sus botones; «Agregar a contactos» agrega y cierra.

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
    DeskApp::new(s, Options { system_fonts: false, no_theme_watch: true, ..Default::default() })
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
    h.run();
}

#[test]
fn files_from_the_system_open_in_their_window() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let files = tempfile::tempdir().unwrap();
    let vcf = files.path().join("tarjeta.vcf");
    std::fs::write(&vcf, "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Zoe Quintana\r\nEMAIL:zoe@q.example\r\nEND:VCARD\r\n")
        .unwrap();
    let eml = files.path().join("aviso.eml");
    std::fs::write(&eml, "From: Ana <ana@example.com>\r\nSubject: Aviso del juzgado\r\n\r\nTexto del aviso.\r\n")
        .unwrap();
    let mut a = app(dir.path());
    a.open_args(vec![
        vcf.to_string_lossy().into_owned(),
        eml.to_string_lossy().into_owned(),
        files.path().join("falta.ics").to_string_lossy().into_owned(),
    ]);
    let mut h = harness(a);
    settle(&mut h);
    assert_eq!(h.state().opened_windows(), 2, "el que falta no abre ventana");
    assert!(h.query_by_label_contains("Zoe Quintana").is_some());
    assert!(h.query_by_label_contains("Aviso del juzgado").is_some());
    assert!(h.query_by_label_contains("Texto del aviso.").is_some());
    assert!(h.query_by_label_contains("Solo lectura").is_some());
    assert!(h.query_all_by_label("Importar a…").count() >= 2);
    assert!(h.query_all_by_label_contains("no existe").next().is_some(), "error humano del que falta");
    let before = h.state_mut().session.contacts().len();
    h.get_by_label("Agregar a contactos").click();
    settle(&mut h);
    assert_eq!(h.state_mut().session.contacts().len(), before + 1);
    assert_eq!(h.state().opened_windows(), 1);
    assert!(h.query_all_by_label_contains("Se agregó «Zoe Quintana»").next().is_some());
}
