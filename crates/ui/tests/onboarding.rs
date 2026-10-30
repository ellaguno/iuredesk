//! UX-4 con egui_kittest: «Configurar manualmente» lleno a mano en la ventana «Agregar cuenta»;
//! si «Probar conexión» falla, todo lo escrito sigue ahí (ni Esc ni un clic fuera lo borran), se
//! corrige y se vuelve a probar, y «Atrás» con la misma dirección devuelve el formulario.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{TimeZone, Utc};
use egui::accesskit::Role;
use egui::{Key, Modifiers, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use iuredesk_accounts::MemoryStore;
use iuredesk_engine::onboarding::{ProbeStatus, WizardStep};
use iuredesk_engine::{OfflineTransport, Session, SessionConfig, ThemePref, Zone};
use iuredesk_model::account::{Protocol, Security};
use iuredesk_store::{Store, sample};
use iuredesk_ui::{DeskApp, Options};

static GPU: Mutex<()> = Mutex::new(());

const EMAIL: &str = "ellaguno@confiabilidad.com.mx";

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
    Harness::builder().with_size(vec2(1440.0, 1000.0)).build_ui_state(
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

/// Corre cuadros hasta que termine la prueba de conexión.
fn wait_test(h: &mut Harness<'_, DeskApp>) {
    let start = Instant::now();
    loop {
        h.run_steps(1);
        let testing = h.state().session.wizard.as_ref().and_then(|w| w.manual.as_ref()).is_some_and(|m| m.testing());
        if !testing {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(30), "la prueba no terminó");
        std::thread::sleep(Duration::from_millis(10));
    }
    settle(h);
}

/// Puerto local sin nadie escuchando: la conexión se rechaza al instante.
fn closed_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// Reemplaza el texto del campo `nth` con nombre `label` (los dos servidores repiten nombres).
fn type_into(h: &mut Harness<'_, DeskApp>, label: &str, nth: usize, text: &str) {
    let role = if label.starts_with("Contraseña") { Role::PasswordInput } else { Role::TextInput };
    h.query_all_by_role_and_label(role, label)
        .nth(nth)
        .unwrap_or_else(|| panic!("no hay campo «{label}» #{nth}"))
        .scroll_to_me();
    h.run();
    h.query_all_by_role_and_label(role, label)
        .nth(nth)
        .unwrap_or_else(|| panic!("no hay campo «{label}» #{nth}"))
        .focus();
    h.run();
    h.key_press_modifiers(Modifiers::COMMAND, Key::A);
    h.event(egui::Event::Text(text.into()));
    h.run();
}

/// Lleva a la vista (el formulario se desplaza) y pulsa.
fn click(h: &mut Harness<'_, DeskApp>, label: &str, nth: usize) {
    h.query_all_by_label(label).nth(nth).unwrap_or_else(|| panic!("no hay «{label}» #{nth}")).scroll_to_me();
    h.run();
    h.query_all_by_label(label).nth(nth).unwrap_or_else(|| panic!("no hay «{label}» #{nth}")).click();
    settle(h);
}

fn port_into(h: &mut Harness<'_, DeskApp>, nth: usize, port: u16) {
    h.query_all_by_role_and_label(Role::SpinButton, "Puerto").nth(nth).unwrap().focus();
    h.run();
    h.key_press_modifiers(Modifiers::COMMAND, Key::A);
    h.event(egui::Event::Text(port.to_string()));
    h.key_press(Key::Enter);
    h.run();
}

fn wizard<'a>(h: &'a Harness<'_, DeskApp>) -> &'a iuredesk_engine::onboarding::Wizard {
    h.state().session.wizard.as_ref().expect("el asistente sigue abierto")
}

#[test]
fn failed_manual_test_keeps_the_form_and_lets_you_retry() {
    let _g = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(app(dir.path()));
    settle(&mut h);
    h.state_mut().demo_email_step(EMAIL, "Eduardo");
    settle(&mut h);
    h.get_by_label("Configurar manualmente").click();
    settle(&mut h);
    assert_eq!(wizard(&h).step, WizardStep::Manual);

    // Se llena todo a mano.
    let (pin, pout) = (closed_port(), closed_port());
    h.get_by_label("POP3").click();
    settle(&mut h);
    for nth in [1, 0] {
        click(&mut h, "Ninguna", nth);
    }
    type_into(&mut h, "Servidor", 0, "127.0.0.1");
    type_into(&mut h, "Servidor", 1, "127.0.0.1");
    port_into(&mut h, 0, pin);
    port_into(&mut h, 1, pout);
    type_into(&mut h, "Usuario", 0, "ellaguno");
    type_into(&mut h, "Usuario", 1, "ellaguno-smtp");
    click(&mut h, "Usar la misma contraseña", 0);
    type_into(&mut h, "Contraseña de salida", 0, "otra");
    type_into(&mut h, "Contraseña", 0, "secreto");
    settle(&mut h);
    let typed = wizard(&h).manual.clone().unwrap();
    assert_eq!(typed.incoming.protocol, Protocol::Pop3);
    assert_eq!((typed.incoming.host.as_str(), typed.incoming.port), ("127.0.0.1", pin));
    assert_eq!((typed.outgoing.host.as_str(), typed.outgoing.port), ("127.0.0.1", pout));
    assert_eq!((typed.incoming.security, typed.outgoing.security), (Security::Plain, Security::Plain));
    assert_eq!((typed.incoming.username.as_str(), typed.outgoing.username.as_str()), ("ellaguno", "ellaguno-smtp"));
    assert_eq!(typed.outgoing_password, "otra");
    assert_eq!(wizard(&h).password, "secreto");

    h.get_by_label("Probar conexión").click();
    wait_test(&mut h);
    let check = |h: &Harness<'_, DeskApp>| {
        let w = wizard(h);
        assert_eq!(w.step, WizardStep::Manual, "se queda en el formulario");
        assert_eq!(w.password, "secreto");
        let m = w.manual.as_ref().unwrap();
        assert!(matches!(m.incoming_test, Some(ProbeStatus::Failed(_))), "{:?}", m.incoming_test);
        assert!(matches!(m.outgoing_test, Some(ProbeStatus::Failed(_))), "{:?}", m.outgoing_test);
        let mut kept = m.clone();
        kept.incoming_test = None;
        kept.outgoing_test = None;
        let mut want = typed.clone();
        want.incoming.username = kept.incoming.username.clone();
        assert_eq!(kept, want);
    };
    check(&h);
    // Cada sección dice qué falló, con el aviso de arriba.
    assert!(h.query_by_label("Corrige lo que se marca abajo y vuelve a probar.").is_some());
    assert_eq!(h.query_all_by_role_and_label(Role::TextInput, "Servidor").count(), 2, "el formulario sigue a la vista");

    // Esc (que también suelta el foco de un campo) y un clic fuera no borran lo escrito.
    type_into(&mut h, "Usuario", 0, "ellaguno");
    h.key_press(Key::Escape);
    settle(&mut h);
    h.hover_at(egui::pos2(5.0, 500.0));
    h.run();
    h.event(egui::Event::PointerButton {
        pos: egui::pos2(5.0, 500.0),
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    h.event(egui::Event::PointerButton {
        pos: egui::pos2(5.0, 500.0),
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    settle(&mut h);
    check(&h);

    // Se corrige y se vuelve a probar.
    type_into(&mut h, "Usuario", 0, EMAIL);
    settle(&mut h);
    assert_eq!(wizard(&h).manual.as_ref().unwrap().incoming.username, EMAIL);
    assert!(wizard(&h).manual.as_ref().unwrap().incoming_test.is_none(), "al editar se limpia su resultado");
    h.get_by_label("Probar conexión").click();
    h.run_steps(1);
    wait_test(&mut h);
    check(&h);
    assert_eq!(wizard(&h).manual.as_ref().unwrap().incoming.username, EMAIL);

    // «Atrás» y de vuelta con la misma dirección: el formulario está tal cual.
    h.get_by_label("Atrás").click();
    settle(&mut h);
    assert_eq!(wizard(&h).step, WizardStep::Email);
    h.get_by_label("Configurar manualmente").click();
    settle(&mut h);
    let w = wizard(&h);
    assert_eq!(w.step, WizardStep::Manual);
    assert_eq!(w.password, "secreto");
    let m = w.manual.as_ref().unwrap();
    assert_eq!((m.incoming.port, m.outgoing.port, m.incoming.username.as_str()), (pin, pout, EMAIL));
    assert!(w.can_continue(), "se puede volver a probar");

    // Otra dirección: formulario nuevo.
    h.get_by_label("Atrás").click();
    settle(&mut h);
    type_into(&mut h, "Correo electrónico", 0, "ana@otro.example");
    h.get_by_label("Configurar manualmente").click();
    settle(&mut h);
    let w = wizard(&h);
    assert!(w.password.is_empty());
    assert_eq!(w.manual.as_ref().unwrap().incoming.host, "mail.otro.example");
}
