//! Autodiscover POX de Exchange (F8.3): con usuario y contraseña, pregunta al servidor de
//! Autodiscover del dominio la URL de EWS del buzón.
//!
//! Exchange local exige autenticación en `autodiscover.xml` (Basic o NTLM, la misma de
//! [`EwsClient`]). La respuesta (esquema `outlook/responseschema/2006a`) trae una de tres cosas:
//! - `settings`: los protocolos del buzón; de `EXPR` (Outlook Anywhere, desde fuera) y `EXCH`
//!   (desde dentro) salen las URL de EWS, primero las de fuera;
//! - `redirectAddr`: el buzón tiene otra dirección principal; se vuelve a preguntar con ella;
//! - `redirectUrl`: preguntar en otro servidor (solo `https` y dentro del dominio del correo o
//!   del servidor que lo pidió; a otro sitio no se le manda la contraseña).
//!
//! Todo acotado: saltos, tamaño de la respuesta y URL que se devuelven.

use std::sync::Arc;
use std::time::Duration;

use crate::soap::{clip, esc};
use crate::{Element, EwsAuth, EwsClient, EwsError, Result};

/// Esquema de la petición.
const REQUEST_SCHEMA: &str = "http://schemas.microsoft.com/exchange/autodiscover/outlook/requestschema/2006";
/// Esquema de respuesta que se acepta.
const RESPONSE_SCHEMA: &str = "http://schemas.microsoft.com/exchange/autodiscover/outlook/responseschema/2006a";
/// Saltos (`redirectAddr`/`redirectUrl`) antes de rendirse; la especificación pide 10 como mucho.
const MAX_HOPS: usize = 5;
/// Tamaño máximo de una respuesta de Autodiscover (una real ocupa unos pocos KiB).
const MAX_BODY: usize = 1024 * 1024;
/// URL de EWS que se devuelven como mucho.
const MAX_URLS: usize = 6;

/// Lo que respondió un servidor de Autodiscover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PoxResponse {
    /// URL de EWS del buzón, de la mejor a la peor (las de fuera primero).
    Settings(Vec<String>),
    /// Volver a preguntar con esta dirección.
    RedirectAddr(String),
    /// Volver a preguntar en esta URL.
    RedirectUrl(String),
    /// Error del servidor (`ErrorCode` y `Message`).
    Error { code: String, message: String },
}

/// Cuerpo de la petición POX para `email`.
pub fn request(email: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?><Autodiscover xmlns="{REQUEST_SCHEMA}"><Request><EMailAddress>{}</EMailAddress><AcceptableResponseSchema>{RESPONSE_SCHEMA}</AcceptableResponseSchema></Request></Autodiscover>"#,
        esc(email)
    )
}

/// Busca el primer descendiente llamado `name` (sin mirar el espacio de nombres: Exchange mezcla
/// los de `2006` y `2006a`).
fn find<'a>(e: &'a Element, name: &str, depth: usize) -> Option<&'a Element> {
    if e.name == name {
        return Some(e);
    }
    if depth == 0 {
        return None;
    }
    e.children.iter().find_map(|c| find(c, name, depth - 1))
}

fn child_text(e: &Element, name: &str) -> Option<String> {
    e.children.iter().find(|c| c.name == name).map(|c| clip(c.text_trim(), 2048)).filter(|t| !t.is_empty())
}

/// Lee una respuesta POX.
pub fn parse(bytes: &[u8]) -> Result<PoxResponse> {
    let root = iuredesk_dav::dav::xml::parse(bytes)?;
    if root.name != "Autodiscover" {
        return Err(EwsError::Invalid(format!("se esperaba Autodiscover y llegó {}", clip(&root.name, 60))));
    }
    if let Some(err) = find(&root, "Error", 3) {
        return Ok(PoxResponse::Error {
            code: child_text(err, "ErrorCode").unwrap_or_default(),
            message: child_text(err, "Message").unwrap_or_default(),
        });
    }
    let account = find(&root, "Account", 3).ok_or_else(|| EwsError::Invalid("respuesta sin Account".into()))?;
    let action = child_text(account, "Action").unwrap_or_else(|| "settings".into());
    match action.as_str() {
        "redirectAddr" => child_text(account, "RedirectAddr")
            .map(PoxResponse::RedirectAddr)
            .ok_or_else(|| EwsError::Invalid("redirectAddr sin dirección".into())),
        "redirectUrl" => child_text(account, "RedirectUrl")
            .map(PoxResponse::RedirectUrl)
            .ok_or_else(|| EwsError::Invalid("redirectUrl sin URL".into())),
        _ => {
            // EXPR (desde fuera) primero, luego EXCH (desde dentro), luego cualquier otro.
            let mut urls: Vec<(u8, String)> = Vec::new();
            for p in account.children.iter().filter(|c| c.name == "Protocol").take(32) {
                let kind = child_text(p, "Type").or_else(|| p.attr("Type").map(str::to_string)).unwrap_or_default();
                let rank = match kind.to_ascii_uppercase().as_str() {
                    "EXPR" => 0u8,
                    "EXCH" => 1,
                    _ => 2,
                };
                for (name, bump) in [("ExternalEwsUrl", 0u8), ("EwsUrl", 1)] {
                    if let Some(u) = child_text(p, name) {
                        urls.push((rank.saturating_mul(2).saturating_add(bump), u));
                    }
                }
            }
            urls.sort_by_key(|(r, _)| *r);
            let mut out: Vec<String> = Vec::new();
            for (_, u) in urls {
                if crate::validate_url(&u).is_ok() && !out.iter().any(|o| o.eq_ignore_ascii_case(&u)) {
                    out.push(u);
                }
            }
            out.truncate(MAX_URLS);
            Ok(PoxResponse::Settings(out))
        }
    }
}

/// Host de una URL, en minúsculas.
fn host_of(url: &str) -> Option<String> {
    reqwest::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_ascii_lowercase))
}

/// ¿`host` es `domain` o un subdominio suyo?
fn within(host: &str, domain: &str) -> bool {
    let d = domain.trim_end_matches('.').to_ascii_lowercase();
    !d.is_empty() && (host == d || host.ends_with(&format!(".{d}")))
}

/// Dominio de una dirección de correo.
fn domain_of(email: &str) -> Option<String> {
    email.rsplit_once('@').map(|(_, d)| d.trim().to_ascii_lowercase()).filter(|d| !d.is_empty())
}

/// Pregunta a los `endpoints` (URL de `autodiscover.xml`, en orden) la URL de EWS de `email`.
///
/// Un servidor que no responde o no tiene Autodiscover se salta; una contraseña rechazada
/// termina ahí (insistir con otros servidores solo acercaría el bloqueo de la cuenta). Devuelve
/// las URL de EWS, de la mejor a la peor.
pub async fn discover(
    endpoints: &[String],
    email: &str,
    auth: Arc<dyn EwsAuth>,
    timeout: Duration,
) -> Result<Vec<String>> {
    let mut last = EwsError::Invalid("no hay servidor de Autodiscover".into());
    for endpoint in endpoints.iter().take(8) {
        match discover_at(endpoint, email, Arc::clone(&auth), timeout).await {
            Ok(urls) if !urls.is_empty() => return Ok(urls),
            Ok(_) => last = EwsError::Invalid("Autodiscover no trae Exchange Web Services".into()),
            Err(EwsError::Unauthorized) => return Err(EwsError::Unauthorized),
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// Un servidor de Autodiscover, siguiendo sus redirecciones (acotadas).
async fn discover_at(endpoint: &str, email: &str, auth: Arc<dyn EwsAuth>, timeout: Duration) -> Result<Vec<String>> {
    let mut url = endpoint.to_string();
    let mut address = email.to_string();
    let mut client = EwsClient::new(&url, Arc::clone(&auth), timeout)?;
    for _ in 0..=MAX_HOPS {
        let (status, bytes) = client.post_xml(&request(&address)).await?;
        if bytes.len() > MAX_BODY {
            return Err(EwsError::TooLarge);
        }
        if !(200..300).contains(&status) {
            return Err(EwsError::Http { status });
        }
        match parse(&bytes)? {
            PoxResponse::Settings(urls) => return Ok(urls),
            PoxResponse::RedirectAddr(next) => {
                if !next.contains('@') || next.eq_ignore_ascii_case(&address) {
                    return Err(EwsError::Invalid("redirectAddr no válido".into()));
                }
                address = next;
            }
            PoxResponse::RedirectUrl(next) => {
                crate::validate_url(&next)?;
                let host = host_of(&next).unwrap_or_default();
                let here = host_of(&url).unwrap_or_default();
                let allowed = host == here
                    || domain_of(email).is_some_and(|d| within(&host, &d))
                    || domain_of(&address).is_some_and(|d| within(&host, &d));
                if !allowed {
                    return Err(EwsError::Invalid(format!("Autodiscover redirige fuera de tu dominio ({host})")));
                }
                url = next;
                client = EwsClient::new(&url, Arc::clone(&auth), timeout)?;
            }
            PoxResponse::Error { code, message } => {
                return Err(EwsError::Soap { code, message });
            }
        }
    }
    Err(EwsError::Invalid("demasiadas redirecciones de Autodiscover".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SETTINGS: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<Autodiscover xmlns="http://schemas.microsoft.com/exchange/autodiscover/responseschema/2006">
  <Response xmlns="http://schemas.microsoft.com/exchange/autodiscover/outlook/responseschema/2006a">
    <User><DisplayName>Ana</DisplayName></User>
    <Account>
      <AccountType>email</AccountType>
      <Action>settings</Action>
      <Protocol><Type>EXCH</Type><Server>mbx01.corp.contoso.com</Server>
        <EwsUrl>https://mbx01.corp.contoso.com/EWS/Exchange.asmx</EwsUrl></Protocol>
      <Protocol><Type>EXPR</Type><Server>mail.contoso.com</Server>
        <EwsUrl>https://mail.contoso.com/EWS/Exchange.asmx</EwsUrl></Protocol>
      <Protocol Type="mapiHttp" Version="1"><Internal><MailStore><InternalUrl>https://x/mapi</InternalUrl></MailStore></Internal></Protocol>
      <Protocol><Type>WEB</Type><EwsUrl>http://plain.contoso.com/EWS/Exchange.asmx</EwsUrl></Protocol>
    </Account>
  </Response>
</Autodiscover>"#;

    #[test]
    fn settings_put_external_urls_first_and_drop_plain_http() {
        let r = parse(SETTINGS.as_bytes()).unwrap();
        assert_eq!(
            r,
            PoxResponse::Settings(vec![
                "https://mail.contoso.com/EWS/Exchange.asmx".into(),
                "https://mbx01.corp.contoso.com/EWS/Exchange.asmx".into(),
            ])
        );
    }

    #[test]
    fn redirects_and_errors() {
        let addr = r#"<Autodiscover xmlns="a"><Response xmlns="b"><Account><Action>redirectAddr</Action><RedirectAddr>ana.perez@contoso.com</RedirectAddr></Account></Response></Autodiscover>"#;
        assert_eq!(parse(addr.as_bytes()).unwrap(), PoxResponse::RedirectAddr("ana.perez@contoso.com".into()));
        let url = r#"<Autodiscover xmlns="a"><Response xmlns="b"><Account><Action>redirectUrl</Action><RedirectUrl>https://ad2.contoso.com/autodiscover/autodiscover.xml</RedirectUrl></Account></Response></Autodiscover>"#;
        assert!(matches!(parse(url.as_bytes()).unwrap(), PoxResponse::RedirectUrl(_)));
        let err = r#"<Autodiscover xmlns="a"><Response><Error Time="1" Id="2"><ErrorCode>500</ErrorCode><Message>The email address can't be found.</Message></Error></Response></Autodiscover>"#;
        assert_eq!(
            parse(err.as_bytes()).unwrap(),
            PoxResponse::Error { code: "500".into(), message: "The email address can't be found.".into() }
        );
        assert!(parse(b"<html>no</html>").is_err());
        assert!(parse(b"\xff\xfe").is_err());
    }

    #[test]
    fn request_escapes_the_address() {
        let r = request("a<b>@contoso.com");
        assert!(r.contains("<EMailAddress>a&lt;b&gt;@contoso.com</EMailAddress>"));
        assert!(r.contains(RESPONSE_SCHEMA));
    }

    #[test]
    fn domain_checks() {
        assert!(within("mail.contoso.com", "contoso.com"));
        assert!(within("contoso.com", "Contoso.com"));
        assert!(!within("evilcontoso.com", "contoso.com"));
        assert!(!within("contoso.com.evil.net", "contoso.com"));
        assert_eq!(domain_of("ana@Contoso.com").as_deref(), Some("contoso.com"));
    }
}
