//! «Abrir IureDesk al iniciar sesión» (opcionalmente minimizado):
//! - Linux: `$XDG_CONFIG_HOME/autostart/iuredesk.desktop` (XDG Autostart; GNOME, KDE, XFCE…).
//! - Windows: valor `IureDesk` en `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.
//!   Instalada desde Microsoft Store (MSIX) la clave `Run` no sirve: se usa la StartupTask
//!   [`crate::store::STARTUP_TASK`] del paquete, que el usuario también ve y apaga en
//!   Configuración → Aplicaciones → Inicio. La StartupTask no pasa argumentos: «minimizado» se
//!   recuerda en [`MINIMIZED_MARKER`] y lo lee [`start_minimized_by_system`].
//! - macOS: `~/Library/LaunchAgents/com.iurefficient.IureDesk.plist` (`RunAtLoad`).
//!
//! El contenido se arma con funciones puras ([`desktop_entry`], [`launch_agent`],
//! [`run_command`]) que se prueban en todos los sistemas.

use std::path::{Path, PathBuf};

use crate::{APP_ID, DesktopError, MINIMIZED_ARG};

/// Nombre del archivo de Linux.
pub const DESKTOP_FILE: &str = "iuredesk.desktop";
/// Valor del registro de Windows.
pub const RUN_VALUE: &str = "IureDesk";
/// Clave del registro de Windows (bajo `HKEY_CURRENT_USER`).
pub const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// Archivo (en la carpeta de configuración de IureDesk, la del paquete si está empaquetada)
/// que indica que la StartupTask debe abrir IureDesk minimizado.
pub const MINIMIZED_MARKER: &str = "autostart-minimized";

/// Estado actual.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct State {
    pub enabled: bool,
    pub minimized: bool,
}

/// Ejecutable que debe arrancar la sesión: el AppImage si se corre desde uno (su ruta real; el
/// binario de dentro vive en un montaje temporal), si no el ejecutable actual.
pub fn current_program() -> Option<PathBuf> {
    if cfg!(target_os = "linux")
        && let Some(p) = std::env::var_os("APPIMAGE").filter(|p| !p.is_empty()).map(PathBuf::from)
        && p.is_absolute()
    {
        return Some(p);
    }
    std::env::current_exe().ok()
}

/// Argumento de `Exec=` según la especificación de entradas de escritorio: entre comillas, con
/// `"`, `` ` ``, `$` y `\` escapados; `%` se dobla.
fn exec_quote(s: &str) -> String {
    let mut out = String::from("\"");
    for ch in s.chars() {
        match ch {
            '"' | '`' | '$' | '\\' => {
                out.push('\\');
                out.push(ch);
            }
            '%' => out.push_str("%%"),
            '\n' | '\r' => {}
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Entrada XDG Autostart.
pub fn desktop_entry(program: &Path, minimized: bool) -> String {
    let mut exec = exec_quote(&program.to_string_lossy());
    if minimized {
        exec.push(' ');
        exec.push_str(MINIMIZED_ARG);
    }
    format!(
        "[Desktop Entry]\nType=Application\nName=IureDesk\nComment=Mail, calendar, contacts, notes and tasks\n\
         Comment[es]=Correo, calendario, contactos, notas y tareas\nExec={exec}\nIcon=iuredesk\nTerminal=false\n\
         X-GNOME-Autostart-enabled=true\nX-IureDesk-Minimized={minimized}\n"
    )
}

fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// `LaunchAgent` de macOS. Dentro de un `.app` se abre con `open -a` (el sistema trata la
/// ventana como la de la app); fuera, el ejecutable directo.
pub fn launch_agent(program: &Path, minimized: bool) -> String {
    let p = program.to_string_lossy();
    let mut args: Vec<String> = match p.find(".app/Contents/MacOS/") {
        Some(i) => {
            let bundle = p.get(..i + ".app".len()).unwrap_or(&p).to_string();
            let mut v = vec!["/usr/bin/open".to_string(), "-a".to_string(), bundle];
            if minimized {
                v.extend(["--args".to_string(), MINIMIZED_ARG.to_string()]);
            }
            v
        }
        None => {
            let mut v = vec![p.into_owned()];
            if minimized {
                v.push(MINIMIZED_ARG.to_string());
            }
            v
        }
    };
    let list: String = args.drain(..).map(|a| format!("\t\t<string>{}</string>\n", xml_escape(&a))).collect();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n\t<key>Label</key>\n\t<string>{APP_ID}</string>\n\
         \t<key>ProgramArguments</key>\n\t<array>\n{list}\t</array>\n\t<key>RunAtLoad</key>\n\t<true/>\n\
         \t<key>ProcessType</key>\n\t<string>Interactive</string>\n</dict>\n</plist>\n"
    )
}

/// Orden del valor `Run` de Windows.
pub fn run_command(program: &Path, minimized: bool) -> String {
    let mut s = format!("\"{}\"", program.to_string_lossy().replace('"', ""));
    if minimized {
        s.push(' ');
        s.push_str(MINIMIZED_ARG);
    }
    s
}

/// Lee el estado de un archivo de inicio (Linux o macOS).
fn state_of_file(path: &Path) -> State {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let minimized = text.lines().any(|l| {
                let l = l.trim();
                (l.starts_with("Exec=") && l.ends_with(MINIMIZED_ARG))
                    || l == format!("<string>{MINIMIZED_ARG}</string>")
            });
            // Una entrada desactivada por el usuario desde los ajustes del escritorio.
            let disabled = text.lines().any(|l| matches!(l.trim(), "Hidden=true" | "X-GNOME-Autostart-enabled=false"));
            State { enabled: !disabled, minimized }
        }
        Err(_) => State::default(),
    }
}

/// Escribe o borra un archivo de inicio (atómico: archivo temporal y `rename`).
fn write_file(path: &Path, content: Option<&str>) -> Result<(), DesktopError> {
    match content {
        None => match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        },
        Some(text) => {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = path.with_extension("tmp");
            std::fs::write(&tmp, text)?;
            std::fs::rename(&tmp, path)?;
            Ok(())
        }
    }
}

/// Dónde vive el archivo de inicio en Linux / macOS (`None` en Windows).
pub fn file_path() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        dirs::home_dir().map(|h| h.join("Library/LaunchAgents").join(format!("{APP_ID}.plist")))
    } else if cfg!(unix) {
        dirs::config_dir().map(|c| c.join("autostart").join(DESKTOP_FILE))
    } else {
        None
    }
}

/// Estado de un archivo en `path` (pruebas y Linux/macOS).
pub fn state_at(path: &Path) -> State {
    state_of_file(path)
}

/// Activa o desactiva con el archivo en `path` (pruebas y Linux/macOS).
pub fn set_at(path: &Path, program: &Path, enabled: bool, minimized: bool) -> Result<(), DesktopError> {
    if !enabled {
        return write_file(path, None);
    }
    // El formato lo decide el archivo (macOS usa siempre un `.plist` en LaunchAgents), no el SO:
    // así las pruebas del `.desktop` valen igual en macOS.
    let content = if path.extension().is_some_and(|e| e == "plist") {
        launch_agent(program, minimized)
    } else {
        desktop_entry(program, minimized)
    };
    write_file(path, Some(&content))
}

/// Estado actual en este sistema.
pub fn state() -> State {
    #[cfg(windows)]
    {
        if crate::store::packaged() {
            return task::state();
        }
        windows_state()
    }
    #[cfg(not(windows))]
    {
        file_path().map(|p| state_of_file(&p)).unwrap_or_default()
    }
}

/// Activa o desactiva el inicio con la sesión.
pub fn set(enabled: bool, minimized: bool) -> Result<(), DesktopError> {
    #[cfg(windows)]
    if crate::store::packaged() {
        return task::set(enabled, minimized);
    }
    let program = current_program().ok_or(DesktopError::Unsupported("ejecutable"))?;
    #[cfg(windows)]
    {
        windows_set(&program, enabled, minimized)
    }
    #[cfg(not(windows))]
    {
        let path = file_path().ok_or(DesktopError::Unsupported("carpeta de inicio"))?;
        set_at(&path, &program, enabled, minimized)
    }
}

/// ¿Hay que abrir minimizado porque lo abrió la StartupTask del paquete con «minimizado»
/// marcado? (Fuera del paquete, la sesión pasa `--minimized` en la orden.)
pub fn start_minimized_by_system() -> bool {
    crate::store::launched_by_startup_task() && marker_path().is_some_and(|p| p.is_file())
}

fn marker_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("iuredesk").join(MINIMIZED_MARKER))
}

/// Recuerda (o borra) la marca de «minimizado» de la StartupTask.
#[cfg_attr(not(windows), allow(dead_code))]
fn write_marker(minimized: bool) -> Result<(), DesktopError> {
    let Some(path) = marker_path() else { return Ok(()) };
    write_file(&path, minimized.then_some("1\n"))
}

/// `Windows.ApplicationModel.StartupTask` (solo dentro del paquete MSIX). Las operaciones
/// asíncronas se esperan en un hilo aparte (no en el de la interfaz, que es STA).
#[cfg(windows)]
mod task {
    use windows::ApplicationModel::{StartupTask, StartupTaskState};
    use windows::core::HSTRING;

    use super::{State, marker_path, write_marker};
    use crate::DesktopError;

    fn on_thread<T: Send + 'static>(
        f: impl FnOnce() -> windows::core::Result<T> + Send + 'static,
    ) -> Result<T, DesktopError> {
        std::thread::spawn(f)
            .join()
            .map_err(|_| DesktopError::System("StartupTask".into()))?
            .map_err(|e| DesktopError::System(e.message()))
    }

    fn get() -> windows::core::Result<StartupTask> {
        StartupTask::GetAsync(&HSTRING::from(crate::store::STARTUP_TASK))?.join()
    }

    pub(super) fn state() -> State {
        let enabled = on_thread(|| get()?.State())
            .is_ok_and(|s| s == StartupTaskState::Enabled || s == StartupTaskState::EnabledByPolicy);
        State { enabled, minimized: enabled && marker_path().is_some_and(|p| p.is_file()) }
    }

    pub(super) fn set(enabled: bool, minimized: bool) -> Result<(), DesktopError> {
        if !enabled {
            on_thread(|| get()?.Disable())?;
            return write_marker(false);
        }
        match on_thread(|| get()?.RequestEnableAsync()?.join())? {
            StartupTaskState::Enabled | StartupTaskState::EnabledByPolicy => write_marker(minimized),
            StartupTaskState::DisabledByUser => Err(DesktopError::System(
                "desactivado en Configuración de Windows → Aplicaciones → Inicio / \
                 turned off in Windows Settings → Apps → Startup"
                    .into(),
            )),
            StartupTaskState::DisabledByPolicy => {
                Err(DesktopError::System("directiva del equipo / group policy".into()))
            }
            other => Err(DesktopError::System(format!("StartupTask {}", other.0))),
        }
    }
}

#[cfg(windows)]
fn windows_state() -> State {
    let Ok(key) = windows_registry::CURRENT_USER.open(RUN_KEY) else { return State::default() };
    match key.get_string(RUN_VALUE) {
        Ok(v) => State { enabled: true, minimized: v.trim_end().ends_with(MINIMIZED_ARG) },
        Err(_) => State::default(),
    }
}

#[cfg(windows)]
fn windows_set(program: &Path, enabled: bool, minimized: bool) -> Result<(), DesktopError> {
    let key = windows_registry::CURRENT_USER.create(RUN_KEY).map_err(|e| DesktopError::System(e.to_string()))?;
    if enabled {
        return key
            .set_string(RUN_VALUE, run_command(program, minimized))
            .map_err(|e| DesktopError::System(e.to_string()));
    }
    match key.remove_value(RUN_VALUE) {
        Ok(()) => Ok(()),
        // Ya no estaba.
        Err(_) if key.get_string(RUN_VALUE).is_err() => Ok(()),
        Err(e) => Err(DesktopError::System(e.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entrada_xdg() {
        let e = desktop_entry(Path::new("/opt/Iure Desk/iuredesk"), true);
        assert!(e.contains("Exec=\"/opt/Iure Desk/iuredesk\" --minimized\n"), "{e}");
        assert!(e.starts_with("[Desktop Entry]\nType=Application\n"));
        let e = desktop_entry(Path::new("/x/$HOME`a\"b\\c%d"), false);
        assert!(e.contains("Exec=\"/x/\\$HOME\\`a\\\"b\\\\c%%d\"\n"), "{e}");
        assert!(!e.contains(MINIMIZED_ARG));
    }

    #[test]
    fn agente_de_macos() {
        let p = launch_agent(Path::new("/Applications/IureDesk.app/Contents/MacOS/iuredesk"), true);
        assert!(p.contains("<string>/usr/bin/open</string>"));
        assert!(p.contains("<string>/Applications/IureDesk.app</string>"));
        assert!(p.contains("<string>--minimized</string>"));
        assert!(p.contains(&format!("<string>{APP_ID}</string>")));
        let p = launch_agent(Path::new("/tmp/a&b<c>/iuredesk"), false);
        assert!(p.contains("<string>/tmp/a&amp;b&lt;c&gt;/iuredesk</string>"));
        assert!(!p.contains("--minimized"));
    }

    #[test]
    fn orden_de_windows() {
        assert_eq!(
            run_command(Path::new(r"C:\Users\Ana\AppData\Local\IureDesk\iuredesk.exe"), true),
            r#""C:\Users\Ana\AppData\Local\IureDesk\iuredesk.exe" --minimized"#
        );
        assert_eq!(run_command(Path::new("C:\\a\"b.exe"), false), "\"C:\\ab.exe\"");
    }

    #[test]
    fn activar_y_desactivar_con_archivo() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("autostart").join(DESKTOP_FILE);
        let program = Path::new("/usr/bin/iuredesk");
        assert_eq!(state_at(&path), State::default());
        set_at(&path, program, true, false).unwrap();
        assert_eq!(state_at(&path), State { enabled: true, minimized: false });
        set_at(&path, program, true, true).unwrap();
        assert_eq!(state_at(&path), State { enabled: true, minimized: true });
        // Desactivada desde los ajustes del escritorio.
        let text = std::fs::read_to_string(&path).unwrap().replace("enabled=true", "enabled=false");
        std::fs::write(&path, text).unwrap();
        assert!(!state_at(&path).enabled);
        set_at(&path, program, false, false).unwrap();
        assert!(!path.exists());
        // Desactivar dos veces no es un error.
        set_at(&path, program, false, false).unwrap();
        // plist.
        let plist = dir.path().join("x.plist");
        set_at(&plist, program, true, true).unwrap();
        assert_eq!(state_at(&plist), State { enabled: true, minimized: true });
    }
}
