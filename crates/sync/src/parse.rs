//! Del servidor al modelo: `FETCH` de IMAP y mensajes crudos (mail-parser 0.11) → sobres.

use chrono::{DateTime, TimeZone, Utc};
use iuredesk_imap::FetchData;
use iuredesk_imap::codec::Address as ImapAddress;
use iuredesk_model::{
    AttachmentMeta, Flags, FolderId, MAX_RECIPIENTS, MAX_REFERENCES, Mailbox, MessageEnvelope, make_preview,
};
use mail_parser::{Address, HeaderValue, Message, MessageParser, MimeHeaders};

/// Caracteres máximos del texto de cuerpo que se indexa.
pub const MAX_BODY_TEXT: usize = 256 * 1024;

/// Banderas IMAP del sistema que no son palabras clave.
const SYSTEM_FLAGS: &[&str] = &["\\seen", "\\flagged", "\\answered", "\\draft", "\\deleted", "\\recent", "$forwarded"];

/// Banderas estándar desde la lista IMAP.
pub fn flags_from_imap(list: &[String]) -> Flags {
    let has = |f: &str| list.iter().any(|x| x.eq_ignore_ascii_case(f));
    Flags {
        seen: has("\\Seen"),
        flagged: has("\\Flagged"),
        answered: has("\\Answered"),
        draft: has("\\Draft"),
        deleted: has("\\Deleted"),
        forwarded: has("$Forwarded"),
    }
}

/// Palabras clave (banderas que no son del sistema).
pub fn keywords_from_imap(list: &[String]) -> Vec<String> {
    list.iter()
        .filter(|f| !SYSTEM_FLAGS.contains(&f.to_ascii_lowercase().as_str()) && !f.starts_with('\\'))
        .take(MAX_RECIPIENTS)
        .cloned()
        .collect()
}

/// Lista IMAP de las banderas estándar activas.
pub fn imap_flags(f: Flags) -> Vec<String> {
    let mut v = Vec::new();
    for (on, name) in [
        (f.seen, "\\Seen"),
        (f.flagged, "\\Flagged"),
        (f.answered, "\\Answered"),
        (f.draft, "\\Draft"),
        (f.deleted, "\\Deleted"),
        (f.forwarded, "$Forwarded"),
    ] {
        if on {
            v.push(name.to_string());
        }
    }
    v
}

/// Decodifica una palabra codificada RFC 2047 (asunto, nombre). Nunca falla.
pub fn decode_words(raw: &[u8]) -> String {
    let lossy = String::from_utf8_lossy(raw);
    if !lossy.contains("=?") {
        return lossy.trim().to_string();
    }
    let mut synthetic = b"Subject: ".to_vec();
    synthetic.extend(raw.iter().map(|b| if matches!(b, b'\r' | b'\n') { b' ' } else { *b }));
    synthetic.extend_from_slice(b"\r\n\r\n");
    MessageParser::default()
        .parse_headers(&synthetic)
        .and_then(|m| m.subject().map(str::to_string))
        .unwrap_or_else(|| lossy.trim().to_string())
}

/// `Message-ID` sin `<>` ni espacios.
pub fn clean_id(s: &str) -> Option<String> {
    let t = s.trim().trim_start_matches('<').trim_end_matches('>').trim();
    if t.is_empty() || t.len() > 998 { None } else { Some(t.to_string()) }
}

fn imap_mailboxes(list: &[ImapAddress]) -> Vec<Mailbox> {
    list.iter()
        .take(MAX_RECIPIENTS)
        .map(|a| {
            let name = a.name.as_deref().map(decode_words).unwrap_or_default();
            Mailbox::new(name, a.email())
        })
        .collect()
}

/// Fecha razonable: la del mensaje si es plausible; si no, la de recepción.
fn plausible(date: Option<DateTime<Utc>>, fallback: DateTime<Utc>) -> DateTime<Utc> {
    let now = Utc::now();
    match date {
        Some(d) if d.timestamp() > 315_532_800 && d <= now + chrono::Duration::days(2) => d,
        _ => fallback,
    }
}

fn parse_date_header(raw: &[u8]) -> Option<DateTime<Utc>> {
    let mut synthetic = b"Date: ".to_vec();
    synthetic.extend(raw.iter().map(|b| if matches!(b, b'\r' | b'\n') { b' ' } else { *b }));
    synthetic.extend_from_slice(b"\r\n\r\n");
    let m = MessageParser::default().parse_headers(&synthetic)?;
    Utc.timestamp_opt(m.date()?.to_timestamp(), 0).single()
}

fn header_ids(v: &HeaderValue<'_>) -> Vec<String> {
    match v {
        HeaderValue::Text(t) => clean_id(t).into_iter().collect(),
        HeaderValue::TextList(l) => l.iter().filter_map(|t| clean_id(t)).collect(),
        _ => Vec::new(),
    }
}

/// Sobre a partir de un `FETCH` de cabeceras (sin cuerpo).
pub fn envelope_from_fetch(f: &FetchData, folder: FolderId) -> Option<MessageEnvelope> {
    let uid = f.uid?;
    let received = f.internal_date.map(|d| d.with_timezone(&Utc)).unwrap_or_else(Utc::now);
    let mut env = MessageEnvelope {
        folder,
        remote_id: Some(uid.to_string()),
        size: f.size.unwrap_or(0),
        date: received,
        ..Default::default()
    };
    if let Some(flags) = &f.flags {
        env.flags = flags_from_imap(flags);
        env.keywords = keywords_from_imap(flags);
    }
    if let Some(labels) = &f.gm_labels {
        for l in labels.iter().take(MAX_RECIPIENTS) {
            if !env.keywords.contains(l) {
                env.keywords.push(l.clone());
            }
        }
    }
    if let Some(e) = &f.envelope {
        env.subject = e.subject.as_deref().map(decode_words).unwrap_or_default();
        env.from = imap_mailboxes(&e.from);
        env.to = imap_mailboxes(&e.to);
        env.cc = imap_mailboxes(&e.cc);
        env.bcc = imap_mailboxes(&e.bcc);
        env.reply_to = imap_mailboxes(&e.reply_to);
        env.message_id = e.message_id.as_deref().and_then(|m| clean_id(&String::from_utf8_lossy(m)));
        env.in_reply_to = e
            .in_reply_to
            .as_deref()
            .and_then(|m| String::from_utf8_lossy(m).split_whitespace().next().and_then(clean_id));
        env.date = plausible(e.date.as_deref().and_then(parse_date_header), received);
    }
    if let Some(section) = f.section("BODY[HEADER.FIELDS")
        && let Some(m) = MessageParser::default().parse_headers(section)
    {
        let refs = header_ids(m.references());
        if !refs.is_empty() {
            let start = refs.len().saturating_sub(MAX_REFERENCES);
            env.references = refs.get(start..).unwrap_or_default().to_vec();
        }
        if env.in_reply_to.is_none() {
            env.in_reply_to = header_ids(m.in_reply_to()).into_iter().next();
        }
    }
    if let Some(bs) = &f.body_structure {
        env.attachments = bs
            .attachments()
            .into_iter()
            .map(|p| AttachmentMeta {
                filename: decode_words(p.filename.as_bytes()),
                mime_type: p.mime_type,
                size: p.size,
                content_id: p.content_id.and_then(|c| clean_id(&c)),
                inline: p.inline,
                blob: None,
            })
            .collect();
        env.has_attachments = env.attachments.iter().any(|a| !a.inline);
    }
    env.clamp();
    Some(env)
}

fn mailboxes(a: Option<&Address<'_>>) -> Vec<Mailbox> {
    let Some(a) = a else { return Vec::new() };
    a.iter()
        .take(MAX_RECIPIENTS)
        .filter_map(|x| {
            let addr = x.address()?.to_string();
            Some(Mailbox::new(x.name().unwrap_or("").to_string(), addr))
        })
        .collect()
}

/// Lo que se extrae de un mensaje completo.
#[derive(Clone, Debug, Default)]
pub struct ParsedBody {
    /// Texto del cuerpo para el índice.
    pub body_text: String,
    /// Extracto.
    pub preview: String,
    /// Adjuntos.
    pub attachments: Vec<AttachmentMeta>,
}

fn body_of(m: &Message<'_>) -> ParsedBody {
    let mut text = String::new();
    for i in 0..m.text_body_count().min(16) {
        if let Some(t) = m.body_text(i) {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&t);
            if text.len() >= MAX_BODY_TEXT {
                break;
            }
        }
    }
    if text.is_empty()
        && let Some(t) = m.body_text(0)
    {
        text = t.into_owned();
    }
    iuredesk_model::truncate_chars(&mut text, MAX_BODY_TEXT);
    let attachments = m
        .attachments()
        .take(MAX_RECIPIENTS)
        .map(|p| {
            let ct = p.content_type();
            let mime = match ct {
                Some(c) => format!("{}/{}", c.ctype(), c.subtype().unwrap_or("octet-stream")).to_ascii_lowercase(),
                None => "application/octet-stream".to_string(),
            };
            let inline = p.content_disposition().is_some_and(|d| d.is_inline());
            AttachmentMeta {
                filename: p.attachment_name().unwrap_or("").to_string(),
                mime_type: mime,
                size: p.len() as u64,
                content_id: p.content_id().and_then(clean_id),
                inline,
                blob: None,
            }
        })
        .collect();
    ParsedBody { preview: make_preview(&text), body_text: text, attachments }
}

/// Cuerpo, extracto y adjuntos de un mensaje crudo.
pub fn parse_body(raw: &[u8]) -> ParsedBody {
    match MessageParser::default().parse(raw) {
        Some(m) => body_of(&m),
        None => ParsedBody::default(),
    }
}

/// Sobre completo a partir del mensaje crudo (POP3, APPEND local) y el texto a indexar.
pub fn envelope_from_raw(raw: &[u8], folder: FolderId, received: DateTime<Utc>) -> (MessageEnvelope, String) {
    let mut env = MessageEnvelope { folder, date: received, size: raw.len() as u64, ..Default::default() };
    let Some(m) = MessageParser::default().parse(raw) else { return (env, String::new()) };
    env.subject = m.subject().unwrap_or("").to_string();
    env.from = mailboxes(m.from());
    env.to = mailboxes(m.to());
    env.cc = mailboxes(m.cc());
    env.bcc = mailboxes(m.bcc());
    env.reply_to = mailboxes(m.reply_to());
    env.message_id = m.message_id().and_then(clean_id);
    env.in_reply_to = header_ids(m.in_reply_to()).into_iter().next();
    env.references = header_ids(m.references());
    let date = m.date().and_then(|d| Utc.timestamp_opt(d.to_timestamp(), 0).single());
    env.date = plausible(date, received);
    let body = body_of(&m);
    env.preview = body.preview;
    env.has_attachments = body.attachments.iter().any(|a| !a.inline);
    env.attachments = body.attachments;
    env.clamp();
    (env, body.body_text)
}

/// Completa un sobre ya existente con lo que da el cuerpo descargado.
pub fn enrich(env: &mut MessageEnvelope, body: ParsedBody) {
    env.preview = body.preview;
    if !body.attachments.is_empty() || env.attachments.is_empty() {
        env.has_attachments = body.attachments.iter().any(|a| !a.inline);
        env.attachments = body.attachments;
    }
    env.clamp();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_encoded_words_and_dates() {
        assert_eq!(decode_words(b"=?utf-8?q?Caf=C3=A9_listo?="), "Café listo");
        assert_eq!(decode_words(b"Hola"), "Hola");
        assert_eq!(decode_words(b"=?roto"), "=?roto");
        let d = parse_date_header(b"Wed, 17 Jul 2024 02:23:25 -0700").unwrap();
        assert_eq!(d.to_rfc3339(), "2024-07-17T09:23:25+00:00");
        assert!(parse_date_header(b"basura").is_none());
    }

    #[test]
    fn raw_message_to_envelope() {
        let raw = b"From: =?utf-8?q?Ana_P=C3=A9rez?= <ana@example.com>\r\nTo: yo@example.org\r\nSubject: Re: Hola\r\nDate: Wed, 17 Jul 2024 02:23:25 -0700\r\nMessage-ID: <b@x>\r\nIn-Reply-To: <a@x>\r\nReferences: <z@x> <a@x>\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nCuerpo   del\r\nmensaje\r\n";
        let (e, text) = envelope_from_raw(raw, FolderId(3), Utc::now());
        assert_eq!(e.from[0].display(), "Ana Pérez");
        assert_eq!(e.message_id.as_deref(), Some("b@x"));
        assert_eq!(e.in_reply_to.as_deref(), Some("a@x"));
        assert_eq!(e.references, vec!["z@x".to_string(), "a@x".to_string()]);
        assert_eq!(e.preview, "Cuerpo del mensaje");
        assert!(text.contains("mensaje"));
        assert_eq!(e.date.to_rfc3339(), "2024-07-17T09:23:25+00:00");
    }

    #[test]
    fn flags_roundtrip() {
        let f = flags_from_imap(&["\\Seen".into(), "$Forwarded".into(), "Trabajo".into()]);
        assert!(f.seen && f.forwarded && !f.flagged);
        assert_eq!(keywords_from_imap(&["\\Seen".into(), "Trabajo".into()]), vec!["Trabajo".to_string()]);
        assert_eq!(imap_flags(f), vec!["\\Seen".to_string(), "$Forwarded".to_string()]);
    }

    proptest::proptest! {
        #[test]
        fn raw_never_panics(b in proptest::collection::vec(proptest::num::u8::ANY, 0..2000)) {
            let _ = envelope_from_raw(&b, FolderId(1), Utc::now());
            let _ = decode_words(&b);
            let _ = parse_body(&b);
        }
    }
}
