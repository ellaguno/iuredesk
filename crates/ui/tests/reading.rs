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

/// Regresión (dueño 2026-11-13): en una conversación, pulsar un mensaje anterior debe abrirlo
/// para leerlo (y volver a pulsarlo lo pliega).
#[test]
fn earlier_message_in_a_conversation_opens_on_click() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app_on(sample_store(dir.path())), vec2(1440.0, 900.0));
    h.state_mut().session.set_conversations(true);
    settle(&mut h);
    let ids: Vec<MessageId> = h.state().session.rows().iter().take(400).map(|r| r.id).collect();
    let (id, first) = ids
        .into_iter()
        .find_map(|id| {
            let conv = h.state_mut().session.conversation(id);
            (conv.len() >= 3).then(|| (conv.last().unwrap().id, conv[0].clone()))
        })
        .expect("una conversación de tres o más");
    focus(&mut h, id);
    let addr = first.from[0].addr.clone();
    let opened = |h: &Harness<'_, DeskApp>| h.query_all_by_label_contains(&format!("{addr}  ·")).next().is_some();
    assert!(!opened(&h), "empieza plegado");
    let name = first.from[0].name.clone().filter(|n| !n.is_empty()).unwrap_or(addr.clone());
    let label = format!("Mensaje de {name}");
    let headers: Vec<_> = h.query_all_by_label(&label).collect();
    assert!(!headers.is_empty(), "cabecera del mensaje anterior");
    // Clic de ratón de verdad sobre el nombre del remitente (texto de la cabecera).
    let header_rect = headers[0].rect();
    let name_rect = h
        .query_all_by_label(&name)
        .map(|n| n.rect())
        .find(|r| header_rect.contains_rect(*r))
        .expect("el nombre dentro de la cabecera");
    let at = name_rect.center();
    h.event(egui::Event::PointerMoved(at));
    h.run_steps(1);
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
        h.run_steps(1);
    }
    settle(&mut h);
    assert!(opened(&h), "al pulsarlo se abre");
    // Volver a pulsarlo lo pliega.
    h.query_all_by_label(&label).next().unwrap().click();
    settle(&mut h);
    assert!(!opened(&h), "y se pliega");

    // En la ventana de lectura propia también (antes no miraba el clic y no se abría).
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, "mail.open_window");
    settle(&mut h);
    assert_eq!(h.state().reader_windows(), vec![id]);
    assert!(!opened(&h));
    h.query_all_by_label(&label).last().expect("cabecera en la ventana").click();
    settle(&mut h);
    assert!(opened(&h), "se abre en la ventana");
}

/// UX-22: el texto de un correo HTML (pintado como imagen) se selecciona arrastrando y se copia
/// con Ctrl+C; un clic suelto quita la selección.
#[test]
fn html_body_text_selects_and_copies() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let app = app_on(sample_store(dir.path()));
    let mut h = harness(app, vec2(1440.0, 900.0));
    settle(&mut h);
    let id = h.state().session.rows()[0].id;
    let env = h.state().session.store().get_envelope(id).unwrap().unwrap();
    let raw = "From: a@example.com\r\nTo: b@example.com\r\nSubject: Seleccionar\r\nMIME-Version: 1.0\r\n\
               Content-Type: text/html; charset=utf-8\r\n\r\n\
               <p style=\"font-size:22px;margin:0\">Texto para copiar del correo</p><p>Segunda línea</p>\r\n";
    h.state().session.store().put_message(env, raw.as_bytes()).unwrap();
    h.state_mut().session.retry_body(id);
    focus(&mut h, id);
    let body = h.get_by_label_contains("Texto para copiar").rect();
    // De la izquierda del cuerpo hasta abajo de todo: las dos líneas.
    let from = body.left_top() + vec2(2.0, 12.0);
    let to = body.left_bottom() + vec2(400.0, -2.0);
    let press = |h: &mut Harness<'_, DeskApp>, pos: egui::Pos2, pressed: bool| {
        h.event(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
        h.run_steps(1);
    };
    h.hover_at(from);
    h.run_steps(1);
    press(&mut h, from, true);
    for k in 1..=8 {
        h.event(egui::Event::PointerMoved(from + (to - from) * (k as f32 / 8.0)));
        h.run_steps(1);
    }
    press(&mut h, to, false);
    h.event(egui::Event::Copy);
    h.run_steps(1);
    let copied: Vec<String> = h
        .output()
        .platform_output
        .commands
        .iter()
        .filter_map(|c| match c {
            egui::OutputCommand::CopyText(t) => Some(t.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(copied, ["Texto para copiar del correo\nSegunda línea"], "{copied:?}");
    // Un clic suelto quita la selección: Ctrl+C ya no copia nada.
    h.hover_at(from);
    press(&mut h, from, true);
    press(&mut h, from, false);
    h.event(egui::Event::Copy);
    h.run_steps(1);
    assert!(!h.output().platform_output.commands.iter().any(|c| matches!(c, egui::OutputCommand::CopyText(_))));
}
