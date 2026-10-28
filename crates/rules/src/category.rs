//! Categorías de la bandeja de entrada, como en Gmail: Principal, Promociones, Social,
//! Notificaciones y Foros (Spam y Papelera siguen siendo carpetas).
//!
//! Fuentes, por orden: lo que el usuario eligió («Mover siempre a…») → la pista del proveedor
//! (Gmail `category:…`, Microsoft 365 Enfocados/Otros) → heurísticas locales sin IA.

use std::collections::BTreeMap;

use iuredesk_model::MessageEnvelope;
use serde::{Deserialize, Serialize};

use crate::Bi;
use crate::facts::Headers;
use crate::focus::Signals;

/// Categoría.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    /// Personas y lo importante.
    #[default]
    Primary,
    /// Ofertas, boletines comerciales.
    Promotions,
    /// Redes sociales.
    Social,
    /// Recibos, avisos, alertas, confirmaciones.
    Updates,
    /// Listas de correo y foros.
    Forums,
}

impl Category {
    /// En el orden de las pestañas.
    pub const ALL: [Category; 5] =
        [Category::Primary, Category::Promotions, Category::Social, Category::Updates, Category::Forums];

    /// Clave estable (`mail.category:<clave>`).
    pub fn key(self) -> &'static str {
        match self {
            Category::Primary => "primary",
            Category::Promotions => "promotions",
            Category::Social => "social",
            Category::Updates => "updates",
            Category::Forums => "forums",
        }
    }

    /// Inversa de [`Category::key`].
    pub fn from_key(s: &str) -> Option<Category> {
        Category::ALL.into_iter().find(|c| c.key() == s)
    }

    /// Nombre de la pestaña.
    pub fn label(self) -> Bi {
        match self {
            Category::Primary => Bi::new("Primary", "Principal"),
            Category::Promotions => Bi::new("Promotions", "Promociones"),
            Category::Social => Bi::new("Social", "Social"),
            Category::Updates => Bi::new("Updates", "Notificaciones"),
            Category::Forums => Bi::new("Forums", "Foros"),
        }
    }

    /// Color de la categoría (0xRRGGBB) para chips y pestañas.
    pub fn color(self) -> u32 {
        match self {
            Category::Primary => 0x2F6FED,
            Category::Promotions => 0x1E8E3E,
            Category::Social => 0x1A73E8,
            Category::Updates => 0xE37400,
            Category::Forums => 0x8E44AD,
        }
    }
}

/// Por qué (se explica al pasar el ratón).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CategoryReason {
    /// «Mover siempre a…».
    Override,
    /// Lo dijo el proveedor (Gmail, Microsoft 365).
    Provider,
    /// Contacto, persona a la que escribiste o de tu organización.
    Person,
    /// Red social conocida.
    SocialNetwork,
    /// Lista de correo o foro.
    MailingList,
    /// Aviso automático, recibo o alerta.
    Automated,
    /// Envío masivo o boletín comercial.
    Bulk,
    /// Nada en contra.
    Default,
}

const SOCIAL_DOMAINS: [&str; 22] = [
    "facebookmail.com",
    "facebook.com",
    "linkedin.com",
    "twitter.com",
    "x.com",
    "instagram.com",
    "tiktok.com",
    "pinterest.com",
    "youtube.com",
    "snapchat.com",
    "tumblr.com",
    "quora.com",
    "meetup.com",
    "nextdoor.com",
    "whatsapp.com",
    "telegram.org",
    "threads.net",
    "bsky.app",
    "mastodon.social",
    "strava.com",
    "goodreads.com",
    "flickr.com",
];

const FORUM_DOMAINS: [&str; 7] = [
    "googlegroups.com",
    "groups.io",
    "discoursemail.com",
    "freelists.org",
    "lists.sourceforge.net",
    "yahoogroups.com",
    "mailman",
];

const AUTOMATED_LOCAL: [&str; 20] = [
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
    "mailer-daemon",
    "postmaster",
    "bounce",
    "updates",
    "security",
    "billing",
    "receipts",
    "invoice",
    "orders",
    "account",
];

const PROMO_LOCAL: [&str; 9] =
    ["newsletter", "newsletters", "news", "marketing", "promo", "promociones", "ofertas", "deals", "offers"];

/// Palabras de asunto de avisos transaccionales (en minúsculas).
const UPDATE_WORDS: [&str; 26] = [
    "recibo",
    "receipt",
    "factura",
    "invoice",
    "pedido",
    "order",
    "envío",
    "shipped",
    "entrega",
    "delivery",
    "confirm",
    "verific",
    "código",
    "code",
    "contraseña",
    "password",
    "alerta",
    "alert",
    "security",
    "seguridad",
    "pago",
    "payment",
    "statement",
    "estado de cuenta",
    "reservación",
    "reservation",
];

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

fn local_matches(local: &str, words: &[&str]) -> bool {
    words.iter().any(|w| {
        local == *w
            || local.starts_with(&format!("{w}-"))
            || local.starts_with(&format!("{w}+"))
            || local.starts_with(&format!("{w}."))
            || local.ends_with(&format!("-{w}"))
            || local.ends_with(&format!(".{w}"))
    })
}

fn in_domains(domain: &str, list: &[&str]) -> bool {
    list.iter()
        .any(|d| domain == *d || domain.ends_with(&format!(".{d}")) || (*d == "mailman" && domain.contains("lists.")))
}

/// Override por dirección o dominio (y dominios padre).
pub fn override_for<T: Copy>(sender: &str, overrides: &BTreeMap<String, T>) -> Option<T> {
    if let Some(c) = overrides.get(sender) {
        return Some(*c);
    }
    let mut d = crate::domain_of(sender);
    for _ in 0..8 {
        if d.is_empty() {
            break;
        }
        if let Some(c) = overrides.get(d) {
            return Some(*c);
        }
        d = d.split_once('.').map_or("", |(_, rest)| rest);
        if !d.contains('.') {
            break;
        }
    }
    None
}

/// Pista del proveedor → categoría (`None` = decidir en local).
fn from_hint(hint: &str, local: Category) -> Option<Category> {
    match hint {
        "promotions" => Some(Category::Promotions),
        "social" => Some(Category::Social),
        "updates" => Some(Category::Updates),
        "forums" => Some(Category::Forums),
        "primary" => Some(Category::Primary),
        // Microsoft 365: «Otros» nunca es Principal; «Enfocados» confirma lo local.
        "other" => Some(if local == Category::Primary { Category::Updates } else { local }),
        _ => None,
    }
}

/// Clasifica un mensaje. `hint` es la pista del proveedor, si la hay; `headers` puede estar
/// vacío (crudo aún no descargado).
pub fn classify(
    env: &MessageEnvelope,
    headers: &Headers,
    signals: &Signals,
    overrides: &BTreeMap<String, Category>,
    hint: Option<&str>,
) -> (Category, CategoryReason) {
    let sender = env.from.first().map(|m| crate::norm_addr(&m.addr)).unwrap_or_default();
    if let Some(c) = override_for(&sender, overrides) {
        return (c, CategoryReason::Override);
    }
    let local = local_category(env, headers, signals, &sender);
    if let Some(h) = hint
        && let Some(c) = from_hint(h, local.0)
    {
        // Gmail sabe más que nosotros; pero una persona conocida siempre es Principal.
        if local.1 == CategoryReason::Person {
            return local;
        }
        return (c, CategoryReason::Provider);
    }
    local
}

fn local_category(
    env: &MessageEnvelope,
    headers: &Headers,
    signals: &Signals,
    sender: &str,
) -> (Category, CategoryReason) {
    let domain = crate::domain_of(sender).to_string();
    if signals.contacts.contains(sender) || signals.replied.contains(sender) || signals.mine.contains(sender) {
        return (Category::Primary, CategoryReason::Person);
    }
    let local = sender.split('@').next().unwrap_or("");
    if in_domains(&domain, &SOCIAL_DOMAINS)
        || headers.has("x-facebook-notify")
        || headers.has("x-linkedin-class")
        || headers.has("x-twitterrecipientid")
    {
        return (Category::Social, CategoryReason::SocialNetwork);
    }
    let list_post = headers.has("list-post") || headers.has("x-mailman-version") || headers.has("x-discourse-post-id");
    if (headers.has("list-id") && list_post) || in_domains(&domain, &FORUM_DOMAINS) {
        return (Category::Forums, CategoryReason::MailingList);
    }
    let subject = env.subject.to_lowercase();
    let transactional = UPDATE_WORDS.iter().any(|w| subject.contains(w));
    let auto = headers.get("auto-submitted").is_some_and(|v| !v.trim().eq_ignore_ascii_case("no"));
    let prec = headers.get("precedence").map(|v| v.trim().to_lowercase()).unwrap_or_default();
    let bulk = headers.has("list-unsubscribe")
        || prec == "bulk"
        || prec == "junk"
        || BULK_HEADERS.iter().any(|h| headers.has(h))
        || local_matches(local, &PROMO_LOCAL);
    let automated = auto || local_matches(local, &AUTOMATED_LOCAL);
    if (automated || bulk) && transactional {
        return (Category::Updates, CategoryReason::Automated);
    }
    if bulk {
        return (Category::Promotions, CategoryReason::Bulk);
    }
    if automated {
        return (Category::Updates, CategoryReason::Automated);
    }
    if headers.has("list-id") {
        return (Category::Forums, CategoryReason::MailingList);
    }
    if !domain.is_empty() && signals.my_domains.contains(&domain) {
        return (Category::Primary, CategoryReason::Person);
    }
    (Category::Primary, CategoryReason::Default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use iuredesk_model::Mailbox;

    fn env(from: &str, subject: &str) -> MessageEnvelope {
        MessageEnvelope { from: vec![Mailbox::addr(from)], subject: subject.into(), ..Default::default() }
    }

    #[test]
    fn heuristics_matrix() {
        let mut s = Signals::default();
        s.contacts.insert("carmen@cliente.example".into());
        s.add_mine("laura@despacho.example");
        let none = Headers::default();
        let ov = BTreeMap::new();
        let c = |from: &str, subj: &str, h: &Headers| classify(&env(from, subj), h, &s, &ov, None).0;
        let unsub = Headers::from_pairs([("List-Unsubscribe", "<https://t.example/u>")]);
        let list = Headers::from_pairs([("List-Id", "<rust.groups.io>"), ("List-Post", "<mailto:x@groups.io>")]);
        let bare_list = Headers::from_pairs([("List-Id", "<equipo.example>")]);
        let auto = Headers::from_pairs([("Auto-Submitted", "auto-generated")]);
        let cases = [
            (c("carmen@cliente.example", "Hola", &unsub), Category::Primary),
            (c("pablo@despacho.example", "Reunión", &none), Category::Primary),
            (c("alguien@empresa.example", "Propuesta", &none), Category::Primary),
            (c("ofertas@tienda.example", "50% de descuento", &unsub), Category::Promotions),
            (c("newsletter@medio.example", "Lo de hoy", &none), Category::Promotions),
            (c("notification@facebookmail.com", "Ana comentó", &none), Category::Social),
            (c("messages-noreply@linkedin.com", "Tienes un mensaje", &unsub), Category::Social),
            (c("lista@rust.groups.io", "[rust] tema", &list), Category::Forums),
            (c("equipo@lista.example", "Minuta", &bare_list), Category::Forums),
            (c("no-reply@banco.example", "Tu estado de cuenta", &none), Category::Updates),
            (c("sistema@banco.example", "Aviso", &auto), Category::Updates),
            (c("ventas@tienda.example", "Tu pedido fue enviado", &unsub), Category::Updates),
            (c("security@cuentas.example", "Nuevo inicio de sesión", &none), Category::Updates),
        ];
        for (i, (got, want)) in cases.iter().enumerate() {
            assert_eq!(got, want, "caso {i}");
        }
    }

    #[test]
    fn provider_hints_and_overrides() {
        let s = Signals::default();
        let none = Headers::default();
        let mut ov = BTreeMap::new();
        let e = env("hola@tienda.example", "Novedades");
        assert_eq!(classify(&e, &none, &s, &ov, Some("promotions")), (Category::Promotions, CategoryReason::Provider));
        assert_eq!(classify(&e, &none, &s, &ov, Some("other")).0, Category::Updates, "Otros de M365 no es Principal");
        assert_eq!(classify(&e, &none, &s, &ov, Some("focused")).0, Category::Primary);
        assert_eq!(classify(&e, &none, &s, &ov, Some("rara")).0, Category::Primary);
        ov.insert("tienda.example".to_string(), Category::Social);
        assert_eq!(classify(&e, &none, &s, &ov, Some("promotions")), (Category::Social, CategoryReason::Override));
        let mut known = Signals::default();
        known.contacts.insert("hola@tienda.example".into());
        assert_eq!(classify(&e, &none, &known, &BTreeMap::new(), Some("promotions")).0, Category::Primary);
        for c in Category::ALL {
            assert_eq!(Category::from_key(c.key()), Some(c));
        }
    }
}
