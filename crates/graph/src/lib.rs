//! Cliente HTTP de Microsoft Graph compartido por el correo (`sync`), el calendario, los contactos
//! y las tareas (`pim-sync`) y el envío (`sendMail`).
//!
//! Lo que resuelve una sola vez:
//! - token por cuenta y audiencia ([`TokenProvider`]); un 401 olvida el token y reintenta una vez;
//! - **limitación** (429/503/504): espera lo que diga `Retry-After` (acotado) y reintenta unas
//!   pocas veces; después devuelve [`GraphError::Throttled`] para que la capa de arriba reintente
//!   más tarde sin perder nada;
//! - respuestas acotadas en tamaño (datos externos hostiles);
//! - paginado (`@odata.nextLink`) y delta (`@odata.deltaLink`, 410 → empezar de nuevo), solo
//!   dentro de la misma raíz: el token nunca viaja a otro sitio.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

pub mod large;
mod send;

use std::sync::Arc;
use std::time::Duration;

use iuredesk_model::account::Account;
use iuredesk_oauth::{TokenAudience, TokenProvider};

use serde_json::Value;

pub use reqwest::Method;
pub use send::{GraphSendTransport, MAX_MIME_BYTES, with_bcc_header};

/// Raíz pública de Microsoft Graph v1.0.
pub const GRAPH_BASE: &str = "https://graph.microsoft.com/v1.0";
/// Tamaño máximo de una respuesta.
pub const MAX_RESPONSE: usize = 64 * 1024 * 1024;
/// Reintentos por limitación antes de rendirse.
const DEFAULT_RETRIES: u32 = 4;
/// Espera máxima por reintento (aunque `Retry-After` pida más).
const DEFAULT_MAX_WAIT: Duration = Duration::from_secs(60);

/// Error de Graph.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GraphError {
    /// Respuesta de error de Graph.
    #[error("graph {status}: {code}")]
    Http {
        /// Código HTTP.
        status: u16,
        /// `error.code` de Graph.
        code: String,
    },
    /// Red (conexión, TLS, tiempo de espera).
    #[error("red: {0}")]
    Network(String),
    /// El servidor rechazó el token dos veces.
    #[error("credencial rechazada")]
    Unauthorized,
    /// Limitado (429/503) tras varios reintentos.
    #[error("limitado por el servidor (reintentar en {retry_after:?} s)")]
    Throttled {
        /// Segundos que pidió el servidor.
        retry_after: Option<u64>,
    },
    /// OAuth.
    #[error("oauth: {0}")]
    OAuth(#[from] iuredesk_oauth::Error),
    /// Respuesta demasiado grande.
    #[error("respuesta de Graph demasiado grande")]
    TooLarge,
    /// Dato con forma inesperada.
    #[error("dato no válido: {0}")]
    Invalid(String),
}

impl GraphError {
    /// ¿Vale la pena reintentar sin que el usuario haga nada?
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Network(_) | Self::Throttled { .. } => true,
            Self::Http { status, .. } => *status >= 500 || *status == 429,
            Self::OAuth(e) => matches!(e, iuredesk_oauth::Error::Http(_)),
            _ => false,
        }
    }

    /// ¿Hay que volver a iniciar sesión?
    pub fn needs_reauth(&self) -> bool {
        match self {
            Self::Unauthorized => true,
            Self::OAuth(e) => e.needs_reconsent(),
            _ => false,
        }
    }

    /// Código HTTP, si lo hubo.
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Http { status, .. } => Some(*status),
            Self::Unauthorized => Some(401),
            Self::Throttled { .. } => Some(429),
            _ => None,
        }
    }
}

impl From<reqwest::Error> for GraphError {
    fn from(e: reqwest::Error) -> Self {
        Self::Network(e.to_string())
    }
}

/// Resultado del crate.
pub type Result<T, E = GraphError> = std::result::Result<T, E>;

/// Respuesta de Graph (cuerpo como JSON si lo era; si no, `Null` y los bytes en `raw`).
#[derive(Debug, Clone, PartialEq)]
pub struct GraphResponse {
    /// Código HTTP.
    pub status: u16,
    /// Cuerpo JSON (o `Null`).
    pub body: Value,
    /// Cuerpo tal cual.
    pub raw: Vec<u8>,
}

impl GraphResponse {
    /// 2xx.
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// Error con el `error.code` de Graph.
    pub fn error(&self) -> GraphError {
        let code = self.body.get("error").and_then(|e| e.get("code")).and_then(Value::as_str).unwrap_or_default();
        GraphError::Http { status: self.status, code: code.chars().take(200).collect() }
    }
}

/// Cuerpo de una petición.
#[derive(Debug, Clone, Copy)]
pub enum Body<'a> {
    /// JSON.
    Json(&'a Value),
    /// Bytes con su tipo (`text/plain` para MIME en base64).
    Raw {
        /// `Content-Type`.
        content_type: &'a str,
        /// Datos.
        data: &'a [u8],
    },
}

/// Resultado de recorrer un delta.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DeltaPage {
    /// Objetos recibidos (incluye los `@removed`).
    pub items: Vec<Value>,
    /// Enlace para la próxima vez: `deltaLink`, o el `nextLink` pendiente si se cortó por páginas.
    pub link: Option<String>,
    /// Se empezó desde cero (no había enlace o caducó con 410).
    pub full: bool,
    /// Quedan páginas por pedir (se cortó por el tope).
    pub more: bool,
}

/// Cliente de Graph para una cuenta.
#[derive(Clone)]
pub struct GraphClient {
    http: reqwest::Client,
    base: String,
    tokens: Arc<dyn TokenProvider>,
    account: Account,
    audience: TokenAudience,
    prefer: String,
    max_pages: usize,
    retries: u32,
    max_wait: Duration,
}

impl std::fmt::Debug for GraphClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GraphClient")
            .field("base", &self.base)
            .field("account", &self.account.id)
            .finish_non_exhaustive()
    }
}

/// Raíz del buzón de una cuenta en Graph: `/me`, o `/users/{dirección}` si es un buzón
/// compartido o delegado (F8.1).
pub fn mailbox_root(account: &Account) -> String {
    if account.shared_of.is_none() {
        return "/me".to_string();
    }
    let addr: String = account.email.trim().chars().take(320).collect();
    // `@` se deja legible; lo demás fuera de lo seguro va codificado (`/`, `?`, `#`…).
    let enc = enc(&addr).replace("%40", "@");
    format!("/users/{enc}")
}

/// `Retry-After` en segundos (solo la forma numérica; la de fecha se trata como ausente).
fn retry_after(h: &reqwest::header::HeaderMap) -> Option<u64> {
    h.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?.trim().parse::<u64>().ok()
}

impl GraphClient {
    /// Cliente con tiempo total `timeout` por petición. `base` es la raíz (`GRAPH_BASE` en uso real).
    pub fn new(account: Account, tokens: Arc<dyn TokenProvider>, base: &str, timeout: Duration) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .connect_timeout(Duration::from_secs(20))
            .user_agent(concat!("IureDesk/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            http,
            base: base.trim_end_matches('/').to_string(),
            tokens,
            // Un buzón compartido pide su token con los alcances `.Shared` (F8.1).
            audience: if account.shared_of.is_some() { TokenAudience::Shared } else { TokenAudience::Api },
            account,
            prefer: "odata.maxpagesize=100".to_string(),
            max_pages: 200,
            retries: DEFAULT_RETRIES,
            max_wait: DEFAULT_MAX_WAIT,
        })
    }

    /// Cabecera `Prefer` de todas las peticiones (zona horaria, ids inmutables, página).
    pub fn with_prefer(mut self, prefer: impl Into<String>) -> Self {
        self.prefer = prefer.into();
        self
    }

    /// Páginas máximas por lista o delta.
    pub fn with_max_pages(mut self, n: usize) -> Self {
        self.max_pages = n.max(1);
        self
    }

    /// Reintentos por limitación y espera máxima por reintento.
    pub fn with_retry(mut self, retries: u32, max_wait: Duration) -> Self {
        self.retries = retries.min(20);
        self.max_wait = max_wait;
        self
    }

    /// Cuenta.
    pub fn account(&self) -> &Account {
        &self.account
    }

    /// Raíz.
    pub fn base(&self) -> &str {
        &self.base
    }

    /// Raíz del buzón en las rutas: `/me`, o `/users/{dirección}` en un buzón compartido o
    /// delegado (F8.1). La dirección va codificada como segmento de ruta.
    pub fn mailbox(&self) -> String {
        mailbox_root(&self.account)
    }

    /// URL absoluta de una ruta (`/me/mailFolders`). En un buzón compartido, `/me` se cambia por
    /// [`GraphClient::mailbox`]: el correo, el calendario, los contactos y el envío escriben sus
    /// rutas una sola vez.
    pub fn url(&self, path: &str) -> String {
        if let Some(rest) = path.strip_prefix("/me")
            && (rest.is_empty() || rest.starts_with('/') || rest.starts_with('?'))
            && self.account.shared_of.is_some()
        {
            return format!("{}{}{rest}", self.base, self.mailbox());
        }
        format!("{}{}", self.base, path)
    }

    /// ¿El enlace es de la misma raíz? (los `nextLink`/`deltaLink` solo se siguen así).
    pub fn same_origin(&self, link: &str) -> bool {
        let origin = |u: &str| reqwest::Url::parse(u).ok().map(|x| x.origin().ascii_serialization());
        origin(link).is_some() && origin(link) == origin(&self.base)
    }

    /// Petición con token, reintento tras 401 y espera ante 429/503/504.
    pub async fn request(
        &self,
        method: Method,
        url: &str,
        body: Option<Body<'_>>,
        headers: &[(&str, &str)],
    ) -> Result<GraphResponse> {
        let mut reauthed = false;
        let mut throttled = 0u32;
        loop {
            let token = self.tokens.access_token(&self.account, self.audience).await?;
            let mut req = self.http.request(method.clone(), url).bearer_auth(&token).header("Prefer", &self.prefer);
            for (k, v) in headers {
                req = req.header(*k, *v);
            }
            match body {
                Some(Body::Json(v)) => req = req.json(v),
                Some(Body::Raw { content_type, data }) => {
                    req = req.header(reqwest::header::CONTENT_TYPE, content_type).body(data.to_vec());
                }
                None => {}
            }
            let mut resp = req.send().await?;
            let status = resp.status().as_u16();
            if status == 401 && !reauthed {
                self.tokens.invalidate(&self.account.id);
                reauthed = true;
                continue;
            }
            if matches!(status, 429 | 503 | 504) {
                let after = retry_after(resp.headers());
                if throttled >= self.retries {
                    return Err(GraphError::Throttled { retry_after: after });
                }
                throttled = throttled.saturating_add(1);
                let backoff = Duration::from_secs(2u64.saturating_pow(throttled));
                let wait = after.map_or(backoff, Duration::from_secs).min(self.max_wait);
                tokio::time::sleep(wait).await;
                continue;
            }
            if status == 401 {
                return Err(GraphError::Unauthorized);
            }
            if resp.content_length().is_some_and(|l| l > MAX_RESPONSE as u64) {
                return Err(GraphError::TooLarge);
            }
            let mut raw = Vec::new();
            while let Some(chunk) = resp.chunk().await? {
                if raw.len().saturating_add(chunk.len()) > MAX_RESPONSE {
                    return Err(GraphError::TooLarge);
                }
                raw.extend_from_slice(&chunk);
            }
            let body = if raw.is_empty() { Value::Null } else { serde_json::from_slice(&raw).unwrap_or(Value::Null) };
            return Ok(GraphResponse { status, body, raw });
        }
    }

    /// Sube un trozo de un adjunto a la URL ya firmada de una sesión de subida (R4.5). Va **sin**
    /// token (Graph lo rechaza en esa URL) y solo por HTTPS (o por HTTP si la raíz de Graph lo es:
    /// servidor falso de pruebas). Devuelve el código HTTP si es 2xx.
    pub async fn upload_chunk(&self, upload_url: &str, start: usize, total: usize, data: &[u8]) -> Result<u16> {
        let scheme_ok = upload_url.starts_with("https://")
            || (upload_url.starts_with("http://") && self.base.starts_with("http://"));
        if !scheme_ok || upload_url.len() > 4096 {
            return Err(GraphError::Invalid("URL de subida no válida".into()));
        }
        if data.is_empty() || start.saturating_add(data.len()) > total {
            return Err(GraphError::Invalid("trozo fuera de rango".into()));
        }
        let end = start.saturating_add(data.len()).saturating_sub(1);
        let resp = self
            .http
            .put(upload_url)
            .header(reqwest::header::CONTENT_RANGE, format!("bytes {start}-{end}/{total}"))
            .header(reqwest::header::CONTENT_LENGTH, data.len())
            .body(data.to_vec())
            .send()
            .await?;
        let status = resp.status().as_u16();
        if (200..300).contains(&status) {
            return Ok(status);
        }
        if status == 429 || status == 503 {
            return Err(GraphError::Throttled { retry_after: retry_after(resp.headers()) });
        }
        Err(GraphError::Http { status, code: String::new() })
    }

    /// `GET` que debe salir bien; devuelve el JSON.
    pub async fn get_json(&self, url: &str) -> Result<Value> {
        let r = self.request(Method::GET, url, None, &[]).await?;
        if !r.is_success() {
            return Err(r.error());
        }
        Ok(r.body)
    }

    /// `GET` de bytes (p. ej. `/messages/{id}/$value`, el MIME).
    pub async fn get_bytes(&self, url: &str) -> Result<Vec<u8>> {
        let r = self.request(Method::GET, url, None, &[]).await?;
        if !r.is_success() {
            return Err(r.error());
        }
        Ok(r.raw)
    }

    /// Lista paginada (`value` + `@odata.nextLink`).
    pub async fn list(&self, path: &str) -> Result<Vec<Value>> {
        let mut out = Vec::new();
        let mut next = Some(if path.starts_with("http") { path.to_string() } else { self.url(path) });
        let mut pages = 0usize;
        while let Some(url) = next.take() {
            let page = self.get_json(&url).await?;
            out.extend(page.get("value").and_then(Value::as_array).cloned().unwrap_or_default());
            pages = pages.saturating_add(1);
            next = page
                .get("@odata.nextLink")
                .and_then(Value::as_str)
                .filter(|l| self.same_origin(l) && pages < self.max_pages)
                .map(str::to_string);
        }
        Ok(out)
    }

    /// Recorre un delta desde `start` (o el enlace guardado). Un enlace caducado (410) vuelve a
    /// empezar desde `start`. Si se llega al tope de páginas, `link` es el `nextLink` pendiente y
    /// `more` es `true`.
    pub async fn delta(&self, saved: Option<&str>, start: &str) -> Result<DeltaPage> {
        let saved = saved.filter(|l| self.same_origin(l));
        let mut full = saved.is_none();
        let mut url = saved.map_or_else(|| self.url(start), str::to_string);
        let mut items = Vec::new();
        for _ in 0..self.max_pages {
            let r = self.request(Method::GET, &url, None, &[]).await?;
            if r.status == 410 && !full {
                full = true;
                items.clear();
                url = self.url(start);
                continue;
            }
            if !r.is_success() {
                return Err(r.error());
            }
            items.extend(r.body.get("value").and_then(Value::as_array).cloned().unwrap_or_default());
            if let Some(next) = r.body.get("@odata.nextLink").and_then(Value::as_str).filter(|l| self.same_origin(l)) {
                url = next.to_string();
                continue;
            }
            let link = r.body.get("@odata.deltaLink").and_then(Value::as_str).map(str::to_string);
            return Ok(DeltaPage { items, link, full, more: false });
        }
        Ok(DeltaPage { items, link: Some(url), full, more: true })
    }
}

/// Codifica un segmento de ruta (ids de Graph: base64 con `/`, `+`, `=`).
pub fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_classify() {
        assert!(GraphError::Throttled { retry_after: Some(3) }.is_transient());
        assert!(GraphError::Http { status: 503, code: String::new() }.is_transient());
        assert!(!GraphError::Http { status: 404, code: String::new() }.is_transient());
        assert!(GraphError::Unauthorized.needs_reauth());
        assert_eq!(GraphError::Throttled { retry_after: None }.status(), Some(429));
    }

    #[test]
    fn shared_mailbox_root() {
        use iuredesk_model::account::{AuthMethod, OAuthProvider, Protocol, Security, ServerConfig};
        let srv = |protocol| ServerConfig {
            protocol,
            host: "outlook.office365.com".into(),
            port: 993,
            security: Security::Tls,
            auth: AuthMethod::OAuth2 { provider: OAuthProvider::Microsoft },
            username: "ana@contoso.com".into(),
        };
        let mut a = Account {
            id: "a2".into(),
            display_name: String::new(),
            email: "ventas@contoso.com".into(),
            kind: Default::default(),
            incoming: srv(Protocol::Imap),
            outgoing: srv(Protocol::Smtp),
            caldav_url: None,
            carddav_url: None,
            identities: vec![],
            shared_of: None,
        };
        assert_eq!(mailbox_root(&a), "/me");
        a.shared_of = Some("a1".into());
        assert_eq!(mailbox_root(&a), "/users/ventas@contoso.com");
        a.email = "x/../y?z@contoso.com".into();
        assert_eq!(mailbox_root(&a), "/users/x%2F..%2Fy%3Fz@contoso.com");
    }

    #[test]
    fn encodes_ids() {
        assert_eq!(enc("AAMk/a+b=="), "AAMk%2Fa%2Bb%3D%3D");
        assert_eq!(enc("inbox"), "inbox");
    }
}
