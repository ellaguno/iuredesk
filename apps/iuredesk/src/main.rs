//! IureDesk — binario de escritorio.
//!
//! `iuredesk [--sample] [--minimized] [mailto:… | archivo.eml|.ics|.vcf | iuredesk://…]` ·
//! `iuredesk --mcp [--sample]`
//! - `--sample`: abre (y la primera vez crea) un buzón de ejemplo en la carpeta de datos.
//! - Sin cuentas ni `--sample` se muestra la bienvenida con el asistente de alta.
//! - Un argumento `mailto:` abre el redactor con los datos del enlace; un `.eml`, `.ics` o `.vcf`
//!   se muestra en su ventana (R1.2); `iuredesk://message/<Message-ID>`, `…/event/<UID>` y
//!   `…/compose?to=…` llevan al mensaje, al evento o al redactor.
//! - **Una sola instancia** (R1.2): si IureDesk ya está abierto con el mismo almacén, se le pasan
//!   los argumentos por el puente IPC (que trae su ventana al frente) y este proceso termina.
//! - `--minimized`: arranca minimizado (lo usa «Abrir IureDesk al iniciar sesión»).
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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use iuredesk_accounts::{AccountStore, KeyringStore, MemoryStore, SecretStore};
use iuredesk_engine::{OfflineTransport, Prefs, Session, SessionConfig, Zone};
use iuredesk_ui::{DeskApp, Notifier, Options};

const HELP: &str = "IureDesk — correo, calendario, contactos, notas y tareas

Uso: iuredesk [--sample] [--minimized] [mailto:… | archivo | iuredesk://…]
     iuredesk --mcp [--sample]

  --sample      buzón de ejemplo (no toca tus cuentas)
  --minimized   arranca minimizado
  --mcp         servidor MCP por stdio (Claude Desktop, VS Code/Copilot)
  --help        esta ayuda
  mailto:…      abre el redactor con los datos del enlace
  archivo       .eml (mensaje), .ics (evento o invitación), .vcf (contactos)
  iuredesk://message/<Message-ID> · iuredesk://event/<UID> · iuredesk://compose?to=…

Si IureDesk ya está abierto, le pasa lo pedido y lo trae al frente.

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

/// Lo que llega con la ventana abierta (otra instancia, clic en una notificación) y cómo
/// despertar a la interfaz para que lo atienda.
#[derive(Clone, Default)]
struct Wake {
    incoming: iuredesk_ui::Incoming,
    ctx: Arc<OnceLock<egui::Context>>,
}

impl Wake {
    fn push(&self, args: Vec<String>) {
        if let Ok(mut q) = self.incoming.lock() {
            // Acotado: una ráfaga de peticiones no crece sin límite.
            if q.len() < 64 {
                q.push(args);
            }
        }
        if let Some(ctx) = self.ctx.get() {
            ctx.request_repaint();
        }
    }
}

/// Notificaciones del sistema (Linux: D-Bus con zbus; Windows: avisos de WinRT; macOS:
/// UserNotifications). El clic abre el mensaje o el evento en esta ventana.
fn notifier(wake: &Wake) -> Option<Notifier> {
    let w = wake.clone();
    let on_click: iuredesk_desktop::notify::OnClick =
        Arc::new(move |link: Option<String>| w.push(link.into_iter().collect()));
    iuredesk_desktop::notify::system_notifier(iuredesk_engine::pick("Open", "Abrir"), on_click)
}

/// Icono de la ventana (X11, Windows y macOS; en Wayland el escritorio lo toma del `.desktop` por
/// el `app_id`).
fn window_icon() -> Option<egui::IconData> {
    const PNG: &[u8] = include_bytes!("../../../assets/icons/iuredesk-128.png");
    let mut dec = png::Decoder::new(std::io::Cursor::new(PNG));
    dec.set_transformations(png::Transformations::EXPAND);
    let mut reader = dec.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    buf.truncate(info.buffer_size());
    if info.bit_depth != png::BitDepth::Eight {
        return None;
    }
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        _ => return None,
    };
    Some(egui::IconData { rgba, width: info.width, height: info.height })
}

/// La línea de órdenes ya interpretada.
#[derive(Debug, Default, PartialEq, Eq)]
struct Cli {
    sample: bool,
    minimized: bool,
    help: bool,
    /// Lo que se abre, con rutas absolutas (se puede reenviar a otra instancia).
    targets: Vec<String>,
    /// Primera opción desconocida.
    unknown: Option<String>,
    /// Argumentos que no se saben abrir (se avisan y se ignoran).
    ignored: Vec<String>,
}

fn parse_cli(args: &[String], cwd: Option<&std::path::Path>) -> Cli {
    let mut cli = Cli::default();
    for a in args.iter().take(256) {
        match a.as_str() {
            "--sample" => cli.sample = true,
            "--minimized" => cli.minimized = true,
            "--help" | "-h" => cli.help = true,
            // macOS antiguo pasa el número de proceso al abrir desde el Finder.
            x if x.starts_with("-psn_") => {}
            x if x.starts_with("--") => {
                if cli.unknown.is_none() {
                    cli.unknown = Some(x.to_string());
                }
            }
            x => match iuredesk_engine::open::parse_target(x, cwd) {
                Some(t) if cli.targets.len() < iuredesk_mcp::bridge::MAX_OPEN_ARGS => cli.targets.push(t.to_arg()),
                Some(_) => {}
                None => cli.ignored.push(x.chars().take(200).collect()),
            },
        }
    }
    cli
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
    let cwd = std::env::current_dir().ok();
    let cli = parse_cli(&args, cwd.as_deref());
    if cli.help {
        print!("{HELP}");
        return ExitCode::SUCCESS;
    }
    if let Some(bad) = &cli.unknown {
        eprintln!("Opción desconocida: {bad}\n\n{HELP}");
        return ExitCode::from(2);
    }
    for x in &cli.ignored {
        eprintln!("IureDesk no sabe abrir «{x}» (se ignora).");
    }
    // Una sola instancia: si ya hay una ventana con este almacén, que lo atienda ella.
    let store_dir = data_dir().join(if cli.sample { "sample" } else { "store" });
    if let Some(client) = iuredesk_mcp::BridgeClient::discover(&store_dir, None) {
        match client.open(&cli.targets) {
            Ok(()) => return ExitCode::SUCCESS,
            // No contesta (se cerró mal): arrancar normalmente.
            Err(iuredesk_mcp::bridge::BridgeCallError::Unreachable(_)) => {}
            Err(e) => eprintln!("IureDesk ya está abierto pero no aceptó la petición: {e}"),
        }
    }

    let changed = Arc::new(AtomicBool::new(false));
    let wake = Wake::default();
    let (mut session, bridge) = match open_session(cli.sample, &changed, &wake) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("IureDesk no pudo abrir tus datos: {e}");
            return ExitCode::FAILURE;
        }
    };
    // «Mi día» o donde se dejó la última vez (Ajustes → General).
    session.open_at_start();
    let native = native_decorations(session.prefs.native_decorations);
    let chrome = (!native).then(|| iuredesk_ui::WindowChrome::for_this_os(button_layout().as_deref()));
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("IureDesk")
        .with_app_id("iuredesk")
        .with_inner_size([1440.0, 900.0])
        .with_min_inner_size([880.0, 560.0]);
    if let Some(icon) = window_icon() {
        viewport = viewport.with_icon(Arc::new(icon));
    }
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
            mailto: None,
            notifier: notifier(&wake),
            external_change: Some(changed),
            chrome,
            open: cli.targets,
            incoming: Some(Arc::clone(&wake.incoming)),
        },
    );

    let options = eframe::NativeOptions { viewport, ..Default::default() };
    let minimize = cli.minimized;
    let ctx_slot = Arc::clone(&wake.ctx);
    let creator = move |cc: &eframe::CreationContext<'_>| -> Result<Box<dyn eframe::App>, _> {
        let _ = ctx_slot.set(cc.egui_ctx.clone());
        Ok(Box::new(App { app, _bridge: bridge, minimize }))
    };
    match eframe::run_native("IureDesk", options, Box::new(creator)) {
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
fn bridge(
    store: &Arc<iuredesk_store::Store>,
    changed: &Arc<AtomicBool>,
    wake: &Wake,
) -> Option<iuredesk_mcp::BridgeHandle> {
    let flag = Arc::clone(changed);
    let w = wake.clone();
    iuredesk_mcp::start_bridge(
        Arc::clone(store),
        iuredesk_mcp::BridgeOptions {
            on_change: Some(Arc::new(move |_tool: &str| flag.store(true, Ordering::Release))),
            // R1.2: otra instancia nos pasa sus archivos y enlaces.
            on_open: Some(Arc::new(move |args: Vec<String>| w.push(args))),
            ..Default::default()
        },
    )
    .map_err(|e| eprintln!("IureDesk: puente MCP no disponible ({e})"))
    .ok()
}

type Opened = (Session, Option<iuredesk_mcp::BridgeHandle>);

fn open_session(sample: bool, changed: &Arc<AtomicBool>, wake: &Wake) -> Result<Opened, String> {
    let dir = data_dir();
    let prefs_path = match data_override() {
        Some(d) => Some(d.join("prefs.json")),
        None => Prefs::default_path(),
    };
    if sample {
        let store = Arc::new(iuredesk_store::sample::open_sample(dir.join("sample")).map_err(|e| e.to_string())?);
        let handle = bridge(&store, changed, wake);
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
    let handle = bridge(&store, changed, wake);
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
    /// `--minimized`: minimizar en el primer cuadro.
    minimize: bool,
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if std::mem::take(&mut self.minimize) {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        }
        self.app.ui(ui);
    }

    fn on_exit(&mut self) {
        self.app.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn linea_de_ordenes() {
        let cwd = std::path::Path::new("/home/ana/Descargas");
        let cli = parse_cli(
            &args(&["--sample", "factura.eml", "mailto:luis@example.com", "-psn_0_1234", "notas.txt", "--minimized"]),
            Some(cwd),
        );
        assert!(cli.sample && cli.minimized && !cli.help);
        assert_eq!(cli.unknown, None);
        assert_eq!(
            cli.targets,
            vec![cwd.join("factura.eml").to_string_lossy().into_owned(), "mailto:luis@example.com".to_string()]
        );
        assert_eq!(cli.ignored, vec!["notas.txt".to_string()]);
        let cli = parse_cli(&args(&["--borrar", "--otra"]), None);
        assert_eq!(cli.unknown.as_deref(), Some("--borrar"));
        let cli = parse_cli(&args(&["iuredesk://message/%3Ca%40b%3E", "file:///tmp/x.vcf"]), None);
        assert_eq!(cli.targets, vec!["iuredesk://message/a@b".to_string(), "/tmp/x.vcf".to_string()]);
        // Acotado.
        let many: Vec<String> = (0..500).map(|i| format!("/tmp/{i}.ics")).collect();
        assert_eq!(parse_cli(&many, None).targets.len(), iuredesk_mcp::bridge::MAX_OPEN_ARGS);
    }

    #[test]
    fn icono_de_la_ventana() {
        let icon = window_icon().unwrap();
        assert_eq!((icon.width, icon.height), (128, 128));
        assert_eq!(icon.rgba.len(), 128 * 128 * 4);
    }
}
