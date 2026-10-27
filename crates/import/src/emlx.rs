//! Mensajes de Apple Mail (`.emlx` y `.partial.emlx`).
//!
//! Formato: una línea con el número de bytes del mensaje, el mensaje RFC 822 y, detrás, un plist
//! XML con metadatos (`flags`, `date-received`…). Los `.partial.emlx` son mensajes cuyos adjuntos
//! grandes Apple Mail guardó aparte, en `…/Attachments/<id>/<parte IMAP>/<archivo>`; la parte vacía
//! lleva la cabecera `X-Apple-Content-Length`. Aquí se vuelven a insertar (codificados como estaban
//! declarados) para que el mensaje importado quede completo.

use std::path::Path;

use base64::Engine as _;
use chrono::{DateTime, TimeZone, Utc};
use iuredesk_model::Flags;
use mail_parser::{MessageParser, PartType};
use quick_xml::Reader;
use quick_xml::events::Event;

/// Contenido de un `.emlx`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Emlx {
    /// Mensaje RFC 822.
    pub raw: Vec<u8>,
    /// Banderas del plist, si las hay.
    pub flags: Option<Flags>,
    /// Fecha de recepción del plist.
    pub received: Option<DateTime<Utc>>,
}

/// Bits del entero `flags` del plist de Apple Mail.
const F_READ: u64 = 1 << 0;
const F_DELETED: u64 = 1 << 1;
const F_ANSWERED: u64 = 1 << 2;
const F_FLAGGED: u64 = 1 << 4;
const F_DRAFT: u64 = 1 << 6;
const F_FORWARDED: u64 = 1 << 8;

/// Banderas a partir del entero de Apple Mail.
pub fn flags_from_apple(v: u64) -> Flags {
    Flags {
        seen: v & F_READ != 0,
        deleted: v & F_DELETED != 0,
        answered: v & F_ANSWERED != 0,
        flagged: v & F_FLAGGED != 0,
        draft: v & F_DRAFT != 0,
        forwarded: v & F_FORWARDED != 0,
    }
}

/// Lee un `.emlx`. Error si no tiene la forma esperada.
pub fn parse_emlx(data: &[u8]) -> Result<Emlx, String> {
    let nl = data.iter().take(32).position(|&b| b == b'\n').ok_or("falta la línea de longitud")?;
    let count: usize = String::from_utf8_lossy(data.get(..nl).unwrap_or(&[]))
        .trim()
        .parse()
        .map_err(|_| "longitud no numérica".to_string())?;
    let start = nl.saturating_add(1);
    let end = start.saturating_add(count).min(data.len());
    let raw = data.get(start..end).unwrap_or(&[]).to_vec();
    if raw.is_empty() {
        return Err("mensaje vacío".into());
    }
    let plist = data.get(end..).unwrap_or(&[]);
    let dict = plist_dict(plist);
    let flags = dict.iter().find(|(k, _)| k == "flags").and_then(|(_, v)| v.trim().parse::<f64>().ok());
    let received = dict
        .iter()
        .find(|(k, _)| k == "date-received")
        .and_then(|(_, v)| v.trim().parse::<f64>().ok())
        .filter(|s| s.is_finite() && *s > 0.0 && *s < 1e11)
        .and_then(|s| Utc.timestamp_opt(s as i64, 0).single());
    Ok(Emlx { raw, flags: flags.filter(|f| f.is_finite() && *f >= 0.0).map(|f| flags_from_apple(f as u64)), received })
}

/// Pares `clave → valor` del primer nivel de un plist XML (solo valores simples).
pub fn plist_dict(xml: &[u8]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut r = Reader::from_reader(xml);
    let mut buf = Vec::new();
    let mut depth = 0usize;
    let mut key: Option<String> = None;
    let mut in_elem: Option<String> = None;
    let mut text = String::new();
    for _ in 0..100_000 {
        buf.clear();
        match r.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                depth = depth.saturating_add(1);
                if name == "dict" || name == "array" {
                    in_elem = None;
                    if depth >= 3 {
                        key = None;
                    }
                } else {
                    in_elem = Some(name);
                    text.clear();
                }
            }
            Ok(Event::Text(t)) => {
                if in_elem.is_some() && text.len() < 4096 {
                    text.push_str(&String::from_utf8_lossy(t.as_ref()));
                }
            }
            Ok(Event::End(_)) => {
                depth = depth.saturating_sub(1);
                if let Some(name) = in_elem.take() {
                    // Primer nivel: plist > dict > (key | valor) → profundidad 2 tras cerrar.
                    if depth == 2 {
                        if name == "key" {
                            key = Some(text.trim().to_string());
                        } else if let Some(k) = key.take() {
                            out.push((k, text.trim().to_string()));
                        }
                    }
                }
            }
            Ok(Event::Empty(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                if depth == 2
                    && let Some(k) = key.take()
                {
                    out.push((k, name));
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            Ok(_) => {}
        }
        if out.len() >= 1_000 {
            break;
        }
    }
    out
}

/// Partes hoja del mensaje con su número IMAP (`1`, `2.1`…) e índice en `parts`.
fn leaf_numbers(m: &mail_parser::Message<'_>) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    match m.parts.first().map(|p| &p.body) {
        Some(PartType::Multipart(children)) => {
            for (i, c) in children.iter().enumerate().take(1_000) {
                walk(m, *c as usize, format!("{}", i.saturating_add(1)), 0, &mut out);
            }
        }
        Some(_) => out.push(("1".to_string(), 0)),
        None => {}
    }
    out
}

fn walk(m: &mail_parser::Message<'_>, id: usize, num: String, depth: usize, out: &mut Vec<(String, usize)>) {
    if depth > 32 || out.len() > 1_000 {
        return;
    }
    match m.parts.get(id).map(|p| &p.body) {
        Some(PartType::Multipart(children)) => {
            for (i, c) in children.iter().enumerate().take(1_000) {
                walk(m, *c as usize, format!("{num}.{}", i.saturating_add(1)), depth.saturating_add(1), out);
            }
        }
        Some(_) => out.push((num, id)),
        None => {}
    }
}

/// Resultado de reinsertar adjuntos en un `.partial.emlx`.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Reattached {
    /// Mensaje completo (o el original si no hubo nada que reinsertar).
    pub raw: Vec<u8>,
    /// Partes reinsertadas.
    pub restored: usize,
    /// Partes que faltaban y no se encontraron.
    pub missing: usize,
}

/// Reinserta en `raw` las partes guardadas aparte en `attachments` (`Attachments/<id>`).
/// Ninguna parte reinsertada supera `max_part`; el total no supera `max_total`.
pub fn reattach(raw: Vec<u8>, attachments: &Path, max_part: u64, max_total: usize) -> Reattached {
    let Some(m) = MessageParser::default().parse(&raw) else {
        return Reattached { raw, ..Default::default() };
    };
    let eol: &[u8] = if raw.windows(2).any(|w| w == b"\r\n") { b"\r\n" } else { b"\n" };
    let mut edits: Vec<(usize, usize, Vec<u8>)> = Vec::new();
    let mut missing = 0usize;
    for (num, id) in leaf_numbers(&m) {
        let Some(part) = m.parts.get(id) else { continue };
        let stripped = part.headers.iter().any(|h| h.name.as_str().eq_ignore_ascii_case("x-apple-content-length"));
        if !stripped {
            continue;
        }
        let encoding = part
            .headers
            .iter()
            .find(|h| h.name.as_str().eq_ignore_ascii_case("content-transfer-encoding"))
            .and_then(|h| raw.get(h.offset_start as usize..h.offset_end as usize))
            .map(|v| String::from_utf8_lossy(v).trim().to_ascii_lowercase())
            .unwrap_or_default();
        let data = first_file(&attachments.join(&num), max_part);
        let Some(data) = data else {
            missing = missing.saturating_add(1);
            continue;
        };
        let encoded = match encoding.as_str() {
            "base64" => {
                let b64 = base64::engine::general_purpose::STANDARD.encode(&data);
                let mut out = Vec::with_capacity(b64.len().saturating_add(b64.len() / 76 * 2));
                for (i, chunk) in b64.as_bytes().chunks(76).enumerate() {
                    if i > 0 {
                        out.extend_from_slice(eol);
                    }
                    out.extend_from_slice(chunk);
                }
                out
            }
            "" | "7bit" | "8bit" | "binary" => data,
            _ => {
                missing = missing.saturating_add(1);
                continue;
            }
        };
        let start = (part.offset_body as usize).min(raw.len());
        let end = (part.offset_end as usize).clamp(start, raw.len());
        // El cuerpo vacío puede contener el salto de línea que precede al delimitador.
        let body = raw.get(start..end).unwrap_or(&[]);
        let end = if body.iter().all(u8::is_ascii_whitespace) { start } else { end };
        edits.push((start, end, encoded));
    }
    let restored = edits.len();
    let added: usize = edits.iter().map(|(_, _, e)| e.len()).sum();
    if restored == 0 || raw.len().saturating_add(added) > max_total {
        return Reattached { raw, restored: 0, missing: missing.saturating_add(restored) };
    }
    edits.sort_by_key(|e| std::cmp::Reverse(e.0));
    let mut out = raw;
    for (start, end, data) in edits {
        if start <= end && end <= out.len() {
            out.splice(start..end, data);
        }
    }
    Reattached { raw: out, restored, missing }
}

/// Primer archivo normal de un directorio (Apple guarda uno por parte), si cabe en `max`.
fn first_file(dir: &Path, max: u64) -> Option<Vec<u8>> {
    let rd = std::fs::read_dir(dir).ok()?;
    let mut files: Vec<_> = rd.flatten().take(64).filter(|e| e.file_type().is_ok_and(|t| t.is_file())).collect();
    files.sort_by_key(|e| e.file_name());
    let f = files.first()?;
    if f.metadata().ok()?.len() > max {
        return None;
    }
    std::fs::read(f.path()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn make_emlx(msg: &[u8], flags: u64, received: i64) -> Vec<u8> {
        let mut v = format!("{}        \n", msg.len()).into_bytes();
        v.extend_from_slice(msg);
        v.extend_from_slice(
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n\t<key>date-last-viewed</key>\n\t<integer>1700000500</integer>\n\t<key>date-received</key>\n\t<integer>{received}</integer>\n\t<key>flags</key>\n\t<integer>{flags}</integer>\n\t<key>remote-id</key>\n\t<string>17</string>\n\t<key>gmail-label-ids</key>\n\t<array><integer>3</integer></array>\n\t<key>conversation-id</key>\n\t<integer>99</integer>\n</dict>\n</plist>\n"
            )
            .as_bytes(),
        );
        v
    }

    #[test]
    fn parses_message_and_plist() {
        let msg = b"Subject: hola\nMessage-ID: <e@x>\n\ncuerpo\n";
        let e = parse_emlx(&make_emlx(msg, (1 << 0) | (1 << 4) | (1 << 8), 1_700_000_000)).unwrap();
        assert_eq!(e.raw, msg);
        let f = e.flags.unwrap();
        assert!(f.seen && f.flagged && f.forwarded && !f.answered && !f.deleted);
        assert_eq!(e.received.unwrap().timestamp(), 1_700_000_000);
        assert!(parse_emlx(b"abc\nxyz").is_err());
        assert!(parse_emlx(b"").is_err());
        // Longitud mayor que el archivo: se toma lo que hay.
        assert_eq!(parse_emlx(b"999\nSubject: x\n\ny").unwrap().raw, b"Subject: x\n\ny");
    }

    #[test]
    fn plist_ignores_nested_values() {
        let d = plist_dict(b"<plist><dict><key>a</key><integer>1</integer><key>n</key><dict><key>b</key><integer>2</integer></dict><key>c</key><true/></dict></plist>");
        assert!(d.contains(&("a".into(), "1".into())));
        assert!(d.contains(&("c".into(), "true".into())));
        assert!(!d.iter().any(|(k, _)| k == "b"));
    }

    #[test]
    fn reattaches_stripped_parts() {
        let dir = tempfile::tempdir().unwrap();
        let att = dir.path().join("Attachments").join("42");
        std::fs::create_dir_all(att.join("2")).unwrap();
        std::fs::write(att.join("2").join("informe.pdf"), b"%PDF-1.4 contenido").unwrap();
        let raw = b"Subject: x\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"b\"\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nhola\r\n--b\r\nContent-Type: application/pdf; name=informe.pdf\r\nContent-Disposition: attachment; filename=informe.pdf\r\nContent-Transfer-Encoding: base64\r\nX-Apple-Content-Length: 18\r\n\r\n\r\n--b--\r\n".to_vec();
        let r = reattach(raw, &att, 1 << 20, 1 << 20);
        assert_eq!((r.restored, r.missing), (1, 0));
        let m = MessageParser::default().parse(&r.raw).unwrap();
        let a = m.attachments().next().unwrap();
        assert_eq!(a.contents(), b"%PDF-1.4 contenido");
        assert_eq!(m.body_text(0).unwrap().trim(), "hola");

        // Sin el archivo en disco: se cuenta como faltante y el mensaje queda igual.
        let raw2 = b"Content-Type: multipart/mixed; boundary=b\n\n--b\nContent-Type: text/plain\n\nx\n--b\nContent-Type: image/png\nContent-Transfer-Encoding: base64\nX-Apple-Content-Length: 5\n\n\n--b--\n".to_vec();
        let r = reattach(raw2.clone(), &dir.path().join("nada"), 1 << 20, 1 << 20);
        assert_eq!((r.restored, r.missing, r.raw), (0, 1, raw2));
    }

    proptest::proptest! {
        #[test]
        fn never_panics(b in proptest::collection::vec(proptest::num::u8::ANY, 0..2000)) {
            let _ = parse_emlx(&b);
            let _ = plist_dict(&b);
            let _ = reattach(b, Path::new("/nonexistent-iuredesk"), 10, 10_000);
        }
    }
}
