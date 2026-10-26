//! Acceso a la red detrás de dos rasgos pequeños ([`Http`] y [`Dns`]), para que todo el
//! descubrimiento se pruebe sin red con datos fijos. Las implementaciones reales usan `reqwest`
//! (rustls) y `hickory-resolver` (Rust puro, tokio).
//!
//! Todo lo que llega de la red es hostil: cuerpos acotados, cabeceras acotadas, listas acotadas y
//! tiempos de espera en cada llamada.

use std::future::Future;
use std::time::Duration;

/// Tope del cuerpo de una respuesta HTTP (un config-v1.1.xml real ocupa 1–5 KiB).
pub const MAX_BODY: usize = 256 * 1024;
/// Tope de registros DNS que se miran por consulta.
pub const MAX_RECORDS: usize = 32;
/// Tiempo de espera de cada petición HTTP.
pub const HTTP_TIMEOUT: Duration = Duration::from_secs(5);
/// Tiempo de espera de cada consulta DNS.
pub const DNS_TIMEOUT: Duration = Duration::from_secs(3);

/// Fallo de red. El descubrimiento lo trata como «esta fuente no dio nada».
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NetError {
    #[error("tiempo de espera agotado")]
    Timeout,
    #[error("no existe")]
    NotFound,
    #[error("respuesta demasiado grande")]
    TooLarge,
    #[error("{0}")]
    Other(String),
}

/// Respuesta HTTP mínima: sin seguir redirecciones (las sigue quien llama, solo a `https`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HttpResponse {
    pub status: u16,
    /// Cabecera `Location`, si la hay.
    pub location: Option<String>,
    pub body: String,
}

/// Registro MX.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MxRecord {
    pub preference: u16,
    /// Nombre del intercambiador, sin el punto final y en minúsculas.
    pub exchange: String,
}

/// Registro SRV (RFC 2782).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SrvRecord {
    pub priority: u16,
    pub weight: u16,
    pub port: u16,
    /// Destino sin el punto final; `""` o `"."` significa «servicio no disponible» (RFC 2782).
    pub target: String,
}

/// Cliente HTTP (solo GET).
pub trait Http {
    fn get(&self, url: &str) -> impl Future<Output = Result<HttpResponse, NetError>> + Send;
}

/// Resolutor DNS.
pub trait Dns {
    fn mx(&self, domain: &str) -> impl Future<Output = Result<Vec<MxRecord>, NetError>> + Send;
    fn srv(&self, name: &str) -> impl Future<Output = Result<Vec<SrvRecord>, NetError>> + Send;
    fn txt(&self, name: &str) -> impl Future<Output = Result<Vec<String>, NetError>> + Send;
    /// ¿El nombre tiene alguna dirección IP?
    fn resolves(&self, host: &str) -> impl Future<Output = bool> + Send;
}

/// Quita el punto final y pasa a minúsculas un nombre DNS.
pub(crate) fn clean_name(name: &str) -> String {
    name.trim().trim_end_matches('.').to_ascii_lowercase()
}

/// Nombre completamente calificado (con punto final) para no pasar por los dominios de búsqueda.
fn fqdn(name: &str) -> String {
    let n = name.trim().trim_end_matches('.');
    format!("{n}.")
}

// ---------------------------------------------------------------------------
// Implementaciones reales
// ---------------------------------------------------------------------------

/// HTTP con `reqwest` + rustls: solo `https`, sin redirecciones automáticas, cuerpo acotado.
#[derive(Clone)]
pub struct ReqwestHttp {
    client: reqwest::Client,
}

impl ReqwestHttp {
    pub fn new() -> Result<Self, NetError> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(HTTP_TIMEOUT)
            .connect_timeout(Duration::from_secs(4))
            .https_only(true)
            .user_agent(concat!("IureDesk/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| NetError::Other(e.to_string()))?;
        Ok(Self { client })
    }
}

impl Http for ReqwestHttp {
    async fn get(&self, url: &str) -> Result<HttpResponse, NetError> {
        let mut resp = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| if e.is_timeout() { NetError::Timeout } else { NetError::Other(e.to_string()) })?;
        let status = resp.status().as_u16();
        let location = resp
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .filter(|v| v.len() <= 2048)
            .map(str::to_string);
        if resp.content_length().is_some_and(|l| l > MAX_BODY as u64) {
            return Err(NetError::TooLarge);
        }
        let mut body: Vec<u8> = Vec::new();
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| if e.is_timeout() { NetError::Timeout } else { NetError::Other(e.to_string()) })?
        {
            if body.len().saturating_add(chunk.len()) > MAX_BODY {
                return Err(NetError::TooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        Ok(HttpResponse { status, location, body: String::from_utf8_lossy(&body).into_owned() })
    }
}

/// DNS con `hickory-resolver` (configuración del sistema; si no se puede leer, Cloudflare).
#[derive(Clone)]
pub struct HickoryDns {
    resolver: hickory_resolver::TokioResolver,
}

impl HickoryDns {
    pub fn new() -> Result<Self, NetError> {
        use hickory_resolver::config::{CLOUDFLARE, ResolverConfig};
        use hickory_resolver::net::runtime::TokioRuntimeProvider;
        let mut builder = match hickory_resolver::TokioResolver::builder_tokio() {
            Ok(b) => b,
            Err(_) => hickory_resolver::TokioResolver::builder_with_config(
                ResolverConfig::udp_and_tcp(&CLOUDFLARE),
                TokioRuntimeProvider::default(),
            ),
        };
        let opts = builder.options_mut();
        opts.timeout = DNS_TIMEOUT;
        opts.attempts = 2;
        let resolver = builder.build().map_err(|e| NetError::Other(e.to_string()))?;
        Ok(Self { resolver })
    }
}

fn dns_err(e: hickory_resolver::net::NetError) -> NetError {
    if e.is_no_records_found() || e.is_nx_domain() { NetError::NotFound } else { NetError::Other(e.to_string()) }
}

impl Dns for HickoryDns {
    async fn mx(&self, domain: &str) -> Result<Vec<MxRecord>, NetError> {
        use hickory_resolver::proto::rr::RData;
        let lookup = self.resolver.mx_lookup(fqdn(domain)).await.map_err(dns_err)?;
        Ok(lookup
            .answers()
            .iter()
            .filter_map(|r| match &r.data {
                RData::MX(mx) => {
                    Some(MxRecord { preference: mx.preference, exchange: clean_name(&mx.exchange.to_ascii()) })
                }
                _ => None,
            })
            .take(MAX_RECORDS)
            .collect())
    }

    async fn srv(&self, name: &str) -> Result<Vec<SrvRecord>, NetError> {
        use hickory_resolver::proto::rr::RData;
        let lookup = self.resolver.srv_lookup(fqdn(name)).await.map_err(dns_err)?;
        Ok(lookup
            .answers()
            .iter()
            .filter_map(|r| match &r.data {
                RData::SRV(s) => Some(SrvRecord {
                    priority: s.priority,
                    weight: s.weight,
                    port: s.port,
                    target: clean_name(&s.target.to_ascii()),
                }),
                _ => None,
            })
            .take(MAX_RECORDS)
            .collect())
    }

    async fn txt(&self, name: &str) -> Result<Vec<String>, NetError> {
        use hickory_resolver::proto::rr::RData;
        let lookup = self.resolver.txt_lookup(fqdn(name)).await.map_err(dns_err)?;
        Ok(lookup
            .answers()
            .iter()
            .filter_map(|r| match &r.data {
                RData::TXT(t) => {
                    let joined: Vec<u8> = t.txt_data.iter().flat_map(|p| p.iter().copied()).take(4096).collect();
                    Some(String::from_utf8_lossy(&joined).into_owned())
                }
                _ => None,
            })
            .take(MAX_RECORDS)
            .collect())
    }

    async fn resolves(&self, host: &str) -> bool {
        matches!(
            tokio::time::timeout(DNS_TIMEOUT, self.resolver.lookup_ip(fqdn(host))).await,
            Ok(Ok(ips)) if ips.iter().next().is_some()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_cleaned() {
        assert_eq!(clean_name("ASPMX.L.Google.com."), "aspmx.l.google.com");
        assert_eq!(fqdn("example.com"), "example.com.");
        assert_eq!(fqdn("example.com."), "example.com.");
    }
}
