//! «Usar IureDesk desde Claude / Copilot»: registrar `iuredesk --mcp` en los asistentes.
//!
//! Envoltorio de `iurefficient_connect::agents`, que escribe solo la entrada
//! `mcpServers.iuredesk` de `claude_desktop_config.json` (con copia previa, atómico) y genera el
//! enlace `vscode:mcp/install?…` para VS Code (es VS Code quien pregunta y guarda). El comando
//! registrado es el ejecutable actual (o el AppImage), así que hay que llamarlo **desde la app**.

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
    AgentsStatus { claude_desktop: agents::claude_desktop_status(MCP_KEY), vscode: agents::vscode_status(MCP_KEY) }
}

/// Registra IureDesk en Claude Desktop (si está) y, con `open_vscode`, abre el enlace de VS Code
/// (si está). Lo dispara el botón de ajustes: el clic del usuario es el consentimiento.
pub fn register_mcp_agents(open_vscode: bool) -> Registration {
    let status = agents_status();
    let mut r = Registration::default();
    if status.claude_desktop.installed {
        match agents::claude_desktop_connect(MCP_KEY) {
            Ok(n) => r.claude_files = n,
            Err(e) => r.claude_error = Some(format!("{e:#}")),
        }
    }
    if status.vscode.installed
        && let Ok(link) = agents::vscode_install_url(MCP_KEY)
    {
        if open_vscode {
            r.vscode_opened = open::that_detached(&link).is_ok();
        }
        r.vscode_link = Some(link);
    }
    r
}

/// Quita IureDesk de Claude Desktop (VS Code se gestiona desde VS Code).
pub fn unregister_mcp_agents() -> Result<(), String> {
    agents::claude_desktop_disconnect(MCP_KEY).map_err(|e| format!("{e:#}"))
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
}
