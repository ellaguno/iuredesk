//! Puente IPC entre `iuredesk --mcp` y la app abierta.
//!
//! **Por qué:** redb es de un solo proceso (bloqueo de archivo) y tantivy también bloquea su
//! índice: mientras la interfaz tiene el almacén abierto, el proceso MCP no puede abrirlo. La app
//! arranca entonces este puente ([`start`]) y el proceso MCP le pasa cada llamada
//! ([`BridgeClient`]); las herramientas corren **dentro de la app**, sobre su `Arc<Store>`, y la
//! interfaz se entera de las escrituras por [`BridgeOptions::on_change`].
//!
//! **Transporte:** socket Unix en Linux y macOS; en Windows (sin `AF_UNIX` en `std` y sin
//! tuberías con nombre sin `unsafe`), TCP en `127.0.0.1` con puerto efímero. En los dos casos:
//! - un archivo de punto de acceso `bridge-<hash del almacén>.json` en la carpeta de ejecución
//!   del usuario (`$XDG_RUNTIME_DIR/iuredesk`, o la caché del usuario), con permisos 0600 en
//!   Unix, que dice dónde escuchar y lleva un **token** de 256 bits;
//! - cada conexión manda una línea JSON `{"v":1,"token":…,"tool":…,"args":{…}}` y recibe una
//!   línea `{"ok":true,"kind":"data"|"text","value":…}` o `{"ok":false,"kind":"params"|"failed",
//!   "error":…}`; tamaños acotados y tiempos de espera en las dos puntas.

use std::io::{BufRead, BufReader, Read, Write};
#[cfg(not(unix))]
use std::net::TcpListener;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use iuredesk_store::Store;
use serde_json::{Value, json};

use crate::tools::{ToolContext, ToolError, ToolOutput, dispatch, is_write};

/// Versión del protocolo del puente.
pub const BRIDGE_VERSION: u64 = 1;
/// Tope de una petición.
const MAX_REQUEST: u64 = 4 * 1024 * 1024;
/// Tope de una respuesta.
const MAX_RESPONSE: u64 = 16 * 1024 * 1024;
/// Conexiones simultáneas máximas.
const MAX_CONNECTIONS: usize = 8;
const IO_TIMEOUT: Duration = Duration::from_secs(30);

/// Fallo al arrancar o usar el puente.
#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    /// E/S.
    #[error("puente IPC: {0}")]
    Io(#[from] std::io::Error),
    /// Respuesta o archivo de punto de acceso no válidos.
    #[error("puente IPC: {0}")]
    Protocol(String),
    /// Ya hay una app sirviendo este almacén.
    #[error("ya hay un puente activo para este almacén")]
    AlreadyRunning,
}

/// Quién avisa a la interfaz de que una herramienta escribió en el almacén (recibe el nombre).
pub type ChangeHook = Arc<dyn Fn(&str) + Send + Sync>;

/// Opciones del puente.
#[derive(Clone, Default)]
pub struct BridgeOptions {
    /// Carpeta de ejecución (por omisión [`runtime_dir`]).
    pub runtime_dir: Option<PathBuf>,
    /// Aviso tras cada herramienta que escribe (`crear_borrador`, `crear_evento`, `crear_nota`):
    /// la interfaz recarga carpetas/agenda/notas. Corre en el hilo del puente: que solo marque.
    pub on_change: Option<ChangeHook>,
    /// Contexto fijo (pruebas). `None` = [`ToolContext::system`] en cada llamada.
    pub context: Option<ToolContext>,
}

/// Carpeta de ejecución del usuario para el puente.
pub fn runtime_dir() -> PathBuf {
    dirs::runtime_dir().or_else(dirs::cache_dir).unwrap_or_else(std::env::temp_dir).join("iuredesk")
}

/// FNV-1a de 64 bits: nombre estable por almacén (no es seguridad, el token sí).
fn store_key(store_dir: &Path) -> String {
    let canon = store_dir.canonicalize().unwrap_or_else(|_| store_dir.to_path_buf());
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in canon.to_string_lossy().as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

fn endpoint_file(dir: &Path, store_dir: &Path) -> PathBuf {
    dir.join(format!("bridge-{}.json", store_key(store_dir)))
}

fn new_token() -> Result<String, BridgeError> {
    let mut b = [0u8; 32];
    getrandom::getrandom(&mut b).map_err(|e| BridgeError::Protocol(format!("azar: {e}")))?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

/// Comparación en tiempo constante (para el token).
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Carpeta privada (0700 en Unix).
fn private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Escribe un archivo legible solo por el usuario (0600 en Unix), de forma atómica.
fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o.open(&tmp)?;
    f.write_all(data)?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path)
}

// ------------------------------------------------------------------ transporte

enum Listener {
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixListener),
    #[cfg(not(unix))]
    Tcp(TcpListener),
}

enum Conn {
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixStream),
    Tcp(TcpStream),
}

#[derive(Clone, Debug)]
enum Endpoint {
    #[cfg(unix)]
    Unix(PathBuf),
    Tcp(SocketAddr),
}

impl Endpoint {
    fn connect(&self) -> std::io::Result<Conn> {
        let c = match self {
            #[cfg(unix)]
            Endpoint::Unix(p) => Conn::Unix(std::os::unix::net::UnixStream::connect(p)?),
            Endpoint::Tcp(a) => Conn::Tcp(TcpStream::connect_timeout(a, Duration::from_secs(3))?),
        };
        c.set_timeouts(IO_TIMEOUT)?;
        Ok(c)
    }

    fn to_json(&self) -> Value {
        match self {
            #[cfg(unix)]
            Endpoint::Unix(p) => json!({"transport": "unix", "path": p}),
            Endpoint::Tcp(a) => json!({"transport": "tcp", "addr": a.to_string()}),
        }
    }

    fn from_json(v: &Value) -> Option<Self> {
        match v.get("transport")?.as_str()? {
            #[cfg(unix)]
            "unix" => Some(Endpoint::Unix(PathBuf::from(v.get("path")?.as_str()?))),
            "tcp" => {
                let a: SocketAddr = v.get("addr")?.as_str()?.parse().ok()?;
                // Solo local: un archivo manipulado no puede mandarnos a otra máquina.
                a.ip().is_loopback().then_some(Endpoint::Tcp(a))
            }
            _ => None,
        }
    }
}

impl Listener {
    fn accept(&self) -> std::io::Result<Conn> {
        match self {
            #[cfg(unix)]
            Listener::Unix(l) => l.accept().map(|(s, _)| Conn::Unix(s)),
            #[cfg(not(unix))]
            Listener::Tcp(l) => l.accept().map(|(s, _)| Conn::Tcp(s)),
        }
    }
}

impl Conn {
    fn set_timeouts(&self, t: Duration) -> std::io::Result<()> {
        match self {
            #[cfg(unix)]
            Conn::Unix(s) => {
                s.set_read_timeout(Some(t))?;
                s.set_write_timeout(Some(t))
            }
            Conn::Tcp(s) => {
                s.set_read_timeout(Some(t))?;
                s.set_write_timeout(Some(t))
            }
        }
    }
}

impl Read for Conn {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(unix)]
            Conn::Unix(s) => s.read(buf),
            Conn::Tcp(s) => s.read(buf),
        }
    }
}

impl Write for Conn {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(unix)]
            Conn::Unix(s) => s.write(buf),
            Conn::Tcp(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            #[cfg(unix)]
            Conn::Unix(s) => s.flush(),
            Conn::Tcp(s) => s.flush(),
        }
    }
}

/// Lee una línea JSON acotada.
fn read_json_line(r: impl Read, max: u64) -> Result<Value, BridgeError> {
    let mut reader = BufReader::new(r.take(max));
    let mut line = Vec::new();
    reader.read_until(b'\n', &mut line)?;
    if line.last() != Some(&b'\n') {
        return Err(BridgeError::Protocol("línea incompleta o demasiado larga".into()));
    }
    serde_json::from_slice(&line).map_err(|e| BridgeError::Protocol(format!("JSON: {e}")))
}

fn write_json_line(w: &mut impl Write, v: &Value) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(v).map_err(std::io::Error::other)?;
    line.push(b'\n');
    w.write_all(&line)?;
    w.flush()
}

// ------------------------------------------------------------------ servidor

/// Puente en marcha. Al soltarlo se detiene y se borra el punto de acceso.
pub struct BridgeHandle {
    stop: Arc<AtomicBool>,
    endpoint: Endpoint,
    endpoint_file: PathBuf,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for BridgeHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BridgeHandle").field("endpoint_file", &self.endpoint_file).finish_non_exhaustive()
    }
}

impl BridgeHandle {
    /// Archivo del punto de acceso (lo lee `iuredesk --mcp`).
    pub fn endpoint_file(&self) -> &Path {
        &self.endpoint_file
    }
}

impl Drop for BridgeHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = std::fs::remove_file(&self.endpoint_file);
        // Despierta al `accept` bloqueado.
        let _ = self.endpoint.connect();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        #[cfg(unix)]
        if let Endpoint::Unix(p) = &self.endpoint {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// Arranca el puente para el almacén de la app. Llamar una vez tras abrir el almacén y guardar
/// el [`BridgeHandle`] mientras dure la app.
pub fn start(store: Arc<Store>, opts: BridgeOptions) -> Result<BridgeHandle, BridgeError> {
    let dir = opts.runtime_dir.clone().unwrap_or_else(runtime_dir);
    private_dir(&dir)?;
    let file = endpoint_file(&dir, store.root());
    let token = new_token()?;

    // ¿Otro puente vivo para este mismo almacén?
    if let Some(c) = BridgeClient::from_file(&file)
        && c.endpoint.connect().is_ok()
    {
        return Err(BridgeError::AlreadyRunning);
    }

    #[cfg(unix)]
    let (listener, endpoint) = {
        let sock = dir.join(format!("bridge-{}.sock", store_key(store.root())));
        let _ = std::fs::remove_file(&sock);
        let l = std::os::unix::net::UnixListener::bind(&sock)?;
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o600))?;
        }
        (Listener::Unix(l), Endpoint::Unix(sock))
    };
    #[cfg(not(unix))]
    let (listener, endpoint) = {
        let l = TcpListener::bind(("127.0.0.1", 0))?;
        let addr = l.local_addr()?;
        (Listener::Tcp(l), Endpoint::Tcp(addr))
    };

    let mut info = endpoint.to_json();
    if let Some(o) = info.as_object_mut() {
        o.insert("v".into(), json!(BRIDGE_VERSION));
        o.insert("token".into(), json!(token));
        o.insert("store".into(), json!(store.root()));
        o.insert("pid".into(), json!(std::process::id()));
    }
    write_private(&file, info.to_string().as_bytes())?;

    let stop = Arc::new(AtomicBool::new(false));
    let active = Arc::new(AtomicUsize::new(0));
    let stop2 = stop.clone();
    let token = Arc::new(token);
    let thread = std::thread::Builder::new().name("iuredesk-mcp-bridge".into()).spawn(move || {
        loop {
            let conn = listener.accept();
            if stop2.load(Ordering::SeqCst) {
                break;
            }
            let Ok(conn) = conn else {
                std::thread::sleep(Duration::from_millis(50));
                continue;
            };
            if active.load(Ordering::SeqCst) >= MAX_CONNECTIONS {
                continue; // se suelta: el cliente verá la conexión cerrada
            }
            active.fetch_add(1, Ordering::SeqCst);
            let (store, opts, token, done) = (store.clone(), opts.clone(), token.clone(), active.clone());
            let spawned = std::thread::Builder::new().name("iuredesk-mcp-call".into()).spawn(move || {
                serve_conn(conn, &store, &opts, &token);
                done.fetch_sub(1, Ordering::SeqCst);
            });
            if spawned.is_err() {
                // No se pudo crear el hilo: la conexión se soltó con él.
                active.fetch_sub(1, Ordering::SeqCst);
            }
        }
    })?;
    Ok(BridgeHandle { stop, endpoint, endpoint_file: file, thread: Some(thread) })
}

fn serve_conn(mut conn: Conn, store: &Store, opts: &BridgeOptions, token: &str) {
    if conn.set_timeouts(IO_TIMEOUT).is_err() {
        return;
    }
    let reply = match read_json_line(&mut conn, MAX_REQUEST) {
        Err(e) => json!({"ok": false, "kind": "params", "error": e.to_string()}),
        Ok(req) => handle(&req, store, opts, token),
    };
    let _ = write_json_line(&mut conn, &reply);
}

fn handle(req: &Value, store: &Store, opts: &BridgeOptions, token: &str) -> Value {
    let given = req.get("token").and_then(Value::as_str).unwrap_or("");
    if !same(given, token) {
        return json!({"ok": false, "kind": "denied", "error": "token"});
    }
    if req.get("v").and_then(Value::as_u64) != Some(BRIDGE_VERSION) {
        return json!({"ok": false, "kind": "params", "error": "versión del puente"});
    }
    let tool = req.get("tool").and_then(Value::as_str).unwrap_or("");
    let args = req.get("args").cloned().unwrap_or_else(|| json!({}));
    let ctx = opts.context.clone().unwrap_or_else(ToolContext::system);
    let out = dispatch(store, tool, &args, &ctx);
    if out.is_ok()
        && is_write(tool)
        && let Some(f) = &opts.on_change
    {
        f(tool);
    }
    match out {
        Ok(ToolOutput::Data(v)) => json!({"ok": true, "kind": "data", "value": v}),
        Ok(ToolOutput::Text(t)) => json!({"ok": true, "kind": "text", "value": t}),
        Err(ToolError::Params(m)) => json!({"ok": false, "kind": "params", "error": m}),
        Err(ToolError::Failed(m)) => json!({"ok": false, "kind": "failed", "error": m}),
    }
}

// ------------------------------------------------------------------ cliente

/// Por qué falló una llamada por el puente.
#[derive(Debug, thiserror::Error)]
pub enum BridgeCallError {
    /// La herramienta respondió con un error (se pasa tal cual al modelo).
    #[error("{0}")]
    Tool(ToolError),
    /// El puente no contestó o contestó algo raro: probar otra vía.
    #[error("{0}")]
    Unreachable(BridgeError),
}

/// Cliente del puente (lo usa `iuredesk --mcp`).
#[derive(Clone, Debug)]
pub struct BridgeClient {
    endpoint: Endpoint,
    token: String,
}

impl BridgeClient {
    fn from_file(file: &Path) -> Option<Self> {
        let meta = std::fs::metadata(file).ok()?;
        if meta.len() > 64 * 1024 {
            return None;
        }
        let v: Value = serde_json::from_slice(&std::fs::read(file).ok()?).ok()?;
        if v.get("v").and_then(Value::as_u64) != Some(BRIDGE_VERSION) {
            return None;
        }
        let token = v.get("token")?.as_str()?.to_string();
        Some(Self { endpoint: Endpoint::from_json(&v)?, token })
    }

    /// Busca el puente de la app que tiene abierto `store_dir`. `None` si no hay archivo de punto
    /// de acceso (la app no está abierta) o no es válido.
    pub fn discover(store_dir: &Path, runtime: Option<&Path>) -> Option<Self> {
        let dir = runtime.map(Path::to_path_buf).unwrap_or_else(runtime_dir);
        Self::from_file(&endpoint_file(&dir, store_dir))
    }

    /// Llama a una herramienta en la app.
    pub fn call(&self, tool: &str, args: &Value) -> Result<ToolOutput, BridgeCallError> {
        let un = BridgeCallError::Unreachable;
        let mut conn = self.endpoint.connect().map_err(|e| un(e.into()))?;
        let req = json!({"v": BRIDGE_VERSION, "token": self.token, "tool": tool, "args": args});
        write_json_line(&mut conn, &req).map_err(|e| un(e.into()))?;
        let r = read_json_line(&mut conn, MAX_RESPONSE).map_err(un)?;
        let kind = r.get("kind").and_then(Value::as_str).unwrap_or("");
        let value = r.get("value").cloned().unwrap_or(Value::Null);
        let error = r.get("error").and_then(Value::as_str).unwrap_or("").to_string();
        match (r.get("ok").and_then(Value::as_bool), kind) {
            (Some(true), "data") => Ok(ToolOutput::Data(value)),
            (Some(true), "text") => Ok(ToolOutput::Text(value.as_str().unwrap_or("").to_string())),
            (Some(false), "params") => Err(BridgeCallError::Tool(ToolError::Params(error))),
            (Some(false), "failed") => Err(BridgeCallError::Tool(ToolError::Failed(error))),
            _ => Err(un(BridgeError::Protocol(format!("respuesta inesperada: {kind} {error}")))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_y_claves() {
        assert!(same("abc", "abc"));
        assert!(!same("abc", "abd"));
        assert!(!same("abc", "abcd"));
        let t = new_token().unwrap();
        assert_eq!(t.len(), 64);
        assert_eq!(store_key(Path::new("/x/y")), store_key(Path::new("/x/y")));
        assert_ne!(store_key(Path::new("/x/y")), store_key(Path::new("/x/z")));
    }

    #[test]
    fn punto_de_acceso_remoto_rechazado() {
        let v = json!({"transport": "tcp", "addr": "10.0.0.1:80"});
        assert!(Endpoint::from_json(&v).is_none());
        let v = json!({"transport": "tcp", "addr": "127.0.0.1:80"});
        assert!(Endpoint::from_json(&v).is_some());
    }

    #[test]
    fn lineas_acotadas() {
        assert!(read_json_line(&b"{\"a\":1}\n"[..], 100).is_ok());
        assert!(read_json_line(&b"{\"a\":1}"[..], 100).is_err());
        assert!(read_json_line(&b"{\"a\":11111111}\n"[..], 5).is_err());
    }
}
