//! Responder, responder a todos y reenviar a partir del correo crudo original (mail-parser).
//!
//! - Asunto: se quitan todos los prefijos conocidos (`Re:`, `RE:`, `Fwd:`, `Fw:`, `RV:`, `RES:`,
//!   `ENC:`, `AW:`, `WG:`, `SV:`, `Re[2]:`…) y se pone uno solo; la etiqueta de lista (`[equipo]`)
//!   se conserva.
//! - Destinatarios: `Reply-To` manda sobre `From`; «responder a todos» respeta `Mail-Followup-To`,
//!   quita las direcciones propias (todas las identidades de la cuenta), no repite nadie y no deja
//!   en CC a quien ya está en Para. Responder a un mensaje propio (desde Enviados) vuelve a sus
//!   destinatarios.
//! - Cuerpo: línea en blanco para escribir, firma (encima o debajo de la cita), línea de
//!   atribución localizada («El lun, 26 oct 2026 a las 10:42, Ana López <ana@acme.mx> escribió:»)
//!   y la cita.
//! - Reenvío en línea (encabezado «Mensaje reenviado» y adjuntos copiados, incluidas las imágenes
//!   en línea con su `Content-ID`) o como adjunto `message/rfc822`.

use std::collections::HashSet;

use chrono::{DateTime, Datelike, FixedOffset, NaiveDate, TimeZone, Timelike};
use iuredesk_model::account::Identity;
use iuredesk_model::{Mailbox, MessageId};
use mail_parser::{Message, MessageParser, MimeHeaders};
use serde::{Deserialize, Serialize};

use crate::address::{address_key, dedupe, validate_address};
use crate::draft::{
    Attachment, AttachmentKind, AttachmentSource, Draft, ForwardMode, Forwarded, OriginalKind, OriginalRef,
};
use crate::error::{ComposeError, Result};
use crate::lang::Lang;
use crate::mime::{clean_msg_id, trim_references};
use crate::rich::{Block, Inline, RichBody};
use crate::signature::{self, FORWARD_MARK, SignaturePlacement};

/// Prefijos de respuesta/reenvío que se reconocen (en minúsculas).
const PREFIXES: &[&str] =
    &["re", "fwd", "fw", "rv", "res", "enc", "aw", "wg", "sv", "vs", "tr", "antw", "rif", "ref", "odp", "vb"];
/// Número máximo de adjuntos que se copian al reenviar.
const MAX_FORWARD_ATTACHMENTS: usize = 200;

/// A quién se responde.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplyMode {
    /// Solo al remitente (o a su `Reply-To`).
    #[default]
    Sender,
    /// A todos.
    All,
}

/// Estilo de los prefijos del asunto.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubjectStyle {
    /// `Re:` y `Fwd:` (Gmail, Apple Mail, Thunderbird; RFC 5322 §3.6.5).
    #[default]
    Standard,
    /// Como Outlook: `RE:`, y `RV:` (español) o `FW:` (inglés).
    Outlook,
}

/// Opciones para responder o reenviar.
#[derive(Clone, Debug, Default)]
pub struct ReplyOptions {
    /// Idioma de la línea de atribución y del encabezado de reenvío.
    pub lang: Lang,
    pub subject_style: SubjectStyle,
    /// Zona horaria para mostrar la fecha del original. `None` = la del propio original.
    pub tz: Option<FixedOffset>,
    /// Firma a insertar.
    pub signature: Option<RichBody>,
    pub placement: SignaturePlacement,
    /// Id local del original (para marcarlo como respondido/reenviado al enviar).
    pub original_id: Option<MessageId>,
}

/// Cabeceras del original que importan para responder.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OriginalHeaders {
    pub from: Vec<Mailbox>,
    pub reply_to: Vec<Mailbox>,
    pub to: Vec<Mailbox>,
    pub cc: Vec<Mailbox>,
    pub mail_followup_to: Vec<Mailbox>,
    /// Destinatario real según el servidor (`Delivered-To`, `X-Original-To`).
    pub delivered_to: Vec<Mailbox>,
    pub subject: String,
    pub message_id: Option<String>,
    pub references: Vec<String>,
    pub in_reply_to: Option<String>,
    pub date: Option<DateTime<FixedOffset>>,
}

fn parse(raw: &[u8]) -> Result<Message<'_>> {
    MessageParser::default().parse(raw).ok_or(ComposeError::Unparseable)
}

fn mailboxes(a: Option<&mail_parser::Address<'_>>) -> Vec<Mailbox> {
    let Some(a) = a else { return Vec::new() };
    a.iter()
        .take(iuredesk_model::MAX_RECIPIENTS)
        .filter_map(|x| {
            let addr = x.address()?.trim();
            // Las direcciones del original pueden ser raras; se conservan si validan.
            let addr = validate_address(addr).ok()?;
            Some(Mailbox::new(x.name().unwrap_or("").trim().to_string(), addr))
        })
        .collect()
}

/// Analiza una cabecera de direcciones cruda (p. ej. `Mail-Followup-To`, que mail-parser no
/// interpreta) envolviéndola en un mensaje mínimo.
fn parse_address_header(raw: &str) -> Vec<Mailbox> {
    let value: String = raw.chars().take(64 * 1024).collect();
    let fake = format!("To: {}\r\n\r\n", value.trim());
    match MessageParser::default().parse(fake.as_bytes()) {
        Some(m) => mailboxes(m.to()),
        None => Vec::new(),
    }
}

fn to_chrono(d: &mail_parser::DateTime) -> Option<DateTime<FixedOffset>> {
    let secs = i32::from(d.tz_hour).saturating_mul(3600).saturating_add(i32::from(d.tz_minute).saturating_mul(60));
    let offset = FixedOffset::east_opt(if d.tz_before_gmt { -secs } else { secs })?;
    let naive = NaiveDate::from_ymd_opt(i32::from(d.year), u32::from(d.month), u32::from(d.day))?.and_hms_opt(
        u32::from(d.hour),
        u32::from(d.minute),
        u32::from(d.second),
    )?;
    offset.from_local_datetime(&naive).single()
}

/// Extrae las cabeceras relevantes del original.
pub fn original_headers(raw: &[u8]) -> Result<OriginalHeaders> {
    let msg = parse(raw)?;
    Ok(headers_of(&msg))
}

fn headers_of(msg: &Message<'_>) -> OriginalHeaders {
    let refs =
        msg.references().as_text_list().map(|l| l.to_vec()).unwrap_or_else(|| {
            msg.references().as_text().map(|t| vec![std::borrow::Cow::Borrowed(t)]).unwrap_or_default()
        });
    let mut delivered = Vec::new();
    for name in ["Delivered-To", "X-Original-To", "Envelope-To"] {
        if let Some(raw) = msg.header_raw(name) {
            delivered.extend(parse_address_header(raw));
        }
    }
    OriginalHeaders {
        from: mailboxes(msg.from()),
        reply_to: mailboxes(msg.reply_to()),
        to: mailboxes(msg.to()),
        cc: mailboxes(msg.cc()),
        mail_followup_to: msg.header_raw("Mail-Followup-To").map(parse_address_header).unwrap_or_default(),
        delivered_to: delivered,
        subject: msg.subject().unwrap_or("").to_string(),
        message_id: msg.message_id().and_then(clean_msg_id),
        references: refs.iter().filter_map(|r| clean_msg_id(r)).collect(),
        in_reply_to: msg.in_reply_to().as_text().and_then(clean_msg_id),
        date: msg.date().and_then(to_chrono),
    }
}

/// Elige la identidad: la que aparece como destinataria del original; si ninguna, la primera.
pub fn choose_identity(identities: &[Identity], orig: &OriginalHeaders) -> Result<Identity> {
    let first = identities.first().cloned().ok_or_else(|| ComposeError::Identity(String::new()))?;
    let addressed: Vec<String> =
        orig.delivered_to.iter().chain(&orig.to).chain(&orig.cc).map(|m| address_key(&m.addr)).collect();
    for key in &addressed {
        if let Some(id) = identities.iter().find(|i| &address_key(&i.email) == key) {
            return Ok(id.clone());
        }
    }
    Ok(first)
}

/// Destinatarios (Para, CC) de la respuesta. `own` son las direcciones propias (todas las
/// identidades).
pub fn reply_recipients(orig: &OriginalHeaders, own: &[String], mode: ReplyMode) -> (Vec<Mailbox>, Vec<Mailbox>) {
    let own: HashSet<String> = own.iter().map(|a| address_key(a)).collect();
    let is_own = |m: &Mailbox| own.contains(&address_key(&m.addr));
    let target = if orig.reply_to.is_empty() { orig.from.clone() } else { orig.reply_to.clone() };
    let from_me = !orig.from.is_empty() && orig.from.iter().all(is_own);

    let (mut to, mut cc) = match mode {
        ReplyMode::Sender => {
            if from_me && !orig.to.is_empty() {
                (orig.to.clone(), Vec::new())
            } else {
                (target.clone(), Vec::new())
            }
        }
        ReplyMode::All => {
            if !orig.mail_followup_to.is_empty() {
                (orig.mail_followup_to.clone(), Vec::new())
            } else if from_me {
                (orig.to.clone(), orig.cc.clone())
            } else {
                let mut to = target.clone();
                to.extend(orig.to.iter().cloned());
                (to, orig.cc.clone())
            }
        }
    };
    to.retain(|m| !is_own(m));
    cc.retain(|m| !is_own(m));
    dedupe(&mut to);
    let in_to: HashSet<String> = to.iter().map(|m| address_key(&m.addr)).collect();
    cc.retain(|m| !in_to.contains(&address_key(&m.addr)));
    dedupe(&mut cc);
    if to.is_empty() && !cc.is_empty() {
        to.push(cc.remove(0));
    }
    if to.is_empty() {
        // Escribirse a uno mismo: mejor eso que un borrador sin destinatario.
        to = target;
        if to.is_empty() {
            to = orig.to.clone();
        }
        dedupe(&mut to);
    }
    (to, cc)
}

/// Quita los prefijos de respuesta/reenvío del inicio. Devuelve (etiqueta de lista, resto).
fn strip_prefixes(subject: &str) -> (Option<String>, String) {
    let mut s = subject.trim().to_string();
    let mut tag: Option<String> = None;
    for _ in 0..32 {
        let before = s.clone();
        if tag.is_none()
            && let Some(rest) = s.strip_prefix('[')
            && let Some(end) = rest.find(']')
            && end < 64
        {
            let after = rest.get(end.saturating_add(1)..).unwrap_or("").trim_start().to_string();
            if !after.is_empty() {
                tag = Some(format!("[{}]", rest.get(..end).unwrap_or("")));
                s = after;
            }
        }
        if let Some(rest) = strip_one_prefix(&s) {
            s = rest;
        }
        if s == before {
            break;
        }
    }
    (tag, s)
}

fn strip_one_prefix(s: &str) -> Option<String> {
    let colon = s.char_indices().take(12).find(|(_, c)| *c == ':' || *c == '：').map(|(i, c)| (i, c.len_utf8()))?;
    let head = s.get(..colon.0)?;
    let word: String = head.chars().take_while(|c| c.is_alphabetic()).collect::<String>().to_lowercase();
    let rest_ok =
        head.chars().skip(word.chars().count()).all(|c| c.is_ascii_digit() || matches!(c, '[' | ']' | '(' | ')' | ' '));
    if word.is_empty() || !rest_ok || !PREFIXES.contains(&word.as_str()) {
        return None;
    }
    Some(s.get(colon.0.saturating_add(colon.1)..).unwrap_or("").trim_start().to_string())
}

fn with_prefix(prefix: &str, subject: &str) -> String {
    let (tag, rest) = strip_prefixes(subject);
    let mut out = String::from(prefix);
    if let Some(t) = tag {
        out.push(' ');
        out.push_str(&t);
    }
    if !rest.is_empty() {
        out.push(' ');
        out.push_str(&rest);
    }
    out
}

/// Asunto de una respuesta.
pub fn reply_subject(subject: &str, style: SubjectStyle) -> String {
    with_prefix(if style == SubjectStyle::Outlook { "RE:" } else { "Re:" }, subject)
}

/// Asunto de un reenvío.
pub fn forward_subject(subject: &str, style: SubjectStyle, lang: Lang) -> String {
    let p = match style {
        SubjectStyle::Standard => "Fwd:",
        SubjectStyle::Outlook => lang.pick("FW:", "RV:"),
    };
    with_prefix(p, subject)
}

const DAYS_ES: [&str; 7] = ["dom", "lun", "mar", "mié", "jue", "vie", "sáb"];
const DAYS_EN: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS_ES: [&str; 12] = ["ene", "feb", "mar", "abr", "may", "jun", "jul", "ago", "sept", "oct", "nov", "dic"];
const MONTHS_EN: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

fn day_month(d: &DateTime<FixedOffset>, lang: Lang) -> (&'static str, &'static str) {
    let wd = d.weekday().num_days_from_sunday() as usize;
    let m = d.month0() as usize;
    match lang {
        Lang::Es => (DAYS_ES.get(wd).copied().unwrap_or(""), MONTHS_ES.get(m).copied().unwrap_or("")),
        Lang::En => (DAYS_EN.get(wd).copied().unwrap_or(""), MONTHS_EN.get(m).copied().unwrap_or("")),
    }
}

/// Fecha legible: «lun, 26 oct 2026 a las 10:42» / «Mon, Oct 26, 2026 at 10:42 AM».
pub fn format_date(d: &DateTime<FixedOffset>, lang: Lang) -> String {
    let (day, month) = day_month(d, lang);
    match lang {
        Lang::Es => format!("{day}, {} {month} {} a las {:02}:{:02}", d.day(), d.year(), d.hour(), d.minute()),
        Lang::En => {
            let (pm, h12) = d.hour12();
            let ampm = if pm { "PM" } else { "AM" };
            format!("{day}, {month} {}, {} at {h12}:{:02} {ampm}", d.day(), d.year(), d.minute())
        }
    }
}

fn who(from: &[Mailbox]) -> String {
    match from.first() {
        Some(m) => match m.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
            Some(n) => format!("{n} <{}>", m.addr),
            None => m.addr.clone(),
        },
        None => String::new(),
    }
}

/// Línea de atribución de la cita.
pub fn attribution(orig: &OriginalHeaders, lang: Lang, tz: Option<FixedOffset>) -> String {
    let sender = who(&orig.from);
    let sender = if sender.is_empty() { lang.pick("someone", "alguien").to_string() } else { sender };
    match orig.date.map(|d| tz.map(|z| d.with_timezone(&z)).unwrap_or(d)) {
        Some(d) => match lang {
            Lang::Es => format!("El {}, {sender} escribió:", format_date(&d, lang)),
            Lang::En => format!("On {}, {sender} wrote:", format_date(&d, lang)),
        },
        None => match lang {
            Lang::Es => format!("{sender} escribió:"),
            Lang::En => format!("{sender} wrote:"),
        },
    }
}

/// Cuerpo del original como documento (HTML si lo hay; si no, texto).
fn original_body(msg: &Message<'_>) -> RichBody {
    let body = if let Some(html) = msg.body_html(0) {
        RichBody::from_html(&html)
    } else if let Some(text) = msg.body_text(0) {
        RichBody::from_plain(&text)
    } else {
        RichBody::default()
    };
    RichBody { blocks: unsign(body.blocks, 0) }
}

/// La firma del original ya no es «la firma» del borrador: se vuelve contenido normal.
fn unsign(blocks: Vec<Block>, depth: usize) -> Vec<Block> {
    if depth > crate::rich::MAX_DEPTH.saturating_mul(2) {
        return blocks;
    }
    let mut out = Vec::with_capacity(blocks.len());
    for b in blocks {
        match b {
            Block::Signature { blocks } => out.extend(unsign(blocks, depth.saturating_add(1))),
            Block::Quote { blocks } => out.push(Block::Quote { blocks: unsign(blocks, depth.saturating_add(1)) }),
            Block::List { ordered, start, items } => out.push(Block::List {
                ordered,
                start,
                items: items.into_iter().map(|it| unsign(it, depth.saturating_add(1))).collect(),
            }),
            other => out.push(other),
        }
    }
    out
}

fn own_addresses(identities: &[Identity]) -> Vec<String> {
    identities.iter().map(|i| i.email.clone()).collect()
}

/// Borrador de respuesta.
pub fn reply(raw: &[u8], identities: &[Identity], mode: ReplyMode, opts: &ReplyOptions) -> Result<Draft> {
    let msg = parse(raw)?;
    let orig = headers_of(&msg);
    let identity = choose_identity(identities, &orig)?;
    let (to, cc) = reply_recipients(&orig, &own_addresses(identities), mode);
    let mut d = Draft::new(identity);
    d.to = to;
    d.cc = cc;
    d.subject = reply_subject(&orig.subject, opts.subject_style);
    d.in_reply_to = orig.message_id.clone();
    let mut refs = orig.references.clone();
    if refs.is_empty()
        && let Some(irt) = &orig.in_reply_to
    {
        refs.push(irt.clone());
    }
    if let Some(id) = &orig.message_id {
        refs.push(id.clone());
    }
    d.references = trim_references(&refs);
    d.original = Some(OriginalRef { message: opts.original_id, kind: OriginalKind::Reply });

    let quoted = original_body(&msg);
    let mut blocks = vec![Block::Paragraph { content: Vec::new() }];
    blocks.push(Block::paragraph(attribution(&orig, opts.lang, opts.tz)));
    blocks.push(Block::Quote { blocks: quoted.blocks });
    d.body = RichBody { blocks };
    signature::apply(&mut d.body, opts.signature.as_ref(), opts.placement);
    Ok(d)
}

/// Encabezado del reenvío en línea.
fn forward_header(orig: &OriginalHeaders, lang: Lang, tz: Option<FixedOffset>) -> Block {
    let mut content =
        vec![Inline::text(format!("{FORWARD_MARK} {} ---------", lang.pick("Forwarded message", "Mensaje reenviado")))];
    let mut line = |label: &str, value: String| {
        if !value.trim().is_empty() {
            content.push(Inline::LineBreak);
            content.push(Inline::text(format!("{label}: {value}")));
        }
    };
    let list = |l: &[Mailbox]| {
        l.iter()
            .map(|m| match m.name.as_deref().filter(|n| !n.trim().is_empty()) {
                Some(n) => format!("{n} <{}>", m.addr),
                None => m.addr.clone(),
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    line(lang.pick("From", "De"), list(&orig.from));
    if let Some(d) = orig.date.map(|d| tz.map(|z| d.with_timezone(&z)).unwrap_or(d)) {
        line(lang.pick("Date", "Fecha"), format_date(&d, lang));
    }
    line(lang.pick("Subject", "Asunto"), orig.subject.clone());
    line(lang.pick("To", "Para"), list(&orig.to));
    line("CC", list(&orig.cc));
    Block::Paragraph { content }
}

/// Adjuntos del original (para el reenvío en línea).
fn original_attachments(msg: &Message<'_>) -> Vec<Attachment> {
    let mut out = Vec::new();
    for (n, part) in msg.attachments().take(MAX_FORWARD_ATTACHMENTS).enumerate() {
        let mime = part
            .content_type()
            .map(|ct| match ct.subtype() {
                Some(st) => format!("{}/{}", ct.ctype(), st),
                None => ct.ctype().to_string(),
            })
            .unwrap_or_default();
        let filename = part.attachment_name().map(str::to_string).unwrap_or_else(|| {
            if part.is_message() { format!("mensaje-{}.eml", n + 1) } else { format!("adjunto-{}", n + 1) }
        });
        let data = part.contents().to_vec();
        let size = data.len() as u64;
        let kind = match part.content_id() {
            Some(cid) if !part.content_disposition().is_some_and(|d| d.is_attachment()) => {
                AttachmentKind::Inline { content_id: cid.trim_matches(['<', '>']).to_string() }
            }
            _ => AttachmentKind::Regular,
        };
        out.push(Attachment { filename, mime_type: mime, source: AttachmentSource::Bytes(data), kind, size });
    }
    out
}

/// F7.1: reabre un borrador guardado (crudo RFC 5322 de «Borradores») para seguir editándolo:
/// identidad del «De:», Para/CC/CCO, asunto, cuerpo enriquecido (HTML → [`RichBody`], o el texto
/// plano), adjuntos (también las imágenes en línea), `In-Reply-To`/`References`, prioridad y
/// acuse de lectura. Si ninguna identidad coincide con el «De:» se usa la primera.
pub fn reopen_draft(raw: &[u8], identities: &[Identity]) -> Result<Draft> {
    let msg = parse(raw)?;
    let orig = headers_of(&msg);
    let from_key = orig.from.first().map(|m| address_key(&m.addr));
    let identity = identities
        .iter()
        .find(|i| Some(address_key(&i.email)) == from_key)
        .or(identities.first())
        .cloned()
        .ok_or_else(|| ComposeError::Identity(String::new()))?;
    let mut d = Draft::new(identity);
    d.to = orig.to.clone();
    d.cc = orig.cc.clone();
    d.bcc = mailboxes(msg.bcc());
    // `undisclosed-recipients:;` (solo CCO) no deja direcciones: nada que quitar.
    d.subject = orig.subject.clone();
    d.in_reply_to = orig.in_reply_to.clone();
    d.references = trim_references(&orig.references);
    let body = if let Some(html) = msg.body_html(0) {
        RichBody::from_html(&html)
    } else if let Some(text) = msg.body_text(0) {
        RichBody::from_plain(&text)
    } else {
        RichBody::default()
    };
    d.body = body;
    d.attachments = original_attachments(&msg);
    d.priority = match msg.header_raw("X-Priority").map(str::trim).and_then(|v| v.chars().next()) {
        Some('1' | '2') => crate::draft::Priority::High,
        Some('4' | '5') => crate::draft::Priority::Low,
        _ => crate::draft::Priority::Normal,
    };
    d.request_read_receipt = msg.header_raw("Disposition-Notification-To").is_some();
    Ok(d)
}

/// Borrador de reenvío.
pub fn forward(raw: &[u8], identities: &[Identity], mode: ForwardMode, opts: &ReplyOptions) -> Result<Draft> {
    let msg = parse(raw)?;
    let orig = headers_of(&msg);
    let identity = choose_identity(identities, &orig)?;
    let mut d = Draft::new(identity);
    d.subject = forward_subject(&orig.subject, opts.subject_style, opts.lang);
    d.references = orig.message_id.iter().cloned().collect();
    d.original = Some(OriginalRef { message: opts.original_id, kind: OriginalKind::Forward });
    let mut blocks = vec![Block::Paragraph { content: Vec::new() }];
    match mode {
        ForwardMode::Inline => {
            blocks.push(forward_header(&orig, opts.lang, opts.tz));
            blocks.extend(original_body(&msg).blocks);
            d.attachments = original_attachments(&msg);
        }
        ForwardMode::AsAttachment => {
            let (_, rest) = strip_prefixes(&orig.subject);
            let base: String =
                rest.chars().filter(|c| !c.is_control() && !"/\\:*?\"<>|".contains(*c)).take(80).collect();
            let base = if base.trim().is_empty() { "mensaje".to_string() } else { base.trim().to_string() };
            d.forwarded =
                Some(Forwarded { source: AttachmentSource::Bytes(raw.to_vec()), filename: format!("{base}.eml") });
        }
    }
    d.body = RichBody { blocks };
    signature::apply(&mut d.body, opts.signature.as_ref(), opts.placement);
    Ok(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subjects() {
        assert_eq!(reply_subject("Propuesta", SubjectStyle::Standard), "Re: Propuesta");
        assert_eq!(reply_subject("RE: Re: Propuesta", SubjectStyle::Standard), "Re: Propuesta");
        assert_eq!(reply_subject("RV: Propuesta", SubjectStyle::Outlook), "RE: Propuesta");
        assert_eq!(reply_subject("Re[3]: AW: x", SubjectStyle::Standard), "Re: x");
        assert_eq!(reply_subject("[equipo] Re: x", SubjectStyle::Standard), "Re: [equipo] x");
        assert_eq!(reply_subject("", SubjectStyle::Standard), "Re:");
        assert_eq!(reply_subject("Reunión: lunes", SubjectStyle::Standard), "Re: Reunión: lunes");
        assert_eq!(forward_subject("Re: x", SubjectStyle::Standard, Lang::Es), "Fwd: x");
        assert_eq!(forward_subject("Fwd: x", SubjectStyle::Outlook, Lang::Es), "RV: x");
        assert_eq!(forward_subject("RV: x", SubjectStyle::Outlook, Lang::En), "FW: x");
        assert_eq!(reply_subject("RES: ENC: Fw: x", SubjectStyle::Standard), "Re: x");
        assert_eq!(reply_subject("Re： x", SubjectStyle::Standard), "Re: x");
    }

    #[test]
    fn dates_localized() {
        let d = FixedOffset::east_opt(-6 * 3600).unwrap().with_ymd_and_hms(2026, 10, 26, 10, 42, 0).unwrap();
        assert_eq!(format_date(&d, Lang::Es), "lun, 26 oct 2026 a las 10:42");
        assert_eq!(format_date(&d, Lang::En), "Mon, Oct 26, 2026 at 10:42 AM");
        let orig = OriginalHeaders {
            from: vec![Mailbox::new("Ana López", "ana@acme.mx")],
            date: Some(d),
            ..Default::default()
        };
        assert_eq!(
            attribution(&orig, Lang::Es, None),
            "El lun, 26 oct 2026 a las 10:42, Ana López <ana@acme.mx> escribió:"
        );
        assert_eq!(
            attribution(&orig, Lang::En, None),
            "On Mon, Oct 26, 2026 at 10:42 AM, Ana López <ana@acme.mx> wrote:"
        );
        let utc = FixedOffset::east_opt(0).unwrap();
        assert!(attribution(&orig, Lang::Es, Some(utc)).contains("a las 16:42"));
    }
}
