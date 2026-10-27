//! IureDesk — binario de escritorio.
//!
//! `iuredesk [--sample] [mailto:…]` · `iuredesk --mcp [--sample]`
//! - `--sample`: abre (y la primera vez crea) un buzón de ejemplo en la carpeta de datos.
//! - Sin cuentas ni `--sample` se muestra la bienvenida con el asistente de alta.
//! - Un argumento `mailto:` abre el redactor con los datos del enlace.
//! - `--mcp`: servidor MCP por stdio (Claude, Copilot). Si la ventana está abierta, cada llamada
//!   corre dentro de ella por el puente IPC; si no, abre el almacén solo durante la llamada.
//! - `IUREDESK_DATA_DIR=/ruta` usa esa carpeta para todo (almacén, cuentas y preferencias): sirve
//!   para probar con una cuenta real sin tocar la instalación normal.
//! - `IUREDESK_NATIVE_DECORATIONS=1` usa la barra de título del sistema (`0`, la integrada); sin
//!   la variable decide la preferencia «Barra de título del sistema».
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use iuredesk_accounts::{AccountStore, KeyringStore, MemoryStore, SecretStore};
use iuredesk_engine::{OfflineTransport, Prefs, Session, SessionConfig, Zone};
use iuredesk_ui::{DeskApp, Notifier, Options};

const HELP: &str = "IureDesk — correo, calendario, contactos, notas y tareas

Uso: iuredesk [--sample] [mailto:…]
     iuredesk --mcp [--sample]

  --sample     buzón de ejemplo (no toca tus cuentas)
  --mcp        servidor MCP por stdio (Claude Desktop, VS Code/Copilot)
  --help       esta ayuda
  mailto:…     abre el redactor con los datos del enlace

Variables:
  IUREDESK_DATA_DIR=/ruta   carpeta de datos propia (almacén, cuentas y preferencias)
  IUREDESK_NATIVE_DECORATIONS=1   barra de título del sistema (0 = la integrada)
";

/// Carpeta de datos elegida con `IUREDESK_DATA_DIR`, si hay.
fn data_override() -> Option<PathBuf> {
    std::env::var_os("IUREDESK_DATA_DIR").filter(|v| !v.is_empty()).map(PathBuf::from)
}

fn data_dir() -> PathBuf {
    data_override().unwrap_or_else(|| dirs::data_dir().unwrap_or_else(std::env::temp_dir).join("iuredesk"))
}

/// Notificaciones del escritorio (Linux: D-Bus con zbus, Rust puro). En otros sistemas, por ahora
/// solo los avisos dentro de la ventana.
#[cfg(target_os = "linux")]
fn notifier() -> Option<Notifier> {
    Some(Arc::new(|title: &str, body: &str| {
        let (t, b) = (title.to_string(), body.to_string());
        // `show` habla con el bus de sesión: en su propio hilo para no frenar la interfaz.
        let _ = std::thread::Builder::new().name("iuredesk-notify".into()).spawn(move || {
            let _ =
                notify_rust::Notification::new().appname("IureDesk").summary(&t).body(&b).icon("mail-unread").show();
        });
    }))
}

#[cfg(not(target_os = "linux"))]
fn notifier() -> Option<Notifier> {
    None
}

/// ¿Barra de título del sistema? La variable manda sobre la preferencia.
fn native_decorations(pref: bool) -> bool {
    match std::env::var("IUREDESK_NATIVE_DECORATIONS").ok().as_deref().map(str::trim) {
        Some("1" | "true" | "yes") => true,
        Some("0" | "false" | "no") => false,
        _ => pref,
    }
}

/// Orden de los botones de la ventana del escritorio (GNOME: `button-layout`) por el portal de
/// configuración XDG. Se espera como mucho 300 ms: si el bus tarda, se usa el orden de siempre.
#[cfg(target_os = "linux")]
fn button_layout() -> Option<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("iuredesk-button-layout".into())
        .spawn(move || {
            let _ = tx.send(read_button_layout());
        })
        .ok()?;
    rx.recv_timeout(std::time::Duration::from_millis(300)).ok().flatten()
}

#[cfg(target_os = "linux")]
fn read_button_layout() -> Option<String> {
    use zbus::zvariant::{OwnedValue, Value};
    let conn = zbus::blocking::Connection::session().ok()?;
    let call = |method: &str| {
        conn.call_method(
            Some("org.freedesktop.portal.Desktop"),
            "/org/freedesktop/portal/desktop",
            Some("org.freedesktop.portal.Settings"),
            method,
            &("org.gnome.desktop.wm.preferences", "button-layout"),
        )
    };
    // `ReadOne` (portales nuevos) da el valor; `Read` (viejos) lo da dentro de otra variante.
    let value: OwnedValue = match call("ReadOne") {
        Ok(m) => m.body().deserialize().ok()?,
        Err(_) => call("Read").ok()?.body().deserialize().ok()?,
    };
    let mut v: &Value = &value;
    // Variantes anidadas, con límite.
    for _ in 0..4 {
        match v {
            Value::Value(inner) => v = inner,
            Value::Str(s) => return Some(s.as_str().chars().take(256).collect()),
            _ => return None,
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
fn button_layout() -> Option<String> {
    None
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Antes de cualquier salida: en modo MCP, stdout es solo del protocolo.
    if args.first().map(String::as_str) == Some("--mcp") {
        let sample = args.iter().any(|a| a == "--sample");
        let dir = data_dir().join(if sample { "sample" } else { "store" });
        return ExitCode::from(u8::try_from(iuredesk_mcp::run_stdio(&dir).clamp(0, 255)).unwrap_or(1));
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{HELP}");
        return ExitCode::SUCCESS;
    }
    let sample = args.iter().any(|a| a == "--sample");
    let mailto = args.iter().find(|a| a.to_ascii_lowercase().starts_with("mailto:")).cloned();
    if let Some(bad) = args.iter().find(|a| a.starts_with("--") && *a != "--sample" && *a != "--mcp") {
        eprintln!("Opción desconocida: {bad}\n\n{HELP}");
        return ExitCode::from(2);
    }

    let changed = Arc::new(AtomicBool::new(false));
    let (session, bridge) = match open_session(sample, &changed) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("IureDesk no pudo abrir tus datos: {e}");
            return ExitCode::FAILURE;
        }
    };
    let native = native_decorations(session.prefs.native_decorations);
    let chrome = (!native).then(|| iuredesk_ui::WindowChrome::for_this_os(button_layout().as_deref()));
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("IureDesk")
        .with_app_id("iuredesk")
        .with_inner_size([1440.0, 900.0])
        .with_min_inner_size([880.0, 560.0]);
    if chrome.is_some() {
        // macOS: los semáforos del sistema se quedan sobre nuestra barra; en Linux y Windows la
        // barra entera es nuestra (sin decoraciones, ventana opaca).
        viewport = if cfg!(target_os = "macos") {
            viewport.with_fullsize_content_view(true).with_titlebar_shown(false).with_title_shown(false)
        } else {
            viewport.with_decorations(false).with_transparent(false)
        };
    }
    let app = DeskApp::new(
        session,
        Options {
            system_fonts: true,
            no_theme_watch: false,
            mailto,
            notifier: notifier(),
            external_change: Some(changed),
            chrome,
        },
    );

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    match eframe::run_native("IureDesk", options, Box::new(|_| Ok(Box::new(App { app, _bridge: bridge })))) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("IureDesk no pudo abrir la ventana: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Puente IPC para `iuredesk --mcp` (redb admite un solo proceso): las herramientas corren dentro
/// de la app y, si escriben, marcan `changed` para que la interfaz recargue. Si no arranca, la app
/// sigue (el MCP dirá que IureDesk está abierto).
fn bridge(store: &Arc<iuredesk_store::Store>, changed: &Arc<AtomicBool>) -> Option<iuredesk_mcp::BridgeHandle> {
    let flag = Arc::clone(changed);
    iuredesk_mcp::start_bridge(
        Arc::clone(store),
        iuredesk_mcp::BridgeOptions {
            on_change: Some(Arc::new(move |_tool: &str| flag.store(true, Ordering::Release))),
            ..Default::default()
        },
    )
    .map_err(|e| eprintln!("IureDesk: puente MCP no disponible ({e})"))
    .ok()
}

type Opened = (Session, Option<iuredesk_mcp::BridgeHandle>);

fn open_session(sample: bool, changed: &Arc<AtomicBool>) -> Result<Opened, String> {
    let dir = data_dir();
    let prefs_path = match data_override() {
        Some(d) => Some(d.join("prefs.json")),
        None => Prefs::default_path(),
    };
    if sample {
        let store = Arc::new(iuredesk_store::sample::open_sample(dir.join("sample")).map_err(|e| e.to_string())?);
        let handle = bridge(&store, changed);
        let secrets: Arc<dyn SecretStore> = Arc::new(MemoryStore::default());
        return Session::new(SessionConfig {
            store,
            accounts: None,
            secrets,
            prefs_path,
            now: None,
            // El buzón de ejemplo pone sus citas en horas de tu zona.
            zone: Zone::Local,
            sample: true,
            // Sin red: enviar deja la copia en «Enviados» sin salir del equipo.
            sync: None,
            transport: Some(Arc::new(OfflineTransport)),
        })
        .map(|s| (s, handle))
        .map_err(|e| e.text().get().to_string());
    }
    let store = Arc::new(iuredesk_store::Store::open(dir.join("store")).map_err(|e| e.to_string())?);
    let handle = bridge(&store, changed);
    let accounts = match data_override() {
        Some(d) => Some(AccountStore::at(d.join("accounts.json"))),
        None => AccountStore::open_default().ok(),
    };
    Session::new(SessionConfig {
        store,
        accounts,
        secrets: Arc::new(KeyringStore),
        prefs_path,
        now: None,
        zone: Zone::Local,
        sample: false,
        sync: Some(iuredesk_engine::iuredesk_sync::SyncOptions::default()),
        transport: None,
    })
    .map(|s| (s, handle))
    .map_err(|e| e.text().get().to_string())
}

/// La ventana y el puente MCP (vive lo que la ventana; al soltarlo se borra el punto de acceso).
struct App {
    app: DeskApp,
    _bridge: Option<iuredesk_mcp::BridgeHandle>,
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.app.ui(ui);
    }

    fn on_exit(&mut self) {
        self.app.shutdown();
    }
}
