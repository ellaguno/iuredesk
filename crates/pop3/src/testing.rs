//! Servidor POP3 falso (solo con la *feature* `test-server`): CAPA, USER/PASS, APOP, AUTH
//! XOAUTH2/PLAIN, STAT, LIST, UIDL, RETR, TOP, DELE, RSET, NOOP, QUIT (con borrado al salir).

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use base64::Engine;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

use crate::{Pop3Config, apop_digest};
use iuredesk_model::account::Security;

/// Usuario de pruebas.
pub const USER: &str = "ana@example.com";
/// Contraseña de pruebas.
pub const PASSWORD: &str = "secreta";
/// Token OAuth de pruebas.
pub const TOKEN: &str = "ya29.pop";
const STAMP: &str = "<1896.697170952@fake.example>";

struct State {
    /// `(uidl, mensaje)`.
    messages: Vec<(String, Vec<u8>)>,
    log: Vec<String>,
}

/// Servidor POP3 falso.
#[derive(Clone)]
pub struct FakePop3 {
    state: Arc<Mutex<State>>,
    port: u16,
}

impl std::fmt::Debug for FakePop3 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakePop3").field("port", &self.port).finish_non_exhaustive()
    }
}

impl FakePop3 {
    /// Arranca el servidor vacío.
    pub async fn start() -> std::io::Result<Self> {
        let l = TcpListener::bind("127.0.0.1:0").await?;
        let port = l.local_addr()?.port();
        let state = Arc::new(Mutex::new(State { messages: Vec::new(), log: Vec::new() }));
        let st = Arc::clone(&state);
        tokio::spawn(async move {
            while let Ok((sock, _)) = l.accept().await {
                let st = Arc::clone(&st);
                tokio::spawn(async move {
                    let _ = serve(sock, &st).await;
                });
            }
        });
        Ok(Self { state, port })
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Configuración de cliente (texto plano).
    pub fn config(&self) -> Pop3Config {
        let mut c = Pop3Config::new("127.0.0.1", self.port, Security::Plain);
        c.io_timeout = std::time::Duration::from_secs(10);
        c
    }

    /// Añade un mensaje.
    pub fn deliver(&self, uidl: &str, raw: &[u8]) {
        self.lock().messages.push((uidl.to_string(), raw.to_vec()));
    }

    /// UIDLs presentes.
    pub fn uidls(&self) -> Vec<String> {
        self.lock().messages.iter().map(|m| m.0.clone()).collect()
    }

    /// Órdenes recibidas (sin secretos).
    pub fn log(&self) -> Vec<String> {
        self.lock().log.clone()
    }
}

fn lock(st: &Mutex<State>) -> MutexGuard<'_, State> {
    st.lock().unwrap_or_else(PoisonError::into_inner)
}

async fn serve(sock: TcpStream, st: &Mutex<State>) -> std::io::Result<()> {
    let mut r = BufReader::new(sock);
    r.get_mut().write_all(format!("+OK Fake POP3 lista {STAMP}\r\n").as_bytes()).await?;
    let mut user: Option<String> = None;
    let mut authed = false;
    let mut deleted: Vec<usize> = Vec::new();
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).await? == 0 {
            return Ok(());
        }
        let line = line.trim_end().to_string();
        let mut w = line.splitn(3, ' ');
        let cmd = w.next().unwrap_or("").to_ascii_uppercase();
        let a1 = w.next().unwrap_or("").to_string();
        let a2 = w.next().unwrap_or("").to_string();
        lock(st).log.push(if matches!(cmd.as_str(), "PASS" | "APOP" | "AUTH") { cmd.clone() } else { line.clone() });
        let mut out: Vec<u8> = Vec::new();
        let msgs: Vec<(String, Vec<u8>)> = lock(st).messages.clone();
        let get = |n: &str| -> Option<(usize, &(String, Vec<u8>))> {
            let i: usize = n.parse::<usize>().ok()?.checked_sub(1)?;
            if deleted.contains(&i) {
                return None;
            }
            msgs.get(i).map(|m| (i, m))
        };
        match cmd.as_str() {
            "CAPA" => out.extend(b"+OK\r\nUSER\r\nUIDL\r\nTOP\r\nSASL PLAIN XOAUTH2\r\n.\r\n"),
            "USER" => {
                user = Some(a1);
                out.extend(b"+OK\r\n");
            }
            "PASS" => {
                if user.as_deref() == Some(USER) && line.get(5..) == Some(PASSWORD) {
                    authed = true;
                    out.extend(b"+OK dentro\r\n");
                } else {
                    out.extend(b"-ERR [AUTH] Authentication failed.\r\n");
                }
            }
            "APOP" => {
                if a1 == USER && a2 == apop_digest(STAMP, PASSWORD) {
                    authed = true;
                    out.extend(b"+OK dentro\r\n");
                } else {
                    out.extend(b"-ERR [AUTH] Authentication failed.\r\n");
                }
            }
            "AUTH" => {
                let d = base64::engine::general_purpose::STANDARD.decode(a2.trim()).unwrap_or_default();
                let d = String::from_utf8_lossy(&d).into_owned();
                let ok = match a1.to_ascii_uppercase().as_str() {
                    "XOAUTH2" => d == format!("user={USER}\x01auth=Bearer {TOKEN}\x01\x01"),
                    "PLAIN" => d == format!("\0{USER}\0{PASSWORD}"),
                    _ => false,
                };
                if ok {
                    authed = true;
                    out.extend(b"+OK dentro\r\n");
                } else {
                    r.get_mut().write_all(b"+ eyJzdGF0dXMiOiI0MDEifQ==\r\n").await?;
                    let mut ack = String::new();
                    r.read_line(&mut ack).await?;
                    out.extend(b"-ERR [AUTH] Authentication failed.\r\n");
                }
            }
            "QUIT" => {
                {
                    let mut s = lock(st);
                    let mut i = 0usize;
                    s.messages.retain(|_| {
                        let keep = !deleted.contains(&i);
                        i = i.saturating_add(1);
                        keep
                    });
                }
                r.get_mut().write_all(b"+OK adios\r\n").await?;
                return Ok(());
            }
            _ if !authed => out.extend(b"-ERR primero autenticate\r\n"),
            "STAT" => {
                let live: Vec<_> = msgs.iter().enumerate().filter(|(i, _)| !deleted.contains(i)).collect();
                let size: usize = live.iter().map(|(_, m)| m.1.len()).sum();
                out.extend(format!("+OK {} {size}\r\n", live.len()).into_bytes());
            }
            "LIST" | "UIDL" => {
                out.extend(b"+OK\r\n");
                for (i, m) in msgs.iter().enumerate() {
                    if deleted.contains(&i) {
                        continue;
                    }
                    let v = if cmd == "LIST" { m.1.len().to_string() } else { m.0.clone() };
                    out.extend(format!("{} {v}\r\n", i.saturating_add(1)).into_bytes());
                }
                out.extend(b".\r\n");
            }
            "RETR" | "TOP" => match get(&a1) {
                None => out.extend(b"-ERR no existe\r\n"),
                Some((_, m)) => {
                    out.extend(b"+OK\r\n");
                    let body = String::from_utf8_lossy(&m.1).into_owned();
                    let limit: Option<usize> = if cmd == "TOP" { a2.parse().ok() } else { None };
                    let mut in_body = false;
                    let mut body_lines = 0usize;
                    for l in body.strip_suffix("\r\n").unwrap_or(&body).split("\r\n") {
                        if in_body && limit.is_some_and(|n| body_lines >= n) {
                            break;
                        }
                        if in_body {
                            body_lines = body_lines.saturating_add(1);
                        }
                        if l.is_empty() {
                            in_body = true;
                        }
                        if l.starts_with('.') {
                            out.push(b'.');
                        }
                        out.extend(l.as_bytes());
                        out.extend(b"\r\n");
                    }
                    out.extend(b".\r\n");
                }
            },
            "DELE" => match get(&a1) {
                None => out.extend(b"-ERR no existe\r\n"),
                Some((i, _)) => {
                    deleted.push(i);
                    out.extend(b"+OK borrado\r\n");
                }
            },
            "RSET" => {
                deleted.clear();
                out.extend(b"+OK\r\n");
            }
            "NOOP" => out.extend(b"+OK\r\n"),
            _ => out.extend(b"-ERR orden desconocida\r\n"),
        }
        r.get_mut().write_all(&out).await?;
    }
}
