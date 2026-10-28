//! «Cancelar suscripción» en un clic: `List-Unsubscribe` (RFC 2369) con
//! `List-Unsubscribe-Post: List-Unsubscribe=One-Click` (RFC 8058) → `POST` por HTTPS sin abrir el
//! navegador; si no, un correo a la dirección `mailto:` (por la bandeja de salida); si solo hay
//! una página, se abre en el navegador. Siempre tras confirmar.

use std::time::Duration;

use iuredesk_compose::Draft;
use iuredesk_model::Mailbox;
use iuredesk_model::account::Identity;

use crate::Bi;
use crate::error::{Result, RulesError};
use crate::facts::Headers;

/// Cómo darse de baja.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Method {
    /// RFC 8058: `POST` a la URL, sin interacción.
    OneClick {
        /// URL HTTPS.
        url: String,
    },
    /// Correo a la lista.
    Mailto {
        /// Dirección.
        to: String,
        /// Asunto pedido (o «unsubscribe»).
        subject: String,
        /// Texto pedido.
        body: String,
    },
    /// Página web (se abre en el navegador).
    Web {
        /// URL.
        url: String,
    },
}

impl Method {
    /// Explicación para la confirmación.
    pub fn explain(&self, sender: &str) -> Bi {
        match self {
            Method::OneClick { .. } => Bi::new(
                format!("IureDesk will ask {sender} to stop sending you mail. No browser needed."),
                format!("IureDesk le pedirá a {sender} que deje de enviarte correo. Sin abrir el navegador."),
            ),
            Method::Mailto { to, .. } => Bi::new(
                format!("IureDesk will send an unsubscribe request to {to}."),
                format!("IureDesk enviará una solicitud de baja a {to}."),
            ),
            Method::Web { url } => Bi::new(
                format!("The sender's unsubscribe page will open: {}", host_of(url)),
                format!("Se abrirá la página de baja del remitente: {}", host_of(url)),
            ),
        }
    }
}

fn host_of(url: &str) -> String {
    url::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default()
}

/// Formas de baja que ofrece el mensaje, la mejor primero.
pub fn methods(h: &Headers) -> Vec<Method> {
    let Some(raw) = h.get("list-unsubscribe") else { return Vec::new() };
    let one_click = h
        .get("list-unsubscribe-post")
        .is_some_and(|v| v.replace(' ', "").eq_ignore_ascii_case("List-Unsubscribe=One-Click"));
    let mut https = Vec::new();
    let mut mails = Vec::new();
    // `<uri>, <uri>` (como mucho 8; lo demás se ignora).
    for part in raw.split('<').skip(1).take(8) {
        let Some(uri) = part.split('>').next().map(str::trim) else { continue };
        let lower = uri.to_ascii_lowercase();
        if lower.starts_with("https://") {
            if url::Url::parse(uri).is_ok() {
                https.push(uri.to_string());
            }
        } else if lower.starts_with("mailto:")
            && let Some(m) = parse_mailto(uri)
        {
            mails.push(m);
        }
    }
    let mut out = Vec::new();
    if one_click && let Some(u) = https.first() {
        out.push(Method::OneClick { url: u.clone() });
    }
    out.extend(mails);
    if !one_click {
        out.extend(https.into_iter().map(|url| Method::Web { url }));
    }
    out
}

fn parse_mailto(uri: &str) -> Option<Method> {
    let u = url::Url::parse(uri).ok()?;
    let to = percent_decode(u.path());
    if !crate::rule::plausible_address(&to) {
        return None;
    }
    let mut subject = String::new();
    let mut body = String::new();
    for (k, v) in u.query_pairs() {
        match k.to_ascii_lowercase().as_str() {
            "subject" => subject = v.chars().take(200).collect(),
            "body" => body = v.chars().take(2_000).collect(),
            _ => {}
        }
    }
    if subject.trim().is_empty() {
        subject = "unsubscribe".into();
    }
    Some(Method::Mailto { to, subject, body })
}

fn percent_decode(s: &str) -> String {
    url::form_urlencoded::parse(format!("x={}", s.replace('+', "%2B")).as_bytes())
        .next()
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default()
}

/// Opciones de la petición en un clic.
#[derive(Clone, Debug)]
pub struct HttpOptions {
    /// Tiempo total.
    pub timeout: Duration,
    /// Permitir `http://127.0.0.1` (solo pruebas).
    pub allow_loopback_http: bool,
}

impl Default for HttpOptions {
    fn default() -> Self {
        Self { timeout: Duration::from_secs(15), allow_loopback_http: false }
    }
}

/// Baja en un clic (RFC 8058 §3.1): `POST` con `List-Unsubscribe=One-Click`, sin cookies ni
/// credenciales, siguiendo como mucho 3 redirecciones HTTPS.
pub async fn one_click(url: &str, opts: &HttpOptions) -> Result<u16> {
    let parsed = url::Url::parse(url).map_err(|e| RulesError::Invalid(e.to_string()))?;
    let loopback = parsed.host_str().is_some_and(|h| h == "127.0.0.1" || h == "localhost" || h == "[::1]");
    let ok_scheme = parsed.scheme() == "https" || (opts.allow_loopback_http && loopback && parsed.scheme() == "http");
    if !ok_scheme {
        return Err(RulesError::Invalid("solo se admite HTTPS".into()));
    }
    let allow_http = opts.allow_loopback_http;
    let client = reqwest::Client::builder()
        .timeout(opts.timeout)
        .connect_timeout(opts.timeout)
        .user_agent(iuredesk_compose::USER_AGENT)
        .redirect(reqwest::redirect::Policy::custom(move |a| {
            if a.previous().len() >= 3 {
                a.stop()
            } else if a.url().scheme() == "https" || allow_http {
                a.follow()
            } else {
                a.stop()
            }
        }))
        .build()
        .map_err(|e| RulesError::Net(e.to_string()))?;
    let resp = client
        .post(parsed)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body("List-Unsubscribe=One-Click")
        .send()
        .await
        .map_err(|e| if e.is_timeout() { RulesError::Timeout } else { RulesError::Net(e.to_string()) })?;
    let status = resp.status().as_u16();
    if resp.status().is_success() || resp.status().is_redirection() {
        Ok(status)
    } else {
        Err(RulesError::Server(format!("HTTP {status}")))
    }
}

/// Borrador de la baja por correo.
pub fn mailto_draft(identity: Identity, to: &str, subject: &str, body: &str) -> Draft {
    let mut d = Draft::new(identity);
    d.to = vec![Mailbox::addr(to)];
    d.subject = subject.to_string();
    d.body = crate::apply::plain(if body.trim().is_empty() { "unsubscribe" } else { body });
    d.plain_only = true;
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_one_click_first() {
        let h = Headers::from_pairs([
            ("List-Unsubscribe", "<mailto:baja@tienda.example?subject=Baja%20ya>, <https://tienda.example/u/abc>"),
            ("List-Unsubscribe-Post", "List-Unsubscribe=One-Click"),
        ]);
        assert_eq!(
            methods(&h),
            vec![
                Method::OneClick { url: "https://tienda.example/u/abc".into() },
                Method::Mailto { to: "baja@tienda.example".into(), subject: "Baja ya".into(), body: String::new() },
            ]
        );
        let web = Headers::from_pairs([("List-Unsubscribe", "<http://x.example/u>, <https://x.example/u>")]);
        assert_eq!(methods(&web), vec![Method::Web { url: "https://x.example/u".into() }], "http en claro no");
        assert!(methods(&Headers::default()).is_empty());
        let junk = Headers::from_pairs([("List-Unsubscribe", "<javascript:alert(1)>, <mailto:no-es-direccion>")]);
        assert!(methods(&junk).is_empty());
    }

    #[tokio::test]
    async fn rejects_plain_http() {
        let e = one_click("http://tienda.example/u", &HttpOptions::default()).await.unwrap_err();
        assert!(matches!(e, RulesError::Invalid(_)));
    }
}
