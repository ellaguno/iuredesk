//! «Usar IureDesk desde Claude / Copilot»: registrar `iuredesk --mcp` en los asistentes.
//!
//! Envoltorio de `iurefficient_connect::agents`, que escribe solo la entrada
//! `mcpServers.iuredesk` de `claude_desktop_config.json` (con copia previa, atómico) y genera el
//! enlace `vscode:mcp/install?…` para VS Code (es VS Code quien pregunta y guarda). El comando
//! registrado es el ejecutable actual (o el AppImage), así que hay que llamarlo **desde la app**.
//!
//! Instalada desde Microsoft Store (MSIX) el comando es el alias de ejecución
//! `%LOCALAPPDATA%\Microsoft\WindowsApps\iuredesk.exe` ([`iuredesk_desktop::store`]): la carpeta del
//! paquete cambia con cada versión y, lanzado por su ruta, el ejecutable corre sin la identidad del
//! paquete (no vería el almacén ni el puente con la ventana). connect solo sabe usar el ejecutable
//! actual (o `APPIMAGE`, que no se puede fijar sin `unsafe` en Rust 2024), así que empaquetada la
//! entrada de Claude Desktop y el enlace de VS Code se arman aquí ([`store`]). La configuración de
//! Claude Desktop se reescribe en su sitio (con copia previa) en vez de con un archivo temporal y
//! `rename`: dentro del paquete un archivo *nuevo* en `AppData` quedaría en la copia virtualizada
//! del paquete, y modificar uno que ya existe sí llega al archivo real.

use iurefficient_connect::agents::{self, Status};

/// Clave del servidor en la configuración de los asistentes.
pub const MCP_KEY: &str = "iuredesk";

/// Estado en los dos asistentes.
#[derive(Clone, Debug)]
pub struct AgentsStatus {
    /// Claude Desktop.
    pub claude_desktop: Status,
    /// VS Code (Copilot).
    pub vscode: Status,
}

impl AgentsStatus {
    /// `true` si está conectado en alguno.
    pub fn any_connected(&self) -> bool {
        self.claude_desktop.connected || self.vscode.connected
    }
}

/// Qué se hizo al registrar.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Registration {
    /// Archivos de Claude Desktop escritos (0 si no está instalado).
    pub claude_files: usize,
    /// Error al escribir la configuración de Claude Desktop (texto para el usuario).
    pub claude_error: Option<String>,
    /// Enlace de instalación para VS Code (si está instalado).
    pub vscode_link: Option<String>,
    /// Se abrió el enlace de VS Code (VS Code pedirá confirmación).
    pub vscode_opened: bool,
}

/// Estado actual (sin escribir nada).
pub fn agents_status() -> AgentsStatus {
    let mut st =
        AgentsStatus { claude_desktop: agents::claude_desktop_status(MCP_KEY), vscode: agents::vscode_status(MCP_KEY) };
    if let Some(alias) = store::command() {
        store::fix_status(&mut st.claude_desktop, &["mcpServers"], &alias);
        store::fix_status(&mut st.vscode, &["servers"], &alias);
    }
    st
}

/// Registra IureDesk en Claude Desktop (si está) y, con `open_vscode`, abre el enlace de VS Code
/// (si está). Lo dispara el botón de ajustes: el clic del usuario es el consentimiento.
pub fn register_mcp_agents(open_vscode: bool) -> Registration {
    let status = agents_status();
    let mut r = Registration::default();
    let alias = store::command();
    if status.claude_desktop.installed {
        let done = match &alias {
            Some(a) => store::claude_connect(a),
            None => agents::claude_desktop_connect(MCP_KEY).map_err(|e| format!("{e:#}")),
        };
        match done {
            Ok(n) => r.claude_files = n,
            Err(e) => r.claude_error = Some(e),
        }
    }
    let link = match &alias {
        Some(a) => Ok(store::vscode_install_url(a)),
        None => agents::vscode_install_url(MCP_KEY),
    };
    if status.vscode.installed
        && let Ok(link) = link
    {
        if open_vscode {
            r.vscode_opened = iuredesk_desktop::launch::open_detached(&link).is_ok();
        }
        r.vscode_link = Some(link);
    }
    r
}

/// Quita IureDesk de Claude Desktop (VS Code se gestiona desde VS Code).
pub fn unregister_mcp_agents() -> Result<(), String> {
    if store::command().is_some() {
        return store::claude_disconnect();
    }
    agents::claude_desktop_disconnect(MCP_KEY).map_err(|e| format!("{e:#}"))
}

/// Registro con el alias de ejecución del paquete MSIX (ver la documentación del módulo).
mod store {
    use std::path::{Path, PathBuf};

    use iurefficient_connect::agents::Status;
    use serde_json::{Map, Value, json};

    use super::MCP_KEY;

    const CLAUDE_CONFIG_FILE: &str = "claude_desktop_config.json";

    /// El alias, si IureDesk corre empaquetada.
    pub(super) fn command() -> Option<String> {
        iuredesk_desktop::store::execution_alias().map(|p| p.to_string_lossy().into_owned())
    }

    /// `{"command": <alias>, "args": ["--mcp"]}`.
    pub(super) fn entry(alias: &str) -> Value {
        json!({ "command": alias, "args": ["--mcp"] })
    }

    /// Lo registrado en `path` bajo `<section>.iuredesk.command`. `None` si no se pudo leer (p. ej.
    /// JSON con comentarios de VS Code): entonces se deja el estado que calculó connect.
    fn registered(path: &Path, section: &[&str]) -> Option<Option<String>> {
        let text = std::fs::read_to_string(path).ok()?;
        if text.trim().is_empty() {
            return Some(None);
        }
        let v: Value = serde_json::from_str(&text).ok()?;
        let mut cur = &v;
        for s in section {
            match cur.get(s) {
                Some(x) => cur = x,
                None => return Some(None),
            }
        }
        Some(cur.get(MCP_KEY).and_then(|e| e.get("command")).and_then(Value::as_str).map(String::from))
    }

    /// Recalcula «conectado» / «hay que actualizar» comparando con el alias.
    pub(super) fn fix_status(st: &mut Status, section: &[&str], alias: &str) {
        let Some(path) = st.config_path.clone() else { return };
        if !Path::new(&path).is_file() {
            return;
        }
        if let Some(reg) = registered(Path::new(&path), section) {
            st.connected = reg.as_deref() == Some(alias);
            st.stale = reg.is_some() && !st.connected;
        }
    }

    /// Carpetas de configuración de Claude Desktop que existen (la normal y la de su paquete MSIX).
    fn claude_dirs() -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> =
            dirs::config_dir().map(|d| d.join("Claude")).filter(|d| d.is_dir()).into_iter().collect();
        if let Some(pkgs) = dirs::data_local_dir().map(|d| d.join("Packages"))
            && let Ok(entries) = std::fs::read_dir(&pkgs)
        {
            for e in entries.flatten() {
                if e.file_name().to_string_lossy().starts_with("Claude_") {
                    let d = e.path().join("LocalCache").join("Roaming").join("Claude");
                    if d.is_dir() {
                        v.push(d);
                    }
                }
            }
        }
        v
    }

    fn read_object(path: &Path) -> Result<Map<String, Value>, String> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        if text.trim().is_empty() {
            return Ok(Map::new());
        }
        match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(m)) => Ok(m),
            _ => Err(format!(
                "La configuración no es JSON válido; no se tocó / The configuration is not valid JSON, so it was left untouched: {}",
                path.display()
            )),
        }
    }

    /// Escribe en su sitio, con copia previa `.json.bak-iuredesk`.
    fn write_object(path: &Path, config: &Map<String, Value>) -> Result<(), String> {
        if path.is_file() {
            let _ = std::fs::copy(path, path.with_extension(format!("json.bak-{MCP_KEY}")));
        }
        let text = serde_json::to_string_pretty(config).map_err(|e| e.to_string())? + "\n";
        std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Pone `mcpServers.iuredesk` en `config`.
    pub(super) fn with_entry(mut config: Map<String, Value>, alias: &str) -> Map<String, Value> {
        let servers = config.entry("mcpServers").or_insert_with(|| json!({}));
        if !servers.is_object() {
            *servers = json!({});
        }
        servers[MCP_KEY] = entry(alias);
        config
    }

    pub(super) fn claude_connect(alias: &str) -> Result<usize, String> {
        let dirs = claude_dirs();
        if dirs.is_empty() {
            return Err(
                "No se encontró Claude Desktop en este equipo / Claude Desktop was not found on this computer.".into(),
            );
        }
        // Todo se valida antes de escribir: o se actualizan todas las copias o ninguna.
        let mut configs = Vec::new();
        for d in &dirs {
            let path = d.join(CLAUDE_CONFIG_FILE);
            configs.push((read_object(&path)?, path));
        }
        for (config, path) in configs {
            write_object(&path, &with_entry(config, alias))?;
        }
        Ok(dirs.len())
    }

    pub(super) fn claude_disconnect() -> Result<(), String> {
        for d in claude_dirs() {
            let path = d.join(CLAUDE_CONFIG_FILE);
            let mut config = read_object(&path)?;
            let removed =
                config.get_mut("mcpServers").and_then(Value::as_object_mut).and_then(|s| s.remove(MCP_KEY)).is_some();
            if removed {
                write_object(&path, &config)?;
            }
        }
        Ok(())
    }

    /// Enlace `vscode:mcp/install?…` con el alias (VS Code pregunta antes de guardarlo).
    pub(super) fn vscode_install_url(alias: &str) -> String {
        let mut e = entry(alias);
        e["name"] = json!(MCP_KEY);
        e["type"] = json!("stdio");
        format!("vscode:mcp/install?{}", percent_encode(&e.to_string()))
    }

    /// Codifica todo salvo los caracteres no reservados de RFC 3986 (el espacio, como `%20`).
    pub(super) fn percent_encode(s: &str) -> String {
        let mut out = String::with_capacity(s.len() * 3);
        for b in s.bytes() {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                out.push(char::from(b));
            } else {
                out.push_str(&format!("%{b:02X}"));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estado_sin_escribir() {
        // No escribe nada; solo lee lo que haya en este equipo.
        let s = agents_status();
        assert!(!s.claude_desktop.download_url.is_empty());
        assert_eq!(MCP_KEY, "iuredesk");
    }

    #[test]
    fn registro_con_el_alias_del_paquete() {
        let alias = r"C:\Users\Ana\AppData\Local\Microsoft\WindowsApps\iuredesk.exe";
        let mut other = serde_json::Map::new();
        other.insert("mcpServers".into(), serde_json::json!({"otro": {"command": "x"}}));
        other.insert("theme".into(), serde_json::json!("dark"));
        let c = store::with_entry(other, alias);
        assert_eq!(c["mcpServers"]["iuredesk"]["command"], alias);
        assert_eq!(c["mcpServers"]["iuredesk"]["args"][0], "--mcp");
        assert_eq!(c["mcpServers"]["otro"]["command"], "x");
        assert_eq!(c["theme"], "dark");
        let link = store::vscode_install_url(alias);
        assert!(link.starts_with("vscode:mcp/install?%7B"), "{link}");
        assert!(link.contains("WindowsApps%5C%5Ciuredesk.exe"), "{link}");
        assert!(!link.contains(' ') && !link.contains('+'));
        assert_eq!(store::percent_encode("a b/é"), "a%20b%2F%C3%A9");
        // Fuera del paquete no hay alias: se usa connect.
        assert!(store::command().is_none());
    }
}
