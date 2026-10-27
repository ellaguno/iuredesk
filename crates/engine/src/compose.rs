//! Mensajes en redacción sobre el modelo de `iuredesk-compose` ([`Draft`]): nuevo, responder,
//! responder a todos, reenviar y enlaces `mailto:`; el borrador guardado en «Borradores» y las
//! horas propuestas para el envío programado.
//!
//! Lo que necesita el almacén o las cuentas (identidades, firmas, el crudo del original, enviar)
//! vive en la sesión; aquí solo hay datos y funciones puras.

use chrono::{DateTime, Datelike, Duration, NaiveTime, Utc, Weekday};
use iuredesk_compose::{Draft, RichBody};
use iuredesk_model::account::Identity;
use iuredesk_model::{AccountId, Mailbox, MessageId};

use crate::lang::Text;
use crate::time::Zone;

/// Tamaño máximo de un campo de un `mailto:`.
const MAX_FIELD: usize = 16 * 1024;

/// Qué originó el borrador.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ComposeKind {
    #[default]
    New,
    Reply,
    ReplyAll,
    Forward,
}

/// Un mensaje en redacción.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComposeDraft {
    pub kind: ComposeKind,
    /// Cuenta desde la que se envía (la del mensaje original, o la primera).
    pub account: Option<AccountId>,
    /// El borrador (identidad, destinatarios, asunto, cuerpo enriquecido, adjuntos…).
    pub draft: Draft,
    /// Versión guardada en «Borradores» (se reemplaza al volver a guardar y se borra al enviar).
    pub saved: Option<MessageId>,
}

impl ComposeDraft {
    /// Borrador vacío para una identidad.
    pub fn new(account: Option<AccountId>, identity: Identity) -> Self {
        ComposeDraft { kind: ComposeKind::New, account, draft: Draft::new(identity), saved: None }
    }

    /// `true` si no hay nada escrito (destinatarios, asunto, texto propio ni adjuntos).
    pub fn is_blank(&self) -> bool {
        let d = &self.draft;
        let mut body = d.body.clone();
        body.remove_signature();
        d.to.is_empty()
            && d.cc.is_empty()
            && d.bcc.is_empty()
            && d.subject.trim().is_empty()
            && body.is_empty()
            && d.attachments.is_empty()
    }
}

/// Campos de un enlace `mailto:` (RFC 6068).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Mailto {
    pub to: String,
    pub cc: String,
    pub bcc: String,
    pub subject: String,
    pub body: String,
}

fn clip(s: &str) -> String {
    match s.char_indices().nth(MAX_FIELD) {
        Some((i, _)) => s.get(..i).unwrap_or(s).to_string(),
        None => s.to_string(),
    }
}

/// Decodifica `%XX` (UTF-8) y, si `plus`, `+` como espacio. Lo mal formado se deja tal cual.
fn percent_decode(s: &str, plus: bool) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while let Some(&b) = bytes.get(i) {
        if b == b'%'
            && let (Some(h), Some(l)) = (bytes.get(i + 1), bytes.get(i + 2))
            && let (Some(h), Some(l)) = ((*h as char).to_digit(16), (*l as char).to_digit(16))
        {
            out.push((h * 16 + l) as u8);
            i += 3;
            continue;
        }
        out.push(if plus && b == b'+' { b' ' } else { b });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl Mailto {
    /// Lee un enlace `mailto:`. `None` si no lo es.
    pub fn parse(url: &str) -> Option<Mailto> {
        let rest = url.trim();
        let rest = rest.get(..7).filter(|p| p.eq_ignore_ascii_case("mailto:")).and_then(|_| rest.get(7..))?;
        let (to, query) = rest.split_once('?').unwrap_or((rest, ""));
        let mut d = Mailto { to: clip(&percent_decode(to, false)), ..Default::default() };
        for pair in query.split('&').take(32) {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            let v = clip(&percent_decode(v, false));
            match k.to_ascii_lowercase().as_str() {
                "to" => {
                    if d.to.is_empty() {
                        d.to = v;
                    } else {
                        d.to = format!("{}, {v}", d.to);
                    }
                }
                "cc" => d.cc = v,
                "bcc" => d.bcc = v,
                "subject" => d.subject = v,
                "body" => d.body = v,
                _ => {}
            }
        }
        Some(d)
    }
}

/// Direcciones escritas a mano («Ana <ana@x>, b@y»), a lo mejor posible: lo que no se entiende se
/// conserva como dirección tal cual (el redactor lo marca en rojo y no deja enviar).
pub fn lenient_addresses(field: &str) -> Vec<Mailbox> {
    field
        .split([',', ';'])
        .take(iuredesk_model::MAX_RECIPIENTS)
        .filter_map(|p| {
            let p = p.trim();
            if p.is_empty() {
                return None;
            }
            Some(iuredesk_compose::parse_address(p).unwrap_or_else(|_| Mailbox::addr(p)))
        })
        .collect()
}

/// Borrador a partir de un `mailto:` (sin firma: la pone la sesión).
pub fn draft_from_mailto(m: &Mailto, identity: Identity) -> Draft {
    let mut d = Draft::new(identity);
    d.to = lenient_addresses(&m.to);
    d.cc = lenient_addresses(&m.cc);
    d.bcc = lenient_addresses(&m.bcc);
    d.subject = m.subject.clone();
    d.body = RichBody::from_plain(&m.body);
    d
}

/// Mensaje RFC 5322 de texto plano para guardar un borrador que aún no se puede construir con
/// MIME completo (p. ej. sin destinatarios).
pub fn plain_rfc822(d: &Draft, date: DateTime<Utc>, message_id: &str) -> Vec<u8> {
    let header_value = |s: &str| -> String {
        let s: String = s.chars().filter(|c| *c != '\r' && *c != '\n').collect();
        if s.is_ascii() { s } else { format!("=?utf-8?B?{}?=", b64(s.as_bytes())) }
    };
    let addr_line = |l: &[Mailbox]| -> String {
        l.iter()
            .map(|m| match &m.name {
                Some(n) if !n.is_empty() => format!("{} <{}>", header_value(n), header_value(&m.addr)),
                _ => header_value(&m.addr),
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    let from = Mailbox::new(d.identity.name.clone(), d.identity.email.clone());
    let mut s = String::new();
    s.push_str(&format!("From: {}\r\n", addr_line(std::slice::from_ref(&from))));
    if !d.to.is_empty() {
        s.push_str(&format!("To: {}\r\n", addr_line(&d.to)));
    }
    if !d.cc.is_empty() {
        s.push_str(&format!("Cc: {}\r\n", addr_line(&d.cc)));
    }
    s.push_str(&format!("Subject: {}\r\n", header_value(&d.subject)));
    s.push_str(&format!("Date: {}\r\n", date.to_rfc2822()));
    s.push_str(&format!("Message-ID: {}\r\n", header_value(message_id)));
    if let Some(r) = &d.in_reply_to {
        s.push_str(&format!("In-Reply-To: {}\r\n", header_value(r)));
    }
    s.push_str(
        "MIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n",
    );
    for line in d.body.to_plain().lines().take(100_000) {
        s.push_str(line);
        s.push_str("\r\n");
    }
    s.into_bytes()
}

fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk.first().copied().unwrap_or(0),
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for (k, shift) in [18u32, 12, 6, 0].into_iter().enumerate() {
            if k <= chunk.len() {
                out.push(char::from(T.get(((n >> shift) & 63) as usize).copied().unwrap_or(b'A')));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Horas propuestas para «Programar envío»: más tarde hoy (18:00, si aún no pasó de las 17:00),
/// mañana por la mañana (08:00) y, de viernes a domingo, el lunes por la mañana.
pub fn schedule_presets(now: DateTime<Utc>, zone: Zone) -> Vec<(Text, DateTime<Utc>)> {
    let local = zone.local(now);
    let today = local.date();
    let at =
        |d: chrono::NaiveDate, h: u32| zone.to_utc(d.and_time(NaiveTime::from_hms_opt(h, 0, 0).unwrap_or_default()));
    let mut out = Vec::new();
    if local.time() < NaiveTime::from_hms_opt(17, 0, 0).unwrap_or_default() {
        out.push((Text::new("Later today (18:00)", "Más tarde hoy (18:00)"), at(today, 18)));
    }
    let tomorrow = today + Duration::days(1);
    out.push((Text::new("Tomorrow morning (08:00)", "Mañana por la mañana (08:00)"), at(tomorrow, 8)));
    if matches!(today.weekday(), Weekday::Fri | Weekday::Sat | Weekday::Sun) {
        let mut d = tomorrow;
        while d.weekday() != Weekday::Mon {
            d += Duration::days(1);
        }
        if d != tomorrow {
            out.push((Text::new("Monday morning (08:00)", "El lunes por la mañana (08:00)"), at(d, 8)));
        }
    }
    out
}

/// Interpreta la fecha y hora que escribe el usuario para programar («2026-10-27 09:30», «27/10/2026
/// 9:30»). `None` si no se entiende o ya pasó.
pub fn parse_schedule(date: &str, time: &str, now: DateTime<Utc>, zone: Zone) -> Option<DateTime<Utc>> {
    let date = date.trim();
    let d = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .or_else(|_| chrono::NaiveDate::parse_from_str(date, "%d/%m/%Y"))
        .ok()?;
    let t = NaiveTime::parse_from_str(time.trim(), "%H:%M").ok()?;
    let at = zone.to_utc(d.and_time(t));
    (at > now).then_some(at)
}

/// Texto corto de una hora programada («mañana 08:00», «lun 27 oct, 09:30»).
pub fn schedule_label(at: DateTime<Utc>, now: DateTime<Utc>, zone: Zone) -> String {
    let (l, n) = (zone.local(at), zone.local(now));
    let hm = l.format("%H:%M");
    if l.date() == n.date() {
        format!("{} {hm}", crate::lang::pick("today", "hoy"))
    } else if Some(l.date()) == n.date().succ_opt() {
        format!("{} {hm}", crate::lang::pick("tomorrow", "mañana"))
    } else {
        let mon: String = crate::time::month_name(l.month()).chars().take(3).collect();
        format!("{} {} {mon}, {hm}", crate::time::weekday_short(l.date()), l.day())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn ident() -> Identity {
        Identity { name: "Yo".into(), email: "yo@example.com".into(), signature: None, reply_to: None }
    }

    #[test]
    fn mailto() {
        let d = Mailto::parse("mailto:ana@example.com?subject=Hola%20Ana&body=L%C3%ADnea+1&cc=b@example.com").unwrap();
        assert_eq!(d.to, "ana@example.com");
        assert_eq!(d.subject, "Hola Ana");
        assert_eq!(d.body, "Línea+1", "en mailto «+» es literal");
        assert_eq!(d.cc, "b@example.com");
        assert!(Mailto::parse("https://example.com").is_none());
        let d = Mailto::parse("MAILTO:?to=x@example.com&to=y@example.com").unwrap();
        assert_eq!(d.to, "x@example.com, y@example.com");
        // Mal formado: no falla.
        let d = Mailto::parse("mailto:%zz%4").unwrap();
        assert_eq!(d.to, "%zz%4");
        let draft = draft_from_mailto(&Mailto::parse("mailto:Ana <ana@example.com>, malo").unwrap(), ident());
        assert_eq!(draft.to.len(), 2);
        assert_eq!(draft.to[0].name.as_deref(), Some("Ana"));
        assert!(draft.validate().is_err(), "«malo» no es una dirección");
    }

    #[test]
    fn blank_ignores_signature() {
        let mut c = ComposeDraft::new(None, ident());
        assert!(c.is_blank());
        c.draft
            .body
            .blocks
            .push(iuredesk_compose::Block::Signature { blocks: vec![iuredesk_compose::Block::paragraph("Yo")] });
        assert!(c.is_blank());
        c.draft.subject = "x".into();
        assert!(!c.is_blank());
    }

    #[test]
    fn plain_fallback_is_parseable() {
        let mut d = Draft::new(Identity { name: "Yó".into(), ..ident() });
        d.subject = "Café".into();
        d.body = RichBody::from_plain("hola\n\nadiós");
        let raw = plain_rfc822(&d, Utc::now(), "<a@b>");
        let body = crate::body::parse(&raw);
        assert!(body.text.contains("adiós"));
        assert!(String::from_utf8_lossy(&raw).contains("Subject: =?utf-8?B?"));
        assert_eq!(b64(b"Ma"), "TWE=");
    }

    #[test]
    fn schedule() {
        crate::lang::set_lang(crate::lang::Lang::Es);
        let z = Zone::utc();
        // Miércoles 10:00: más tarde hoy y mañana.
        let wed = Utc.with_ymd_and_hms(2026, 10, 21, 10, 0, 0).unwrap();
        let p = schedule_presets(wed, z);
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].1, Utc.with_ymd_and_hms(2026, 10, 21, 18, 0, 0).unwrap());
        assert_eq!(p[1].1, Utc.with_ymd_and_hms(2026, 10, 22, 8, 0, 0).unwrap());
        // Viernes por la noche: mañana y el lunes.
        let fri = Utc.with_ymd_and_hms(2026, 10, 23, 20, 0, 0).unwrap();
        let p = schedule_presets(fri, z);
        assert_eq!(p.len(), 2);
        assert_eq!(p[1].1, Utc.with_ymd_and_hms(2026, 10, 26, 8, 0, 0).unwrap());
        assert_eq!(
            parse_schedule("2026-10-27", "09:30", wed, z),
            Some(Utc.with_ymd_and_hms(2026, 10, 27, 9, 30, 0).unwrap())
        );
        assert_eq!(parse_schedule("27/10/2026", "9:30", wed, z).map(|t| t.day()), Some(27));
        assert_eq!(parse_schedule("2020-01-01", "09:30", wed, z), None, "en el pasado");
        assert_eq!(parse_schedule("mañana", "x", wed, z), None);
        assert_eq!(schedule_label(p[0].1, fri, z), "mañana 08:00");
    }
}
