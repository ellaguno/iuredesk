//! Cliente MCP genérico por stdio.
//!
//! Lanza `<programa> --mcp` (o lo que se pida) y le habla JSON-RPC 2.0, un mensaje por línea
//! (transporte stdio de la spec MCP): `initialize` con la versión `2025-06-18`,
//! `notifications/initialized`, `tools/list` (con paginación) y `tools/call`.
//!
//! - Síncrono: un hilo lee la salida del hijo y la pasa por un canal; cada petición espera su
//!   respuesta con un tiempo límite. Si se agota, se manda `notifications/cancelled`.
//! - El servidor es hostil hasta que se demuestre lo contrario: líneas acotadas
//!   ([`MAX_LINE_BYTES`]), lo que no es JSON se ignora (algunas apps escriben avisos en stdout),
//!   texto de resultado acotado ([`MAX_TEXT_CHARS`]).
//! - Peticiones del servidor al cliente (`roots/list`, `sampling/…`) se rechazan con
//!   «método no soportado».
//! - Al soltar el cliente se mata el proceso hijo y se espera (sin zombis).

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::error::{IntegrationError, Result};

/// Versión del protocolo que pide el cliente.
pub const PROTOCOL_VERSION: &str = "2025-06-18";
/// Tope de una línea del servidor (una respuesta con texto largo cabe de sobra).
pub const MAX_LINE_BYTES: usize = 32 * 1024 * 1024;
/// Tope del texto que se conserva de un resultado.
pub const MAX_TEXT_CHARS: usize = 4 * 1024 * 1024;
/// Páginas máximas de `tools/list`.
const MAX_LIST_PAGES: usize = 20;

/// Opciones del cliente.
#[derive(Clone, Debug)]
pub struct McpOptions {
    /// Tiempo para arrancar y negociar.
    pub init_timeout: Duration,
    /// Tiempo por omisión de cada llamada.
    pub call_timeout: Duration,
    /// Nombre que se presenta en `clientInfo`.
    pub client_name: String,
    /// Variables de entorno extra para el hijo.
    pub env: Vec<(String, String)>,
}

impl Default for McpOptions {
    fn default() -> Self {
        Self {
            init_timeout: Duration::from_secs(20),
            call_timeout: Duration::from_secs(120),
            client_name: "iuredesk".into(),
            env: Vec::new(),
        }
    }
}

/// Lo que el servidor dijo de sí mismo.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServerInfo {
    /// `serverInfo.name`.
    pub name: String,
    /// `serverInfo.version`.
    pub version: String,
    /// Versión del protocolo acordada.
    pub protocol_version: String,
    /// `instructions`, si las hay.
    pub instructions: Option<String>,
}

/// Una herramienta de `tools/list`.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolInfo {
    /// Nombre.
    pub name: String,
    /// Descripción.
    pub description: String,
    /// `inputSchema`.
    pub input_schema: Value,
}

/// Resultado de `tools/call`.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolResult {
    /// Texto concatenado de los bloques `text` (acotado).
    pub text: String,
    /// `structuredContent`, si vino.
    pub structured: Option<Value>,
    /// `isError`.
    pub is_error: bool,
}

impl ToolResult {
    /// Los datos: `structuredContent` o, si no vino, el texto interpretado como JSON.
    pub fn data(&self) -> Option<Value> {
        self.structured.clone().or_else(|| serde_json::from_str(&self.text).ok())
    }

    /// Convierte `isError` en [`IntegrationError::Tool`].
    pub fn into_ok(self) -> Result<Self> {
        if self.is_error { Err(IntegrationError::Tool(self.text)) } else { Ok(self) }
    }
}

/// Cliente de un servidor MCP hijo.
pub struct McpClient {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Value>,
    next_id: u64,
    server: ServerInfo,
    call_timeout: Duration,
}

impl std::fmt::Debug for McpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpClient").field("server", &self.server).finish_non_exhaustive()
    }
}

impl McpClient {
    /// Lanza el servidor y negocia. Falla si no arranca o no responde a tiempo.
    pub fn spawn(program: &Path, args: &[&str], opts: &McpOptions) -> Result<Self> {
        let mut cmd = Command::new(program);
        cmd.args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        for (k, v) in &opts.env {
            cmd.env(k, v);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // CREATE_NO_WINDOW: sin consola parpadeando.
            cmd.creation_flags(0x0800_0000);
        }
        let mut child = cmd.spawn()?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(IntegrationError::Protocol("sin stdin/stdout".into()));
        };
        let (tx, rx) = channel();
        std::thread::Builder::new().name("mcp-client-reader".into()).spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut buf = Vec::new();
            loop {
                match read_line_capped(&mut reader, &mut buf, MAX_LINE_BYTES) {
                    Ok(None) => break,
                    Ok(Some(true)) => {
                        if let Ok(v) = serde_json::from_slice::<Value>(&buf)
                            && v.is_object()
                            && tx.send(v).is_err()
                        {
                            break;
                        }
                    }
                    // Línea demasiado larga: se descarta entera.
                    Ok(Some(false)) => {}
                    Err(_) => break,
                }
            }
        })?;
        let mut client =
            Self { child, stdin, rx, next_id: 1, server: ServerInfo::default(), call_timeout: opts.call_timeout };
        let init = client.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": opts.client_name, "title": "IureDesk", "version": env!("CARGO_PKG_VERSION")},
            }),
            opts.init_timeout,
        )?;
        let s = |p: &str| init.pointer(p).and_then(Value::as_str).map(String::from);
        client.server = ServerInfo {
            name: s("/serverInfo/name").unwrap_or_default(),
            version: s("/serverInfo/version").unwrap_or_default(),
            protocol_version: s("/protocolVersion")
                .ok_or_else(|| IntegrationError::Protocol("initialize sin protocolVersion".into()))?,
            instructions: s("/instructions"),
        };
        client.notify("notifications/initialized", json!({}))?;
        Ok(client)
    }

    /// Lo que el servidor dijo de sí mismo.
    pub fn server(&self) -> &ServerInfo {
        &self.server
    }

    /// Las herramientas del servidor (sigue `nextCursor`, con tope de páginas).
    pub fn list_tools(&mut self) -> Result<Vec<ToolInfo>> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_LIST_PAGES {
            let params = match &cursor {
                Some(c) => json!({"cursor": c}),
                None => json!({}),
            };
            let r = self.request("tools/list", params, self.call_timeout)?;
            let tools = r
                .get("tools")
                .and_then(Value::as_array)
                .ok_or_else(|| IntegrationError::Protocol("tools/list sin «tools»".into()))?;
            for t in tools {
                let Some(name) = t.get("name").and_then(Value::as_str) else { continue };
                out.push(ToolInfo {
                    name: name.to_string(),
                    description: t.get("description").and_then(Value::as_str).unwrap_or("").to_string(),
                    input_schema: t.get("inputSchema").cloned().unwrap_or(Value::Null),
                });
            }
            cursor = r.get("nextCursor").and_then(Value::as_str).map(String::from);
            if cursor.is_none() {
                break;
            }
        }
        Ok(out)
    }

    /// Llama a una herramienta. `timeout = None` usa el de las opciones.
    pub fn call_tool(&mut self, name: &str, arguments: Value, timeout: Option<Duration>) -> Result<ToolResult> {
        let r = self.request(
            "tools/call",
            json!({"name": name, "arguments": arguments}),
            timeout.unwrap_or(self.call_timeout),
        )?;
        let mut text = String::new();
        for block in r.get("content").and_then(Value::as_array).into_iter().flatten() {
            if block.get("type").and_then(Value::as_str) == Some("text")
                && let Some(t) = block.get("text").and_then(Value::as_str)
            {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(t);
            }
        }
        if let Some((cut, _)) = text.char_indices().nth(MAX_TEXT_CHARS) {
            text.truncate(cut);
        }
        Ok(ToolResult {
            text,
            structured: r.get("structuredContent").filter(|v| !v.is_null()).cloned(),
            is_error: r.get("isError").and_then(Value::as_bool).unwrap_or(false),
        })
    }

    fn send(&mut self, msg: &Value) -> Result<()> {
        let mut line = serde_json::to_vec(msg).map_err(|e| IntegrationError::Protocol(e.to_string()))?;
        line.push(b'\n');
        self.stdin.write_all(&line).and_then(|()| self.stdin.flush()).map_err(|e| self.closed(e.to_string()))
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}))
    }

    fn closed(&mut self, why: String) -> IntegrationError {
        match self.child.try_wait() {
            Ok(Some(status)) => IntegrationError::Closed(format!("{why} ({status})")),
            _ => IntegrationError::Closed(why),
        }
    }

    /// Petición con respuesta. Las notificaciones del servidor (progreso, registro) se ignoran.
    fn request(&mut self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let msg = match self.rx.recv_timeout(left) {
                Ok(m) => m,
                Err(RecvTimeoutError::Timeout) => {
                    let _ = self.notify("notifications/cancelled", json!({"requestId": id, "reason": "timeout"}));
                    return Err(IntegrationError::Timeout(timeout));
                }
                Err(RecvTimeoutError::Disconnected) => return Err(self.closed("stdout cerrado".into())),
            };
            let has_method = msg.get("method").is_some();
            match msg.get("id") {
                // Petición del servidor al cliente: no ofrecemos nada (roots, sampling…).
                Some(sid) if has_method => {
                    let reply = json!({"jsonrpc": "2.0", "id": sid, "error": {"code": -32601, "message": "method not supported"}});
                    self.send(&reply)?;
                }
                Some(rid) if rid.as_u64() == Some(id) => {
                    if let Some(err) = msg.get("error") {
                        return Err(IntegrationError::Rpc {
                            code: err.get("code").and_then(Value::as_i64).unwrap_or(0),
                            message: err.get("message").and_then(Value::as_str).unwrap_or("").to_string(),
                        });
                    }
                    return msg
                        .get("result")
                        .cloned()
                        .ok_or_else(|| IntegrationError::Protocol("respuesta sin result".into()));
                }
                // Notificación o respuesta a otra petición (p. ej. una cancelada): se ignora.
                _ => {}
            }
        }
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Lee una línea (sin el `\n`) en `buf`, con tope. `Ok(None)` = fin; `Ok(Some(false))` = la línea
/// superó el tope y se descartó.
fn read_line_capped(r: &mut impl BufRead, buf: &mut Vec<u8>, max: usize) -> std::io::Result<Option<bool>> {
    buf.clear();
    let mut too_long = false;
    let mut any = false;
    loop {
        let chunk = r.fill_buf()?;
        if chunk.is_empty() {
            return Ok(if any { Some(!too_long) } else { None });
        }
        any = true;
        let (part, done) = match chunk.iter().position(|&b| b == b'\n') {
            Some(i) => (chunk.get(..i).unwrap_or_default(), Some(i + 1)),
            None => (chunk, None),
        };
        if !too_long {
            if buf.len().saturating_add(part.len()) > max {
                too_long = true;
                buf.clear();
            } else {
                buf.extend_from_slice(part);
            }
        }
        let used = done.unwrap_or(chunk.len());
        r.consume(used);
        if done.is_some() {
            if buf.last() == Some(&b'\r') {
                buf.pop();
            }
            return Ok(Some(!too_long));
        }
    }
}

/// Lee como mucho `max` bytes de `r` (respuestas de sockets).
pub(crate) fn read_capped(r: impl Read, max: u64) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    r.take(max).read_to_end(&mut out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lineas_acotadas() {
        let data = b"{\"a\":1}\r\nxxxxxxxxxxxxxxxxxxxx\n{\"b\":2}";
        let mut r = BufReader::with_capacity(4, &data[..]);
        let mut buf = Vec::new();
        assert_eq!(read_line_capped(&mut r, &mut buf, 10).unwrap(), Some(true));
        assert_eq!(buf, b"{\"a\":1}");
        assert_eq!(read_line_capped(&mut r, &mut buf, 10).unwrap(), Some(false));
        assert_eq!(read_line_capped(&mut r, &mut buf, 10).unwrap(), Some(true));
        assert_eq!(buf, b"{\"b\":2}");
        assert_eq!(read_line_capped(&mut r, &mut buf, 10).unwrap(), None);
    }

    #[test]
    fn programa_inexistente_es_error_de_es() {
        let r = McpClient::spawn(Path::new("/no/existe/iuredesk-fake"), &["--mcp"], &McpOptions::default());
        assert!(matches!(r, Err(IntegrationError::Io(_))));
    }
}
