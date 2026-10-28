//! UX-1: borrar contactos (selección múltiple + Supr → confirmación → borrados → deshacer) y
//! «Ajustes → Cuentas» (primera sección, filas con estado y acciones, editor de la cuenta).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{TimeZone, Utc};
use egui::{Key, Modifiers, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use iuredesk_accounts::{AccountStore, MemoryStore};
use iuredesk_engine::{Module, OfflineTransport, Session, SessionConfig, ThemePref, Zone};
use iuredesk_model::account::{Account, AccountKind, AuthMethod, Protocol, Security, ServerConfig};
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

fn imap_account() -> Account {
    let server = |protocol, host: &str, port| ServerConfig {
        protocol,
        host: host.into(),
        port,
        security: Security::Tls,
        auth: AuthMethod::Password,
        username: "laura@despacho.example.org".into(),
    };
    Account {
        id: "imap-laura".into(),
        display_name: "Laura Méndez".into(),
        email: "laura@despacho.example.org".into(),
        kind: AccountKind::Generic,
        incoming: server(Protocol::Imap, "imap.despacho.example.org", 993),
        outgoing: server(Protocol::Smtp, "smtp.despacho.example.org", 465),
        caldav_url: None,
        carddav_url: None,
        identities: Vec::new(),
    }
}

fn app(dir: &std::path::Path, accounts: Option<AccountStore>) -> DeskApp {
    spanish();
    let store = Store::open(dir.join("store")).unwrap();
    sample::populate(&store, sample::DEFAULT_SEED, now()).unwrap();
    let mut s = Session::new(SessionConfig {
        store: Arc::new(store),
        accounts,
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
fn contacts_multi_select_delete_confirm_and_undo() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app(dir.path(), None));
    settle(&mut h);
    h.state_mut().session.module = Module::Contacts;
    settle(&mut h);
    let before = h.state_mut().session.contacts().len();
    assert!(before >= 10);
    let first = h.state_mut().session.contacts()[0].full_name.clone();
    // Clic en la primera fila y Mayús+↓ dos veces: tres seleccionados.
    h.query_all_by_label_contains(&first).next().expect("fila del primer contacto").click();
    settle(&mut h);
    h.key_press_modifiers(Modifiers::SHIFT, Key::ArrowDown);
    h.run();
    h.key_press_modifiers(Modifiers::SHIFT, Key::ArrowDown);
    settle(&mut h);
    assert_eq!(h.state().session.contact_selection().len(), 3, "tres contactos seleccionados");

    // Supr: con más de uno pide confirmación (no borra todavía).
    h.key_press(Key::Delete);
    settle(&mut h);
    assert_eq!(h.state_mut().session.contacts().len(), before, "sin confirmar no se borra");
    h.get_by_label("Eliminar 3").click();
    settle(&mut h);
    assert_eq!(h.state_mut().session.contacts().len(), before - 3);
    assert!(!h.state_mut().session.contacts().iter().any(|c| c.full_name == first));
    assert!(h.query_all_by_label_contains("Se eliminaron 3 contactos").next().is_some(), "aviso con deshacer");

    // Deshacer los devuelve.
    h.get_by_label("Deshacer").click();
    settle(&mut h);
    assert_eq!(h.state_mut().session.contacts().len(), before);
    assert!(h.state_mut().session.contacts().iter().any(|c| c.full_name == first));

    // Uno solo se borra sin preguntar.
    let one = h.state_mut().session.contacts()[1].id;
    h.state_mut().session.set_contact_selection(vec![one]);
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, "contacts.delete");
    settle(&mut h);
    assert_eq!(h.state_mut().session.contacts().len(), before - 1);
}

#[test]
fn empty_contacts_get_a_name_or_are_hidden() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut a = app(dir.path(), None);
    // Un contacto vacío (como los que llegaban de Graph) y uno con solo un teléfono.
    let st = a.session.store().clone();
    let book = st.address_books().unwrap()[0].id;
    st.put_contact(&iuredesk_model::Contact { address_book: book, uid: "vacio".into(), ..Default::default() }).unwrap();
    st.put_contact(&iuredesk_model::Contact {
        address_book: book,
        uid: "tel".into(),
        phones: vec![iuredesk_model::Labeled::new("cell", "+52 55 0000 1111")],
        ..Default::default()
    })
    .unwrap();
    a.session.module = Module::Contacts;
    let mut h = harness(a);
    settle(&mut h);
    assert!(h.query_all_by_label_contains("+52 55 0000 1111").next().is_some(), "el teléfono como nombre");
    assert!(h.query_by_label("(Sin nombre)").is_none(), "el vacío se oculta");
    h.get_by_label("Mostrar contactos vacíos (1)").click();
    settle(&mut h);
    assert!(h.query_all_by_label_contains("(Sin nombre)").next().is_some(), "con el interruptor se ve");
}

#[test]
fn settings_open_on_accounts_with_status_actions_and_editor() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let accounts = AccountStore::at(dir.path().join("accounts.json"));
    accounts.upsert(imap_account()).unwrap();
    let mut h = harness(app(dir.path(), Some(accounts)));
    settle(&mut h);
    h.get_by_label("Apariencia").click();
    h.run();
    h.get_by_label("Ajustes…").click();
    settle(&mut h);
    // «Cuentas» es la primera sección y la que se abre.
    let sections: Vec<String> = ["Cuentas", "General", "Apariencia", "Correo", "Integraciones"]
        .iter()
        .filter(|l| h.query_all_by_label_contains(l).next().is_some())
        .map(|s| s.to_string())
        .collect();
    assert!(sections.contains(&"Cuentas".to_string()), "{sections:?}");
    assert!(h.query_all_by_label("laura@despacho.example.org").next().is_some(), "fila de la cuenta");
    assert!(h.query_all_by_label("IMAP").next().is_some(), "tipo de cuenta");
    for b in ["Editar…", "Volver a iniciar sesión…", "Sincronizar ahora", "Eliminar cuenta…"] {
        assert!(h.query_by_label(b).is_some(), "falta «{b}»");
    }
    // Editar abre el editor con los servidores y «Probar conexión».
    h.get_by_label("Editar…").click();
    settle(&mut h);
    assert!(h.query_all_by_label("Editar cuenta").next().is_some(), "ventana del editor");
    assert!(h.query_by_label("Probar conexión").is_some());
    assert!(h.query_all_by_label("Servidor").next().is_some());
    // «Eliminar cuenta…» explica qué se borra antes de confirmar.
    // El modal termina de medirse en un par de cuadros más.
    h.run_steps(4);
    h.get_by_label("Cancelar").click();
    settle(&mut h);
    h.get_by_label("Eliminar cuenta…").click();
    settle(&mut h);
    assert!(h.query_all_by_label_contains("En el servidor no se borra nada").next().is_some());
    h.get_by_label("Sí, eliminar").click();
    settle(&mut h);
    assert!(h.state().session.account("imap-laura").is_none(), "cuenta quitada");
}
