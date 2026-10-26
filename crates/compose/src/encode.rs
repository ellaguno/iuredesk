//! Codificación de cabeceras: RFC 2047 (palabras codificadas en asuntos y nombres) y RFC 2231
//! (parámetros con nombres de archivo no ASCII).
//!
//! `mail-builder` mete las palabras codificadas entre comillas (`"=?utf-8?B?…?="`), cosa que
//! RFC 2047 §5 prohíbe y que algunos clientes muestran tal cual; por eso las cabeceras con texto
//! del usuario se escriben aquí y se entregan como cabeceras crudas.

use base64::Engine as _;
use iuredesk_model::Mailbox;

use crate::address::{AddressError, ascii_address};

/// Longitud máxima de una palabra codificada (RFC 2047 §2).
const MAX_WORD: usize = 75;
/// `=?utf-8?B?` + `?=`.
const WORD_OVERHEAD: usize = 12;

/// Quita lo que permitiría inyectar cabeceras: CR, LF y otros controles (el tabulador pasa a
/// espacio).
pub(crate) fn clean(s: &str) -> String {
    s.chars().map(|c| if c == '\t' { ' ' } else { c }).filter(|c| !c.is_control()).collect()
}

fn needs_encoding(s: &str) -> bool {
    !s.is_ascii() || s.contains("=?") || s.chars().any(|c| c.is_ascii_control())
}

/// Texto no estructurado (`Subject`): tal cual si es ASCII; si no, palabras codificadas.
pub(crate) fn encode_text(s: &str) -> String {
    let s = clean(s);
    if !needs_encoding(&s) { s } else { encoded_words(&s) }
}

/// Palabras codificadas separadas por espacio. Q si casi todo es ASCII (se lee mejor), B si no.
pub(crate) fn encoded_words(s: &str) -> String {
    let non_ascii = s.bytes().filter(|b| !b.is_ascii()).count();
    let use_q = non_ascii.saturating_mul(2) <= s.len();
    let mut words = Vec::new();
    let mut chunk = String::new();
    let mut chunk_len = 0usize;
    for c in s.chars() {
        let mut buf = [0u8; 4];
        let bytes = c.encode_utf8(&mut buf).as_bytes();
        let add = if use_q {
            bytes.iter().map(|&b| q_len(b)).sum::<usize>()
        } else {
            // Longitud base64 del trozo con este carácter.
            b64_len(chunk.len().saturating_add(bytes.len())).saturating_sub(b64_len(chunk.len()))
        };
        if !chunk.is_empty() && chunk_len.saturating_add(add).saturating_add(WORD_OVERHEAD) > MAX_WORD {
            words.push(word(&chunk, use_q));
            chunk.clear();
            chunk_len = 0;
        }
        chunk.push(c);
        chunk_len = if use_q { chunk_len.saturating_add(add) } else { b64_len(chunk.len()) };
    }
    if !chunk.is_empty() {
        words.push(word(&chunk, use_q));
    }
    words.join(" ")
}

fn b64_len(n: usize) -> usize {
    n.div_ceil(3).saturating_mul(4)
}

fn q_safe(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'!' | b'*' | b'+' | b'-' | b'/')
}

fn q_len(b: u8) -> usize {
    if q_safe(b) || b == b' ' { 1 } else { 3 }
}

fn word(s: &str, q: bool) -> String {
    if q {
        let mut out = String::from("=?utf-8?Q?");
        for &b in s.as_bytes() {
            if b == b' ' {
                out.push('_');
            } else if q_safe(b) {
                out.push(char::from(b));
            } else {
                out.push_str(&format!("={b:02X}"));
            }
        }
        out.push_str("?=");
        out
    } else {
        format!("=?utf-8?B?{}?=", base64::engine::general_purpose::STANDARD.encode(s.as_bytes()))
    }
}

/// Nombre visible para una cabecera de direcciones (RFC 5322 `phrase`).
pub(crate) fn encode_phrase(name: &str) -> String {
    let name = clean(name);
    let name = name.trim();
    if needs_encoding(name) {
        return encoded_words(name);
    }
    let atom_ok = name.chars().all(|c| c.is_ascii_alphanumeric() || " !#$%&'*+-/=?^_`{|}~".contains(c));
    if atom_ok { name.to_string() } else { format!("\"{}\"", name.replace('\\', "\\\\").replace('"', "\\\"")) }
}

/// `Nombre <dir@dominio-ascii>` (o solo la dirección si no hay nombre).
pub(crate) fn format_mailbox(m: &Mailbox) -> Result<String, AddressError> {
    let addr = ascii_address(&m.addr)?;
    match m.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => Ok(format!("{} <{addr}>", encode_phrase(n))),
        None => Ok(addr),
    }
}

/// Lista de buzones separados por `, `.
pub(crate) fn format_mailbox_list(list: &[Mailbox]) -> Result<String, AddressError> {
    let mut parts = Vec::with_capacity(list.len());
    for m in list {
        parts.push(format_mailbox(m)?);
    }
    Ok(parts.join(", "))
}

/// Carácter permitido sin codificar en un valor de parámetro RFC 2231 (`attribute-char`).
fn attr_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$&+-.^_`|~".contains(&b)
}

/// Parámetro de `Content-Type`/`Content-Disposition`. ASCII sencillo → `nombre="valor"`; si no,
/// RFC 2231 (`nombre*=utf-8''…`, con continuaciones `nombre*0*=` si es largo).
pub(crate) fn param(name: &str, value: &str) -> String {
    let value = clean(value);
    if value.is_ascii() && value.len() <= 60 {
        return format!("{name}=\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""));
    }
    let mut encoded = String::with_capacity(value.len().saturating_mul(3));
    for &b in value.as_bytes() {
        if attr_char(b) {
            encoded.push(char::from(b));
        } else {
            encoded.push_str(&format!("%{b:02X}"));
        }
    }
    if encoded.len() <= 60 {
        return format!("{name}*=utf-8''{encoded}");
    }
    // Continuaciones: trozos de ~60 sin partir un %XX.
    let mut parts = Vec::new();
    let mut rest = encoded.as_str();
    while !rest.is_empty() {
        let mut cut = rest.len().min(60);
        while cut > 0 && cut < rest.len() {
            // ¿Corta dentro de un %XX?
            let window = rest.get(cut.saturating_sub(2)..cut).unwrap_or("");
            if window.contains('%') {
                cut = cut.saturating_sub(1);
            } else {
                break;
            }
        }
        if cut == 0 {
            cut = rest.len();
        }
        let (head, tail) = rest.split_at(cut);
        parts.push(head.to_string());
        rest = tail;
    }
    parts
        .iter()
        .enumerate()
        .map(|(i, p)| if i == 0 { format!("{name}*0*=utf-8''{p}") } else { format!("{name}*{i}*={p}") })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Parámetro de nombre «a la antigua» (RFC 2047 entre comillas) para `Content-Type: …; name=`,
/// que es lo que leen Outlook y los clientes viejos.
pub(crate) fn legacy_name_param(value: &str) -> String {
    let value = clean(value);
    if value.is_ascii() {
        format!("name=\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        format!("name=\"{}\"", encoded_words(&value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_encoding() {
        assert_eq!(encode_text("Hola"), "Hola");
        assert_eq!(encode_text("Hola\r\nBcc: x@y.z"), "HolaBcc: x@y.z");
        assert_eq!(encode_text("Cotización"), "=?utf-8?Q?Cotizaci=C3=B3n?=");
        let long = "ñ".repeat(100);
        let enc = encode_text(&long);
        for w in enc.split(' ') {
            assert!(w.len() <= MAX_WORD, "{w}");
            assert!(w.starts_with("=?utf-8?B?"));
        }
        let long = "Reunión de seguimiento del proyecto con el equipo de ventas y la dirección general";
        for w in encode_text(long).split(' ') {
            assert!(w.len() <= MAX_WORD, "{w}");
        }
    }

    #[test]
    fn phrases_and_mailboxes() {
        assert_eq!(encode_phrase("Ana"), "Ana");
        assert_eq!(encode_phrase("López, Ana"), "=?utf-8?Q?L=C3=B3pez=2C_Ana?=");
        assert_eq!(encode_phrase("Perez, Ana"), "\"Perez, Ana\"");
        assert_eq!(encode_phrase("a\"b"), "\"a\\\"b\"");
        let m = Mailbox::new("José", "jose@correo.méxico");
        assert_eq!(format_mailbox(&m).unwrap(), "=?utf-8?Q?Jos=C3=A9?= <jose@correo.xn--mxico-bsa>");
        assert_eq!(format_mailbox(&Mailbox::addr("a@b.co")).unwrap(), "a@b.co");
    }

    #[test]
    fn rfc2231() {
        assert_eq!(param("filename", "a b.pdf"), "filename=\"a b.pdf\"");
        assert_eq!(param("filename", "año.pdf"), "filename*=utf-8''a%C3%B1o.pdf");
        let long = param("filename", &"ñ".repeat(40));
        assert!(long.starts_with("filename*0*=utf-8''%C3%B1"));
        assert!(long.contains("; filename*1*=%C3"));
        for part in long.split("; ") {
            let v = part.split('=').skip(1).collect::<Vec<_>>().join("=");
            assert!(!v.ends_with('%') && !v.contains("%;"), "{part}");
        }
        assert_eq!(legacy_name_param("año.pdf"), "name=\"=?utf-8?Q?a=C3=B1o=2Epdf?=\"");
    }
}
