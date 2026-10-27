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
    /// Aviso sobre el cuerpo (p. ej. no se pudo descargar y solo hay extracto).
    pub note: Option<crate::lang::Text>,
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
    MessageBody { text, html, inline, attachments, note: None }
}

/// ¿Es una línea de atribución de una respuesta («El lun…, Ana escribió:», «On …, Ana wrote:»)?
fn is_attribution(line: &str) -> bool {
    let l = line.trim().to_lowercase();
    if l.len() > 400 {
        return false;
    }
    let wrote = ["wrote:", "escribió:", "escribio:", "a écrit :", "a écrit:", "schrieb:", "scrisse:", "escreveu:"];
    wrote.iter().any(|w| l.ends_with(w))
}

/// ¿Empieza aquí un mensaje original reenviado o citado al estilo Outlook?
fn is_original_header(line: &str) -> bool {
    let l = line.trim().to_lowercase();
    let t = l.trim_matches('-').trim();
    l.starts_with("-----") && matches!(t, "original message" | "mensaje original" | "message d'origine")
}

/// Separa el texto nuevo de la historia citada al final (como el «…» de Gmail): devuelve
/// `(visible, citado)`. Solo hay cita si **todo** lo que sigue a su inicio (salvo líneas en
/// blanco) va citado con `>`, o si empieza con un encabezado «-----Original Message-----». Si el
/// mensaje entero es cita, no se oculta nada.
pub fn split_quoted(text: &str) -> (String, Option<String>) {
    let lines: Vec<&str> = text.lines().take(20_000).collect();
    let quoted = |l: &str| l.trim_start().starts_with('>');
    let mut start: Option<usize> = None;
    // Desde el final: el último bloque continuo de líneas citadas (o en blanco).
    let mut i = lines.len();
    while i > 0 {
        let l = lines.get(i - 1).copied().unwrap_or("");
        if quoted(l) || l.trim().is_empty() {
            i -= 1;
        } else {
            break;
        }
    }
    let has_quote = lines.get(i..).is_some_and(|rest| rest.iter().any(|l| quoted(l)));
    if has_quote {
        // La atribución (una o dos líneas) va con la cita.
        let mut s = i;
        if s > 0 && lines.get(s - 1).is_some_and(|l| is_attribution(l)) {
            s -= 1;
            if s > 0 && lines.get(s - 1).is_some_and(|l| l.trim_start().to_lowercase().starts_with("on ")) {
                s -= 1;
            }
        }
        start = Some(s);
    }
    if let Some(h) = lines.iter().position(|l| is_original_header(l)) {
        start = Some(start.map_or(h, |s| s.min(h)));
    }
    let Some(s) = start else { return (text.to_string(), None) };
    let visible = lines.get(..s).unwrap_or(&[]).join("\n");
    if visible.trim().is_empty() {
        return (text.to_string(), None);
    }
    let rest = lines.get(s..).unwrap_or(&[]).join("\n");
    (visible.trim_end().to_string(), Some(rest))
}

/// Lo mismo para HTML: corta en el primer bloque citado conocido (`gmail_quote`,
/// `<blockquote type="cite">` de Apple Mail y Thunderbird, `divRplyFwdMsg`/`appendonsend` de
/// Outlook). Devuelve el HTML sin la cita, o `None` si no hay o si no quedaría nada visible.
pub fn strip_quoted_html(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let markers = [
        "class=\"gmail_quote",
        "class=\"gmail_attr",
        "<blockquote type=\"cite\"",
        "id=\"divrplyfwdmsg\"",
        "id=\"appendonsend\"",
        "class=\"moz-cite-prefix",
        "class=\"yahoo_quoted",
    ];
    let at = markers.iter().filter_map(|m| lower.find(m)).min()?;
    // Inicio de la etiqueta que contiene el marcador.
    let tag = lower.get(..at)?.rfind('<')?;
    let head = html.get(..tag)?;
    let visible_text: String = {
        let mut out = String::new();
        let mut in_tag = false;
        for c in head.chars() {
            match c {
                '<' => in_tag = true,
                '>' => in_tag = false,
                c if !in_tag => out.push(c),
                _ => {}
            }
        }
        out
    };
    if visible_text.trim().is_empty() {
        return None;
    }
    Some(format!("{head}</body></html>"))
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
    fn quoted_history_is_split_off() {
        let t = "Perfecto, gracias.\n\nEl 13/10/2026 10:13, Ana escribió:\n> Hola\n>\n> > viejo\n";
        let (v, q) = split_quoted(t);
        assert_eq!(v, "Perfecto, gracias.");
        assert!(q.unwrap().starts_with("El 13/10/2026 10:13, Ana escribió:"));
        // Respuesta intercalada: no se oculta nada.
        let t = "> pregunta\nrespuesta\n> otra\nmás";
        assert_eq!(split_quoted(t), (t.to_string(), None));
        // Todo citado: se muestra.
        assert_eq!(split_quoted("> solo cita").1, None);
        // Atribución en dos líneas y estilo Outlook.
        let (v, q) = split_quoted("Ok\nOn Mon, Oct 13, 2026 at 10:13 AM\nAna <a@x.com> wrote:\n> hola");
        assert_eq!(v, "Ok");
        assert!(q.unwrap().starts_with("On Mon"));
        let (v, q) = split_quoted("Va\n\n-----Original Message-----\nFrom: x\nhola");
        assert_eq!(v, "Va");
        assert!(q.is_some());
        assert_eq!(split_quoted(""), (String::new(), None));
    }

    #[test]
    fn quoted_html_is_cut() {
        let h = "<html><body><p>Nuevo</p><div class=\"gmail_quote\"><blockquote>viejo</blockquote></div></body></html>";
        let cut = strip_quoted_html(h).unwrap();
        assert!(cut.contains("Nuevo") && !cut.contains("viejo"));
        assert!(strip_quoted_html("<p>sin cita</p>").is_none());
        assert!(strip_quoted_html("<div class=\"gmail_quote\">solo</div>").is_none());
    }

    #[test]
    fn garbage_never_panics() {
        for raw in [&b""[..], b"\xff\xfe\x00", b"Content-Type: multipart/mixed; boundary=\r\n\r\n--\r\n--"] {
            let _ = parse(raw);
        }
        assert_eq!(clip_chars("áéíóú", 2), "áé");
    }
}
