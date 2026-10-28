//! F5.3 con egui_kittest: crear una regla con el editor (sin ratón ni código), y las pestañas de
//! categorías de la bandeja. El arnés verifica además los nombres accesibles.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{TimeZone, Utc};
use egui::accesskit::Role;
use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use iuredesk_accounts::MemoryStore;
use iuredesk_engine::iuredesk_rules::{Action, Condition, Field, TextOp};
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
    h.run();
}

#[test]
fn rule_editor_creates_a_rule() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app(dir.path()));
    settle(&mut h);
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, "app.rules");
    settle(&mut h);
    h.get_by_label("Nueva regla").click();
    settle(&mut h);
    assert!(h.state().rule_editor_open());

    // Guardar sin acciones explica qué falta.
    h.get_by_label("Guardar regla").click();
    settle(&mut h);
    assert!(h.query_by_label("Elige qué hacer con el mensaje.").is_some());

    h.get_by_label("+ Añadir condición").click();
    settle(&mut h);
    h.get_by_role_and_label(Role::TextInput, "Texto que se busca").focus();
    h.run();
    h.event(egui::Event::Text("boletin@tienda.example".into()));
    settle(&mut h);
    h.get_by_label("+ Añadir acción").click();
    settle(&mut h);
    h.get_by_label("Guardar regla").click();
    settle(&mut h);
    assert!(!h.state().rule_editor_open(), "guardada: vuelve a la lista");
    let rules = h.state_mut().session.rule_book().rules.clone();
    assert_eq!(rules.len(), 1);
    assert_eq!(
        rules[0].conditions,
        vec![Condition::Text { field: Field::From, op: TextOp::Contains, value: "boletin@tienda.example".into() }]
    );
    assert!(matches!(rules[0].actions[0], Action::MoveTo { .. }));
    // Aparece en la lista con su resumen en lenguaje de cualquier usuario.
    assert!(h.query_all_by_label_contains("Cuando el remitente contiene «boletin@tienda.example»").next().is_some());
}

#[test]
fn category_tabs_switch_the_inbox() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app(dir.path()));
    settle(&mut h);
    let primary = h.state().session.rows().len();
    h.get_by_label_contains("Promociones").click();
    settle(&mut h);
    let promos = h.state().session.rows().len();
    assert!(promos > 0 && promos != primary);
    assert_eq!(h.state().session.category_tab(), iuredesk_engine::iuredesk_rules::Category::Promotions);
}
