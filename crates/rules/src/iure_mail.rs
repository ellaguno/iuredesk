//! UX-5: correo que envía la **propia instancia** de Iurefficient (avisos, menciones, tickets,
//! tareas asignadas…), para ponerle la cápsula de Iurefficient en la lista aunque el remitente no
//! sea un contacto del CRM.
//!
//! La instancia no documenta cabeceras propias en `iurefficient-connect`, así que se mira, sin
//! red y sin IA, por orden:
//! 1. cualquier cabecera `X-Iurefficient-*` (si la instancia algún día las pone; `-Type` dice el
//!    tipo);
//! 2. el asunto con la etiqueta `[Iurefficient…]` que ponen sus avisos;
//! 3. el dominio de `From`, `Return-Path`, `Sender`, `List-Id` o del `Message-ID`: el anfitrión
//!    de la instancia configurada (o un subdominio suyo) y el servicio `iurefficient.com`;
//! 4. una dirección automática (`noreply@`, `notificaciones@`…) del dominio padre del anfitrión
//!    (`crm.acme.com` → `noreply@acme.com`); una persona de ese dominio (un colega) **no** cuenta.
//!
//! Todo dato es hostil: tamaños acotados, sin índices.

use iuredesk_model::MessageEnvelope;

use crate::facts::Headers;

/// Dominio del servicio de Iurefficient (instancias `*.iurefficient.com`).
pub const SAAS_DOMAIN: &str = "iurefficient.com";

/// Partes locales de las direcciones automáticas que se aceptan del dominio padre del anfitrión.
const AUTOMATED: [&str; 12] = [
    "noreply",
    "no-reply",
    "no_reply",
    "donotreply",
    "do-not-reply",
    "notificaciones",
    "notifications",
    "notificacion",
    "notification",
    "avisos",
    "alertas",
    "iurefficient",
];

/// Largo máximo del texto que acompaña al aviso.
const MAX_WHAT: usize = 120;

/// Tipo de aviso, deducido del asunto o de `X-Iurefficient-Type`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InstanceKind {
    Ticket,
    Task,
    Opportunity,
    Lead,
    Case,
    /// Mención, aviso general u otro.
    Notice,
}

/// Un correo enviado por la instancia.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstanceHit {
    pub kind: InstanceKind,
    /// Asunto sin la etiqueta `[Iurefficient]` (acotado).
    pub what: String,
}

/// Anfitrión normalizado: sin esquema, ruta, puerto ni punto final; en minúsculas.
pub fn normalize_host(host: &str) -> String {
    let h = host.trim();
    let h = h.split_once("://").map_or(h, |(_, rest)| rest);
    let h = h.split(['/', '?', '#']).next().unwrap_or_default();
    let h = h.rsplit_once('@').map_or(h, |(_, rest)| rest);
    let h = h.split(':').next().unwrap_or_default();
    h.trim_end_matches('.').chars().take(253).collect::<String>().to_ascii_lowercase()
}

/// Dominio de una dirección (`Ana <ana@Acme.com>` → `acme.com`).
pub fn addr_domain(addr: &str) -> Option<String> {
    let a = addr.trim().trim_start_matches('<').trim_end_matches('>');
    let (_, d) = a.rsplit_once('@')?;
    let d = d.trim().trim_end_matches(['>', '.']).to_ascii_lowercase();
    (!d.is_empty() && d.len() <= 253 && !d.contains(char::is_whitespace)).then_some(d)
}

fn local_part(addr: &str) -> String {
    let a = addr.trim().trim_start_matches('<');
    a.rsplit_once('@').map_or("", |(l, _)| l).chars().take(64).collect::<String>().to_ascii_lowercase()
}

/// `d` es `base` o un subdominio suyo.
fn within(d: &str, base: &str) -> bool {
    !base.is_empty() && (d == base || d.strip_suffix(base).is_some_and(|p| p.ends_with('.')))
}

/// El dominio es de la instancia (o del servicio).
fn instance_domain(d: &str, host: &str) -> bool {
    within(d, SAAS_DOMAIN) || within(d, host)
}

/// Quita la etiqueta inicial `[Iurefficient…]` del asunto; `None` si no la tiene.
pub fn strip_tag(subject: &str) -> Option<&str> {
    let s = subject.trim_start();
    let head: String = s.chars().take(14).collect::<String>().to_ascii_lowercase();
    if !head.starts_with("[iurefficient") {
        return None;
    }
    // La etiqueta cierra pronto; si no, no es una etiqueta.
    let close = s.char_indices().take(60).find(|(_, c)| *c == ']').map(|(i, _)| i)?;
    Some(s.get(close.saturating_add(1)..).unwrap_or_default().trim_start())
}

/// Asunto para mostrar: sin la etiqueta `[Iurefficient]` si la tiene.
pub fn display_subject(subject: &str) -> &str {
    match strip_tag(subject) {
        Some(rest) if !rest.trim().is_empty() => rest,
        _ => subject,
    }
}

/// Tipo de aviso por las palabras del asunto (o el valor de `X-Iurefficient-Type`).
pub fn kind_of(text: &str) -> InstanceKind {
    let lower: String = text.chars().take(300).collect::<String>().to_lowercase();
    let words: Vec<&str> = lower.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).take(60).collect();
    let any = |prefixes: &[&str]| words.iter().any(|w| prefixes.iter().any(|p| w.starts_with(p)));
    if any(&["ticket"]) {
        InstanceKind::Ticket
    } else if any(&["tarea", "task"]) {
        InstanceKind::Task
    } else if any(&["oportunidad", "opportunit", "venta", "deal"]) {
        InstanceKind::Opportunity
    } else if words.iter().any(|w| *w == "lead" || *w == "leads") || any(&["prospect"]) {
        InstanceKind::Lead
    } else if words.iter().any(|w| *w == "caso" || *w == "casos" || *w == "case" || *w == "cases")
        || any(&["proyecto", "project", "expediente"])
    {
        InstanceKind::Case
    } else {
        InstanceKind::Notice
    }
}

/// Dominio entre `<…@dominio>` de un `Message-ID`.
fn msgid_domain(mid: &str) -> Option<String> {
    let m = mid.trim().trim_start_matches('<').trim_end_matches('>');
    addr_domain(m)
}

/// Dominio de un `List-Id` (`Avisos <avisos.crm.acme.com>` → `avisos.crm.acme.com`).
fn list_id_domain(v: &str) -> Option<String> {
    let inner = match (v.rfind('<'), v.rfind('>')) {
        (Some(a), Some(b)) if b > a => v.get(a.saturating_add(1)..b).unwrap_or_default(),
        _ => v,
    };
    let d = inner.trim().trim_end_matches('.').to_ascii_lowercase();
    (!d.is_empty() && d.len() <= 253 && !d.contains(char::is_whitespace)).then_some(d)
}

/// ¿Lo envió la instancia? `host` es el anfitrión de la instancia configurada (si hay sesión);
/// `headers`, las cabeceras si el crudo ya está (si no, solo el sobre).
pub fn from_instance(env: &MessageEnvelope, headers: Option<&Headers>, host: Option<&str>) -> Option<InstanceHit> {
    let host = host.map(normalize_host).unwrap_or_default();
    let tagged = strip_tag(&env.subject);
    let what_src = tagged.filter(|r| !r.trim().is_empty()).unwrap_or(&env.subject);
    let hit = |kind: InstanceKind| InstanceHit { kind, what: what_src.trim().chars().take(MAX_WHAT).collect() };
    // 1. Cabeceras propias.
    if let Some(h) = headers {
        if let Some(t) = h.get("x-iurefficient-type").filter(|t| !t.trim().is_empty()) {
            return Some(hit(kind_of(t)));
        }
        if h.names().take(200).any(|n| n.starts_with("x-iurefficient")) {
            return Some(hit(kind_of(what_src)));
        }
    }
    // 2. Etiqueta del asunto.
    let mut found = tagged.is_some();
    // 3. Dominios del sobre y de las cabeceras.
    if !found {
        let mut domains: Vec<(String, String)> = Vec::new();
        for m in env.from.iter().chain(env.reply_to.iter()).take(8) {
            if let Some(d) = addr_domain(&m.addr) {
                domains.push((d, local_part(&m.addr)));
            }
        }
        if let Some(d) = env.message_id.as_deref().and_then(msgid_domain) {
            domains.push((d, String::new()));
        }
        if let Some(h) = headers {
            for name in ["return-path", "sender"] {
                if let Some(v) = h.get(name)
                    && let Some(d) = addr_domain(v)
                {
                    domains.push((d, local_part(v)));
                }
            }
            if let Some(d) = h.get("list-id").and_then(list_id_domain) {
                domains.push((d, String::new()));
            }
        }
        found = domains.iter().any(|(d, _)| instance_domain(d, &host));
        // 4. Dirección automática del dominio padre del anfitrión.
        if !found && host.contains('.') {
            found = domains.iter().any(|(d, local)| {
                d.contains('.') && within(&host, d) && AUTOMATED.iter().any(|a| local == a || local.starts_with(a))
            });
        }
    }
    found.then(|| hit(kind_of(what_src)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use iuredesk_model::Mailbox;
    use proptest::prelude::*;

    fn env(from: &str, subject: &str) -> MessageEnvelope {
        MessageEnvelope {
            from: vec![Mailbox { name: Some("X".into()), addr: from.into() }],
            subject: subject.into(),
            ..Default::default()
        }
    }

    #[test]
    fn hosts_are_normalized() {
        assert_eq!(normalize_host("https://2.DS.iurefficient.com/"), "2.ds.iurefficient.com");
        assert_eq!(normalize_host("crm.acme.com:8443/app"), "crm.acme.com");
        assert_eq!(normalize_host(" crm.acme.com. "), "crm.acme.com");
    }

    #[test]
    fn subject_tag_is_detected_and_stripped() {
        assert_eq!(strip_tag("[Iurefficient] Te mencionaron en ACME"), Some("Te mencionaron en ACME"));
        assert_eq!(strip_tag("  [IUREFFICIENT · Ventas] Nueva oportunidad"), Some("Nueva oportunidad"));
        assert_eq!(strip_tag("Re: [Iurefficient] x"), None);
        assert_eq!(strip_tag("[Iurefficient sin cierre"), None);
        assert_eq!(display_subject("[Iurefficient]"), "[Iurefficient]");
        let e = env("juan@gmail.com", "[Iurefficient] Ticket #42 asignado");
        let h = from_instance(&e, None, None).unwrap();
        assert_eq!(h.kind, InstanceKind::Ticket);
        assert_eq!(h.what, "Ticket #42 asignado");
    }

    #[test]
    fn instance_domain_and_saas_count() {
        let host = Some("crm.acme.com");
        assert!(from_instance(&env("avisos@crm.acme.com", "Hola"), None, host).is_some());
        assert!(from_instance(&env("x@mail.crm.acme.com", "Hola"), None, host).is_some());
        assert!(from_instance(&env("noreply@iurefficient.com", "Hola"), None, None).is_some());
        assert!(from_instance(&env("noreply@2.ds.iurefficient.com", "Hola"), None, None).is_some());
        // Un dominio que solo termina igual no cuenta.
        assert!(from_instance(&env("x@evilcrm.acme.com", "Hola"), None, host).is_none());
        assert!(from_instance(&env("x@notiurefficient.com", "Hola"), None, None).is_none());
    }

    #[test]
    fn parent_domain_only_for_automated_senders() {
        let host = Some("crm.acme.com");
        assert!(from_instance(&env("noreply@acme.com", "Tarea asignada: revisar"), None, host).is_some());
        assert!(from_instance(&env("notificaciones@acme.com", "x"), None, host).is_some());
        // Un colega del mismo dominio no es la instancia.
        assert!(from_instance(&env("juan@acme.com", "Comida"), None, host).is_none());
        // Ni el dominio de primer nivel.
        assert!(from_instance(&env("noreply@com", "x"), None, Some("acme.com")).is_none());
    }

    #[test]
    fn message_id_and_headers_count() {
        let mut e = env("juan@gmail.com", "Aviso");
        e.message_id = Some("<abc.123@crm.acme.com>".into());
        assert!(from_instance(&e, None, Some("crm.acme.com")).is_some());
        let e = env("bounce@sendgrid.net", "Te mencionaron");
        let h = Headers::from_pairs([("Return-Path", "<bounces@crm.acme.com>")]);
        assert!(from_instance(&e, Some(&h), Some("crm.acme.com")).is_some());
        let h = Headers::from_pairs([("List-Id", "Avisos <avisos.crm.acme.com>")]);
        assert!(from_instance(&e, Some(&h), Some("crm.acme.com")).is_some());
        let h = Headers::from_pairs([("X-Iurefficient-Type", "opportunity")]);
        assert_eq!(from_instance(&e, Some(&h), None).map(|h| h.kind), Some(InstanceKind::Opportunity));
        let h = Headers::from_pairs([("X-Iurefficient-Event", "mention")]);
        assert_eq!(from_instance(&e, Some(&h), None).map(|h| h.kind), Some(InstanceKind::Notice));
        assert!(from_instance(&e, Some(&Headers::default()), Some("crm.acme.com")).is_none());
    }

    #[test]
    fn kinds_by_subject_words() {
        assert_eq!(kind_of("Nueva tarea asignada"), InstanceKind::Task);
        assert_eq!(kind_of("Oportunidad ACME ganada"), InstanceKind::Opportunity);
        assert_eq!(kind_of("New lead: Bob"), InstanceKind::Lead);
        assert_eq!(kind_of("Team leader update"), InstanceKind::Notice);
        assert_eq!(kind_of("Caso 2024-118 actualizado"), InstanceKind::Case);
        assert_eq!(kind_of("Te mencionaron"), InstanceKind::Notice);
    }

    proptest! {
        #[test]
        fn never_panics(from in ".{0,80}", subject in ".{0,200}", host in ".{0,80}", mid in ".{0,80}") {
            let mut e = env(&from, &subject);
            e.message_id = Some(mid.clone());
            let h = Headers::from_pairs([("Return-Path", mid.as_str()), ("List-Id", from.as_str())]);
            let _ = from_instance(&e, Some(&h), Some(&host));
            let _ = display_subject(&subject);
            let _ = normalize_host(&host);
        }
    }
}
