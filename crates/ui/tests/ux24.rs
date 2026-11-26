//! UX-24: las personas como objetos. El remitente y los destinatarios del lector abren una
//! tarjeta (al pasar el ratón o con un clic) con sus datos, «copiar» y acciones: ver sus correos,
//! seleccionarlos en la lista, escribirle, agregarla a Contactos.

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
use std::collections::HashMap;

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

/// Un remitente con varios correos en la lista y el primero de ellos.
fn busy_sender(h: &mut Harness<'_, DeskApp>) -> (String, usize, MessageId) {
    let ids: Vec<MessageId> = h.state().session.rows_shared().iter().map(|r| r.id).collect();
    let s = &mut h.state_mut().session;
    let mut by: HashMap<String, (usize, MessageId)> = HashMap::new();
    for id in ids {
        let a = s.envelope(id).unwrap().from[0].addr.clone();
        by.entry(a).or_insert((0, id)).0 += 1;
    }
    let (addr, (n, first)) = by.into_iter().max_by_key(|(_, (n, _))| *n).unwrap();
    assert!(n > 1);
    (addr, n, first)
}

fn open(h: &mut Harness<'_, DeskApp>, id: MessageId) {
    h.state_mut().session.request_focus(id);
    settle(h);
    assert_eq!(h.state().session.focused(), Some(id));
}

/// Abre la tarjeta del remitente con un clic en su nombre.
fn click_card(h: &mut Harness<'_, DeskApp>) {
    h.get_all_by_label_contains(CARD).next().unwrap().click();
    settle(h);
}

const CARD: &str = "Tarjeta de contacto de";

#[test]
fn the_sender_card_copies_and_selects_their_messages() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app(dir.path()));
    settle(&mut h);
    let (addr, n, first) = busy_sender(&mut h);
    open(&mut h, first);

    // Al pasar el ratón sale la tarjeta flotante con «copiar».
    let copy = format!("Copiar {addr}");
    assert!(h.query_by_label(&copy).is_none());
    h.get_all_by_label_contains(CARD).next().unwrap().hover();
    for _ in 0..90 {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(10));
        if h.query_by_label(&copy).is_some() {
            break;
        }
    }
    assert!(h.query_by_label(&copy).is_some(), "la tarjeta aparece al pasar el ratón");

    // Con un clic queda fija; «copiar» deja la dirección en el portapapeles.
    click_card(&mut h);
    h.get_by_label(&copy).click();
    h.run_steps(1);
    let copied =
        h.output().platform_output.commands.iter().any(|c| matches!(c, egui::OutputCommand::CopyText(t) if t == &addr));
    settle(&mut h);
    assert!(copied || h.query_all_by_label_contains(&format!("Copiado: {addr}")).next().is_some());
    assert_eq!(h.state().session.focused(), Some(first), "el clic en el nombre no pliega ni cambia el mensaje");

    // «Seleccionar»: sus correos de la lista quedan marcados (con la barra en lote).
    click_card(&mut h);
    h.get_by_label("Seleccionar").click();
    settle(&mut h);
    assert_eq!(h.state().session.selection().len(), n);
    assert!(h.query_by_label(&format!("{n} seleccionados")).is_some(), "la barra en lote aparece");
    h.key_press(egui::Key::Escape);
    settle(&mut h);

    // «Sus correos»: búsqueda `de:` (también en el campo de la barra).
    open(&mut h, first);
    click_card(&mut h);
    h.get_by_label("Sus correos").click();
    settle(&mut h);
    assert_eq!(h.state().session.search_query(), format!("de:{addr}"));
    assert_eq!(h.state().mail_search(), format!("de:{addr}"));
}

#[test]
fn add_a_sender_to_contacts_from_the_card() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app(dir.path()));
    settle(&mut h);
    // Un remitente que no está en Contactos.
    let ids: Vec<MessageId> = h.state().session.rows_shared().iter().map(|r| r.id).collect();
    let id = ids
        .into_iter()
        .find(|id| {
            let s = &mut h.state_mut().session;
            let a = s.envelope(*id).unwrap().from[0].addr.clone();
            s.contact_by_email(&a).is_none()
        })
        .unwrap();
    open(&mut h, id);
    click_card(&mut h);
    h.get_by_label("Agregar a contactos").click();
    settle(&mut h);
    assert!(h.state().contact_editor_open(), "se abre el editor con su correo");
}
