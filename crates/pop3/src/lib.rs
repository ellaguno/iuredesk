//! Cliente POP3 asíncrono (capa L3, RFC 1939 / 2449 / 2595 / 5034) y planificador de descarga
//! incremental por `UIDL` con «dejar copia en el servidor» y «borrar tras N días».
//!
//! El servidor es hostil: líneas y respuestas multilínea acotadas, cada lectura con tiempo de
//! espera, y la transparencia de puntos (`..`) deshecha con cuidado.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

mod plan;
#[cfg(feature = "test-server")]
pub mod testing;

use std::io;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use iuredesk_model::account::{Security, ServerConfig};
use iuredesk_net::{ClientConfig, NetError, Stream, map_io};
use md5::{Digest, Md5};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

pub use plan::{DownloadPlan, Pop3Policy, plan_download};

/// Tope de una línea.
const MAX_LINE: usize = 64 * 1024;
/// Líneas máximas de un listado (`UIDL`, `LIST`, `CAPA`).
const MAX_LIST_LINES: usize = 1_000_000;

/// Fallo POP3.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Pop3Error {
    /// Red.
    #[error(transparent)]
    Net(#[from] NetError),
    /// El servidor respondió `-ERR`.
    #[error("el servidor respondió: {0}")]
    Err(String),
    /// Credenciales rechazadas.
    #[error("autenticación rechazada: {0}")]
    AuthFailed(String),
    /// STARTTLS (STLS) pedido y no disponible.
    #[error("el servidor no ofrece STLS")]
    StartTlsUnavailable,
    /// Falta una capacidad.
    #[error("el servidor no admite {0}")]
    Unsupported(String),
    /// Respuesta por encima del tope.
    #[error("{0} demasiado grande")]
    TooLarge(&'static str),
    /// Respuesta inesperada.
    #[error("respuesta inesperada del servidor: {0}")]
    Protocol(String),
    /// Dato que no se puede enviar.
    #[error("dato no válido: {0}")]
    InvalidInput(String),
}

impl Pop3Error {
    /// `true` si conviene reintentar más tarde.
    pub fn is_transient(&self) -> bool {
        match self {
            Pop3Error::Net(e) => !matches!(e, NetError::TlsCertificate(_) | NetError::HostNotFound { .. }),
            Pop3Error::Err(t) => {
                let t = t.to_ascii_uppercase();
                t.contains("[IN-USE]") || t.contains("[SYS/TEMP]") || t.contains("LOCKED")
            }
            _ => false,
        }
    }

    /// `true` si son credenciales.
    pub fn is_auth(&self) -> bool {
        matches!(self, Pop3Error::AuthFailed(_))
    }
}

/// Resultado POP3.
pub type Result<T, E = Pop3Error> = std::result::Result<T, E>;

/// Dónde y cómo conectar.
#[derive(Clone)]
pub struct Pop3Config {
    /// Servidor.
    pub host: String,
    /// Puerto.
    pub port: u16,
    /// Seguridad.
    pub security: Security,
    /// Tiempo de cada operación.
    pub io_timeout: Duration,
    /// Tamaño máximo de un mensaje descargado.
    pub max_message: usize,
    /// TLS propio (pruebas).
    pub tls: Option<Arc<ClientConfig>>,
}

impl std::fmt::Debug for Pop3Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pop3Config").field("host", &self.host).field("port", &self.port).finish_non_exhaustive()
    }
}

impl Pop3Config {
    /// Valores por omisión.
    pub fn new(host: impl Into<String>, port: u16, security: Security) -> Self {
        Self {
            host: host.into(),
            port,
            security,
            io_timeout: Duration::from_secs(60),
            max_message: 64 * 1024 * 1024,
            tls: None,
        }
    }

    /// Desde el servidor entrante de la cuenta.
    pub fn from_server(s: &ServerConfig) -> Self {
        Self::new(s.host.clone(), s.port, s.security)
    }
}

fn check(s: &str) -> Result<()> {
    if s.is_empty() || s.bytes().any(|b| matches!(b, b'\r' | b'\n' | 0)) {
        return Err(Pop3Error::InvalidInput("vacío o con saltos de línea".into()));
    }
    Ok(())
}

/// Cliente POP3.
pub struct Pop3Client {
    r: BufReader<Stream>,
    cfg: Pop3Config,
    capa: Vec<String>,
    apop_stamp: Option<String>,
    greeting: String,
}

impl std::fmt::Debug for Pop3Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pop3Client").field("host", &self.cfg.host).finish_non_exhaustive()
    }
}

impl Pop3Client {
    /// Conecta, lee el saludo, pide `CAPA` y negocia STLS si toca.
    pub async fn connect(cfg: Pop3Config) -> Result<Self> {
        let tls = cfg.security == Security::Tls;
        let stream = iuredesk_net::connect_with(&cfg.host, cfg.port, tls, cfg.io_timeout, cfg.tls.clone()).await?;
        let mut c =
            Self { r: BufReader::new(stream), cfg, capa: Vec::new(), apop_stamp: None, greeting: String::new() };
        let greet = c.read_line().await?;
        let rest = ok_rest(&greet)?;
        c.greeting = rest.chars().take(300).collect();
        c.apop_stamp = apop_timestamp(rest);
        c.capa().await;
        if c.cfg.security == Security::StartTls {
            if !c.has_capa("STLS") {
                return Err(Pop3Error::StartTlsUnavailable);
            }
            c.command("STLS").await?;
            let Self { r, cfg, apop_stamp, greeting, .. } = c;
            let buffered = r.buffer().len();
            let stream =
                iuredesk_net::starttls(r.into_inner(), buffered, &cfg.host, cfg.io_timeout, cfg.tls.clone()).await?;
            c = Self { r: BufReader::new(stream), cfg, capa: Vec::new(), apop_stamp, greeting };
            c.capa().await;
        }
        Ok(c)
    }

    /// Saludo del servidor.
    pub fn greeting(&self) -> &str {
        &self.greeting
    }

    /// Capacidades (`CAPA`), en mayúsculas.
    pub fn capabilities(&self) -> &[String] {
        &self.capa
    }

    /// ¿Se anuncia `cap` (primera palabra de la línea)?
    pub fn has_capa(&self, cap: &str) -> bool {
        self.capa.iter().any(|c| c.split_whitespace().next().is_some_and(|w| w.eq_ignore_ascii_case(cap)))
    }

    /// ¿`SASL` incluye `mech`?
    pub fn has_sasl(&self, mech: &str) -> bool {
        self.capa.iter().any(|c| {
            let mut w = c.split_whitespace();
            w.next().is_some_and(|x| x.eq_ignore_ascii_case("SASL")) && w.any(|m| m.eq_ignore_ascii_case(mech))
        })
    }

    /// ¿Ofrece APOP (saludo con marca de tiempo)?
    pub fn has_apop(&self) -> bool {
        self.apop_stamp.is_some()
    }

    async fn capa(&mut self) {
        // `CAPA` es opcional (RFC 2449): si falla, no hay capacidades conocidas.
        if self.write_line("CAPA").await.is_err() {
            return;
        }
        let Ok(line) = self.read_line().await else { return };
        if ok_rest(&line).is_err() {
            return;
        }
        if let Ok(lines) = self.read_multiline_lines(4096).await {
            self.capa = lines.into_iter().map(|l| String::from_utf8_lossy(&l).to_ascii_uppercase()).collect();
        }
    }

    async fn write_line(&mut self, line: &str) -> Result<()> {
        let host = self.cfg.host.clone();
        let data = format!("{line}\r\n");
        let s = self.r.get_mut();
        match tokio::time::timeout(self.cfg.io_timeout, async {
            s.write_all(data.as_bytes()).await?;
            s.flush().await
        })
        .await
        {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(Pop3Error::Net(map_io(&e, &host))),
            Err(_) => Err(Pop3Error::Net(NetError::Timeout)),
        }
    }

    async fn read_raw_line(&mut self) -> Result<Vec<u8>> {
        let mut buf: Vec<u8> = Vec::new();
        let r = &mut self.r;
        let res = tokio::time::timeout(self.cfg.io_timeout, async {
            loop {
                let avail = r.fill_buf().await?;
                if avail.is_empty() {
                    return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
                }
                if let Some(pos) = avail.iter().position(|b| *b == b'\n') {
                    buf.extend_from_slice(avail.get(..pos).unwrap_or_default());
                    r.consume(pos.saturating_add(1));
                    return Ok(());
                }
                let n = avail.len();
                buf.extend_from_slice(avail);
                r.consume(n);
                if buf.len() > MAX_LINE {
                    return Err(io::Error::new(io::ErrorKind::InvalidInput, "línea"));
                }
            }
        })
        .await;
        match res {
            Ok(Ok(())) => {}
            Ok(Err(e)) if e.kind() == io::ErrorKind::InvalidInput => return Err(Pop3Error::TooLarge("línea")),
            Ok(Err(e)) => return Err(Pop3Error::Net(map_io(&e, &self.cfg.host))),
            Err(_) => return Err(Pop3Error::Net(NetError::Timeout)),
        }
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
        Ok(buf)
    }

    async fn read_line(&mut self) -> Result<String> {
        let l = self.read_raw_line().await?;
        Ok(String::from_utf8_lossy(&l).into_owned())
    }

    /// Lee un bloque multilínea hasta `.`, deshaciendo `..`; con tope de bytes y de líneas.
    async fn read_multiline(&mut self, max_bytes: usize) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        for _ in 0..MAX_LIST_LINES.saturating_mul(64) {
            let line = self.read_raw_line().await?;
            if line == b"." {
                return Ok(out);
            }
            let content = line.strip_prefix(b".").filter(|_| line.starts_with(b"..")).unwrap_or(&line);
            if out.len().saturating_add(content.len()).saturating_add(2) > max_bytes {
                return Err(Pop3Error::TooLarge("mensaje"));
            }
            out.extend_from_slice(content);
            out.extend_from_slice(b"\r\n");
        }
        Err(Pop3Error::TooLarge("mensaje"))
    }

    async fn read_multiline_lines(&mut self, max_lines: usize) -> Result<Vec<Vec<u8>>> {
        let mut out = Vec::new();
        loop {
            let line = self.read_raw_line().await?;
            if line == b"." {
                return Ok(out);
            }
            if out.len() >= max_lines {
                return Err(Pop3Error::TooLarge("listado"));
            }
            let content = line.strip_prefix(b".").filter(|_| line.starts_with(b"..")).unwrap_or(&line).to_vec();
            out.push(content);
        }
    }

    /// Envía una orden y devuelve el texto del `+OK`.
    async fn command(&mut self, cmd: &str) -> Result<String> {
        self.write_line(cmd).await?;
        let line = self.read_line().await?;
        ok_rest(&line).map(str::to_string)
    }

    /// `USER` + `PASS`.
    pub async fn login(&mut self, user: &str, pass: &str) -> Result<()> {
        check(user)?;
        check(pass)?;
        self.command(&format!("USER {user}")).await.map_err(auth_err)?;
        self.command(&format!("PASS {pass}")).await.map_err(auth_err)?;
        Ok(())
    }

    /// `APOP` (MD5 de la marca de tiempo del saludo + contraseña).
    pub async fn apop(&mut self, user: &str, pass: &str) -> Result<()> {
        check(user)?;
        check(pass)?;
        let stamp = self.apop_stamp.clone().ok_or_else(|| Pop3Error::Unsupported("APOP".into()))?;
        self.command(&format!("APOP {user} {}", apop_digest(&stamp, pass))).await.map_err(auth_err)?;
        Ok(())
    }

    /// `AUTH <mecanismo> <respuesta inicial base64>` (RFC 5034), p. ej. XOAUTH2.
    pub async fn auth_sasl(&mut self, mech: &str, initial_b64: &str) -> Result<()> {
        check(mech)?;
        check(initial_b64)?;
        if !mech.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
            return Err(Pop3Error::InvalidInput("mecanismo".into()));
        }
        self.write_line(&format!("AUTH {mech} {initial_b64}")).await?;
        for _ in 0..4 {
            let line = self.read_line().await?;
            if line.starts_with("+OK") {
                return Ok(());
            }
            if line.starts_with("-ERR") {
                return Err(Pop3Error::AuthFailed(clip(&line)));
            }
            if line.starts_with('+') {
                // Reto de error SASL (XOAUTH2): se contesta vacío y llega el -ERR.
                self.write_line("").await?;
                continue;
            }
            return Err(Pop3Error::Protocol(clip(&line)));
        }
        Err(Pop3Error::Protocol("intercambio SASL demasiado largo".into()))
    }

    /// `AUTH PLAIN`.
    pub async fn auth_plain(&mut self, user: &str, pass: &str) -> Result<()> {
        check(user)?;
        check(pass)?;
        let ir = base64::engine::general_purpose::STANDARD.encode(format!("\0{user}\0{pass}"));
        self.auth_sasl("PLAIN", &ir).await
    }

    /// `STAT` → (mensajes, octetos).
    pub async fn stat(&mut self) -> Result<(u32, u64)> {
        let rest = self.command("STAT").await?;
        let mut w = rest.split_whitespace();
        let n = w.next().and_then(|x| x.parse().ok());
        let size = w.next().and_then(|x| x.parse().ok());
        match (n, size) {
            (Some(n), Some(s)) => Ok((n, s)),
            _ => Err(Pop3Error::Protocol(format!("STAT: {}", clip(&rest)))),
        }
    }

    /// `LIST` → (número, octetos).
    pub async fn list(&mut self) -> Result<Vec<(u32, u64)>> {
        self.command("LIST").await?;
        let lines = self.read_multiline_lines(MAX_LIST_LINES).await?;
        Ok(lines
            .iter()
            .filter_map(|l| {
                let s = String::from_utf8_lossy(l);
                let mut w = s.split_whitespace();
                Some((w.next()?.parse().ok()?, w.next()?.parse().ok()?))
            })
            .collect())
    }

    /// `UIDL` → (número, id único). Los ids no válidos (RFC 1939: 1–70 caracteres imprimibles) se
    /// descartan.
    pub async fn uidl(&mut self) -> Result<Vec<(u32, String)>> {
        self.command("UIDL").await.map_err(|e| match e {
            Pop3Error::Err(_) => Pop3Error::Unsupported("UIDL".into()),
            other => other,
        })?;
        let lines = self.read_multiline_lines(MAX_LIST_LINES).await?;
        Ok(lines
            .iter()
            .filter_map(|l| {
                let s = String::from_utf8_lossy(l);
                let mut w = s.split_whitespace();
                let n: u32 = w.next()?.parse().ok()?;
                let id = w.next()?;
                (id.len() <= 70 && id.bytes().all(|b| (0x21..0x7f).contains(&b))).then(|| (n, id.to_string()))
            })
            .collect())
    }

    /// `RETR n` → mensaje RFC 822.
    pub async fn retr(&mut self, n: u32) -> Result<Vec<u8>> {
        self.command(&format!("RETR {n}")).await?;
        let max = self.cfg.max_message;
        self.read_multiline(max).await
    }

    /// `TOP n lines` → cabeceras + `lines` líneas del cuerpo.
    pub async fn top(&mut self, n: u32, lines: u32) -> Result<Vec<u8>> {
        self.command(&format!("TOP {n} {lines}")).await?;
        let max = self.cfg.max_message;
        self.read_multiline(max).await
    }

    /// `DELE n` (efectivo al `QUIT`).
    pub async fn dele(&mut self, n: u32) -> Result<()> {
        self.command(&format!("DELE {n}")).await.map(|_| ())
    }

    /// `RSET`.
    pub async fn rset(&mut self) -> Result<()> {
        self.command("RSET").await.map(|_| ())
    }

    /// `NOOP`.
    pub async fn noop(&mut self) -> Result<()> {
        self.command("NOOP").await.map(|_| ())
    }

    /// `QUIT`: confirma los borrados.
    pub async fn quit(mut self) -> Result<()> {
        self.command("QUIT").await.map(|_| ())
    }
}

fn clip(s: &str) -> String {
    s.chars().take(300).collect()
}

fn ok_rest(line: &str) -> Result<&str> {
    if let Some(r) = line.strip_prefix("+OK") {
        Ok(r.trim_start())
    } else if let Some(r) = line.strip_prefix("-ERR") {
        Err(Pop3Error::Err(clip(r.trim_start())))
    } else {
        Err(Pop3Error::Protocol(clip(line)))
    }
}

fn auth_err(e: Pop3Error) -> Pop3Error {
    match e {
        Pop3Error::Err(t) => Pop3Error::AuthFailed(t),
        other => other,
    }
}

/// `<…@…>` del saludo (APOP).
fn apop_timestamp(greeting: &str) -> Option<String> {
    let start = greeting.find('<')?;
    let end = greeting.get(start..)?.find('>')?.saturating_add(start);
    let stamp = greeting.get(start..=end)?;
    (stamp.contains('@') && stamp.len() <= 512 && stamp.is_ascii()).then(|| stamp.to_string())
}

/// MD5 hexadecimal de `stamp + pass`.
pub fn apop_digest(stamp: &str, pass: &str) -> String {
    let d = Md5::digest(format!("{stamp}{pass}").as_bytes());
    d.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apop_vectors() {
        // RFC 1939 §7.
        assert_eq!(apop_digest("<1896.697170952@dbc.mtview.ca.us>", "tanstaaf"), "c4c9334bac560ecc979e58001b3e22fb");
        assert_eq!(
            apop_timestamp("POP3 server ready <1896.697170952@dbc.mtview.ca.us>").as_deref(),
            Some("<1896.697170952@dbc.mtview.ca.us>")
        );
        assert_eq!(apop_timestamp("listo"), None);
        assert_eq!(apop_timestamp("<sin arroba>"), None);
    }

    #[test]
    fn ok_or_err() {
        assert_eq!(ok_rest("+OK 2 320").unwrap(), "2 320");
        assert!(matches!(ok_rest("-ERR no"), Err(Pop3Error::Err(_))));
        assert!(matches!(ok_rest("basura"), Err(Pop3Error::Protocol(_))));
    }
}
