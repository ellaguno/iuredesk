//! Autodiscover de Exchange Server local (F8.3), la parte que no necesita contraseña.
//!
//! Para cada servidor del dominio (`autodiscover.<dominio>`, `<dominio>` y el destino de
//! `_autodiscover._tcp.<dominio>` si está dentro del dominio):
//! - **Autodiscover v2** (`/autodiscover/autodiscover.json/v1.0/<correo>?Protocol=EWS`, Exchange
//!   2016 en adelante, sin autenticación): da la URL de EWS directamente. Si apunta a Microsoft
//!   365, el buzón vive en la nube (híbrido) y se usa Microsoft 365.
//! - **POX** (`/autodiscover/autodiscover.xml`): sin contraseña responde 401 con NTLM/Negotiate
//!   cuando hay un Exchange detrás (IIS). Eso basta para saber que es Exchange; la URL de EWS la
//!   pide después, ya con la contraseña, el crate `ews`.
//!
//! Solo `https`; las redirecciones las sigue [`crate::Discoverer`] (solo a `https`). El servidor
//! del propio dominio recibe la dirección completa (como el autoconfig del dominio).

use serde::{Deserialize, Serialize};

use crate::email::url_encode;
use crate::xml::{host_matches, valid_host};

/// Lo que se supo de Exchange sin contraseña.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExchangeHint {
    /// URL de EWS dada por Autodiscover v2, si la hubo.
    pub ews_url: Option<String>,
    /// URL de `autodiscover.xml` que respondieron como Exchange (para preguntar con contraseña),
    /// en orden de preferencia.
    pub autodiscover: Vec<String>,
}

impl ExchangeHint {
    pub fn is_empty(&self) -> bool {
        self.ews_url.is_none() && self.autodiscover.is_empty()
    }

    /// Junta otra pista (sin repetir, acotado).
    pub fn merge(&mut self, other: ExchangeHint) {
        if self.ews_url.is_none() {
            self.ews_url = other.ews_url;
        }
        for u in other.autodiscover {
            if self.autodiscover.len() < 6 && !self.autodiscover.contains(&u) {
                self.autodiscover.push(u);
            }
        }
    }
}

/// `https://host[:puerto]`.
pub fn origin(host: &str, port: u16) -> String {
    if port == 443 || port == 0 { format!("https://{host}") } else { format!("https://{host}:{port}") }
}

/// URL de Autodiscover v2 para EWS.
pub fn v2_url(origin: &str, email: &str) -> String {
    format!("{origin}/autodiscover/autodiscover.json/v1.0/{}?Protocol=EWS", url_encode(email))
}

/// URL de Autodiscover POX.
pub fn pox_url(origin: &str) -> String {
    format!("{origin}/autodiscover/autodiscover.xml")
}

/// ¿`host` es `domain` o un subdominio suyo?
pub fn within(host: &str, domain: &str) -> bool {
    let (h, d) = (host.trim_end_matches('.').to_ascii_lowercase(), domain.trim_end_matches('.').to_ascii_lowercase());
    !d.is_empty() && (h == d || h.ends_with(&format!(".{d}")))
}

/// ¿El servidor es de Microsoft 365 (buzón en la nube)?
pub fn is_microsoft_cloud(host: &str) -> bool {
    ["office365.com", "outlook.com", "office.com", "microsoft.com"].iter().any(|d| host_matches(host, d))
}

/// Lee la respuesta de Autodiscover v2: `{"Protocol":"EWS","Url":"https://…/EWS/Exchange.asmx"}`.
/// Solo URL `https` con un host válido.
pub fn parse_v2(body: &str) -> Option<String> {
    if body.len() > 64 * 1024 {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let protocol = v.get("Protocol").and_then(|p| p.as_str()).unwrap_or("EWS");
    if !protocol.eq_ignore_ascii_case("ews") {
        return None;
    }
    let url = v.get("Url")?.as_str()?.trim();
    let host = crate::url_host(url)?;
    (url.len() <= 2048 && valid_host(&host) && !url.chars().any(char::is_whitespace)).then(|| url.to_string())
}

/// ¿Un 401 de `autodiscover.xml` es de un Exchange? (IIS ofrece NTLM o Negotiate; un hospedaje
/// con «Autodiscover» de imitación para Outlook no.)
pub fn looks_like_exchange(status: u16, auth_schemes: &[String]) -> bool {
    status == 401 && auth_schemes.iter().any(|s| s == "ntlm" || s == "negotiate")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v2_responses() {
        assert_eq!(
            parse_v2(r#"{"Protocol":"EWS","Url":"https://mail.contoso.com/EWS/Exchange.asmx"}"#).as_deref(),
            Some("https://mail.contoso.com/EWS/Exchange.asmx")
        );
        assert_eq!(parse_v2(r#"{"Protocol":"EWS","Url":"http://mail.contoso.com/EWS/Exchange.asmx"}"#), None);
        assert_eq!(parse_v2(r#"{"Protocol":"ActiveSync","Url":"https://m/x"}"#), None);
        assert_eq!(parse_v2(r#"{"ErrorCode":"InternalServerError"}"#), None);
        assert_eq!(parse_v2("<html>"), None);
    }

    #[test]
    fn urls_and_domains() {
        assert_eq!(
            v2_url("https://autodiscover.contoso.com", "ana@contoso.com"),
            "https://autodiscover.contoso.com/autodiscover/autodiscover.json/v1.0/ana%40contoso.com?Protocol=EWS"
        );
        assert_eq!(
            pox_url(&origin("ad.contoso.com", 8443)),
            "https://ad.contoso.com:8443/autodiscover/autodiscover.xml"
        );
        assert!(within("ad.corp.contoso.com", "contoso.com"));
        assert!(!within("contoso.com.evil.net", "contoso.com"));
        assert!(is_microsoft_cloud("outlook.office365.com"));
        assert!(!is_microsoft_cloud("mail.contoso.com"));
        assert!(looks_like_exchange(401, &["negotiate".into(), "ntlm".into()]));
        assert!(!looks_like_exchange(401, &["basic".into()]));
        assert!(!looks_like_exchange(200, &["ntlm".into()]));
    }
}
