//! Cuerpo de un mensaje para el panel de lectura: texto plano (se muestra al instante), HTML (lo
//! pinta `html-view` en segundo plano) y recursos en línea (`cid:`). El correo es hostil: tamaños
//! acotados y nada de pánicos ante MIME roto.

use mail_parser::{MessageParser, MimeHeaders};

/// Texto plano máximo que se guarda para mostrar.
pub const MAX_TEXT_CHARS: usize = 200_000;
/// HTML máximo que se pasa al render.
pub const MAX_HTML_BYTES: usize = 4 * 1024 * 1024;
/// Recursos en línea máximos por mensaje.
const MAX_INLINE: usize = 32;
/// Tamaño máximo de un recurso en línea.
const MAX_INLINE_BYTES: usize = 5 * 1024 * 1024;

/// Un adjunto (para la lista bajo la cabecera).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachmentInfo {
    pub name: String,
    pub size: usize,
    pub mime: String,
}

/// Cuerpo listo para mostrar.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MessageBody {
    pub text: String,
    /// Solo si el mensaje trae una parte `text/html` de verdad.
    pub html: Option<String>,
    /// Recursos `cid:` (sin `<>`) → bytes.
    pub inline: Vec<(String, Vec<u8>)>,
    pub attachments: Vec<AttachmentInfo>,
}

fn clip_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => s.get(..i).unwrap_or(s).to_string(),
        None => s.to_string(),
    }
}

/// Interpreta un mensaje RFC 822. Si no se puede, el texto crudo (acotado) hace de cuerpo.
pub fn parse(raw: &[u8]) -> MessageBody {
    let Some(msg) = MessageParser::default().parse(raw) else {
        return MessageBody { text: clip_chars(&String::from_utf8_lossy(raw), MAX_TEXT_CHARS), ..Default::default() };
    };
    let html = msg
        .html_part(0)
        .filter(|p| p.is_text_html())
        .and_then(|p| p.text_contents())
        .filter(|h| h.len() <= MAX_HTML_BYTES)
        .map(str::to_string);
    let text = msg.body_text(0).map(|t| clip_chars(&t, MAX_TEXT_CHARS)).unwrap_or_default();
    let mut inline = Vec::new();
    let mut attachments = Vec::new();
    for part in msg.attachments().take(500) {
        let size = part.len();
        if let Some(cid) = part.content_id() {
            let cid = cid.trim().trim_start_matches('<').trim_end_matches('>').to_string();
            if inline.len() < MAX_INLINE && size <= MAX_INLINE_BYTES && !cid.is_empty() {
                inline.push((cid, part.contents().to_vec()));
                continue;
            }
        }
        let mime = part
            .content_type()
            .map(|c| match c.subtype() {
                Some(s) => format!("{}/{s}", c.ctype()),
                None => c.ctype().to_string(),
            })
            .unwrap_or_else(|| "application/octet-stream".into());
        attachments.push(AttachmentInfo { name: part.attachment_name().unwrap_or("adjunto").to_string(), size, mime });
    }
    MessageBody { text, html, inline, attachments }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_and_html() {
        let plain =
            b"From: a@example.com\r\nSubject: x\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nHola mundo\r\n";
        let b = parse(plain);
        assert!(b.text.contains("Hola mundo"));
        assert!(b.html.is_none(), "el texto plano no se convierte en HTML");

        let alt = b"From: a@example.com\r\nSubject: x\r\nMIME-Version: 1.0\r\nContent-Type: multipart/alternative; boundary=B\r\n\r\n--B\r\nContent-Type: text/plain\r\n\r\nTexto\r\n--B\r\nContent-Type: text/html\r\n\r\n<p>Hola <b>HTML</b></p>\r\n--B--\r\n";
        let b = parse(alt);
        assert!(b.text.contains("Texto"));
        assert!(b.html.as_deref().is_some_and(|h| h.contains("<b>HTML</b>")));
    }

    #[test]
    fn garbage_never_panics() {
        for raw in [&b""[..], b"\xff\xfe\x00", b"Content-Type: multipart/mixed; boundary=\r\n\r\n--\r\n--"] {
            let _ = parse(raw);
        }
        assert_eq!(clip_chars("áéíóú", 2), "áé");
    }
}
