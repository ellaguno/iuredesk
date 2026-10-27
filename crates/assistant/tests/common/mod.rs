//! Servidor HTTP falso y programable: cada conexión recibe la siguiente respuesta de la lista
//! (con `Connection: close`), opcionalmente en trozos con pausa (flujo SSE). Graba las peticiones.
#![allow(dead_code)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Debug, Clone)]
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub chunks: Vec<Vec<u8>>,
    /// Pausa antes de responder.
    pub delay: Duration,
    /// Pausa entre trozos.
    pub gap: Duration,
}

impl Reply {
    pub fn json(status: u16, body: &str) -> Self {
        Self {
            status,
            headers: vec![("Content-Type".into(), "application/json".into())],
            chunks: vec![body.as_bytes().to_vec()],
            delay: Duration::ZERO,
            gap: Duration::ZERO,
        }
    }

    /// Flujo SSE: cada elemento es un trozo de bytes tal cual.
    pub fn sse(chunks: &[&str]) -> Self {
        Self {
            status: 200,
            headers: vec![("Content-Type".into(), "text/event-stream".into())],
            chunks: chunks.iter().map(|c| c.as_bytes().to_vec()).collect(),
            delay: Duration::ZERO,
            gap: Duration::from_millis(5),
        }
    }

    pub fn header(mut self, k: &str, v: &str) -> Self {
        self.headers.push((k.into(), v.into()));
        self
    }

    pub fn delayed(mut self, d: Duration) -> Self {
        self.delay = d;
        self
    }
}

#[derive(Debug, Clone)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Recorded {
    pub fn header(&self, k: &str) -> Option<&str> {
        self.headers.iter().find(|(h, _)| h.eq_ignore_ascii_case(k)).map(|(_, v)| v.as_str())
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).unwrap()
    }
}

pub struct FakeServer {
    pub url: String,
    pub requests: Arc<Mutex<Vec<Recorded>>>,
}

impl FakeServer {
    pub fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }
}

async fn read_request(sock: &mut tokio::net::TcpStream) -> Option<Recorded> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
    let header_end = loop {
        let n = sock.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = head.split("\r\n");
    let first = lines.next()?.to_string();
    let mut parts = first.split(' ');
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    let headers: Vec<(String, String)> =
        lines.filter_map(|l| l.split_once(':').map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))).collect();
    let len = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse::<usize>().ok())
        .unwrap_or(0);
    while buf.len() < header_end + len {
        let n = sock.read(&mut tmp).await.ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    let body = String::from_utf8_lossy(&buf[header_end..]).to_string();
    Some(Recorded { method, path, headers, body })
}

/// Arranca el servidor con las respuestas en orden (la última se repite si se acaban).
pub async fn serve(replies: Vec<Reply>) -> FakeServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let rec = requests.clone();
    tokio::spawn(async move {
        let mut i = 0usize;
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { return };
            let reply = replies.get(i).or(replies.last()).cloned().unwrap();
            i += 1;
            let rec = rec.clone();
            tokio::spawn(async move {
                let Some(req) = read_request(&mut sock).await else { return };
                rec.lock().unwrap().push(req);
                tokio::time::sleep(reply.delay).await;
                let mut head = format!("HTTP/1.1 {} X\r\nConnection: close\r\n", reply.status);
                for (k, v) in &reply.headers {
                    head.push_str(&format!("{k}: {v}\r\n"));
                }
                head.push_str("\r\n");
                if sock.write_all(head.as_bytes()).await.is_err() {
                    return;
                }
                for c in &reply.chunks {
                    if sock.write_all(c).await.is_err() {
                        return;
                    }
                    sock.flush().await.ok();
                    tokio::time::sleep(reply.gap).await;
                }
                sock.shutdown().await.ok();
            });
        }
    });
    FakeServer { url: format!("http://127.0.0.1:{port}"), requests }
}
