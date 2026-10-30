//! UX-5 con egui_kittest: la cápsula de Iurefficient en la lista (nombre accesible con el
//! tooltip «Ticket: …») y la pestaña «Iurefficient» como primera de la bandeja.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{TimeZone, Utc};
use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use iuredesk_accounts::MemoryStore;
use iuredesk_engine::{OfflineTransport, Session, SessionConfig, ThemePref, Zone};
use iuredesk_model::{Mailbox, MessageId};
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
    h.run();
}

/// Agrega un aviso de la instancia (`demo.iurefficient.com` en el ejemplo).
fn put_notice(app: &mut DeskApp, subject: &str) -> MessageId {
    let base = app.session.rows()[0].id;
    let mut env = app.session.store().get_envelope(base).unwrap().unwrap();
    env.id = MessageId::UNASSIGNED;
    env.remote_id = Some("ux5-kit".into());
    env.message_id = Some("<ux5-kit@demo.iurefficient.com>".into());
    env.from = vec![Mailbox { name: Some("Iurefficient".into()), addr: "noreply@demo.iurefficient.com".into() }];
    env.reply_to = Vec::new();
    env.subject = subject.into();
    env.date = now();
    env.flags.seen = false;
    let id = app.session.store().put_message(env, b"Subject: x\r\n\r\nhola").unwrap();
    app.session.rebuild_rows();
    id
}

#[test]
fn capsule_names_the_relation() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut a = app(dir.path());
    put_notice(&mut a, "[Iurefficient] Ticket #7 asignado a ti");
    let mut h = harness(a);
    settle(&mut h);
    h.state_mut().session.set_iure_tab();
    settle(&mut h);
    // La fila dice la relación como el tooltip de la cápsula.
    assert!(h.query_all_by_label_contains("Ticket #7 asignado a ti").next().is_some());
}

#[test]
fn iurefficient_tab_comes_first_and_filters() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut a = app(dir.path());
    let notice = put_notice(&mut a, "[Iurefficient] Te mencionaron en ACME");
    let mut h = harness(a);
    settle(&mut h);
    let tab = h.get_by_label_contains("Iurefficient, ");
    let primary = h.get_by_label_contains("Principal");
    assert!(tab.rect().left() < primary.rect().left(), "la pestaña Iurefficient va primero");
    tab.click();
    settle(&mut h);
    assert!(h.state().session.iure_tab());
    let rows: Vec<MessageId> = h.state().session.rows().iter().map(|r| r.id).collect();
    assert!(rows.contains(&notice));
    let total = h.state().session.iure_tab_counts().0;
    // Con conversaciones, las filas son hilos y el contador cuenta mensajes (como las categorías).
    assert!(!rows.is_empty() && rows.len() <= total, "{} {total}", rows.len());
    // Otra pestaña la quita.
    h.get_by_label_contains("Principal").click();
    settle(&mut h);
    assert!(!h.state().session.iure_tab());
}
