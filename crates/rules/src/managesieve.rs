//! Cliente ManageSieve (RFC 5804): sube, activa y lee scripts Sieve del servidor.
//!
//! Puerto 4190 con STARTTLS obligatorio (salvo `127.0.0.1` en pruebas), `AUTHENTICATE "PLAIN"`
//! (y `XOAUTH2`/`OAUTHBEARER` con token), literales `{n+}`, respuestas y líneas acotadas, tiempo de
//! espera en cada paso.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use iuredesk_net::{ClientConfig, Stream};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

use crate::error::{Result, RulesError};

/// Puerto estándar.
pub const PORT: u16 = 4190;
/// Línea máxima.
const MAX_LINE: usize = 64 * 1024;
/// Literal máximo (un script).
const MAX_LITERAL: usize = crate::sieve::MAX_SCRIPT;
/// Líneas máximas de una respuesta.
const MAX_LINES: usize = 10_000;

/// Cifrado de la conexión.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Security {
    /// STARTTLS (lo normal en 4190).
    #[default]
    StartTls,
    /// TLS desde el primer byte.
    Tls,
    /// Sin cifrar: solo se acepta con `127.0.0.1`/`localhost` (pruebas).
    PlainLoopback,
}

/// Credenciales.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Auth {
    /// Usuario y contraseña (SASL PLAIN).
    Password {
        /// Usuario.
        user: String,
        /// Contraseña.
        password: String,
    },
    /// Token OAuth 2 (`OAUTHBEARER` o `XOAUTH2`).
    OAuth {
        /// Usuario.
        user: String,
        /// Token de acceso.
        token: String,
    },
}

/// Configuración de la conexión.
#[derive(Clone)]
pub struct Config {
    /// Servidor.
    pub host: String,
    /// Puerto.
    pub port: u16,
    /// Cifrado.
    pub security: Security,
    /// Tiempo de cada paso.
    pub timeout: Duration,
    /// Raíces TLS propias (pruebas).
    pub tls: Option<Arc<ClientConfig>>,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config").field("host", &self.host).field("port", &self.port).finish_non_exhaustive()
    }
}

impl Config {
    /// Servidor en el puerto 4190 con STARTTLS.
    pub fn new(host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            port: PORT,
            security: Security::StartTls,
            timeout: Duration::from_secs(20),
            tls: None,
        }
    }
}

/// Capacidades anunciadas.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Capabilities {
    /// Servidor (`Dovecot Pigeonhole`).
    pub implementation: String,
    /// Extensiones Sieve, en minúsculas.
    pub sieve: Vec<String>,
    /// Ofrece STARTTLS.
    pub starttls: bool,
    /// Mecanismos SASL, en mayúsculas.
    pub sasl: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Item {
    Atom(String),
    Str(String),
}

impl Item {
    fn text(&self) -> &str {
        match self {
            Item::Atom(s) | Item::Str(s) => s,
        }
    }
}

#[derive(Debug)]
struct Response {
    lines: Vec<Vec<Item>>,
    ok: bool,
    bye: bool,
    text: String,
}

/// Conexión autenticada.
pub struct Client {
    io: BufReader<Stream>,
    caps: Capabilities,
    timeout: Duration,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client").field("caps", &self.caps).finish_non_exhaustive()
    }
}

fn net(e: iuredesk_net::NetError) -> RulesError {
    match e {
        iuredesk_net::NetError::Timeout => RulesError::Timeout,
        other => RulesError::Net(other.to_string()),
    }
}

fn io_err(e: std::io::Error) -> RulesError {
    RulesError::Net(e.to_string())
}

fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        if c != '\r' && c != '\n' {
            out.push(c);
        }
    }
    out.push('"');
    out
}

async fn timed<T>(d: Duration, f: impl std::future::Future<Output = Result<T>>) -> Result<T> {
    tokio::time::timeout(d, f).await.map_err(|_| RulesError::Timeout)?
}

/// Lee una línea lógica (con literales `{n}` / `{n+}` dentro).
async fn read_items(io: &mut BufReader<Stream>) -> Result<Vec<Item>> {
    let mut items = Vec::new();
    loop {
        let mut buf = Vec::new();
        let n = (&mut *io).take(MAX_LINE as u64 + 1).read_until(b'\n', &mut buf).await.map_err(io_err)?;
        if n == 0 {
            return Err(RulesError::Net("el servidor cerró la conexión".into()));
        }
        if buf.len() > MAX_LINE {
            return Err(RulesError::Server("línea demasiado larga".into()));
        }
        let line = String::from_utf8_lossy(&buf).trim_end_matches(['\r', '\n']).to_string();
        let literal = parse_line(&line, &mut items)?;
        match literal {
            None => return Ok(items),
            Some(len) => {
                if len > MAX_LITERAL {
                    return Err(RulesError::Server("respuesta demasiado grande".into()));
                }
                let mut data = vec![0u8; len];
                io.read_exact(&mut data).await.map_err(io_err)?;
                items.push(Item::Str(String::from_utf8_lossy(&data).into_owned()));
                // Lo que sigue al literal continúa la misma línea lógica.
            }
        }
        if items.len() > 10_000 {
            return Err(RulesError::Server("respuesta demasiado larga".into()));
        }
    }
}

/// Parte una línea en átomos y textos; devuelve el tamaño de un literal final, si lo hay.
fn parse_line(line: &str, items: &mut Vec<Item>) -> Result<Option<usize>> {
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0usize;
    while let Some(&c) = chars.get(i) {
        match c {
            ' ' | '\t' => i += 1,
            '"' => {
                i += 1;
                let mut s = String::new();
                loop {
                    match chars.get(i) {
                        None => return Err(RulesError::Server("texto sin cerrar".into())),
                        Some('"') => {
                            i += 1;
                            break;
                        }
                        Some('\\') => {
                            if let Some(n) = chars.get(i + 1) {
                                s.push(*n);
                            }
                            i += 2;
                        }
                        Some(c) => {
                            s.push(*c);
                            i += 1;
                        }
                    }
                }
                items.push(Item::Str(s));
            }
            '{' => {
                let rest: String = chars.get(i + 1..).unwrap_or_default().iter().collect();
                let inner = rest.split('}').next().unwrap_or("");
                let num = inner.trim_end_matches('+');
                let len: usize = num.parse().map_err(|_| RulesError::Server("literal mal formado".into()))?;
                return Ok(Some(len));
            }
            '(' => {
                // Código de respuesta: `(QUOTA/MAXSIZE)`, se guarda como átomo.
                let start = i;
                while chars.get(i).is_some_and(|c| *c != ')') {
                    i += 1;
                }
                i += 1;
                items.push(Item::Atom(chars.get(start..i.min(chars.len())).unwrap_or_default().iter().collect()));
            }
            _ => {
                let start = i;
                while chars.get(i).is_some_and(|c| !c.is_whitespace()) {
                    i += 1;
                }
                items.push(Item::Atom(chars.get(start..i).unwrap_or_default().iter().collect()));
            }
        }
    }
    Ok(None)
}

async fn read_response(io: &mut BufReader<Stream>) -> Result<Response> {
    let mut lines = Vec::new();
    for _ in 0..MAX_LINES {
        let items = read_items(io).await?;
        let head = items.first().map(|i| i.text().to_ascii_uppercase());
        if let (Some(Item::Atom(_)), Some(h)) = (items.first(), head.as_deref())
            && matches!(h, "OK" | "NO" | "BYE")
        {
            let text = items.iter().skip(1).map(Item::text).collect::<Vec<_>>().join(" ");
            return Ok(Response { lines, ok: h == "OK", bye: h == "BYE", text });
        }
        lines.push(items);
    }
    Err(RulesError::Server("respuesta demasiado larga".into()))
}

fn capabilities(lines: &[Vec<Item>]) -> Capabilities {
    let mut c = Capabilities::default();
    for l in lines {
        let key = l.first().map(|i| i.text().to_ascii_uppercase()).unwrap_or_default();
        let val = l.get(1).map(Item::text).unwrap_or("");
        match key.as_str() {
            "IMPLEMENTATION" => c.implementation = val.to_string(),
            "SIEVE" => c.sieve = val.split_whitespace().map(|s| s.to_ascii_lowercase()).collect(),
            "STARTTLS" => c.starttls = true,
            "SASL" => c.sasl = val.split_whitespace().map(|s| s.to_ascii_uppercase()).collect(),
            _ => {}
        }
    }
    c
}

fn is_loopback(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost" | "::1" | "[::1]")
}

async fn open(cfg: &Config) -> Result<(BufReader<Stream>, Capabilities)> {
    if cfg.security == Security::PlainLoopback && !is_loopback(&cfg.host) {
        return Err(RulesError::Invalid("sin cifrar solo en este equipo".into()));
    }
    let tls = cfg.security == Security::Tls;
    let stream =
        iuredesk_net::connect_with(&cfg.host, cfg.port, tls, cfg.timeout, cfg.tls.clone()).await.map_err(net)?;
    let mut io = BufReader::new(stream);
    let greet = timed(cfg.timeout, read_response(&mut io)).await?;
    if !greet.ok {
        return Err(RulesError::Server(greet.text));
    }
    let mut caps = capabilities(&greet.lines);
    if cfg.security == Security::StartTls {
        if !caps.starttls {
            return Err(RulesError::Net("el servidor no ofrece una conexión cifrada".into()));
        }
        io.get_mut().write_all(b"STARTTLS\r\n").await.map_err(io_err)?;
        let r = timed(cfg.timeout, read_response(&mut io)).await?;
        if !r.ok {
            return Err(RulesError::Server(r.text));
        }
        let buffered = io.buffer().len();
        let stream = io.into_inner();
        let stream =
            iuredesk_net::starttls(stream, buffered, &cfg.host, cfg.timeout, cfg.tls.clone()).await.map_err(net)?;
        io = BufReader::new(stream);
        // RFC 5804 §2.2: tras STARTTLS el servidor vuelve a mandar sus capacidades.
        let again = timed(cfg.timeout, read_response(&mut io)).await?;
        caps = capabilities(&again.lines);
    }
    Ok((io, caps))
}

/// Pregunta al servidor qué sabe hacer, sin iniciar sesión (detección).
pub async fn probe(cfg: &Config) -> Result<Capabilities> {
    let (mut io, caps) = open(cfg).await?;
    let _ = io.get_mut().write_all(b"LOGOUT\r\n").await;
    Ok(caps)
}

impl Client {
    /// Conecta e inicia sesión.
    pub async fn connect(cfg: &Config, auth: &Auth) -> Result<Client> {
        let (io, caps) = open(cfg).await?;
        let mut c = Client { io, caps, timeout: cfg.timeout };
        c.authenticate(auth).await?;
        Ok(c)
    }

    /// Capacidades (las de después de cifrar).
    pub fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    async fn command(&mut self, line: &str, literal: Option<&str>) -> Result<Response> {
        let mut out = line.as_bytes().to_vec();
        if let Some(body) = literal {
            out.extend_from_slice(format!(" {{{}+}}\r\n", body.len()).as_bytes());
            out.extend_from_slice(body.as_bytes());
        }
        out.extend_from_slice(b"\r\n");
        let t = self.timeout;
        self.io.get_mut().write_all(&out).await.map_err(io_err)?;
        self.io.get_mut().flush().await.map_err(io_err)?;
        let r = timed(t, read_response(&mut self.io)).await?;
        if r.bye {
            return Err(RulesError::Server(r.text));
        }
        Ok(r)
    }

    async fn authenticate(&mut self, auth: &Auth) -> Result<()> {
        let b64 = |s: &str| base64::engine::general_purpose::STANDARD.encode(s.as_bytes());
        let (mech, initial) = match auth {
            Auth::Password { user, password } => ("PLAIN", b64(&format!("\0{user}\0{password}"))),
            Auth::OAuth { user, token } if self.caps.sasl.iter().any(|m| m == "OAUTHBEARER") => {
                ("OAUTHBEARER", b64(&format!("n,a={user},\x01auth=Bearer {token}\x01\x01")))
            }
            Auth::OAuth { user, token } => ("XOAUTH2", b64(&format!("user={user}\x01auth=Bearer {token}\x01\x01"))),
        };
        if !self.caps.sasl.is_empty() && !self.caps.sasl.iter().any(|m| m == mech) {
            return Err(RulesError::Server(format!("el servidor no admite {mech}")));
        }
        let r = self.command(&format!("AUTHENTICATE {} {}", quote(mech), quote(&initial)), None).await?;
        if !r.ok {
            return Err(RulesError::Auth);
        }
        Ok(())
    }

    /// Scripts y cuál está activo.
    pub async fn list_scripts(&mut self) -> Result<Vec<(String, bool)>> {
        let r = self.command("LISTSCRIPTS", None).await?;
        if !r.ok {
            return Err(RulesError::Server(r.text));
        }
        Ok(r.lines
            .iter()
            .filter_map(|l| {
                let name = l.first()?.text().to_string();
                let active = l.get(1).is_some_and(|a| a.text().eq_ignore_ascii_case("ACTIVE"));
                Some((name, active))
            })
            .collect())
    }

    /// Lee un script.
    pub async fn get_script(&mut self, name: &str) -> Result<String> {
        let r = self.command(&format!("GETSCRIPT {}", quote(name)), None).await?;
        if !r.ok {
            return Err(RulesError::Server(r.text));
        }
        Ok(r.lines.first().and_then(|l| l.first()).map(|i| i.text().to_string()).unwrap_or_default())
    }

    /// Comprueba un script sin guardarlo (`CHECKSCRIPT`, RFC 5804 §2.12).
    pub async fn check_script(&mut self, content: &str) -> Result<()> {
        let r = self.command("CHECKSCRIPT", Some(&crlf(content))).await?;
        if r.ok { Ok(()) } else { Err(RulesError::Server(r.text)) }
    }

    /// Guarda un script.
    pub async fn put_script(&mut self, name: &str, content: &str) -> Result<()> {
        let r = self.command(&format!("PUTSCRIPT {}", quote(name)), Some(&crlf(content))).await?;
        if r.ok { Ok(()) } else { Err(RulesError::Server(r.text)) }
    }

    /// Activa un script (vacío = ninguno).
    pub async fn set_active(&mut self, name: &str) -> Result<()> {
        let r = self.command(&format!("SETACTIVE {}", quote(name)), None).await?;
        if r.ok { Ok(()) } else { Err(RulesError::Server(r.text)) }
    }

    /// Cierra la sesión.
    pub async fn logout(mut self) {
        let _ = self.command("LOGOUT", None).await;
    }
}

fn crlf(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\n', "\r\n")
}

/// Sube el script de IureDesk y lo activa. Si ya había otro script activo (escrito a mano o de
/// otro cliente), no se pisa: se devuelve su nombre y el nuestro queda guardado sin activar.
pub async fn upload(cfg: &Config, auth: &Auth, script: &str) -> Result<UploadOutcome> {
    let mut c = Client::connect(cfg, auth).await?;
    let caps = c.caps.clone();
    let scripts = c.list_scripts().await?;
    let other_active = scripts.iter().find(|(n, a)| *a && n != crate::sieve::SCRIPT_NAME).map(|(n, _)| n.clone());
    c.put_script(crate::sieve::SCRIPT_NAME, script).await?;
    if other_active.is_none() {
        c.set_active(crate::sieve::SCRIPT_NAME).await?;
    }
    c.logout().await;
    Ok(UploadOutcome { capabilities: caps, other_active })
}

/// Resultado de [`upload`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadOutcome {
    /// Capacidades del servidor.
    pub capabilities: Capabilities,
    /// Script ajeno que estaba activo (el nuestro no se activó).
    pub other_active: Option<String>,
}

/// Servidores donde no hay ManageSieve (no se intenta: Gmail, Microsoft 365, iCloud, Yahoo).
pub fn known_without_sieve(host: &str) -> bool {
    let h = host.to_ascii_lowercase();
    [
        "gmail.com",
        "googlemail.com",
        "office365.com",
        "outlook.com",
        "hotmail.com",
        "live.com",
        "icloud.com",
        "me.com",
        "yahoo.com",
        "aol.com",
    ]
    .iter()
    .any(|d| h == *d || h.ends_with(&format!(".{d}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lines_and_codes() {
        let mut items = Vec::new();
        assert_eq!(parse_line(r#""SIEVE" "fileinto vacation""#, &mut items).unwrap(), None);
        assert_eq!(items, vec![Item::Str("SIEVE".into()), Item::Str("fileinto vacation".into())]);
        let mut items = Vec::new();
        parse_line(r#"NO (QUOTA/MAXSIZE) "Demasiado grande""#, &mut items).unwrap();
        assert_eq!(items[1], Item::Atom("(QUOTA/MAXSIZE)".into()));
        let mut items = Vec::new();
        assert_eq!(parse_line("{12}", &mut items).unwrap(), Some(12));
        assert_eq!(parse_line("{3+}", &mut Vec::new()).unwrap(), Some(3));
        assert!(parse_line("{x}", &mut Vec::new()).is_err());
        assert!(parse_line("\"abierto", &mut Vec::new()).is_err());
    }

    #[test]
    fn known_hosts() {
        assert!(known_without_sieve("imap.gmail.com"));
        assert!(known_without_sieve("outlook.office365.com"));
        assert!(!known_without_sieve("mail.despacho.example"));
    }

    #[tokio::test]
    async fn refuses_plain_to_remote() {
        let cfg = Config { security: Security::PlainLoopback, ..Config::new("mail.example.com") };
        assert!(matches!(probe(&cfg).await, Err(RulesError::Invalid(_))));
    }
}
