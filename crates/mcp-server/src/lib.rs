//! IureDesk como servidor MCP (`iuredesk --mcp`, capa L4).
//!
//! Claude Desktop, Copilot en VS Code, IureVoz o el asistente de Iurefficient lanzan
//! `iuredesk --mcp` y le hablan por stdio (protocolo de `iurefficient_connect::mcp_server`).
//! Herramientas ([`tools`]): `buscar_correo`, `leer_hilo`, `crear_borrador` (nunca envía),
//! `agenda`, `crear_evento`, `buscar_contacto`, `crear_nota`, `mis_tareas`.
//!
//! **Acceso al almacén** (redb es de un solo proceso):
//! 1. Si la app está abierta, tiene el almacén y un [`bridge`] IPC: la llamada se le pasa y corre
//!    dentro de la app.
//! 2. Si no, el proceso MCP abre el almacén **solo durante la llamada** (y lo suelta), para no
//!    impedir que la app arranque después.
//!
//! [`agents`] registra `iuredesk --mcp` en Claude Desktop y VS Code (botón de ajustes).
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

pub mod agents;
pub mod bridge;
pub mod tools;

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use iuredesk_store::Store;
use iurefficient_connect::mcp_server::{Call, CallError, Handler, Reply, ServerInfo, serve};
use serde_json::{Value, json};

pub use agents::{AgentsStatus, MCP_KEY, Registration, agents_status, register_mcp_agents, unregister_mcp_agents};
pub use bridge::{BridgeClient, BridgeError, BridgeHandle, BridgeOptions, start as start_bridge};
pub use tools::{ToolContext, ToolError, ToolOutput, dispatch, tool_list};

/// El servidor MCP de IureDesk sobre un almacén (por su carpeta).
pub struct DeskMcp {
    store_dir: PathBuf,
    runtime_dir: Option<PathBuf>,
    context: Option<ToolContext>,
    /// Acceso directo de uno en uno (redb no admite dos aperturas ni en el mismo proceso).
    direct: Mutex<()>,
}

impl DeskMcp {
    /// Servidor para el almacén de `store_dir` (la carpeta que abre la app: `…/iuredesk/store`).
    pub fn new(store_dir: impl Into<PathBuf>) -> Self {
        Self { store_dir: store_dir.into(), runtime_dir: None, context: None, direct: Mutex::new(()) }
    }

    /// Carpeta donde buscar el puente (pruebas).
    pub fn with_runtime_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.runtime_dir = Some(dir.into());
        self
    }

    /// Contexto fijo (pruebas).
    pub fn with_context(mut self, ctx: ToolContext) -> Self {
        self.context = Some(ctx);
        self
    }

    fn ctx(&self) -> ToolContext {
        self.context.clone().unwrap_or_else(ToolContext::system)
    }

    /// Ejecuta una herramienta: por el puente si la app está abierta, si no sobre el almacén.
    pub fn run(&self, name: &str, args: &Value) -> Result<ToolOutput, ToolError> {
        let ctx = self.ctx();
        if let Some(client) = BridgeClient::discover(&self.store_dir, self.runtime_dir.as_deref()) {
            match client.call(name, args) {
                Ok(out) => return Ok(out),
                Err(bridge::BridgeCallError::Tool(e)) => return Err(e),
                // Archivo huérfano (la app se cerró mal): se intenta directo.
                Err(bridge::BridgeCallError::Unreachable(_)) => {}
            }
        }
        if !self.store_dir.join("meta.redb").is_file() {
            return Err(ToolError::Failed(if ctx.spanish {
                "IureDesk aún no tiene datos: ábrelo y agrega una cuenta.".into()
            } else {
                "IureDesk has no data yet: open it and add an account.".into()
            }));
        }
        let _one = self.direct.lock().unwrap_or_else(|e| e.into_inner());
        let store = Store::open(&self.store_dir).map_err(|e| {
            ToolError::Failed(if ctx.spanish {
                format!("IureDesk está abierto pero no aceptó la consulta; actualízalo o reinícialo. ({e})")
            } else {
                format!("IureDesk is open but did not accept the request; update or restart it. ({e})")
            })
        })?;
        dispatch(&store, name, args, &ctx)
    }
}

impl Handler for DeskMcp {
    fn info(&self) -> ServerInfo {
        ServerInfo {
            name: "iuredesk",
            title: "IureDesk",
            version: env!("CARGO_PKG_VERSION"),
            instructions: "Local mail, calendar, contacts, notes and tasks of the user (IureDesk). Search mail with \
                buscar_correo and read a conversation with leer_hilo. crear_borrador only saves a draft: nothing is ever \
                sent. · Correo, calendario, contactos, notas y tareas locales del usuario (IureDesk). Busca correo con \
                buscar_correo y lee una conversación con leer_hilo. crear_borrador solo guarda un borrador: nunca se \
                envía nada."
                .into(),
        }
    }

    fn tools(&self) -> Value {
        tool_list()
    }

    fn call(&self, name: &str, args: &Value, _call: &Call) -> Result<Reply, CallError> {
        match self.run(name, args) {
            Ok(ToolOutput::Data(v)) => Ok(Reply::Data(v)),
            Ok(ToolOutput::Text(t)) => {
                Ok(Reply::Raw(json!({"content": [{"type": "text", "text": t}], "isError": false})))
            }
            Err(ToolError::Params(m)) => Err(CallError::Params(m)),
            Err(ToolError::Failed(m)) => Err(CallError::Tool(anyhow::anyhow!(m))),
        }
    }
}

/// `iuredesk --mcp`: atiende stdin/stdout hasta que el cliente cierre. Devuelve el código de
/// salida. Stdout es del protocolo: nada más debe escribir ahí.
pub fn run_stdio(store_dir: &Path) -> i32 {
    serve(DeskMcp::new(store_dir))
}
