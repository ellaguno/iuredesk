//! UX-3: leer cómodo. Panel contextual plegable (riel) y desplegable, «Abrir en ventana» por
//! comando y Mayús+Intro, lectura enfocada con Esc, zoom del cuerpo, y adjuntos que
//! se abren con la app del sistema (con confirmación para los ejecutables).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{TimeZone, Utc};
use egui::{Key, Modifiers, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use iuredesk_accounts::MemoryStore;
use iuredesk_engine::{OfflineTransport, Session, SessionConfig, ThemePref, Zone};
use iuredesk_model::{AttachmentMeta, MessageId};
use iuredesk_store::{Store, sample};
use iuredesk_ui::{DeskApp, Options};

static GPU: Mutex<()> = Mutex::new(());

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 21, 11, 20, 0).unwrap()
}

fn spanish() {
    iuredesk_ui::iurefficient_ui::lang::set_lang(iuredesk_ui::iurefficient_ui::lang::Lang::Es);
    iuredesk_engine::lang::set_lang(iuredesk_engine::lang::Lang::Es);
}

fn sample_store(dir: &std::path::Path) -> Store {
    spanish();
    let store = Store::open(dir).unwrap();
    sample::populate(&store, sample::DEFAULT_SEED, now()).unwrap();
    store
}

fn app_on(store: Store) -> DeskApp {
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
    s.prefs.mark_read_delay_ms = 0;
    s.set_conversations(false);
    DeskApp::new(s, Options { system_fonts: false, no_theme_watch: true, ..Default::default() })
}

fn harness(app: DeskApp, size: egui::Vec2) -> Harness<'static, DeskApp> {
    Harness::builder().with_size(size).build_ui_state(
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

/// Un mensaje cuyo remitente es un contacto (el panel contextual tiene algo que mostrar).
fn message_with_contact(h: &mut Harness<'_, DeskApp>) -> MessageId {
    let ids: Vec<MessageId> = h.state().session.rows().iter().take(200).map(|r| r.id).collect();
    for id in ids {
        if !h.state_mut().session.context_card(id).is_empty() {
            return id;
        }
    }
    panic!("el buzón de ejemplo no tiene mensajes de contactos");
}

fn focus(h: &mut Harness<'_, DeskApp>, id: MessageId) {
    h.state_mut().session.request_focus(id);
    settle(h);
    assert_eq!(h.state().session.focused(), Some(id));
}

#[test]
fn context_panel_collapses_to_a_rail_and_back() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app_on(sample_store(dir.path())), vec2(1440.0, 900.0));
    settle(&mut h);
    let id = message_with_contact(&mut h);
    focus(&mut h, id);
    assert!(!h.state().session.prefs.context_collapsed);

    // Ventana ancha: columna con su botón de plegar en la cabecera.
    h.get_by_label_contains("Plegar el panel").click();
    settle(&mut h);
    assert!(h.state().session.prefs.context_collapsed, "plegado (se recuerda en las preferencias)");
    assert!(h.query_by_label_contains("Plegar el panel").is_none());
    // El riel: un clic lo despliega.
    h.get_by_label_contains("Mostrar el panel contextual").click();
    settle(&mut h);
    assert!(!h.state().session.prefs.context_collapsed);

    // El comando (paleta, atajo Ctrl+Shift+I) hace lo mismo.
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::I);
    settle(&mut h);
    assert!(h.state().session.prefs.context_collapsed);
}

#[test]
fn narrow_window_puts_the_context_in_a_strip() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app_on(sample_store(dir.path())), vec2(1200.0, 860.0));
    settle(&mut h);
    let id = message_with_contact(&mut h);
    focus(&mut h, id);
    // Bajo 1300 px: franja compacta (sin columna ni riel).
    assert!(h.query_by_label_contains("Plegar el panel").is_none());
    assert!(h.query_by_label_contains("Mostrar el panel contextual").is_none());
    let strip = h.get_by_label("Contexto del mensaje");
    strip.click_accesskit();
    settle(&mut h);
    assert!(h.query_by_label_contains("Ocultar detalles").is_some(), "franja desplegada");
}

#[test]
fn open_in_window_by_command_and_keyboard() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app_on(sample_store(dir.path())), vec2(1440.0, 900.0));
    settle(&mut h);
    let rows: Vec<MessageId> = h.state().session.rows().iter().take(4).map(|r| r.id).collect();
    focus(&mut h, rows[1]);

    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, "mail.open_window");
    settle(&mut h);
    assert_eq!(h.state().reader_windows(), vec![rows[1]]);
    // Sin backend de ventanas múltiples egui la dibuja dentro de la principal, con «Cerrar».
    assert!(h.query_by_label_contains("Cerrar ventana").is_some());
    // Otra vez el mismo mensaje: no se duplica.
    h.state_mut().run(&ctx, "mail.open_window");
    settle(&mut h);
    assert_eq!(h.state().reader_windows().len(), 1);

    // Mayús+Intro con otro mensaje: una segunda ventana.
    focus(&mut h, rows[2]);
    h.key_press_modifiers(Modifiers::SHIFT, Key::Enter);
    settle(&mut h);
    assert_eq!(h.state().reader_windows(), vec![rows[1], rows[2]]);

    // «Siguiente» en la ventana cambia de mensaje.
    let next = h.query_all_by_label_contains("Mensaje siguiente").last().unwrap();
    next.click_accesskit();
    settle(&mut h);
    assert_eq!(h.state().reader_windows(), vec![rows[1], rows[3]]);

    // Cerrar es seguro: quedan las demás.
    h.query_all_by_label_contains("Cerrar ventana").next().unwrap().click_accesskit();
    settle(&mut h);
    assert_eq!(h.state().reader_windows().len(), 1);
}

#[test]
fn focused_reading_hides_the_list_and_esc_returns() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app_on(sample_store(dir.path())), vec2(1440.0, 900.0));
    settle(&mut h);
    let id = h.state().session.rows()[0].id;
    focus(&mut h, id);
    h.get_by_label_contains("Lectura enfocada").click();
    settle(&mut h);
    assert!(h.state().reading_focus());
    assert!(h.query_by_label("Nuevo mensaje").is_none(), "sin carpetas");
    h.key_press(Key::Escape);
    settle(&mut h);
    assert!(!h.state().reading_focus());
    assert!(h.query_by_label("Nuevo mensaje").is_some());

    // Zoom del cuerpo (solo en el correo): Ctrl + y Ctrl 0.
    h.key_press_modifiers(Modifiers::COMMAND, Key::Plus);
    settle(&mut h);
    assert_eq!(h.state().session.prefs.reading_zoom_pct, 110);
    h.key_press_modifiers(Modifiers::COMMAND, Key::Num0);
    settle(&mut h);
    assert_eq!(h.state().session.prefs.reading_zoom_pct, 100);
}

/// Reemplaza el original de un mensaje por uno con estos adjuntos (nombre, tipo, base64).
fn with_files(store: &Store, id: MessageId, files: &[(&str, &str, &str)]) {
    let mut env = store.get_envelope(id).unwrap().unwrap();
    let mut raw = String::from(
        "From: a@example.com\r\nTo: b@example.com\r\nSubject: Adjuntos\r\nMIME-Version: 1.0\r\n\
         Content-Type: multipart/mixed; boundary=\"XX\"\r\n\r\n--XX\r\nContent-Type: text/plain\r\n\r\nHola\r\n",
    );
    env.attachments.clear();
    for (name, mime, b64) in files {
        raw.push_str(&format!(
            "--XX\r\nContent-Type: {mime}; name=\"{name}\"\r\nContent-Disposition: attachment; filename=\"{name}\"\r\n\
             Content-Transfer-Encoding: base64\r\n\r\n{b64}\r\n"
        ));
        env.attachments.push(AttachmentMeta {
            filename: name.to_string(),
            mime_type: mime.to_string(),
            ..Default::default()
        });
    }
    raw.push_str("--XX--\r\n");
    env.has_attachments = true;
    store.put_message(env, raw.as_bytes()).unwrap();
}

#[test]
fn attachments_open_with_the_system_and_executables_ask_first() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let mut app = app_on(sample_store(dir.path()));
    app.set_attachment_cache(cache.path().to_path_buf());
    let mut h = harness(app, vec2(1440.0, 900.0));
    settle(&mut h);
    let id = h.state().session.rows()[0].id;
    // «%PDF-1.4 hola» y «MZ» (un ejecutable de Windows).
    with_files(
        h.state().session.store(),
        id,
        &[
            ("../informe final.pdf", "application/pdf", "JVBERi0xLjQgaG9sYQ=="),
            ("setup.exe", "application/octet-stream", "TVo="),
        ],
    );
    h.state_mut().session.retry_body(id);
    focus(&mut h, id);

    iuredesk_ui::capture_opened_urls();
    // Clic en el PDF: se escribe en la caché privada (nombre saneado) y se abre.
    h.get_by_label("Adjunto ../informe final.pdf").click();
    settle(&mut h);
    let opened = iuredesk_ui::opened_urls();
    assert_eq!(opened.len(), 1, "{opened:?}");
    let path = opened[0].strip_prefix("open:").unwrap();
    assert!(path.starts_with(&cache.path().display().to_string()) && path.ends_with("informe final.pdf"), "{path}");
    assert_eq!(std::fs::read(path).unwrap(), b"%PDF-1.4 hola");

    // El ejecutable pide confirmación y no se abre solo.
    h.get_by_label("Adjunto setup.exe").click();
    settle(&mut h);
    assert!(h.state().attachment_confirm_open());
    assert_eq!(iuredesk_ui::opened_urls().len(), 1, "nada se abrió aún");
    h.get_by_label("Cancelar").click();
    settle(&mut h);
    assert!(!h.state().attachment_confirm_open());
    assert_eq!(iuredesk_ui::opened_urls().len(), 1);
    // Clic derecho → «Abrir» → confirmar.
    h.get_by_label("Adjunto setup.exe").click_secondary();
    h.run();
    h.get_by_label("Abrir").click();
    settle(&mut h);
    assert!(h.state().attachment_confirm_open());
    h.get_by_label("Abrir de todos modos").click();
    settle(&mut h);
    let opened = iuredesk_ui::opened_urls();
    assert_eq!(opened.len(), 2, "{opened:?}");
    assert!(opened[1].starts_with("open:") && opened[1].ends_with("setup.exe"));
}
