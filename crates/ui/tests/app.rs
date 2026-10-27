//! Pruebas de interacción (egui_kittest) sobre el buzón de ejemplo: paleta y comandos, selección
//! y panel de lectura, densidad, atajos por esquema, bienvenida, y una prueba de rendimiento con
//! 100 000 mensajes. El arnés verifica además que todo control tenga nombre accesible.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{TimeZone, Utc};
use egui::{Key, Modifiers, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use iuredesk_accounts::MemoryStore;
use iuredesk_engine::{DensityPref, OfflineTransport, Session, SessionConfig, ShortcutScheme, ThemePref, Zone};
use iuredesk_model::{AccountId, Folder, FolderRole, MessageId};
use iuredesk_store::{AccountLabel, Store, sample};
use iuredesk_ui::{DeskApp, Options};

/// Las pruebas que renderizan se turnan (algunos controladores no aguantan dos dispositivos).
static GPU: Mutex<()> = Mutex::new(());

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 21, 11, 20, 0).unwrap()
}

fn spanish() {
    iuredesk_ui::iurefficient_ui::lang::set_lang(iuredesk_ui::iurefficient_ui::lang::Lang::Es);
    iuredesk_engine::lang::set_lang(iuredesk_engine::lang::Lang::Es);
}

fn session_on(store: Store, conversations: bool) -> Session {
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
    s.set_conversations(conversations);
    s
}

fn sample_app(dir: &std::path::Path, conversations: bool) -> DeskApp {
    spanish();
    let store = Store::open(dir).unwrap();
    sample::populate(&store, sample::DEFAULT_SEED, now()).unwrap();
    DeskApp::new(
        session_on(store, conversations),
        Options { system_fonts: false, no_theme_watch: true, ..Default::default() },
    )
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

/// Corre cuadros hasta que no quede trabajo de fondo y la UI se calme.
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

fn folder_role(app: &DeskApp, id: MessageId) -> FolderRole {
    let f = app.session.store().get_envelope(id).unwrap().unwrap().folder;
    app.session.folder(f).unwrap().role
}

#[test]
fn palette_opens_with_ctrl_k_and_runs_a_command() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(sample_app(dir.path(), false), vec2(1440.0, 900.0));
    settle(&mut h);
    let id = h.state().session.rows()[2].id;
    h.state_mut().session.request_focus(id);
    settle(&mut h);
    assert_eq!(h.state().session.focused(), Some(id));

    h.key_press_modifiers(Modifiers::COMMAND, Key::K);
    h.run();
    assert!(h.state().palette.is_open());
    h.event(egui::Event::Text("archivar".into()));
    h.run();
    h.key_press(Key::Enter);
    settle(&mut h);
    assert!(!h.state().palette.is_open());
    assert_eq!(folder_role(h.state(), id), FolderRole::Archive);
    assert!(h.query_all_by_label_contains("Mensaje archivado").next().is_some(), "aviso con deshacer");

    // «Deshacer» del aviso.
    h.get_by_label("Deshacer").click();
    settle(&mut h);
    assert_eq!(folder_role(h.state(), id), FolderRole::Inbox);
}

#[test]
fn clicking_a_row_shows_the_message_in_the_reading_pane() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(sample_app(dir.path(), true), vec2(1440.0, 900.0));
    settle(&mut h);
    let row = h.state().session.rows()[4];
    let subject = h.state_mut().session.envelope(row.id).unwrap().subject.clone();
    assert!(h.state().session.focused().is_none());
    h.query_all_by_label_contains(&subject).next().expect("fila visible").click();
    settle(&mut h);
    assert_eq!(h.state().session.focused(), Some(row.id));
    let (base, _) = iuredesk_model::normalize_subject(&subject);
    assert!(h.query_by_label(&base).is_some(), "el asunto encabeza el panel de lectura");
    assert!(h.query_all_by_label_contains("Responder").next().is_some(), "barra de acciones contextual");
}

#[test]
fn density_switch_restyles_and_persists_in_the_session() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(sample_app(dir.path(), false), vec2(1440.0, 900.0));
    settle(&mut h);
    assert_eq!(h.state().session.prefs.density, DensityPref::Compact);
    // Menú rápido de la barra de título: tema y densidad.
    h.get_by_label("Apariencia").click();
    h.run();
    h.get_by_label("Densa").click();
    settle(&mut h);
    assert_eq!(h.state().session.prefs.density, DensityPref::Dense);
    assert_eq!(iuredesk_ui::iurefficient_ui::theme::density(&h.ctx), iuredesk_ui::iurefficient_ui::Density::Dense);
    // En densa hay encabezado de columnas.
    assert!(h.query_by_label_contains("Asunto").is_some());
    h.get_by_label("Apariencia").click();
    h.run();
    h.get_by_label("Cómoda").click();
    settle(&mut h);
    assert_eq!(h.state().session.prefs.density, DensityPref::Comfortable);
}

#[test]
fn integrated_header_has_named_window_controls_and_a_drag_area() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    spanish();
    let store = Store::open(dir.path()).unwrap();
    sample::populate(&store, sample::DEFAULT_SEED, now()).unwrap();
    let chrome = iuredesk_ui::WindowChrome::named("adwaita");
    let app = DeskApp::new(
        session_on(store, true),
        Options { system_fonts: false, no_theme_watch: true, chrome, ..Default::default() },
    );
    // El arnés exige nombre accesible en todos los controles.
    let mut h = harness(app, vec2(1280.0, 800.0));
    settle(&mut h);
    for label in ["Minimizar", "Maximizar", "Cerrar", "Barra de título", "Comandos", "Apariencia", "IureDesk"] {
        assert!(h.query_by_label(label).is_some(), "falta «{label}» en la barra");
    }
    assert!(h.query_all_by_label_contains("Entrada · ").next().is_some(), "título de la carpeta");
    // La zona de arrastre ocupa la barra de lado a lado, arriba de todo.
    let drag = h.get_by_label("Barra de título").rect();
    assert!(drag.top() <= 1.0 && drag.width() >= 1270.0, "{drag:?}");
    // La búsqueda de la barra filtra el correo.
    h.get_by_label("Buscar en el correo  (de: tiene:adjunto …)").click();
    h.run();
    h.event(egui::Event::Text("Northwind".into()));
    settle(&mut h);
    assert_eq!(h.state().session.search_query(), "Northwind");

    // Con la barra del sistema la fila sigue, sin botones de ventana ni arrastre.
    let dir2 = tempfile::tempdir().unwrap();
    let mut h2 = harness(sample_app(dir2.path(), true), vec2(1280.0, 800.0));
    settle(&mut h2);
    assert!(h2.query_by_label("Comandos").is_some());
    assert!(h2.query_by_label("Minimizar").is_none());
    assert!(h2.query_by_label("Barra de título").is_none());
}

#[test]
fn gmail_scheme_archives_with_e_and_outlook_does_not() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(sample_app(dir.path(), false), vec2(1440.0, 900.0));
    settle(&mut h);
    let id = h.state().session.rows()[1].id;
    h.state_mut().session.request_focus(id);
    settle(&mut h);
    h.key_press(Key::E);
    settle(&mut h);
    assert_eq!(folder_role(h.state(), id), FolderRole::Inbox, "en Outlook la E no archiva");
    h.state_mut().session.prefs.scheme = ShortcutScheme::Gmail;
    h.key_press(Key::E);
    settle(&mut h);
    assert_eq!(folder_role(h.state(), id), FolderRole::Archive);
    h.key_press(Key::Z);
    settle(&mut h);
    assert_eq!(folder_role(h.state(), id), FolderRole::Inbox, "Z deshace en Gmail");
}

#[test]
fn modules_render_without_unnamed_controls() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(sample_app(dir.path(), true), vec2(1280.0, 800.0));
    settle(&mut h);
    for (label, expect) in [
        ("Calendario", "Semana laboral"),
        ("Contactos", "Andrés Brown"),
        ("Notas", "Notas"),
        ("Tareas", "Despacho"),
        ("Asistente", "La IA propone"),
        ("Correo (", "Entrada"),
    ] {
        h.query_all_by_label_contains(label).next().unwrap_or_else(|| panic!("riel: {label}")).click();
        settle(&mut h);
        assert!(h.query_all_by_label_contains(expect).next().is_some(), "{label}: falta «{expect}»");
    }
}

#[test]
fn welcome_screen_when_there_are_no_accounts() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    spanish();
    let dir = tempfile::tempdir().unwrap();
    let mut s = session_on(Store::open(dir.path()).unwrap(), false);
    s.prefs.theme = ThemePref::Light;
    let app = DeskApp::new(s, Options::default());
    let mut h = harness(app, vec2(1100.0, 760.0));
    settle(&mut h);
    assert!(h.query_by_label("Agrega tu cuenta de correo").is_some());
    if let Some(w) = h.state_mut().session.wizard.as_mut() {
        w.email = "laura@gmail.com".into();
    }
    h.run();
    assert!(h.state().session.wizard.as_ref().is_some_and(|w| w.can_continue()));
    // La tarjeta del proveedor (sin red) ofrece un solo camino.
    h.state_mut().demo_provider_step("laura@gmail.com");
    settle(&mut h);
    assert!(h.query_all_by_label_contains("Gmail").next().is_some());
    assert!(
        h.query_all_by_label_contains("contraseña de app").next().is_some()
            || h.query_all_by_label_contains("Continuar con Google").next().is_some()
    );
}

#[test]
fn mailto_opens_a_compose_window_and_saves_a_draft() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    spanish();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    sample::populate(&store, sample::DEFAULT_SEED, now()).unwrap();
    let app = DeskApp::new(
        session_on(store, false),
        Options { mailto: Some("mailto:ana@example.com?subject=Hola".into()), ..Default::default() },
    );
    let mut h = harness(app, vec2(1440.0, 900.0));
    settle(&mut h);
    assert!(h.query_by_label_contains("Nuevo mensaje: Hola").is_some());
    let drafts_before = h.state_mut().session.unified_unread(FolderRole::Drafts);
    let _ = drafts_before;
    h.get_by_label("Guardar borrador").click();
    settle(&mut h);
    assert!(h.query_all_by_label_contains("Borrador guardado").next().is_some());
}

#[test]
fn list_scrolling_over_100k_rows_stays_cheap() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    spanish();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let account = AccountId(1);
    store
        .put_account_label(&AccountLabel { id: account, name: "Prueba".into(), email: "p@example.com".into() })
        .unwrap();
    let inbox = store
        .put_folder(&Folder {
            account,
            name: "Entrada".into(),
            role: FolderRole::Inbox,
            remote_path: "INBOX".into(),
            ..Default::default()
        })
        .unwrap();
    let t = Instant::now();
    let mut rng = sample::Rng::new(7);
    for _ in 0..10 {
        let mut batch = sample::synthetic_messages(&mut rng, &[inbox], 10_000, now());
        for m in &mut batch {
            m.raw = None;
            m.body_text = None;
            m.envelope.account = account;
        }
        store.put_messages(batch).unwrap();
    }
    eprintln!("100 000 mensajes insertados en {:?}", t.elapsed());
    let mut h = harness(
        DeskApp::new(
            session_on(store, false),
            Options { system_fonts: false, no_theme_watch: true, ..Default::default() },
        ),
        vec2(1280.0, 800.0),
    );
    settle(&mut h);
    assert_eq!(h.state().session.rows().len(), 100_000);
    let first = h.state().session.rows()[0].id;
    // El cuerpo de la lista recibe el foco con un clic en la primera fila.
    let subject = h.state_mut().session.envelope(first).unwrap().subject.clone();
    h.query_all_by_label_contains(&subject).next().unwrap().click();
    h.run();
    assert_eq!(h.state().mail_cursor(), Some(0));
    let t = Instant::now();
    let frames = 60;
    for _ in 0..frames {
        h.key_press(Key::PageDown);
        h.run_steps(1);
    }
    let per_frame = t.elapsed() / frames;
    assert!(h.state().mail_cursor().is_some_and(|c| c > 600), "RePág avanza: {:?}", h.state().mail_cursor());
    eprintln!("RePág sobre 100 000 filas: {per_frame:?} por cuadro");
    h.key_press(Key::End);
    settle(&mut h);
    assert_eq!(h.state().mail_cursor(), Some(99_999));
    let last = h.state().session.rows()[99_999].id;
    assert_eq!(h.state().session.focused(), Some(last), "Fin llega a la última fila");
    assert!(per_frame < Duration::from_millis(250), "{per_frame:?} por cuadro");
}

#[test]
fn compose_send_lands_in_the_outbox_and_undo_reopens_it() {
    use egui::accesskit::Role;
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(sample_app(dir.path(), false), vec2(1440.0, 900.0));
    settle(&mut h);
    h.get_by_label("Nuevo mensaje").click();
    settle(&mut h);
    assert_eq!(h.state().composer_count(), 1);
    // Destinatario: al escribir la coma se vuelve burbuja.
    h.get_by_role_and_label(Role::TextInput, "Para").focus();
    h.run();
    h.event(egui::Event::Text("ana@example.com,".into()));
    settle(&mut h);
    assert!(h.query_all_by_label_contains("ana@example.com").next().is_some(), "burbuja del destinatario");
    h.get_by_role_and_label(Role::TextInput, "Asunto").focus();
    h.run();
    h.event(egui::Event::Text("Prueba desde kittest".into()));
    settle(&mut h);
    assert!(h.query_all_by_label_contains("Nuevo mensaje: Prueba desde kittest").next().is_some());
    h.get_by_label("Enviar").click();
    settle(&mut h);
    assert_eq!(h.state().composer_count(), 0, "la ventana se cierra al enviar");
    assert_eq!(h.state().session.outbox_entries().len(), 1, "espera en la bandeja de salida");
    assert!(h.query_all_by_label_contains("Enviando").next().is_some(), "aviso «Enviando… Deshacer»");
    assert!(h.query_all_by_label_contains("Bandeja de salida").next().is_some(), "aparece en los favoritos");
    h.get_by_label("Deshacer").click();
    settle(&mut h);
    assert!(h.state().session.outbox_entries().is_empty(), "deshacer lo saca de la cola");
    assert_eq!(h.state().composer_count(), 1, "y vuelve al redactor");
    assert_eq!(h.state().composer_subject().as_deref(), Some("Prueba desde kittest"));
}

#[test]
fn quoted_history_is_collapsed_in_the_reading_pane() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(sample_app(dir.path(), false), vec2(1440.0, 900.0));
    settle(&mut h);
    let id = {
        let s = &mut h.state_mut().session;
        let rows: Vec<_> = s.rows().iter().map(|r| r.id).take(300).collect();
        rows.into_iter().find(|&id| s.envelope(id).is_some_and(|e| e.in_reply_to.is_some())).unwrap()
    };
    h.state_mut().session.request_focus(id);
    settle(&mut h);
    let toggle = h.get_by_label("… mostrar texto citado");
    toggle.click();
    settle(&mut h);
    assert!(h.query_by_label("Ocultar texto citado").is_some());
}
