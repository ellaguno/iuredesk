//! Borrador → mensaje RFC 5322/MIME con `mail-builder`.
//!
//! Estructura (se omite lo que no haga falta):
//!
//! ```text
//! multipart/mixed
//! ├─ multipart/alternative
//! │  ├─ text/plain; format=flowed
//! │  ├─ multipart/related
//! │  │  ├─ text/html
//! │  │  └─ image/* (Content-ID, inline)
//! │  └─ text/calendar; method=REQUEST      ← invitación (Outlook la busca aquí)
//! ├─ adjuntos (Content-Disposition: attachment; filename*=utf-8''…)
//! ├─ message/rfc822                         ← reenvío como adjunto
//! └─ application/ics; name="invite.ics"     ← copia de la invitación
//! ```
//!
//! Cabeceras: `Date` con la zona del usuario, `Message-ID` con el dominio de la identidad,
//! `User-Agent: IureDesk/…`, `In-Reply-To`/`References`, prioridad, acuse de lectura. **Bcc nunca
//! va en las cabeceras**: solo en el sobre ([`BuiltMessage::recipients`]). La codificación de
//! transferencia (7bit, quoted-printable o base64) la elige `mail-builder` por contenido.

use std::borrow::Cow;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, FixedOffset, Local, Utc};
use iuredesk_model::{MAX_REFERENCES, Mailbox};
use iuredesk_store::Store;
use mail_builder::MessageBuilder;
use mail_builder::headers::content_type::ContentType;
use mail_builder::headers::raw::Raw;
use mail_builder::mime::{BodyPart, MimePart};

use crate::USER_AGENT;
use crate::address::{ascii_address, domain_ascii, parse_address};
use crate::draft::{Attachment, AttachmentKind, AttachmentSource, Draft, Priority};
use crate::encode::{self, clean};
use crate::error::{ComposeError, Result};

/// A partir de este tamaño se sugiere compartir los archivos como enlace (IureDav).
pub const LINK_SUGGEST_BYTES: u64 = 25 * 1024 * 1024;
/// Tamaño absoluto máximo de un mensaje que se construye.
pub const MAX_MESSAGE_BYTES: u64 = 150 * 1024 * 1024;

/// Consejo sobre el tamaño del mensaje.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SizeAdvice {
    /// Tamaño normal.
    Fine,
    /// Muchos servidores rechazan mensajes de este tamaño: mejor un enlace a IureDav.
    SuggestCloudLink { size: u64 },
}

impl SizeAdvice {
    /// Consejo para un tamaño (bytes del mensaje ya codificado).
    pub fn for_size(size: u64) -> Self {
        if size > LINK_SUGGEST_BYTES { SizeAdvice::SuggestCloudLink { size } } else { SizeAdvice::Fine }
    }
}

/// Opciones de construcción.
#[derive(Clone, Debug)]
pub struct BuildOptions {
    /// Valor de `Date`.
    pub date: DateTime<FixedOffset>,
    /// `Message-ID` a usar (con `<…>`). `None` = se genera. La bandeja de salida lo fija al
    /// encolar para que los reintentos no dupliquen el mensaje.
    pub message_id: Option<String>,
    /// Tamaño máximo absoluto.
    pub max_size: u64,
    /// Conservar `Bcc:` en las cabeceras: solo para guardar borradores (que se reabren con sus
    /// destinatarios ocultos); nunca para enviar.
    pub keep_bcc: bool,
}

impl BuildOptions {
    /// Fecha actual en la zona local y Message-ID nuevo.
    pub fn now() -> Self {
        Self { date: Local::now().fixed_offset(), message_id: None, max_size: MAX_MESSAGE_BYTES, keep_bcc: false }
    }

    /// Fecha dada (en UTC) y Message-ID nuevo.
    pub fn at(date: DateTime<Utc>) -> Self {
        Self { date: date.fixed_offset(), message_id: None, max_size: MAX_MESSAGE_BYTES, keep_bcc: false }
    }
}

impl Default for BuildOptions {
    fn default() -> Self {
        Self::now()
    }
}

/// Mensaje construido, listo para el transporte.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuiltMessage {
    /// Bytes RFC 5322 (CRLF).
    pub raw: Vec<u8>,
    /// `Message-ID` con `<…>`.
    pub message_id: String,
    /// Remitente del sobre (`MAIL FROM`), dominio en ASCII.
    pub envelope_from: String,
    /// Destinatarios del sobre (`RCPT TO`): Para + CC + CCO, en ASCII y sin repetir.
    pub recipients: Vec<String>,
    /// Consejo de tamaño.
    pub size_advice: SizeAdvice,
}

/// Quien entrega el contenido de los adjuntos.
pub trait ContentResolver {
    /// Contenido completo, o un texto con el motivo del fallo. `max` es el tope en bytes.
    fn load(&self, source: &AttachmentSource, max: u64) -> std::result::Result<Vec<u8>, String>;
}

/// Lee archivos del disco y bytes en memoria (no tiene acceso a blobs).
#[derive(Debug, Clone, Copy, Default)]
pub struct FsResolver;

fn read_file(path: &Path, max: u64) -> std::result::Result<Vec<u8>, String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err("no es un archivo".to_string());
    }
    if meta.len() > max {
        return Err(format!("demasiado grande ({} bytes)", meta.len()));
    }
    std::fs::read(path).map_err(|e| e.to_string())
}

impl ContentResolver for FsResolver {
    fn load(&self, source: &AttachmentSource, max: u64) -> std::result::Result<Vec<u8>, String> {
        match source {
            AttachmentSource::Path(p) => read_file(p, max),
            AttachmentSource::Bytes(b) if b.len() as u64 <= max => Ok(b.clone()),
            AttachmentSource::Bytes(b) => Err(format!("demasiado grande ({} bytes)", b.len())),
            AttachmentSource::Blob(_) => Err("blob sin almacén".to_string()),
        }
    }
}

/// Lee además los blobs del almacén.
#[derive(Debug, Clone, Copy)]
pub struct StoreResolver<'a> {
    pub store: &'a Store,
}

impl ContentResolver for StoreResolver<'_> {
    fn load(&self, source: &AttachmentSource, max: u64) -> std::result::Result<Vec<u8>, String> {
        match source {
            AttachmentSource::Blob(h) => {
                let data = self.store.blobs().get(h).map_err(|e| e.to_string())?;
                if data.len() as u64 > max {
                    return Err(format!("demasiado grande ({} bytes)", data.len()));
                }
                Ok(data)
            }
            other => FsResolver.load(other, max),
        }
    }
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// 64 bits aleatorios (del SO; si fallara, reloj + contador mezclados).
pub(crate) fn random_u64() -> u64 {
    let mut buf = [0u8; 8];
    if getrandom::getrandom(&mut buf).is_ok() {
        return u64::from_le_bytes(buf);
    }
    let nanos = Utc::now().timestamp_nanos_opt().unwrap_or(0) as u64;
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    // SplitMix64.
    let mut z = nanos ^ n.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// `Message-ID` nuevo (`<marca.azar@dominio>`) con el dominio (ASCII) de la dirección dada.
pub fn generate_message_id(from_addr: &str) -> String {
    let domain =
        from_addr.rsplit_once('@').and_then(|(_, d)| domain_ascii(d)).unwrap_or_else(|| "iuredesk.invalid".to_string());
    let ts = Utc::now().timestamp_millis().max(0) as u64;
    format!("<{}.{:016x}@{domain}>", base36(ts), random_u64())
}

fn base36(mut n: u64) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = Vec::new();
    loop {
        out.push(DIGITS.get((n % 36) as usize).copied().unwrap_or(b'0'));
        n /= 36;
        if n == 0 {
            break;
        }
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

/// Normaliza un identificador de mensaje (`<x@y>`): `None` si no lo parece.
pub(crate) fn clean_msg_id(id: &str) -> Option<String> {
    let t = id.trim();
    let inner = t.strip_prefix('<').and_then(|x| x.strip_suffix('>')).unwrap_or(t);
    let ok = !inner.is_empty()
        && inner.len() <= 250
        && inner.contains('@')
        && inner.chars().all(|c| c.is_ascii_graphic() && c != '<' && c != '>');
    ok.then(|| format!("<{inner}>"))
}

/// Recorta `References` como recomienda RFC 5322: el primero y los más recientes.
pub(crate) fn trim_references(refs: &[String]) -> Vec<String> {
    let clean: Vec<String> = refs.iter().filter_map(|r| clean_msg_id(r)).collect();
    let mut out: Vec<String> = Vec::new();
    for r in clean {
        if !out.contains(&r) {
            out.push(r);
        }
    }
    let max = MAX_REFERENCES.min(30);
    if out.len() > max {
        let first = out.first().cloned();
        let tail_start = out.len().saturating_sub(max.saturating_sub(1));
        let mut kept: Vec<String> = first.into_iter().collect();
        kept.extend(out.get(tail_start..).unwrap_or(&[]).iter().cloned());
        return kept;
    }
    out
}

/// Construye el mensaje.
pub fn build_message(draft: &Draft, resolver: &dyn ContentResolver, opts: &BuildOptions) -> Result<BuiltMessage> {
    draft.validate()?;
    let from = Mailbox::new(draft.identity.name.clone(), draft.identity.email.clone());
    let from_header =
        encode::format_mailbox(&from).map_err(|_| ComposeError::Identity(draft.identity.email.clone()))?;
    let envelope_from =
        ascii_address(&draft.identity.email).map_err(|_| ComposeError::Identity(draft.identity.email.clone()))?;
    let mut recipients = Vec::new();
    for m in draft.all_recipients() {
        let a = ascii_address(&m.addr)?;
        if !recipients.iter().any(|r: &String| r.eq_ignore_ascii_case(&a)) {
            recipients.push(a);
        }
    }
    let message_id =
        opts.message_id.as_deref().and_then(clean_msg_id).unwrap_or_else(|| generate_message_id(&draft.identity.email));

    // --- Cabeceras -------------------------------------------------------------------------
    let mut b = MessageBuilder::new();
    let mut h = |name: &'static str, value: String| {
        b.headers.push((Cow::Borrowed(name), Raw::new(value).into()));
    };
    h("Date", opts.date.to_rfc2822());
    h("From", from_header.clone());
    if let Some(rt) = draft.identity.reply_to.as_deref().map(str::trim).filter(|r| !r.is_empty())
        && let Ok(m) = parse_address(rt)
        && let Ok(v) = encode::format_mailbox(&m)
    {
        h("Reply-To", v);
    }
    if draft.to.is_empty() {
        // Solo CCO: grupo vacío, como hacen los demás clientes (RFC 5322 §3.6.3).
        h("To", "undisclosed-recipients:;".to_string());
    } else {
        h("To", encode::format_mailbox_list(&draft.to)?);
    }
    if !draft.cc.is_empty() {
        h("Cc", encode::format_mailbox_list(&draft.cc)?);
    }
    if opts.keep_bcc && !draft.bcc.is_empty() {
        h("Bcc", encode::format_mailbox_list(&draft.bcc)?);
    }
    h("Subject", encode::encode_text(&draft.subject));
    h("Message-ID", message_id.clone());
    if let Some(irt) = draft.in_reply_to.as_deref().and_then(clean_msg_id) {
        h("In-Reply-To", irt);
    }
    let refs = trim_references(&draft.references);
    if !refs.is_empty() {
        h("References", refs.join(" "));
    }
    h("MIME-Version", "1.0".to_string());
    h("User-Agent", USER_AGENT.to_string());
    match draft.priority {
        Priority::High => {
            h("X-Priority", "1 (Highest)".to_string());
            h("Importance", "high".to_string());
        }
        Priority::Low => {
            h("X-Priority", "5 (Lowest)".to_string());
            h("Importance", "low".to_string());
        }
        Priority::Normal => {}
    }
    if draft.request_read_receipt {
        h("Disposition-Notification-To", from_header);
    }
    if let Some(a) = draft.auto_submitted {
        // RFC 3834 §5: que nadie (ni otro contestador) responda a esto.
        h("Auto-Submitted", a.header_value().to_string());
        if a == crate::draft::AutoSubmitted::AutoReplied {
            h("X-Auto-Response-Suppress", "All".to_string());
        }
    }

    // --- Cuerpo ----------------------------------------------------------------------------
    let mut budget = opts.max_size;
    let text_part = MimePart::new(
        ContentType::new("text/plain").attribute("charset", "utf-8").attribute("format", "flowed"),
        BodyPart::Text(draft.body.to_plain().into()),
    );

    let mut inline_parts = Vec::new();
    let mut attachment_parts = Vec::new();
    for a in &draft.attachments {
        let data = load(resolver, a, budget)?;
        budget = budget.saturating_sub(data.len() as u64);
        match &a.kind {
            AttachmentKind::Inline { content_id } if !draft.plain_only => {
                inline_parts.push(inline_part(a, content_id, data));
            }
            _ => attachment_parts.push(attachment_part(a, data)),
        }
    }
    if let Some(fwd) = &draft.forwarded {
        let raw = resolver
            .load(&fwd.source, budget)
            .map_err(|reason| ComposeError::Attachment { name: fwd.filename.clone(), reason })?;
        attachment_parts.push(message_part(&fwd.filename, &raw));
    }

    let mut alternative = vec![text_part];
    if !draft.plain_only {
        let html_part = MimePart::new(
            ContentType::new("text/html").attribute("charset", "utf-8"),
            BodyPart::Text(draft.body.to_html().into()),
        );
        if inline_parts.is_empty() {
            alternative.push(html_part);
        } else {
            let mut related = vec![html_part];
            related.extend(inline_parts);
            alternative.push(MimePart::new(ContentType::new("multipart/related"), related));
        }
    }
    if let Some(cal) = &draft.calendar {
        let ics = String::from_utf8_lossy(&cal.ics).into_owned();
        alternative.push(MimePart::new(
            ContentType::new("text/calendar").attribute("charset", "utf-8").attribute("method", cal.method.as_str()),
            BodyPart::Text(ics.into()),
        ));
        if cal.attach_copy {
            let mut part = MimePart::raw(BodyPart::Binary(cal.ics.clone().into()));
            part.headers.push((
                "Content-Type".into(),
                Raw::new(format!("application/ics; {}", encode::legacy_name_param("invite.ics"))).into(),
            ));
            part.headers.push((
                "Content-Disposition".into(),
                Raw::new(format!("attachment; {}", encode::param("filename", "invite.ics"))).into(),
            ));
            attachment_parts.push(part);
        }
    }

    let body = if alternative.len() == 1 && attachment_parts.is_empty() {
        alternative.pop().unwrap_or_else(|| MimePart::new("text/plain", ""))
    } else {
        let alt = if alternative.len() == 1 {
            alternative.pop().unwrap_or_else(|| MimePart::new("text/plain", ""))
        } else {
            MimePart::new(ContentType::new("multipart/alternative"), alternative)
        };
        if attachment_parts.is_empty() {
            alt
        } else {
            let mut mixed = vec![alt];
            mixed.extend(attachment_parts);
            MimePart::new(ContentType::new("multipart/mixed"), mixed)
        }
    };
    let raw = b.body(body).write_to_vec().map_err(|e| ComposeError::Build(e.to_string()))?;
    let size = raw.len() as u64;
    if size > opts.max_size {
        return Err(ComposeError::TooLarge { size, max: opts.max_size });
    }
    Ok(BuiltMessage { raw, message_id, envelope_from, recipients, size_advice: SizeAdvice::for_size(size) })
}

fn load(resolver: &dyn ContentResolver, a: &Attachment, budget: u64) -> Result<Vec<u8>> {
    resolver.load(&a.source, budget).map_err(|reason| ComposeError::Attachment { name: clean(&a.filename), reason })
}

fn display_filename(name: &str) -> String {
    let n: String = clean(name).replace(['/', '\\'], "_").trim().chars().take(255).collect();
    if n.is_empty() { "adjunto".to_string() } else { n }
}

fn attachment_part(a: &Attachment, data: Vec<u8>) -> MimePart<'static> {
    let name = display_filename(&a.filename);
    let mut part = MimePart::raw(BodyPart::Binary(data.into()));
    part.headers.push((
        "Content-Type".into(),
        Raw::new(format!("{}; {}", a.effective_mime(), encode::legacy_name_param(&name))).into(),
    ));
    part.headers.push((
        "Content-Disposition".into(),
        Raw::new(format!("attachment; {}", encode::param("filename", &name))).into(),
    ));
    part
}

fn inline_part(a: &Attachment, content_id: &str, data: Vec<u8>) -> MimePart<'static> {
    let name = display_filename(&a.filename);
    let cid: String = content_id
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .chars()
        .filter(|c| c.is_ascii_graphic() && *c != '<' && *c != '>')
        .collect();
    let mut part = MimePart::raw(BodyPart::Binary(data.into()));
    part.headers.push((
        "Content-Type".into(),
        Raw::new(format!("{}; {}", a.effective_mime(), encode::legacy_name_param(&name))).into(),
    ));
    part.headers
        .push(("Content-Disposition".into(), Raw::new(format!("inline; {}", encode::param("filename", &name))).into()));
    part.headers.push(("Content-ID".into(), Raw::new(format!("<{cid}>")).into()));
    part
}

/// Correo original como `message/rfc822`. Según RFC 2046 §5.2.1 no admite base64: va en 7bit u
/// 8bit con los finales de línea normalizados a CRLF.
fn message_part(filename: &str, raw: &[u8]) -> MimePart<'static> {
    let mut norm = Vec::with_capacity(raw.len().saturating_add(raw.len() / 50));
    let mut prev = 0u8;
    for &c in raw {
        if c == b'\n' && prev != b'\r' {
            norm.push(b'\r');
        }
        if c == b'\r' {
            // Un CR suelto se convierte en CRLF al ver lo siguiente.
            norm.push(c);
            prev = c;
            continue;
        }
        if prev == b'\r' && c != b'\n' {
            norm.push(b'\n');
        }
        norm.push(c);
        prev = c;
    }
    if prev == b'\r' {
        norm.push(b'\n');
    }
    let cte = if norm.is_ascii() { "7bit" } else { "8bit" };
    let name = display_filename(filename);
    let mut part = MimePart::raw(BodyPart::Binary(norm.into()));
    part.headers.push((
        "Content-Type".into(),
        Raw::new(format!("message/rfc822; {}", encode::legacy_name_param(&name))).into(),
    ));
    part.headers.push((
        "Content-Disposition".into(),
        Raw::new(format!("attachment; {}", encode::param("filename", &name))).into(),
    ));
    part.headers.push(("Content-Transfer-Encoding".into(), Raw::new(cte).into()));
    part
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_ids() {
        let id = generate_message_id("ana@correo.méxico");
        assert!(id.starts_with('<') && id.ends_with("@correo.xn--mxico-bsa>"), "{id}");
        assert_ne!(generate_message_id("a@b.co"), generate_message_id("a@b.co"));
        assert!(generate_message_id("roto").ends_with("@iuredesk.invalid>"));
        assert_eq!(clean_msg_id(" x@y "), Some("<x@y>".into()));
        assert_eq!(clean_msg_id("<x y@z>"), None);
        assert_eq!(clean_msg_id("<>"), None);
        assert_eq!(base36(0), "0");
        assert_eq!(base36(36), "10");
    }

    #[test]
    fn references_trimmed() {
        let refs: Vec<String> = (0..100).map(|i| format!("<{i}@x>")).collect();
        let t = trim_references(&refs);
        assert_eq!(t.len(), 30);
        assert_eq!(t[0], "<0@x>");
        assert_eq!(t.last().unwrap(), "<99@x>");
        assert_eq!(trim_references(&["<a@b>".into(), "a@b".into(), "basura".into()]), vec!["<a@b>".to_string()]);
    }

    #[test]
    fn rfc822_line_endings() {
        let p = message_part("x.eml", b"A: b\nC: d\r\n\r\nhola\rmundo");
        let BodyPart::Binary(b) = &p.contents else { panic!() };
        assert_eq!(b.as_ref(), b"A: b\r\nC: d\r\n\r\nhola\r\nmundo");
    }
}
