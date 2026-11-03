//! UX-11 con egui_kittest: el editor de citas ofrece la videollamada del servidor (Meet), dice
//! qué se manda a los invitados y el botón lo anuncia; guardar pone la invitación por correo en
//! la bandeja de salida cuando el servidor no la manda.

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

#[test]
fn meet_toggle_and_invite_line() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app(dir.path()));
    settle(&mut h);
    assert!(h.state_mut().demo_new_meeting(true));
    settle(&mut h);
    assert!(h.state().event_editor_open());
    h.get_by_label("Agregar videollamada de Google Meet");
    assert!(h.query_all_by_label_contains("se crea al guardar").next().is_some());
    h.get_by_label("Tu servidor de calendario enviará la invitación a 2 personas.");
    h.get_by_label("Guardar y enviar").click();
    settle(&mut h);
    assert!(!h.state().event_editor_open());
    assert!(h.state().session.outbox_entries().is_empty(), "lo manda el servidor");
}

#[test]
fn email_invitations_go_to_the_outbox() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app(dir.path()));
    settle(&mut h);
    assert!(h.state_mut().demo_new_meeting(false));
    settle(&mut h);
    assert!(h.query_all_by_label("Agregar videollamada de Google Meet").next().is_none(), "sin servidor que la cree");
    h.get_by_label("IureDesk enviará por correo la invitación a 2 personas.");
    h.get_by_label("Guardar y enviar").click();
    settle(&mut h);
    let out = h.state().session.outbox_entries();
    assert_eq!(out.len(), 1);
    assert!(out[0].subject.starts_with("Invitación: Revisión del contrato"), "{}", out[0].subject);
    assert!(h.query_all_by_label_contains("aviso a 2 personas").next().is_some());
}
