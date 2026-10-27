//! IureDesk — binario de escritorio.
//!
//! `iuredesk [--sample] [mailto:…]`
//! - `--sample`: abre (y la primera vez crea) un buzón de ejemplo en la carpeta de datos.
//! - Sin cuentas ni `--sample` se muestra la bienvenida con el asistente de alta.
//! - Un argumento `mailto:` abre el redactor con los datos del enlace.
//! - `IUREDESK_DATA_DIR=/ruta` usa esa carpeta para todo (almacén, cuentas y preferencias): sirve
//!   para probar con una cuenta real sin tocar la instalación normal.
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

use iuredesk_accounts::{AccountStore, KeyringStore, MemoryStore, SecretStore};
use iuredesk_engine::{OfflineTransport, Prefs, Session, SessionConfig, Zone};
use iuredesk_ui::{DeskApp, Notifier, Options};

const HELP: &str = "IureDesk — correo, calendario, contactos, notas y tareas

Uso: iuredesk [--sample] [mailto:…]

  --sample     buzón de ejemplo (no toca tus cuentas)
  --help       esta ayuda
  mailto:…     abre el redactor con los datos del enlace

Variables:
  IUREDESK_DATA_DIR=/ruta   carpeta de datos propia (almacén, cuentas y preferencias)
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

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{HELP}");
        return ExitCode::SUCCESS;
    }
    let sample = args.iter().any(|a| a == "--sample");
    let mailto = args.iter().find(|a| a.to_ascii_lowercase().starts_with("mailto:")).cloned();
    if let Some(bad) = args.iter().find(|a| a.starts_with("--") && *a != "--sample") {
        eprintln!("Opción desconocida: {bad}\n\n{HELP}");
        return ExitCode::from(2);
    }

    let session = match open_session(sample) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("IureDesk no pudo abrir tus datos: {e}");
            return ExitCode::FAILURE;
        }
    };
    let app =
        DeskApp::new(session, Options { system_fonts: true, no_theme_watch: false, mailto, notifier: notifier() });

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("IureDesk")
            .with_app_id("iuredesk")
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([880.0, 560.0]),
        ..Default::default()
    };
    match eframe::run_native("IureDesk", options, Box::new(|_| Ok(Box::new(App(app))))) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("IureDesk no pudo abrir la ventana: {e}");
            ExitCode::FAILURE
        }
    }
}

fn open_session(sample: bool) -> Result<Session, String> {
    let dir = data_dir();
    let prefs_path = match data_override() {
        Some(d) => Some(d.join("prefs.json")),
        None => Prefs::default_path(),
    };
    if sample {
        let store = iuredesk_store::sample::open_sample(dir.join("sample")).map_err(|e| e.to_string())?;
        let secrets: Arc<dyn SecretStore> = Arc::new(MemoryStore::default());
        return Session::new(SessionConfig {
            store: Arc::new(store),
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
        .map_err(|e| e.text().get().to_string());
    }
    let store = iuredesk_store::Store::open(dir.join("store")).map_err(|e| e.to_string())?;
    let accounts = match data_override() {
        Some(d) => Some(AccountStore::at(d.join("accounts.json"))),
        None => AccountStore::open_default().ok(),
    };
    Session::new(SessionConfig {
        store: Arc::new(store),
        accounts,
        secrets: Arc::new(KeyringStore),
        prefs_path,
        now: None,
        zone: Zone::Local,
        sample: false,
        sync: Some(iuredesk_engine::iuredesk_sync::SyncOptions::default()),
        transport: None,
    })
    .map_err(|e| e.text().get().to_string())
}

struct App(DeskApp);

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.0.ui(ui);
    }

    fn on_exit(&mut self) {
        self.0.shutdown();
    }
}
