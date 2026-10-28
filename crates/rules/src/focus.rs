//! «Enfocados / Otros»: separa lo que escribe una persona de boletines, avisos y correo masivo,
//! con heurísticas locales (sin IA ni servidores). Lo que diga el usuario («Mover siempre a…»)
//! manda sobre todo lo demás.

use std::collections::{BTreeMap, HashSet};

use iuredesk_model::MessageEnvelope;
use serde::{Deserialize, Serialize};

use crate::Bi;
use crate::facts::Headers;

/// Pestaña de la bandeja de entrada.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Focus {
    /// Enfocados: personas y lo importante.
    #[default]
    Focused,
    /// Otros: boletines, avisos automáticos, promociones.
    Other,
}

impl Focus {
    /// Nombre de la pestaña.
    pub fn label(self) -> Bi {
        match self {
            Focus::Focused => Bi::new("Focused", "Enfocados"),
            Focus::Other => Bi::new("Other", "Otros"),
        }
    }
}

/// Por qué se clasificó así (se explica en la interfaz).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusReason {
    /// «Mover siempre a…» del usuario (dirección).
    Override,
    /// «Mover siempre a…» del usuario (dominio).
    DomainOverride,
    /// Está en tus contactos.
    Contact,
    /// Le has escrito.
    RepliedTo,
    /// Es de tu organización.
    SameDomain,
    /// Boletín o lista (`List-Unsubscribe`, `List-Id`).
    Newsletter,
    /// Correo masivo (`Precedence: bulk`, cabeceras de plataformas de envío).
    Bulk,
    /// Aviso automático (`Auto-Submitted`, `noreply@`…).
    Automated,
    /// Nada en contra: una persona.
    Default,
}

impl FocusReason {
    /// Explicación para cualquier usuario.
    pub fn explain(self) -> Bi {
        match self {
            FocusReason::Override => Bi::new("You chose this for this sender.", "Lo elegiste para este remitente."),
            FocusReason::DomainOverride => Bi::new("You chose this for this domain.", "Lo elegiste para este dominio."),
            FocusReason::Contact => Bi::new("The sender is one of your contacts.", "El remitente es un contacto tuyo."),
            FocusReason::RepliedTo => Bi::new("You've written to this sender.", "Le has escrito a este remitente."),
            FocusReason::SameDomain => Bi::new("It's from your organization.", "Es de tu organización."),
            FocusReason::Newsletter => Bi::new("It's a newsletter or mailing list.", "Es un boletín o una lista."),
            FocusReason::Bulk => Bi::new("It was sent in bulk.", "Se envió de forma masiva."),
            FocusReason::Automated => Bi::new("It's an automatic notice.", "Es un aviso automático."),
            FocusReason::Default => Bi::new("It looks like a person wrote it.", "Parece escrito por una persona."),
        }
    }
}

/// Lo que se sabe del usuario para clasificar.
#[derive(Clone, Debug, Default)]
pub struct Signals {
    /// Direcciones de mis contactos (minúsculas).
    pub contacts: HashSet<String>,
    /// Direcciones a las que he escrito (minúsculas).
    pub replied: HashSet<String>,
    /// Dominios propios (de mis cuentas), salvo los de correo gratuito.
    pub my_domains: HashSet<String>,
    /// Mis direcciones.
    pub mine: HashSet<String>,
}

const FREEMAIL: [&str; 14] = [
    "gmail.com",
    "googlemail.com",
    "outlook.com",
    "hotmail.com",
    "live.com",
    "msn.com",
    "yahoo.com",
    "icloud.com",
    "me.com",
    "aol.com",
    "proton.me",
    "protonmail.com",
    "gmx.com",
    "yandex.com",
];

impl Signals {
    /// Agrega mis direcciones (y sus dominios, si no son de correo gratuito).
    pub fn add_mine(&mut self, addr: &str) {
        let a = crate::norm_addr(addr);
        let d = crate::domain_of(&a).to_string();
        if !d.is_empty() && !FREEMAIL.contains(&d.as_str()) {
            self.my_domains.insert(d);
        }
        self.mine.insert(a);
    }
}

/// Partes locales típicas de envíos automáticos o masivos.
const AUTOMATED_LOCAL: [&str; 21] = [
    "noreply",
    "no-reply",
    "no_reply",
    "donotreply",
    "do-not-reply",
    "notifications",
    "notification",
    "notify",
    "alerts",
    "alert",
    "newsletter",
    "newsletters",
    "news",
    "marketing",
    "promo",
    "promociones",
    "ofertas",
    "mailer",
    "mailer-daemon",
    "bounce",
    "updates",
];

/// Cabeceras de plataformas de envío masivo.
const BULK_HEADERS: [&str; 8] = [
    "x-campaign",
    "x-campaignid",
    "x-mailchimp-campaign",
    "x-mc-user",
    "x-sg-eid",
    "x-mailgun-tag",
    "feedback-id",
    "x-ses-outgoing",
];

/// Clasifica un mensaje. `headers` puede estar vacío (crudo aún no descargado).
pub fn classify(
    env: &MessageEnvelope,
    headers: &Headers,
    signals: &Signals,
    overrides: &BTreeMap<String, Focus>,
) -> (Focus, FocusReason) {
    let sender = env.from.first().map(|m| crate::norm_addr(&m.addr)).unwrap_or_default();
    let domain = crate::domain_of(&sender).to_string();
    if let Some(f) = overrides.get(&sender) {
        return (*f, FocusReason::Override);
    }
    // Dominio y dominios padre (`news.tienda.example` → `tienda.example`).
    let mut d = domain.as_str();
    for _ in 0..8 {
        if d.is_empty() {
            break;
        }
        if let Some(f) = overrides.get(d) {
            return (*f, FocusReason::DomainOverride);
        }
        d = d.split_once('.').map_or("", |(_, rest)| rest);
        if !d.contains('.') {
            break;
        }
    }
    if signals.contacts.contains(&sender) {
        return (Focus::Focused, FocusReason::Contact);
    }
    if signals.replied.contains(&sender) {
        return (Focus::Focused, FocusReason::RepliedTo);
    }
    if signals.mine.contains(&sender) {
        return (Focus::Focused, FocusReason::Default);
    }
    if headers.has("list-unsubscribe") || headers.has("list-id") || headers.has("list-post") {
        return (Focus::Other, FocusReason::Newsletter);
    }
    let prec = headers.get("precedence").map(|v| v.trim().to_lowercase()).unwrap_or_default();
    if prec == "bulk" || prec == "list" || prec == "junk" || BULK_HEADERS.iter().any(|h| headers.has(h)) {
        return (Focus::Other, FocusReason::Bulk);
    }
    if headers.get("auto-submitted").is_some_and(|v| !v.trim().eq_ignore_ascii_case("no")) {
        return (Focus::Other, FocusReason::Automated);
    }
    let local = sender.split('@').next().unwrap_or("");
    let automated = AUTOMATED_LOCAL.iter().any(|w| {
        local == *w
            || local.starts_with(&format!("{w}-"))
            || local.starts_with(&format!("{w}+"))
            || local.starts_with(&format!("{w}."))
            || local.ends_with(&format!("-{w}"))
            || local.ends_with(&format!(".{w}"))
    });
    if automated {
        return (Focus::Other, FocusReason::Automated);
    }
    if !domain.is_empty() && signals.my_domains.contains(&domain) {
        return (Focus::Focused, FocusReason::SameDomain);
    }
    (Focus::Focused, FocusReason::Default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use iuredesk_model::Mailbox;

    fn env(from: &str) -> MessageEnvelope {
        MessageEnvelope { from: vec![Mailbox::addr(from)], ..Default::default() }
    }

    #[test]
    fn heuristics() {
        let mut s = Signals::default();
        s.contacts.insert("carmen@cliente.example".into());
        s.replied.insert("juez@tribunal.example".into());
        s.add_mine("laura@despacho.example");
        s.add_mine("laura@gmail.com");
        let none = Headers::default();
        let ov = BTreeMap::new();
        let c = |from: &str, h: &Headers| classify(&env(from), h, &s, &ov);

        assert_eq!(c("Carmen@Cliente.example", &none), (Focus::Focused, FocusReason::Contact));
        assert_eq!(c("juez@tribunal.example", &none), (Focus::Focused, FocusReason::RepliedTo));
        assert_eq!(c("pablo@despacho.example", &none), (Focus::Focused, FocusReason::SameDomain));
        assert_eq!(c("alguien@gmail.com", &none), (Focus::Focused, FocusReason::Default), "gmail no es mi dominio");
        assert_eq!(c("desconocido@empresa.example", &none), (Focus::Focused, FocusReason::Default));
        let news = Headers::from_pairs([("List-Unsubscribe", "<https://x.example/u>")]);
        assert_eq!(c("boletin@tienda.example", &news), (Focus::Other, FocusReason::Newsletter));
        let bulk = Headers::from_pairs([("Precedence", "bulk")]);
        assert_eq!(c("ventas@tienda.example", &bulk), (Focus::Other, FocusReason::Bulk));
        let esp = Headers::from_pairs([("Feedback-ID", "1:2:3")]);
        assert_eq!(c("hola@tienda.example", &esp).0, Focus::Other);
        let auto = Headers::from_pairs([("Auto-Submitted", "auto-generated")]);
        assert_eq!(c("sistema@banco.example", &auto), (Focus::Other, FocusReason::Automated));
        assert_eq!(c("no-reply@banco.example", &none), (Focus::Other, FocusReason::Automated));
        assert_eq!(c("notifications@github.example", &none), (Focus::Other, FocusReason::Automated));
        assert_eq!(c("noticias@x.example", &none).0, Focus::Focused, "«noticias» no es una palabra de la lista");
        // Un contacto con boletín sigue en Enfocados.
        assert_eq!(c("carmen@cliente.example", &news).0, Focus::Focused);
    }

    #[test]
    fn overrides_win() {
        let s = Signals::default();
        let mut ov = BTreeMap::new();
        ov.insert("boletin@tienda.example".to_string(), Focus::Focused);
        ov.insert("banco.example".to_string(), Focus::Focused);
        ov.insert("amigo@persona.example".to_string(), Focus::Other);
        let news = Headers::from_pairs([("List-Id", "<x>")]);
        assert_eq!(classify(&env("boletin@tienda.example"), &news, &s, &ov), (Focus::Focused, FocusReason::Override));
        assert_eq!(
            classify(&env("no-reply@avisos.banco.example"), &Headers::default(), &s, &ov),
            (Focus::Focused, FocusReason::DomainOverride)
        );
        assert_eq!(classify(&env("amigo@persona.example"), &Headers::default(), &s, &ov).0, Focus::Other);
    }
}
