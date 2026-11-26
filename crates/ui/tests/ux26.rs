//! UX-26: atajos de Gmail que faltaban (`j`/`k`, `x`, `* u`, `g t`, `?`), con las teclas como
//! llegan de verdad: el evento de la tecla y el carácter escrito en el mismo cuadro.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{TimeZone, Utc};
use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
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
    s.prefs.scheme = iuredesk_engine::ShortcutScheme::Gmail;
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

/// Pulsa una tecla y escribe su carácter (como hace el sistema), en el mismo cuadro.
fn press(h: &mut Harness<'_, DeskApp>, key: egui::Key, text: &str, shift: bool) {
    let modifiers = if shift { egui::Modifiers::SHIFT } else { egui::Modifiers::NONE };
    h.event(egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers });
    h.event(egui::Event::Text(text.into()));
    h.event(egui::Event::Key { key, physical_key: None, pressed: false, repeat: false, modifiers });
    h.run_steps(1);
    settle(h);
}

/// Primera tecla de un atajo de dos («g», «*»): sin esperar (la segunda debe llegar en 1,5 s).
fn prefix(h: &mut Harness<'_, DeskApp>, key: egui::Key, text: &str, shift: bool) {
    let modifiers = if shift { egui::Modifiers::SHIFT } else { egui::Modifiers::NONE };
    h.event(egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers });
    h.event(egui::Event::Text(text.into()));
    h.event(egui::Event::Key { key, physical_key: None, pressed: false, repeat: false, modifiers });
    h.run_steps(2);
}

#[test]
fn gmail_keys_move_select_and_go() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app(dir.path()));
    settle(&mut h);
    let rows: Vec<MessageId> = h.state().session.rows_shared().iter().map(|r| r.id).collect();
    // j / k.
    press(&mut h, egui::Key::J, "j", false);
    assert_eq!(h.state().session.focused(), Some(rows[0]));
    press(&mut h, egui::Key::J, "j", false);
    assert_eq!(h.state().session.focused(), Some(rows[1]));
    press(&mut h, egui::Key::K, "k", false);
    assert_eq!(h.state().session.focused(), Some(rows[0]));
    // x marca, j baja, x marca otro: dos seleccionados con su barra.
    press(&mut h, egui::Key::X, "x", false);
    press(&mut h, egui::Key::J, "j", false);
    press(&mut h, egui::Key::X, "x", false);
    assert_eq!(h.state().session.selection().len(), 2, "{:?}", h.state().session.selection());
    assert!(h.query_by_label("2 seleccionados").is_some());
    // * n quita; * u los no leídos (la «u» no dispara «volver a la lista» ni otra cosa).
    prefix(&mut h, egui::Key::Num8, "*", true);
    press(&mut h, egui::Key::N, "n", false);
    assert!(h.state().session.selection().is_empty(), "{:?}", h.state().session.selection());
    let unread = rows.iter().filter(|id| !h.state_mut().session.envelope(**id).unwrap().flags.seen).count();
    prefix(&mut h, egui::Key::Num8, "*", true);
    press(&mut h, egui::Key::U, "u", false);
    assert_eq!(h.state().session.selection().len(), unread);
    // * a: todos, y la «a» no abre «Responder a todos».
    prefix(&mut h, egui::Key::Num8, "*", true);
    press(&mut h, egui::Key::A, "a", false);
    assert_eq!(h.state().session.selection().len(), rows.len());
    assert!(h.query_by_label("Enviar").is_none(), "no se abrió un redactor");
    // g t: Enviados.
    prefix(&mut h, egui::Key::G, "g", false);
    press(&mut h, egui::Key::T, "t", false);
    assert_eq!(h.state().session.folder_sel(), iuredesk_engine::FolderSel::Unified(iuredesk_model::FolderRole::Sent));
    // ?: la hoja de atajos, con los de dos teclas; Esc la cierra.
    press(&mut h, egui::Key::Slash, "?", true);
    assert!(h.query_all_by_label("Atajos de teclado").next().is_some());
    assert!(h.query_all_by_label("Ir a Recibidos").next().is_some());
    h.key_press(egui::Key::Escape);
    settle(&mut h);
    assert!(h.query_by_label("Ir a Recibidos").is_none());
}

#[test]
fn gmail_keys_work_after_clicking_a_row() {
    // Regresión: tras un clic en una fila la lista se queda con el foco del teclado y los atajos
    // de una tecla dejaban de responder.
    use egui_kittest::kittest::NodeT;
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app(dir.path()));
    settle(&mut h);
    let rows: Vec<MessageId> = h.state().session.rows_shared().iter().map(|r| r.id).collect();
    let label = h
        .query_all_by_role(egui::accesskit::Role::CheckBox)
        .filter_map(|c| c.accesskit_node().label())
        .find(|l| l.starts_with("Seleccionar "))
        .unwrap();
    h.get_by_label(label.trim_start_matches("Seleccionar ")).click();
    settle(&mut h);
    assert_eq!(h.state().session.focused(), Some(rows[0]));
    assert!(h.ctx.memory(|m| m.focused()).is_some(), "la lista tiene el foco");
    press(&mut h, egui::Key::J, "j", false);
    assert_eq!(h.state().session.focused(), Some(rows[1]));
}
