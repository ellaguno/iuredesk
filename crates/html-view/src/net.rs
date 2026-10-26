//! Proveedor de red de Blitz: la única puerta por la que el documento obtiene recursos.
//!
//! - `cid:` → [`RenderOptions::inline_resources`](crate::RenderOptions).
//! - `data:` → se decodifica aquí (base64 o %-codificado).
//! - `http(s):` → solo si se permiten imágenes remotas y hay [`Fetcher`]; si no, se anota en la
//!   lista de bloqueados.
//! - Cualquier otro esquema (`file:`…) → nada.
//!
//! Siempre se responde (con bytes vacíos si no hay recurso) y en el acto: así Blitz nunca se queda
//! esperando una hoja de estilo «crítica» que no va a llegar.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use blitz_traits::net::{Bytes, NetHandler, NetProvider, Request};

use crate::Fetcher;

pub(crate) struct MailNet {
    inline: HashMap<String, Bytes>,
    fetcher: Option<Arc<dyn Fetcher>>,
    allow_remote: bool,
    max_bytes: usize,
    blocked: Mutex<Vec<String>>,
    /// Respuestas entregadas; el bucle de `resolve` se repite mientras cambie.
    pub(crate) served: AtomicUsize,
}

/// Peticiones máximas por documento (un correo con 10 000 imágenes no debe colgarnos).
const MAX_REQUESTS: usize = 2_000;

impl MailNet {
    pub(crate) fn new(
        inline: &HashMap<String, Bytes>,
        fetcher: Option<Arc<dyn Fetcher>>,
        allow_remote: bool,
        max_bytes: usize,
    ) -> Self {
        // Los Content-ID se comparan sin <> y sin distinguir mayúsculas (los clientes no se ponen
        // de acuerdo).
        let inline = inline
            .iter()
            .map(|(k, v)| (k.trim().trim_start_matches('<').trim_end_matches('>').to_ascii_lowercase(), v.clone()))
            .collect();
        Self { inline, fetcher, allow_remote, max_bytes, blocked: Mutex::new(Vec::new()), served: AtomicUsize::new(0) }
    }

    pub(crate) fn take_blocked(&self) -> Vec<String> {
        self.blocked.lock().map(|mut b| std::mem::take(&mut *b)).unwrap_or_default()
    }

    fn block(&self, url: &str) {
        if let Ok(mut b) = self.blocked.lock()
            && !b.iter().any(|u| u == url)
            && b.len() < MAX_REQUESTS
        {
            b.push(url.to_owned());
        }
    }

    fn resolve(&self, url: &str) -> Option<Bytes> {
        let (scheme, rest) = url.split_once(':')?;
        let bytes = match scheme.to_ascii_lowercase().as_str() {
            "cid" => {
                let id = percent_decode(rest.as_bytes());
                let id = String::from_utf8_lossy(&id).trim().to_ascii_lowercase();
                self.inline.get(&id).cloned()
            }
            "data" => decode_data_url(rest).map(Bytes::from),
            "http" | "https" => {
                if self.allow_remote {
                    self.fetcher.as_ref().and_then(|f| f.fetch(url))
                } else {
                    self.block(url);
                    None
                }
            }
            _ => None,
        }?;
        if bytes.len() > self.max_bytes || !svg_is_safe(&bytes) {
            return None;
        }
        Some(bytes)
    }
}

impl NetProvider for MailNet {
    fn fetch(&self, _doc_id: usize, request: Request, handler: Box<dyn NetHandler>) {
        let n = self.served.fetch_add(1, Ordering::Relaxed);
        let url = request.url.to_string();
        let bytes = if n < MAX_REQUESTS { self.resolve(&url) } else { None };
        handler.bytes(url, bytes.unwrap_or_default());
    }
}

/// Una imagen SVG (de `cid:`, `data:` o remota) se descarta si referencia algo que no sea un
/// fragmento propio o una imagen `data:`: usvg leería archivos locales por su cuenta.
pub(crate) fn svg_is_safe(bytes: &[u8]) -> bool {
    let head = bytes.get(..bytes.len().min(4096)).unwrap_or(bytes);
    let head = String::from_utf8_lossy(head).to_ascii_lowercase();
    if !head.contains("<svg") && !head.trim_start().starts_with("<?xml") {
        return true;
    }
    let text = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    let mut rest = text.as_str();
    while let Some(i) = rest.find("href") {
        rest = rest.get(i + 4..).unwrap_or("");
        let after = rest.trim_start();
        let Some(after) = after.strip_prefix('=') else { continue };
        let value = after.trim_start().trim_start_matches(['"', '\'']).trim_start();
        let ok = value.starts_with('#') || (value.starts_with("data:image/") && !value.starts_with("data:image/svg"));
        if !ok {
            return false;
        }
    }
    true
}

/// Decodifica la parte posterior a `data:` de una URL `data:`.
pub(crate) fn decode_data_url(rest: &str) -> Option<Vec<u8>> {
    let (meta, payload) = rest.split_once(',')?;
    if meta.to_ascii_lowercase().ends_with(";base64") {
        let decoded = percent_decode(payload.as_bytes());
        base64_decode(&decoded)
    } else {
        Some(percent_decode(payload.as_bytes()))
    }
}

fn percent_decode(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while let Some(&b) = input.get(i) {
        if b == b'%'
            && let (Some(h), Some(l)) = (input.get(i + 1).and_then(hex), input.get(i + 2).and_then(hex))
        {
            out.push(h << 4 | l);
            i += 3;
            continue;
        }
        out.push(b);
        i += 1;
    }
    out
}

fn hex(b: &u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Base64 estándar o «url-safe», tolerante a espacios y saltos de línea (frecuentes en correo).
pub(crate) fn base64_decode(input: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for &c in input {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            c if c.is_ascii_whitespace() => continue,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xff) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_roundtrip() {
        assert_eq!(base64_decode(b"SG9sYQ==").as_deref(), Some(&b"Hola"[..]));
        assert_eq!(base64_decode(b"SG9s\r\nYSBt").as_deref(), Some(&b"Hola m"[..]));
        assert!(base64_decode(b"**").is_none());
    }

    #[test]
    fn data_urls() {
        assert_eq!(decode_data_url("image/png;base64,SG9sYQ==").as_deref(), Some(&b"Hola"[..]));
        assert_eq!(decode_data_url("text/plain,a%20b").as_deref(), Some(&b"a b"[..]));
        assert!(decode_data_url("sin-coma").is_none());
    }

    #[test]
    fn svg_safety() {
        assert!(svg_is_safe(b"\x89PNG..."));
        assert!(svg_is_safe(br##"<svg><use href="#a"/></svg>"##));
        assert!(!svg_is_safe(br#"<svg><image href="/etc/passwd"/></svg>"#));
        assert!(!svg_is_safe(br#"<svg><image xlink:href = 'file:///x'/></svg>"#));
    }

    #[test]
    fn remote_blocked_and_listed() {
        let net = MailNet::new(&HashMap::new(), None, false, 1024);
        assert!(net.resolve("https://tracker.example/p.gif?u=1").is_none());
        assert!(net.resolve("https://tracker.example/p.gif?u=1").is_none());
        assert_eq!(net.take_blocked(), vec!["https://tracker.example/p.gif?u=1".to_owned()]);
    }

    #[test]
    fn cid_lookup_is_lenient() {
        let mut m = HashMap::new();
        m.insert("<Logo@Corp>".to_owned(), Bytes::from_static(b"x"));
        let net = MailNet::new(&m, None, false, 1024);
        assert!(net.resolve("cid:logo@corp").is_some());
        assert!(net.resolve("cid:Logo%40Corp").is_some());
        assert!(net.resolve("cid:otro").is_none());
    }

    #[test]
    fn file_scheme_never() {
        let net = MailNet::new(&HashMap::new(), None, true, 1024);
        assert!(net.resolve("file:///etc/passwd").is_none());
        assert!(net.take_blocked().is_empty());
    }
}
