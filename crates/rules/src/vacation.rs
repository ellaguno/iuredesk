//! Respuesta de vacaciones y protección contra bucles de lo que se envía solo (RFC 3834).
//!
//! En el servidor (Sieve `vacation`) responde aunque la app esté cerrada; en local, solo mientras
//! IureDesk está abierto (la interfaz lo dice). En los dos casos: una vez por remitente cada N días,
//! nunca a listas, boletines, avisos automáticos ni a uno mismo.

use chrono::{DateTime, Duration, NaiveDate, Utc};
use iuredesk_compose::{Draft, ReplyMode, ReplyOptions};
use iuredesk_model::account::Identity;
use iuredesk_store::Store;
use serde::{Deserialize, Serialize};

use crate::book::{MAX_SENDS_PER_HOUR, SendLog};
use crate::facts::Facts;

/// Respuesta de vacaciones.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Vacation {
    /// Activada.
    pub enabled: bool,
    /// Desde (inclusive, fecha local); `None` = ya.
    #[serde(default)]
    pub start: Option<NaiveDate>,
    /// Hasta (inclusive); `None` = hasta desactivarla.
    #[serde(default)]
    pub end: Option<NaiveDate>,
    /// Asunto.
    #[serde(default)]
    pub subject: String,
    /// Texto.
    #[serde(default)]
    pub body: String,
    /// Días entre respuestas al mismo remitente.
    #[serde(default = "seven")]
    pub days: u32,
    /// Solo a mis contactos.
    #[serde(default)]
    pub contacts_only: bool,
}

fn seven() -> u32 {
    7
}

impl Default for Vacation {
    fn default() -> Self {
        Self {
            enabled: false,
            start: None,
            end: None,
            subject: String::new(),
            body: String::new(),
            days: 7,
            contacts_only: false,
        }
    }
}

impl Vacation {
    /// Activa hoy (`today` = fecha local).
    pub fn active_on(&self, today: NaiveDate) -> bool {
        self.enabled
            && !self.body.trim().is_empty()
            && self.start.is_none_or(|s| today >= s)
            && self.end.is_none_or(|e| today <= e)
    }
}

/// Remitentes que nunca reciben nada automático.
fn automated_sender(addr: &str) -> bool {
    let local = addr.split('@').next().unwrap_or("").to_lowercase();
    const BAD: [&str; 12] = [
        "mailer-daemon",
        "postmaster",
        "noreply",
        "no-reply",
        "no_reply",
        "donotreply",
        "do-not-reply",
        "do_not_reply",
        "bounce",
        "bounces",
        "listserv",
        "majordomo",
    ];
    local.is_empty()
        || BAD.iter().any(|b| local == *b || local.starts_with(&format!("{b}+")) || local.starts_with(&format!("{b}-")))
}

/// El mensaje lo generó una máquina o una lista (RFC 3834 §2): no se le responde ni reenvía solo.
pub fn is_automated(f: &Facts) -> bool {
    let h = &f.headers;
    let auto = h.get("auto-submitted").is_some_and(|v| !v.trim().eq_ignore_ascii_case("no"));
    let prec = h.get("precedence").is_some_and(|v| {
        let v = v.trim().to_lowercase();
        v == "bulk" || v == "list" || v == "junk"
    });
    let ms_auto = h.get("x-auto-response-suppress").is_some_and(|v| {
        let v = v.to_lowercase();
        v.contains("all") || v.contains("oof") || v.contains("autoreply")
    });
    let null_return = h.get("return-path").is_some_and(|v| v.trim() == "<>");
    auto || prec || ms_auto || null_return || h.has("x-loop") || f.is_list() || automated_sender(&f.sender())
}

fn is_mine(addr: &str, mine: &[String]) -> bool {
    let a = crate::norm_addr(addr);
    mine.iter().any(|m| crate::norm_addr(m) == a)
}

/// ¿Se puede reenviar solo este mensaje a `to`?
pub fn may_forward(f: &Facts, to: &str, mine: &[String], log: &SendLog, now: DateTime<Utc>) -> bool {
    let sender = f.sender();
    !sender.is_empty()
        && !is_mine(&sender, mine)
        && crate::norm_addr(to) != sender
        && !is_mine(to, mine)
        && !f.headers.has("x-loop")
        && f.headers.get("auto-submitted").is_none_or(|v| v.trim().eq_ignore_ascii_case("no"))
        && !log.was_forwarded(f.env.message_id.as_deref())
        && log.sends_last_hour(now) < MAX_SENDS_PER_HOUR
}

/// ¿Se puede responder solo a este mensaje? (una vez cada `days` días por remitente).
pub fn may_auto_reply(f: &Facts, mine: &[String], log: &SendLog, now: DateTime<Utc>, days: u32) -> bool {
    let sender = f.sender();
    if sender.is_empty() || is_mine(&sender, mine) || is_automated(f) {
        return false;
    }
    // RFC 3834 §3.1.1: solo si me lo enviaron a mí (no copias de listas ni CCO masivos).
    let to_me = f.env.to.iter().chain(f.env.cc.iter()).any(|m| is_mine(&m.addr, mine));
    if !mine.is_empty() && !to_me {
        return false;
    }
    if log.replied.get(&sender).is_some_and(|t| now.signed_duration_since(*t) < Duration::days(i64::from(days.max(1))))
    {
        return false;
    }
    log.sends_last_hour(now) < MAX_SENDS_PER_HOUR
}

/// Borrador de la respuesta automática: responde al remitente con el asunto y texto dados.
pub fn reply_draft(
    store: &Store,
    f: &Facts,
    identities: &[Identity],
    subject: &str,
    body: &str,
    opts: &ReplyOptions,
) -> Option<Draft> {
    let mut d = match store.get_raw(f.env.id) {
        Ok(raw) => iuredesk_compose::reply(&raw, identities, ReplyMode::Sender, opts).ok()?,
        Err(_) => {
            let ident = identities.first()?.clone();
            let mut d = Draft::new(ident);
            d.to = f.env.reply_to.first().or(f.env.from.first()).cloned().into_iter().collect();
            d.subject = iuredesk_compose::reply_subject(&f.env.subject, Default::default());
            d.in_reply_to = f.env.message_id.as_ref().map(|m| format!("<{m}>"));
            d.references = d.in_reply_to.clone().into_iter().collect();
            d
        }
    };
    if d.to.is_empty() {
        return None;
    }
    d.cc.clear();
    d.bcc.clear();
    d.original = None;
    if !subject.trim().is_empty() {
        d.subject = subject.trim().to_string();
    }
    d.body = crate::apply::plain(body);
    // RFC 3834 §5: así ningún otro contestador nos responde (ni hay bucles).
    d.auto_submitted = Some(iuredesk_compose::AutoSubmitted::AutoReplied);
    Some(d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::Headers;
    use iuredesk_model::{Mailbox, MessageEnvelope};

    fn facts(from: &str, headers: Vec<(&'static str, &'static str)>) -> Facts {
        Facts {
            env: MessageEnvelope {
                from: vec![Mailbox::addr(from)],
                to: vec![Mailbox::addr("yo@despacho.example")],
                message_id: Some("m1@x".into()),
                ..Default::default()
            },
            headers: Headers::from_pairs(headers),
            body: String::new(),
            known_sender: false,
        }
    }

    #[test]
    fn loop_protection() {
        let mine = vec!["yo@despacho.example".to_string()];
        let now = Utc::now();
        let mut log = SendLog::default();
        let human = facts("ana@cliente.example", vec![]);
        assert!(may_auto_reply(&human, &mine, &log, now, 7));
        assert!(may_forward(&human, "socio@despacho.example", &mine, &log, now));
        // A mí mismo, al propio remitente o a un buzón mío: no.
        assert!(!may_forward(&human, "ana@cliente.example", &mine, &log, now));
        assert!(!may_forward(&human, "YO@despacho.example", &mine, &log, now));
        for (h, v) in [
            ("Auto-Submitted", "auto-replied"),
            ("Precedence", "bulk"),
            ("Precedence", "list"),
            ("Precedence", "junk"),
            ("Auto-Submitted", "auto-generated"),
            ("List-Id", "<x.example>"),
            ("X-Loop", "yo"),
            ("Return-Path", "<>"),
            ("X-Auto-Response-Suppress", "All"),
        ] {
            assert!(!may_auto_reply(&facts("ana@cliente.example", vec![(h, v)]), &mine, &log, now, 7), "{h}");
        }
        assert!(!may_auto_reply(&facts("MAILER-DAEMON@x.example", vec![]), &mine, &log, now, 7));
        assert!(!may_auto_reply(&facts("no-reply@x.example", vec![]), &mine, &log, now, 7));
        assert!(!may_auto_reply(&facts("yo@despacho.example", vec![]), &mine, &log, now, 7));
        // Una vez por remitente cada N días.
        log.record_reply("ana@cliente.example", now);
        assert!(!may_auto_reply(&human, &mine, &log, now + Duration::days(6), 7));
        assert!(may_auto_reply(&human, &mine, &log, now + Duration::days(8), 7));
        // Ya reenviado.
        log.record_forward(Some("m1@x"), now);
        assert!(!may_forward(&human, "socio@despacho.example", &mine, &log, now));
        // Tope por hora.
        let mut busy = SendLog::default();
        for _ in 0..MAX_SENDS_PER_HOUR {
            busy.record_reply("z@z.example", now);
        }
        assert!(!may_auto_reply(&facts("b@c.example", vec![]), &mine, &busy, now, 7));
    }

    /// F7.1: la respuesta automática sale con `Auto-Submitted: auto-replied` (RFC 3834 §5).
    #[test]
    fn auto_reply_is_marked_auto_submitted() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let ident =
            Identity { name: "Yo".into(), email: "yo@despacho.example".into(), signature: None, reply_to: None };
        let f = facts("ana@cliente.example", vec![]);
        let d = reply_draft(&store, &f, &[ident], "Fuera de la oficina", "Vuelvo el lunes.", &ReplyOptions::default())
            .unwrap();
        assert_eq!(d.auto_submitted, Some(iuredesk_compose::AutoSubmitted::AutoReplied));
        assert_eq!(d.to.first().map(|m| m.addr.as_str()), Some("ana@cliente.example"));
    }

    #[test]
    fn vacation_window() {
        let d = |m, day| NaiveDate::from_ymd_opt(2026, m, day).unwrap();
        let v = Vacation {
            enabled: true,
            start: Some(d(12, 20)),
            end: Some(d(12, 31)),
            body: "Vuelvo el 2 de enero.".into(),
            ..Default::default()
        };
        assert!(!v.active_on(d(12, 19)));
        assert!(v.active_on(d(12, 20)));
        assert!(v.active_on(d(12, 31)));
        assert!(!v.active_on(d(11, 1)));
        assert!(!Vacation { body: String::new(), ..v }.active_on(d(12, 25)));
    }
}
