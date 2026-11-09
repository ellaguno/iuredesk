//! Cliente de Exchange Web Services para Exchange Server local (2016/2019/Subscription Edition),
//! que no habla Microsoft Graph (F8.2).
//!
//! Lo que resuelve:
//! - SOAP sobre HTTPS con `RequestServerVersion` fijo (Exchange 2013 SP1: lo entienden todos los
//!   servidores con soporte);
//! - autenticación enchufable ([`EwsAuth`]: Basic sobre TLS, token Bearer para Exchange híbrido;
//!   NTLM/Negotiate después);
//! - **limitación**: `ErrorServerBusy` con `BackOffMilliseconds` (y 503) espera lo pedido
//!   (acotado) y reintenta unas pocas veces; después [`EwsError::ServerBusy`];
//! - respuestas acotadas en tamaño y XML acotado en profundidad y elementos (datos hostiles);
//! - los errores de cada elemento (`ResponseCode`) se devuelven por elemento, no tumban el lote.
//!
//! Las operaciones de correo están en [`ops`].
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

pub mod ntlm;
pub mod ops;
pub mod soap;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

pub use iuredesk_dav::dav::xml::Element;
pub use ops::{Addr, FolderInfo, HierarchySync, ItemInfo, ItemSync, MessageDisposition};

/// Ruta habitual del servicio (`https://servidor/EWS/Exchange.asmx`).
pub const EWS_PATH: &str = "/EWS/Exchange.asmx";
/// Tamaño máximo de una respuesta (un MIME grande viaja en base64 dentro del XML).
pub const MAX_RESPONSE: usize = 96 * 1024 * 1024;
/// Reintentos por limitación antes de rendirse.
const DEFAULT_RETRIES: u32 = 4;
/// Espera máxima por reintento (aunque el servidor pida más).
const DEFAULT_MAX_WAIT: Duration = Duration::from_secs(60);

/// Error de EWS.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EwsError {
    /// Red (conexión, TLS, tiempo de espera).
    #[error("red: {0}")]
    Network(String),
    /// El servidor rechazó las credenciales (401).
    #[error("credencial rechazada")]
    Unauthorized,
    /// Respuesta HTTP inesperada (403 con EWS desactivado para el usuario, 404 sin EWS…).
    #[error("ews http {status}")]
    Http {
        /// Código HTTP.
        status: u16,
    },
    /// Error de EWS (`ResponseCode` de un elemento o de un `soap:Fault`).
    #[error("ews {code}: {message}")]
    Soap {
        /// `ErrorItemNotFound`, `ErrorAccessDenied`…
        code: String,
        /// `MessageText` (acotado).
        message: String,
    },
    /// Limitado (`ErrorServerBusy`/503) tras varios reintentos.
    #[error("servidor ocupado (reintentar en {retry_after_ms:?} ms)")]
    ServerBusy {
        /// Lo que pidió el servidor.
        retry_after_ms: Option<u64>,
    },
    /// Respuesta demasiado grande.
    #[error("respuesta de EWS demasiado grande")]
    TooLarge,
    /// XML mal formado o con forma inesperada.
    #[error("respuesta no válida: {0}")]
    Invalid(String),
    /// Autenticación (token, NTLM…).
    #[error("autenticación: {0}")]
    Auth(String),
}

impl EwsError {
    /// ¿Vale la pena reintentar sin que el usuario haga nada?
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Network(_) | Self::ServerBusy { .. } => true,
            Self::Http { status } => *status >= 500,
            Self::Soap { code, .. } => matches!(code.as_str(), "ErrorServerBusy" | "ErrorInternalServerTransientError"),
            _ => false,
        }
    }

    /// ¿Hay que volver a iniciar sesión (contraseña cambiada o caducada)?
    pub fn needs_reauth(&self) -> bool {
        matches!(self, Self::Unauthorized)
    }

    /// ¿El elemento ya no existe? (borrar o mover algo que ya no está da igual).
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::Soap { code, .. } if code == "ErrorItemNotFound" || code == "ErrorFolderNotFound")
    }

    /// `ResponseCode` de EWS, si lo hubo.
    pub fn code(&self) -> Option<&str> {
        match self {
            Self::Soap { code, .. } => Some(code),
            _ => None,
        }
    }
}

impl From<reqwest::Error> for EwsError {
    fn from(e: reqwest::Error) -> Self {
        Self::Network(e.to_string())
    }
}

impl From<iuredesk_dav::Error> for EwsError {
    fn from(e: iuredesk_dav::Error) -> Self {
        Self::Invalid(e.to_string())
    }
}

/// Resultado del crate.
pub type Result<T, E = EwsError> = std::result::Result<T, E>;

/// Futuro en caja que se puede mandar entre hilos.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Cómo se autentica cada petición.
pub trait EwsAuth: Send + Sync {
    /// Valor de la cabecera `Authorization` para la próxima petición.
    fn authorization(&self) -> BoxFuture<'_, Result<String>>;
    /// El servidor rechazó la última credencial (401): olvidar lo que haya en caché.
    fn invalidate(&self) {}
    /// Usuario y contraseña para NTLM, si esta autenticación los tiene (un token no). Se usan
    /// cuando el servidor no ofrece Basic y sí NTLM/Negotiate.
    fn ntlm(&self) -> Option<BoxFuture<'_, Result<ntlm::Credentials>>> {
        None
    }
}

/// Usuario y contraseña (Basic) sobre TLS. Exchange local acepta `DOMINIO\usuario` o el UPN.
pub struct BasicAuth {
    header: String,
    creds: ntlm::Credentials,
}

impl BasicAuth {
    pub fn new(username: &str, password: &str) -> Self {
        use base64::Engine as _;
        let raw = format!("{username}:{password}");
        Self {
            header: format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(raw.as_bytes())),
            creds: ntlm::Credentials::from_login(username, password),
        }
    }

    /// Valor de la cabecera `Authorization`.
    pub fn header(&self) -> &str {
        &self.header
    }
}

impl std::fmt::Debug for BasicAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BasicAuth(…)")
    }
}

impl EwsAuth for BasicAuth {
    fn authorization(&self) -> BoxFuture<'_, Result<String>> {
        Box::pin(async move { Ok(self.header.clone()) })
    }

    fn ntlm(&self) -> Option<BoxFuture<'_, Result<ntlm::Credentials>>> {
        Some(Box::pin(async move { Ok(self.creds.clone()) }))
    }
}

/// Token OAuth (Exchange híbrido con *modern auth*): lo da quien conoce la cuenta.
pub struct BearerAuth {
    token: Box<dyn Fn() -> BoxFuture<'static, Result<String>> + Send + Sync>,
    on_reject: Box<dyn Fn() + Send + Sync>,
}

impl BearerAuth {
    /// `token` da un token de acceso vigente; `on_reject` se llama tras un 401.
    pub fn new(
        token: impl Fn() -> BoxFuture<'static, Result<String>> + Send + Sync + 'static,
        on_reject: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        Self { token: Box::new(token), on_reject: Box::new(on_reject) }
    }
}

impl EwsAuth for BearerAuth {
    fn authorization(&self) -> BoxFuture<'_, Result<String>> {
        let fut = (self.token)();
        Box::pin(async move { Ok(format!("Bearer {}", fut.await?)) })
    }

    fn invalidate(&self) {
        (self.on_reject)();
    }
}

/// Cliente de EWS para un buzón.
#[derive(Clone)]
pub struct EwsClient {
    http: reqwest::Client,
    url: String,
    auth: Arc<dyn EwsAuth>,
    retries: u32,
    max_wait: Duration,
    /// Esquema NTLM elegido (`NTLM` o `Negotiate`) cuando el servidor no ofrece Basic.
    ntlm_scheme: Arc<std::sync::Mutex<Option<&'static str>>>,
    /// NTLM autentica la conexión: los intercambios van de uno en uno por la misma.
    ntlm_lock: Arc<tokio::sync::Mutex<()>>,
}

impl std::fmt::Debug for EwsClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EwsClient").field("url", &self.url).finish_non_exhaustive()
    }
}

/// Valida la URL del servicio: `https` (o `http` solo hacia este equipo, para pruebas).
pub fn validate_url(url: &str) -> Result<()> {
    let u = reqwest::Url::parse(url).map_err(|e| EwsError::Invalid(format!("{url}: {e}")))?;
    let local = matches!(u.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
    if u.scheme() != "https" && !(u.scheme() == "http" && local) {
        return Err(EwsError::Invalid(format!("{url}: se exige https")));
    }
    Ok(())
}

/// URL de EWS a partir de lo que escribió el usuario: un servidor (`mail.contoso.com`) o la URL
/// completa.
pub fn service_url(input: &str) -> Result<String> {
    let t = input.trim().trim_end_matches('/');
    if t.is_empty() {
        return Err(EwsError::Invalid("falta el servidor".into()));
    }
    let url = if t.contains("://") {
        if t.to_ascii_lowercase().ends_with(".asmx") { t.to_string() } else { format!("{t}{EWS_PATH}") }
    } else {
        format!("https://{t}{EWS_PATH}")
    };
    validate_url(&url)?;
    Ok(url)
}

/// URL del servicio de una cuenta: `host` es el servidor (con `port` si no es el de siempre) o
/// la URL completa; `tls` = `https`.
pub fn server_url(host: &str, port: u16, tls: bool) -> Result<String> {
    let h = host.trim();
    if h.contains("://") {
        return service_url(h);
    }
    let (scheme, default) = if tls { ("https", 443) } else { ("http", 80) };
    let authority = if port == 0 || port == default { h.to_string() } else { format!("{h}:{port}") };
    service_url(&format!("{scheme}://{authority}"))
}

/// `BackOffMilliseconds` de un `soap:Fault` de `ErrorServerBusy`.
fn back_off(root: &Element) -> Option<u64> {
    fn find(e: &Element) -> Option<u64> {
        if e.name == "Value" && e.attr("Name") == Some("BackOffMilliseconds") {
            return e.text_trim().parse().ok();
        }
        e.children.iter().find_map(find)
    }
    find(root)
}

impl EwsClient {
    /// Cliente con tiempo total `timeout` por petición. `url` es la del servicio
    /// (`https://mail.contoso.com/EWS/Exchange.asmx`).
    pub fn new(url: &str, auth: Arc<dyn EwsAuth>, timeout: Duration) -> Result<Self> {
        validate_url(url)?;
        // HTTP/1.1 y una sola conexión en reposo: NTLM autentica la conexión, así que la
        // negociación y la petición deben ir por la misma. `tls_info` da el certificado para el
        // channel binding.
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .connect_timeout(Duration::from_secs(20))
            .user_agent(concat!("IureDesk/", env!("CARGO_PKG_VERSION")))
            .http1_only()
            .pool_max_idle_per_host(1)
            .tls_info(true)
            .build()?;
        Ok(Self {
            http,
            url: url.to_string(),
            auth,
            retries: DEFAULT_RETRIES,
            max_wait: DEFAULT_MAX_WAIT,
            ntlm_scheme: Arc::new(std::sync::Mutex::new(None)),
            ntlm_lock: Arc::new(tokio::sync::Mutex::new(())),
        })
    }

    /// Reintentos por limitación y espera máxima por reintento.
    pub fn with_retry(mut self, retries: u32, max_wait: Duration) -> Self {
        self.retries = retries.min(20);
        self.max_wait = max_wait;
        self
    }

    /// URL del servicio.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Manda `body` (el contenido de `soap:Body`) y devuelve el elemento de respuesta (el primer
    /// hijo de `soap:Body`). Un `soap:Fault` es error; los errores por elemento los mira quien
    /// llama ([`soap::messages`]).
    pub async fn call(&self, body: &str) -> Result<Element> {
        let envelope = soap::envelope(body);
        let mut retried_auth = false;
        let mut attempt = 0u32;
        loop {
            let scheme = *self.ntlm_scheme.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let resp = match scheme {
                Some(scheme) => self.ntlm_send(&envelope, scheme).await?,
                None => {
                    let auth = self.auth.authorization().await?;
                    self.post(&envelope).header("Authorization", auth).send().await?
                }
            };
            let status = resp.status().as_u16();
            if status == 401 {
                // Sin Basic pero con NTLM/Negotiate (lo habitual en Exchange local): se cambia a
                // NTLM una vez y se recuerda para las siguientes.
                if scheme.is_none()
                    && let Some(next) = ntlm_offer(resp.headers())
                    && self.auth.ntlm().is_some()
                {
                    *self.ntlm_scheme.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(next);
                    continue;
                }
                self.auth.invalidate();
                if retried_auth {
                    return Err(EwsError::Unauthorized);
                }
                retried_auth = true;
                continue;
            }
            if resp.content_length().is_some_and(|l| l > MAX_RESPONSE as u64) {
                return Err(EwsError::TooLarge);
            }
            let bytes = read_capped(resp).await?;
            // Un fallo de SOAP llega con 500 y XML; otro 5xx (o 503 sin XML) se trata como ocupado.
            let parsed = if bytes.is_empty() { None } else { iuredesk_dav::dav::xml::parse(&bytes).ok() };
            let busy = match &parsed {
                Some(root) => soap::fault(root).filter(|(code, _)| code == "ErrorServerBusy").map(|_| back_off(root)),
                None if status == 503 => Some(None),
                None => None,
            };
            if let Some(wait) = busy {
                if attempt >= self.retries {
                    return Err(EwsError::ServerBusy { retry_after_ms: wait });
                }
                attempt = attempt.saturating_add(1);
                let ms = wait.unwrap_or(1_000u64.saturating_mul(u64::from(attempt)));
                tokio::time::sleep(Duration::from_millis(ms).min(self.max_wait)).await;
                continue;
            }
            let Some(root) = parsed else {
                return Err(if (200..300).contains(&status) {
                    EwsError::Invalid("respuesta vacía o no XML".into())
                } else {
                    EwsError::Http { status }
                });
            };
            if let Some((code, message)) = soap::fault(&root) {
                return Err(EwsError::Soap { code, message });
            }
            if !(200..300).contains(&status) {
                return Err(EwsError::Http { status });
            }
            return soap::body_response(root);
        }
    }
}

impl EwsClient {
    fn post(&self, body: &str) -> reqwest::RequestBuilder {
        self.http
            .post(&self.url)
            .header("Content-Type", "text/xml; charset=utf-8")
            .header("Accept", "text/xml")
            .body(body.to_string())
    }

    /// Un intercambio NTLM completo por la misma conexión: `NEGOTIATE` con un POST vacío,
    /// `CHALLENGE` del servidor y la petición con `AUTHENTICATE`.
    async fn ntlm_send(&self, envelope: &str, scheme: &'static str) -> Result<reqwest::Response> {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD;
        let creds = match self.auth.ntlm() {
            Some(f) => f.await?,
            None => return Err(EwsError::Auth("esta cuenta no tiene usuario y contraseña para NTLM".into())),
        };
        let _one_at_a_time = self.ntlm_lock.lock().await;
        let negotiate = ntlm::negotiate();
        let first = self
            .http
            .post(&self.url)
            .header("Content-Type", "text/xml; charset=utf-8")
            .header("Content-Length", "0")
            .header("Authorization", format!("{scheme} {}", b64.encode(&negotiate)))
            .send()
            .await?;
        let cert =
            first.extensions().get::<reqwest::tls::TlsInfo>().and_then(|i| i.peer_certificate()).map(<[u8]>::to_vec);
        let challenge = first
            .headers()
            .get_all(reqwest::header::WWW_AUTHENTICATE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .find_map(|v| v.strip_prefix(scheme).and_then(|r| r.strip_prefix(' ')))
            .map(str::trim)
            .and_then(|t| b64.decode(t).ok());
        // Se vacía la respuesta para que la conexión quede libre para el siguiente paso.
        let _ = read_capped(first).await;
        let Some(challenge) = challenge else { return Err(EwsError::Unauthorized) };
        let challenge = ntlm::parse_challenge(&challenge).map_err(|e| EwsError::Auth(e.to_string()))?;
        let host =
            reqwest::Url::parse(&self.url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
        let nonce = ntlm::ClientNonce::random().map_err(|e| EwsError::Auth(e.to_string()))?;
        let auth = ntlm::authenticate(&creds, &negotiate, &challenge, cert.as_deref(), &format!("HTTP/{host}"), nonce);
        Ok(self.post(envelope).header("Authorization", format!("{scheme} {}", b64.encode(&auth))).send().await?)
    }
}

/// ¿El servidor ofrece NTLM (o Negotiate) y no Basic? Devuelve el esquema que se usará.
fn ntlm_offer(headers: &reqwest::header::HeaderMap) -> Option<&'static str> {
    let offered: Vec<String> = headers
        .get_all(reqwest::header::WWW_AUTHENTICATE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .map(|v| v.split_whitespace().next().unwrap_or_default().to_ascii_lowercase())
        .collect();
    if offered.iter().any(|s| s == "basic") {
        return None;
    }
    if offered.iter().any(|s| s == "ntlm") {
        Some("NTLM")
    } else if offered.iter().any(|s| s == "negotiate") {
        // IIS acepta un token NTLM dentro de Negotiate.
        Some("Negotiate")
    } else {
        None
    }
}

/// Lee el cuerpo sin pasar de [`MAX_RESPONSE`].
async fn read_capped(mut resp: reqwest::Response) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        if out.len().saturating_add(chunk.len()) > MAX_RESPONSE {
            return Err(EwsError::TooLarge);
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_urls() {
        assert_eq!(service_url(" mail.contoso.com ").unwrap(), "https://mail.contoso.com/EWS/Exchange.asmx");
        assert_eq!(
            service_url("https://mail.contoso.com/EWS/Exchange.asmx").unwrap(),
            "https://mail.contoso.com/EWS/Exchange.asmx"
        );
        assert_eq!(service_url("https://mail.contoso.com/").unwrap(), "https://mail.contoso.com/EWS/Exchange.asmx");
        assert!(service_url("http://mail.contoso.com").is_err(), "sin TLS no");
        assert!(service_url("http://127.0.0.1:8080").is_ok(), "pruebas locales");
        assert!(service_url("").is_err());
    }

    #[test]
    fn server_urls() {
        assert_eq!(server_url("mail.contoso.com", 443, true).unwrap(), "https://mail.contoso.com/EWS/Exchange.asmx");
        assert_eq!(
            server_url("mail.contoso.com", 8443, true).unwrap(),
            "https://mail.contoso.com:8443/EWS/Exchange.asmx"
        );
        assert_eq!(
            server_url("https://owa.contoso.com/ews/exchange.asmx", 443, true).unwrap(),
            "https://owa.contoso.com/ews/exchange.asmx"
        );
        assert!(server_url("mail.contoso.com", 80, false).is_err(), "sin TLS solo en este equipo");
        assert_eq!(server_url("127.0.0.1", 8080, false).unwrap(), "http://127.0.0.1:8080/EWS/Exchange.asmx");
    }

    #[test]
    fn errors_classify() {
        assert!(EwsError::ServerBusy { retry_after_ms: Some(1) }.is_transient());
        assert!(EwsError::Unauthorized.needs_reauth());
        let nf = EwsError::Soap { code: "ErrorItemNotFound".into(), message: String::new() };
        assert!(nf.is_not_found() && !nf.is_transient());
        assert!(!format!("{:?}", BasicAuth::new("u", "secreto")).contains("secreto"));
    }
}
