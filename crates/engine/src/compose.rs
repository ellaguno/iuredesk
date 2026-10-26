//! Borradores de mensaje: nuevo, responder, responder a todos, reenviar y enlaces `mailto:`.
//! El envío real llega en F1 (SMTP); por ahora el borrador se guarda en «Borradores».

use iuredesk_model::{AccountId, Mailbox, MessageEnvelope, MessageId};

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
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ComposeDraft {
    pub kind: ComposeKind,
    /// Cuenta desde la que se envía (la del mensaje original, o la primera).
    pub account: Option<AccountId>,
    pub to: String,
    pub cc: String,
    pub subject: String,
    pub body: String,
    /// Mensaje al que responde o que reenvía.
    pub source: Option<MessageId>,
    pub in_reply_to: Option<String>,
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

fn fmt_list(list: &[Mailbox]) -> String {
    list.iter()
        .map(|m| match &m.name {
            Some(n) if !n.is_empty() => format!("{n} <{}>", m.addr),
            _ => m.addr.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Asunto con prefijo, sin repetirlo («Re: Re: …»).
fn prefixed(prefix: &str, subject: &str) -> String {
    let (base, _) = iuredesk_model::normalize_subject(subject);
    format!("{prefix} {base}")
}

fn quote(text: &str) -> String {
    text.lines().take(400).map(|l| format!("> {l}")).collect::<Vec<_>>().join("\n")
}

impl ComposeDraft {
    /// Borrador a partir de un enlace `mailto:` (RFC 6068): destinatarios, `cc`, `subject`, `body`.
    /// Devuelve `None` si no es un `mailto:`.
    pub fn from_mailto(url: &str) -> Option<ComposeDraft> {
        let rest = url.trim();
        let rest = rest.get(..7).filter(|p| p.eq_ignore_ascii_case("mailto:")).and_then(|_| rest.get(7..))?;
        let (to, query) = rest.split_once('?').unwrap_or((rest, ""));
        let mut d = ComposeDraft { to: clip(&percent_decode(to, false)), ..Default::default() };
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
                "subject" => d.subject = v,
                "body" => d.body = v,
                _ => {}
            }
        }
        Some(d)
    }

    /// Respuesta (a todos si `all`) a `env`, citando `body`. `me` son mis direcciones (no me
    /// incluyo en «Para»/«Cc»).
    pub fn reply(env: &MessageEnvelope, body: &str, all: bool, me: &[String]) -> ComposeDraft {
        let is_me = |m: &Mailbox| me.iter().any(|a| a.eq_ignore_ascii_case(&m.addr));
        let target = if env.reply_to.is_empty() { &env.from } else { &env.reply_to };
        let mut to: Vec<Mailbox> = target.iter().filter(|m| !is_me(m)).cloned().collect();
        if to.is_empty() {
            // Respondo a algo que envié yo: va a los destinatarios originales.
            to = env.to.clone();
        }
        let mut cc = Vec::new();
        if all {
            for m in env.to.iter().chain(env.cc.iter()) {
                if !is_me(m) && !to.iter().any(|t| t.addr.eq_ignore_ascii_case(&m.addr)) {
                    cc.push(m.clone());
                }
            }
        }
        let who = env.from.first().map(|m| m.display().to_string()).unwrap_or_default();
        ComposeDraft {
            kind: if all { ComposeKind::ReplyAll } else { ComposeKind::Reply },
            account: Some(env.account),
            to: fmt_list(&to),
            cc: fmt_list(&cc),
            subject: prefixed("Re:", &env.subject),
            body: format!("\n\n{who} {}:\n{}", crate::lang::pick("wrote", "escribió"), quote(body)),
            source: Some(env.id),
            in_reply_to: env.message_id.clone(),
        }
    }

    /// Reenvío de `env` con su cuerpo.
    pub fn forward(env: &MessageEnvelope, body: &str) -> ComposeDraft {
        let header = format!(
            "\n\n---------- {} ----------\n{}: {}\n{}: {}\n{}: {}\n\n",
            crate::lang::pick("Forwarded message", "Mensaje reenviado"),
            crate::lang::pick("From", "De"),
            fmt_list(&env.from),
            crate::lang::pick("Subject", "Asunto"),
            env.subject,
            crate::lang::pick("To", "Para"),
            fmt_list(&env.to),
        );
        ComposeDraft {
            kind: ComposeKind::Forward,
            account: Some(env.account),
            subject: prefixed("Fwd:", &env.subject),
            body: format!("{header}{body}"),
            source: Some(env.id),
            ..Default::default()
        }
    }

    /// Direcciones de un campo «Para»/«Cc» escrito a mano («Ana <ana@x>, b@y»).
    pub fn parse_addresses(field: &str) -> Vec<Mailbox> {
        field
            .split([',', ';'])
            .take(iuredesk_model::MAX_RECIPIENTS)
            .filter_map(|p| {
                let p = p.trim();
                if p.is_empty() {
                    return None;
                }
                match (p.rfind('<'), p.rfind('>')) {
                    (Some(a), Some(b)) if a < b => {
                        let addr = p.get(a + 1..b)?.trim();
                        let name = p.get(..a).unwrap_or("").trim().trim_matches('"');
                        Some(if name.is_empty() { Mailbox::addr(addr) } else { Mailbox::new(name, addr) })
                    }
                    _ => Some(Mailbox::addr(p)),
                }
            })
            .collect()
    }

    /// `true` si no hay nada escrito.
    pub fn is_blank(&self) -> bool {
        self.to.trim().is_empty() && self.cc.trim().is_empty() && self.subject.trim().is_empty() && self.body.trim().is_empty()
    }
}

/// Encabezado con caracteres no ASCII codificado como palabra MIME (RFC 2047, base64 sencillo).
fn header_value(s: &str) -> String {
    let s: String = s.chars().filter(|c| *c != '\r' && *c != '\n').collect();
    if s.is_ascii() {
        return s;
    }
    format!("=?utf-8?B?{}?=", b64(s.as_bytes()))
}

fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk.first().copied().unwrap_or(0), chunk.get(1).copied().unwrap_or(0), chunk.get(2).copied().unwrap_or(0)];
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

/// Mensaje RFC 822 (texto plano UTF-8) de un borrador.
pub fn render_rfc822(from: &Mailbox, d: &ComposeDraft, date: chrono::DateTime<chrono::Utc>) -> Vec<u8> {
    let addr_line = |l: &[Mailbox]| -> String {
        l.iter()
            .map(|m| match &m.name {
                Some(n) if !n.is_empty() => format!("{} <{}>", header_value(n), m.addr),
                _ => m.addr.clone(),
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut s = String::new();
    s.push_str(&format!("From: {}\r\n", addr_line(std::slice::from_ref(from))));
    let to = ComposeDraft::parse_addresses(&d.to);
    if !to.is_empty() {
        s.push_str(&format!("To: {}\r\n", addr_line(&to)));
    }
    let cc = ComposeDraft::parse_addresses(&d.cc);
    if !cc.is_empty() {
        s.push_str(&format!("Cc: {}\r\n", addr_line(&cc)));
    }
    s.push_str(&format!("Subject: {}\r\n", header_value(&d.subject)));
    s.push_str(&format!("Date: {}\r\n", date.to_rfc2822()));
    if let Some(r) = &d.in_reply_to {
        s.push_str(&format!("In-Reply-To: <{}>\r\n", header_value(r)));
    }
    s.push_str("MIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n");
    for line in d.body.lines() {
        s.push_str(line);
        s.push_str("\r\n");
    }
    s.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mailto() {
        let d = ComposeDraft::from_mailto("mailto:ana@example.com?subject=Hola%20Ana&body=L%C3%ADnea+1&cc=b@example.com")
            .unwrap();
        assert_eq!(d.to, "ana@example.com");
        assert_eq!(d.subject, "Hola Ana");
        assert_eq!(d.body, "Línea+1", "en mailto «+» es literal");
        assert_eq!(d.cc, "b@example.com");
        assert!(ComposeDraft::from_mailto("https://example.com").is_none());
        let d = ComposeDraft::from_mailto("MAILTO:?to=x@example.com&to=y@example.com").unwrap();
        assert_eq!(d.to, "x@example.com, y@example.com");
        // Mal formado: no falla.
        let d = ComposeDraft::from_mailto("mailto:%zz%4").unwrap();
        assert_eq!(d.to, "%zz%4");
    }

    #[test]
    fn reply_all_excludes_me_and_dedups() {
        let env = MessageEnvelope {
            from: vec![Mailbox::new("Ana", "ana@example.com")],
            to: vec![Mailbox::addr("yo@example.com"), Mailbox::addr("luis@example.com")],
            cc: vec![Mailbox::addr("ana@example.com"), Mailbox::addr("marta@example.com")],
            subject: "Re: Propuesta".into(),
            ..Default::default()
        };
        let me = vec!["YO@example.com".to_string()];
        let r = ComposeDraft::reply(&env, "hola\nadiós", true, &me);
        assert_eq!(r.to, "Ana <ana@example.com>");
        assert_eq!(r.cc, "luis@example.com, marta@example.com");
        assert_eq!(r.subject, "Re: Propuesta");
        assert!(r.body.contains("> hola\n> adiós"));
        let f = ComposeDraft::forward(&env, "cuerpo");
        assert_eq!(f.subject, "Fwd: Propuesta");
        assert!(f.to.is_empty());
    }

    #[test]
    fn addresses_and_rfc822() {
        let a = ComposeDraft::parse_addresses("Ana López <ana@example.com>; b@example.com,  ");
        assert_eq!(a.len(), 2);
        assert_eq!(a[0].name.as_deref(), Some("Ana López"));
        assert_eq!(b64(b"Ma"), "TWE=");
        assert_eq!(b64(b"Man"), "TWFu");
        let d = ComposeDraft { to: "ana@example.com".into(), subject: "Café".into(), body: "hola".into(), ..Default::default() };
        let raw = render_rfc822(&Mailbox::new("Yo", "yo@example.com"), &d, chrono::Utc::now());
        let body = crate::body::parse(&raw);
        assert!(body.text.contains("hola"));
        let text = String::from_utf8_lossy(&raw);
        assert!(text.contains("Subject: =?utf-8?B?"));
    }
}
