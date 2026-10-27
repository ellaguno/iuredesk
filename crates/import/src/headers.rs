//! Lectura rápida de cabeceras (sin parsear el MIME completo): banderas que dejan Thunderbird,
//! mutt y Gmail en los mbox, etiquetas de Gmail Takeout y la clave de deduplicación.

use iuredesk_model::Flags;
use sha2::{Digest, Sha256};

/// Cabeceras máximas que se recorren.
const MAX_HEADER_LINES: usize = 2_000;
/// Bytes máximos de la sección de cabeceras que se examinan.
const MAX_HEADER_BYTES: usize = 512 * 1024;

/// Cabeceras que cambian al mover/leer el mensaje y no deben afectar a la deduplicación.
const VOLATILE: &[&str] = &[
    "x-mozilla-status",
    "x-mozilla-status2",
    "x-mozilla-keys",
    "status",
    "x-status",
    "x-keywords",
    "x-uid",
    "content-length",
    "x-gmail-labels",
    "x-gm-thrid",
    "x-apple-",
    "lines",
];

/// Separa cabeceras y cuerpo (en la primera línea vacía, CRLF o LF).
pub fn split_header_body(raw: &[u8]) -> (&[u8], &[u8]) {
    let limit = raw.len().min(MAX_HEADER_BYTES);
    let mut i = 0usize;
    while i < limit {
        let Some(nl) = raw.get(i..limit).and_then(|s| s.iter().position(|&b| b == b'\n')) else { break };
        let line_end = i.saturating_add(nl);
        let line = raw.get(i..line_end).unwrap_or(&[]);
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            let body_start = line_end.saturating_add(1);
            return (raw.get(..i).unwrap_or(&[]), raw.get(body_start..).unwrap_or(&[]));
        }
        i = line_end.saturating_add(1);
    }
    (raw.get(..limit).unwrap_or(raw), raw.get(limit..).unwrap_or(&[]))
}

/// Cabeceras desplegadas: `(nombre en minúsculas, valor)`.
pub fn header_fields(headers: &[u8]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in headers.split(|&b| b == b'\n').take(MAX_HEADER_LINES) {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.first().is_some_and(|b| *b == b' ' || *b == b'\t') {
            if let Some((_, v)) = out.last_mut() {
                v.push(' ');
                v.push_str(String::from_utf8_lossy(line).trim());
            }
            continue;
        }
        let Some(colon) = line.iter().position(|&b| b == b':') else { continue };
        let name = String::from_utf8_lossy(line.get(..colon).unwrap_or(&[])).trim().to_ascii_lowercase();
        if name.is_empty() || name.contains(' ') {
            continue;
        }
        let value = String::from_utf8_lossy(line.get(colon.saturating_add(1)..).unwrap_or(&[])).trim().to_string();
        out.push((name, value));
    }
    out
}

/// Primer valor de una cabecera.
pub fn first<'a>(fields: &'a [(String, String)], name: &str) -> Option<&'a str> {
    fields.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
}

/// Banderas leídas de las cabeceras de un mbox.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MboxFlags {
    /// Banderas estándar, si alguna cabecera las indicaba.
    pub flags: Option<Flags>,
    /// Palabras clave (`X-Mozilla-Keys`, `X-Keywords`).
    pub keywords: Vec<String>,
    /// Thunderbird lo marcó como borrado (expurgado, pendiente de compactar).
    pub expunged: bool,
    /// Etiquetas de Gmail (`X-Gmail-Labels`), si las hay.
    pub gmail_labels: Option<Vec<String>>,
}

/// Bits de `X-Mozilla-Status`.
const MOZ_READ: u32 = 0x0001;
const MOZ_REPLIED: u32 = 0x0002;
const MOZ_MARKED: u32 = 0x0004;
const MOZ_EXPUNGED: u32 = 0x0008;
const MOZ_FORWARDED: u32 = 0x1000;
/// Bit de `X-Mozilla-Status2` para borrado IMAP.
const MOZ2_IMAP_DELETED: u32 = 0x0020_0000;

fn hex_u32(s: &str) -> Option<u32> {
    u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok()
}

/// Lee banderas de `X-Mozilla-Status(2)`, `Status`/`X-Status` (mutt, Gmail), `X-Mozilla-Keys`,
/// `X-Keywords` y `X-Gmail-Labels`.
pub fn mbox_flags(fields: &[(String, String)]) -> MboxFlags {
    let mut out = MboxFlags::default();
    let mut flags = Flags::default();
    let mut known = false;
    if let Some(v) = first(fields, "x-mozilla-status").and_then(hex_u32) {
        known = true;
        flags.seen = v & MOZ_READ != 0;
        flags.answered = v & MOZ_REPLIED != 0;
        flags.flagged = v & MOZ_MARKED != 0;
        flags.forwarded = v & MOZ_FORWARDED != 0;
        out.expunged = v & MOZ_EXPUNGED != 0;
    }
    if let Some(v) = first(fields, "x-mozilla-status2").and_then(hex_u32)
        && v & MOZ2_IMAP_DELETED != 0
    {
        flags.deleted = true;
    }
    if !known {
        if let Some(s) = first(fields, "status") {
            known = true;
            flags.seen = s.contains('R');
        }
        if let Some(x) = first(fields, "x-status") {
            known = true;
            flags.answered |= x.contains('A');
            flags.flagged |= x.contains('F');
            flags.draft |= x.contains('T');
            flags.deleted |= x.contains('D');
        }
    }
    if known {
        out.flags = Some(flags);
    }
    for name in ["x-mozilla-keys", "x-keywords"] {
        for v in fields.iter().filter(|(n, _)| n == name).map(|(_, v)| v) {
            for k in v.split([' ', ',']).map(str::trim).filter(|k| !k.is_empty()) {
                if k.len() <= 200 && !out.keywords.iter().any(|x| x == k) && out.keywords.len() < 100 {
                    out.keywords.push(k.to_string());
                }
            }
        }
    }
    if let Some(v) = first(fields, "x-gmail-labels") {
        out.gmail_labels = Some(split_labels(&iuredesk_sync::parse::decode_words(v.as_bytes())));
    }
    out
}

/// Separa la lista de etiquetas de Gmail (`Inbox,"Clientes, 2024",Starred`).
pub fn split_labels(v: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in v.chars() {
        match c {
            '"' => quoted = !quoted,
            ',' if !quoted => {
                let t = cur.trim();
                if !t.is_empty() {
                    out.push(t.to_string());
                }
                cur.clear();
            }
            c => cur.push(c),
        }
        if out.len() >= 200 {
            break;
        }
    }
    let t = cur.trim();
    if !t.is_empty() && out.len() < 200 {
        out.push(t.to_string());
    }
    out
}

/// Clave de deduplicación de un mensaje: `Message-ID` + huella del cuerpo (o, si no hay
/// `Message-ID`, huella de las cabeceras estables + cuerpo). Saltos de línea normalizados, así un
/// mismo correo guardado con CRLF o LF da la misma clave.
pub fn dedup_key(raw: &[u8]) -> String {
    let (headers, body) = split_header_body(raw);
    let fields = header_fields(headers);
    let mut h = Sha256::new();
    match first(&fields, "message-id").map(clean_msgid).filter(|m| !m.is_empty()) {
        Some(mid) => {
            h.update(b"m\0");
            h.update(mid.as_bytes());
            h.update(b"\0");
        }
        None => {
            h.update(b"h\0");
            for (n, v) in &fields {
                if VOLATILE.iter().any(|p| n.starts_with(p)) || n == "received" || n.starts_with("x-") {
                    continue;
                }
                h.update(n.as_bytes());
                h.update(b":");
                h.update(v.as_bytes());
                h.update(b"\n");
            }
        }
    }
    hash_normalized(&mut h, body);
    let d = h.finalize();
    let mut s = String::from("imp:");
    for b in d.iter().take(20) {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn clean_msgid(s: &str) -> String {
    s.trim().trim_start_matches('<').trim_end_matches('>').trim().to_string()
}

/// Huella del cuerpo con CRLF → LF y sin espacios al final.
fn hash_normalized(h: &mut Sha256, body: &[u8]) {
    let mut end = body.len();
    while end > 0 && body.get(end.saturating_sub(1)).is_some_and(|b| b.is_ascii_whitespace()) {
        end = end.saturating_sub(1);
    }
    let body = body.get(..end).unwrap_or(&[]);
    for chunk in body.split(|&b| b == b'\r') {
        h.update(chunk);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mozilla_status_bits() {
        let raw = b"X-Mozilla-Status: 1005\r\nX-Mozilla-Status2: 00000000\r\nX-Mozilla-Keys: $label1 trabajo\r\nSubject: x\r\n\r\nb";
        let (h, b) = split_header_body(raw);
        assert_eq!(b, b"b");
        let f = mbox_flags(&header_fields(h));
        let fl = f.flags.unwrap();
        assert!(fl.seen && fl.flagged && fl.forwarded && !fl.answered);
        assert!(!f.expunged);
        assert_eq!(f.keywords, vec!["$label1".to_string(), "trabajo".to_string()]);
        let f = mbox_flags(&header_fields(b"X-Mozilla-Status: 0009\n"));
        assert!(f.expunged);
    }

    #[test]
    fn mutt_status() {
        let f = mbox_flags(&header_fields(b"Status: RO\nX-Status: AF\n"));
        let fl = f.flags.unwrap();
        assert!(fl.seen && fl.answered && fl.flagged && !fl.draft);
        assert_eq!(mbox_flags(&header_fields(b"Subject: x\n")).flags, None);
    }

    #[test]
    fn gmail_labels() {
        assert_eq!(
            split_labels("Inbox,\"Clientes, 2024\",Starred, Category Personal"),
            vec!["Inbox", "Clientes, 2024", "Starred", "Category Personal"]
        );
        let f = mbox_flags(&header_fields(b"X-Gmail-Labels: Archived,Opened\r\n"));
        assert_eq!(f.gmail_labels, Some(vec!["Archived".into(), "Opened".into()]));
    }

    #[test]
    fn folded_headers() {
        let f = header_fields(b"Subject: uno\r\n  dos\r\nX-A: 1\r\n");
        assert_eq!(first(&f, "subject"), Some("uno dos"));
        assert_eq!(first(&f, "x-a"), Some("1"));
    }

    #[test]
    fn dedup_ignores_line_endings_and_volatile_headers() {
        let a = b"Message-ID: <a@x>\r\nX-Mozilla-Status: 0001\r\nSubject: hola\r\n\r\nCuerpo\r\nlinea\r\n";
        let b = b"Message-ID: <a@x>\nSubject: hola\nStatus: RO\n\nCuerpo\nlinea\n\n";
        assert_eq!(dedup_key(a), dedup_key(b));
        let c = b"Message-ID: <a@x>\nSubject: hola\n\nOtro cuerpo\n";
        assert_ne!(dedup_key(a), dedup_key(c));
        let d = b"Subject: sin id\nX-Mozilla-Status: 0001\n\nhola\n";
        let e = b"Subject: sin id\nX-Mozilla-Status: 0000\n\nhola\n";
        assert_eq!(dedup_key(d), dedup_key(e));
        assert!(dedup_key(a).starts_with("imp:"));
    }

    proptest::proptest! {
        #[test]
        fn never_panics(b in proptest::collection::vec(proptest::num::u8::ANY, 0..2000)) {
            let (h, _) = split_header_body(&b);
            let _ = mbox_flags(&header_fields(h));
            let _ = dedup_key(&b);
        }
    }
}
