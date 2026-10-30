//! Lo que se sabe de un mensaje para evaluar reglas: el sobre, las cabeceras del crudo (si ya se
//! descargó) y el texto. Todo dato externo es hostil: cabeceras y texto acotados.

use iuredesk_model::{Mailbox, MessageEnvelope};

/// Cabeceras que se conservan como mucho.
const MAX_HEADERS: usize = 300;
/// Bytes por valor de cabecera.
const MAX_HEADER_VALUE: usize = 8 * 1024;
/// Bytes del bloque de cabeceras que se leen.
const MAX_HEADER_BLOCK: usize = 256 * 1024;
/// Caracteres de texto del mensaje que miran las reglas.
pub const MAX_BODY_CHARS: usize = 256 * 1024;

/// Cabeceras de un mensaje: nombre en minúsculas y valor desplegado y decodificado (RFC 2047).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Headers(Vec<(String, String)>);

impl Headers {
    /// Lee el bloque de cabeceras de un crudo RFC 5322.
    pub fn parse(raw: &[u8]) -> Headers {
        let head = raw.get(..raw.len().min(MAX_HEADER_BLOCK)).unwrap_or_default();
        let mut out: Vec<(String, Vec<u8>)> = Vec::new();
        for line in head.split(|b| *b == b'\n') {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if line.is_empty() {
                break;
            }
            if line.first().is_some_and(|b| *b == b' ' || *b == b'\t') {
                if let Some(last) = out.last_mut()
                    && last.1.len() < MAX_HEADER_VALUE
                {
                    last.1.push(b' ');
                    last.1.extend_from_slice(trim_bytes(line));
                }
                continue;
            }
            if out.len() >= MAX_HEADERS {
                break;
            }
            let Some(colon) = line.iter().position(|b| *b == b':') else { continue };
            let name = String::from_utf8_lossy(line.get(..colon).unwrap_or_default()).trim().to_ascii_lowercase();
            if name.is_empty() || name.contains(' ') {
                continue;
            }
            let value = trim_bytes(line.get(colon.saturating_add(1)..).unwrap_or_default()).to_vec();
            out.push((name, value));
        }
        Headers(
            out.into_iter()
                .map(|(n, mut v)| {
                    v.truncate(MAX_HEADER_VALUE);
                    (n, iuredesk_sync::parse::decode_words(&v))
                })
                .collect(),
        )
    }

    /// Desde pares ya hechos (pruebas, proveedores sin crudo).
    pub fn from_pairs<'a>(pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> Headers {
        Headers(pairs.into_iter().take(MAX_HEADERS).map(|(n, v)| (n.to_ascii_lowercase(), v.to_string())).collect())
    }

    /// Primer valor de una cabecera (`name` sin distinguir mayúsculas).
    pub fn get(&self, name: &str) -> Option<&str> {
        let n = name.to_ascii_lowercase();
        self.0.iter().find(|(k, _)| *k == n).map(|(_, v)| v.as_str())
    }

    /// Todos los valores de una cabecera.
    pub fn all<'a>(&'a self, name: &str) -> impl Iterator<Item = &'a str> + 'a {
        let n = name.to_ascii_lowercase();
        self.0.iter().filter(move |(k, _)| *k == n).map(|(_, v)| v.as_str())
    }

    /// Nombres de las cabeceras (en minúsculas, en orden).
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(|(k, _)| k.as_str())
    }

    /// `true` si existe.
    pub fn has(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    /// `true` si no hay cabeceras (crudo aún no descargado).
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

fn trim_bytes(b: &[u8]) -> &[u8] {
    let start = b.iter().position(|c| !c.is_ascii_whitespace()).unwrap_or(b.len());
    let end = b.iter().rposition(|c| !c.is_ascii_whitespace()).map_or(start, |e| e.saturating_add(1));
    b.get(start..end.max(start)).unwrap_or_default()
}

/// Hechos de un mensaje.
#[derive(Clone, Debug, Default)]
pub struct Facts {
    /// Sobre.
    pub env: MessageEnvelope,
    /// Cabeceras (vacías si el crudo aún no está).
    pub headers: Headers,
    /// Texto del mensaje (o el extracto si aún no hay crudo).
    pub body: String,
    /// El remitente es un contacto o está vinculado en Iurefficient (lo decide quien llama).
    pub known_sender: bool,
}

impl Facts {
    /// Hechos solo con el sobre (el texto es el extracto).
    pub fn from_envelope(env: MessageEnvelope) -> Facts {
        let body = env.preview.clone();
        Facts { env, headers: Headers::default(), body, known_sender: false }
    }

    /// Hechos con el crudo: cabeceras y texto plano (o HTML sin etiquetas).
    pub fn from_raw(env: MessageEnvelope, raw: &[u8]) -> Facts {
        let headers = Headers::parse(raw);
        let body = mail_parser::MessageParser::default()
            .parse(raw)
            .and_then(|m| m.body_text(0).map(|t| t.chars().take(MAX_BODY_CHARS).collect::<String>()))
            .unwrap_or_else(|| env.preview.clone());
        Facts { env, headers, body, known_sender: false }
    }

    /// `List-Id` (o `None`).
    pub fn list_id(&self) -> Option<&str> {
        self.headers.get("list-id")
    }

    /// Dirección del remitente principal, en minúsculas.
    pub fn sender(&self) -> String {
        self.env.from.first().map(|m| crate::norm_addr(&m.addr)).unwrap_or_default()
    }

    /// Es una lista o un boletín.
    pub fn is_list(&self) -> bool {
        self.headers.has("list-id") || self.headers.has("list-unsubscribe") || self.headers.has("list-post")
    }

    /// Es una respuesta.
    pub fn is_reply(&self) -> bool {
        self.env.in_reply_to.is_some() || iuredesk_model::normalize_subject(&self.env.subject).1
    }
}

/// «Nombre <dir>» para comparar con texto.
pub fn mailbox_text(m: &Mailbox) -> String {
    match &m.name {
        Some(n) if !n.trim().is_empty() => format!("{n} <{}>", m.addr),
        _ => m.addr.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_folded_and_encoded_headers() {
        let raw = b"From: a@b.c\r\nList-Id: Ofertas\r\n <ofertas.tienda.example>\r\nSubject: =?UTF-8?Q?Caf=C3=A9?=\r\nX-Rare\r\n\r\nBody: no\r\n";
        let h = Headers::parse(raw);
        assert_eq!(h.get("LIST-ID"), Some("Ofertas <ofertas.tienda.example>"));
        assert_eq!(h.get("subject"), Some("Café"));
        assert!(h.get("body").is_none(), "el cuerpo no es cabecera");
        assert!(!h.has("x-rare"));
    }

    #[test]
    fn bounded() {
        let mut raw = Vec::new();
        for i in 0..1_000 {
            raw.extend_from_slice(format!("X-H{i}: {}\r\n", "v".repeat(100)).as_bytes());
        }
        let h = Headers::parse(&raw);
        assert!(h.0.len() <= MAX_HEADERS);
    }

    proptest::proptest! {
        #[test]
        fn never_panics(raw in proptest::collection::vec(proptest::num::u8::ANY, 0..2_000)) {
            let _ = Headers::parse(&raw);
        }
    }
}
