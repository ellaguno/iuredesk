//! Transporte HTTP del cliente DAV.

use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{Method, StatusCode, Url};

use super::auth::{AuthProvider, SharedAuth};
use super::quirks;
use super::xml::{self, Multistatus};
use crate::{Error, Result};

/// Opciones del cliente.
#[derive(Debug, Clone)]
pub struct DavOptions {
    /// Tiempo máximo de una petición completa (incluido el cuerpo).
    pub timeout: Duration,
    /// Tiempo máximo para conectar.
    pub connect_timeout: Duration,
    /// Tamaño máximo de una respuesta.
    pub max_response_bytes: usize,
    /// Redirecciones máximas por petición.
    pub max_redirects: usize,
    /// `href` máximos por `multiget`.
    pub multiget_batch: usize,
    /// Páginas máximas de un `sync-collection` truncado (507).
    pub max_sync_pages: usize,
    /// `User-Agent`.
    pub user_agent: String,
}

impl Default for DavOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(60),
            connect_timeout: Duration::from_secs(15),
            max_response_bytes: 64 * 1024 * 1024,
            max_redirects: 5,
            multiget_batch: 100,
            max_sync_pages: 50,
            user_agent: format!("IureDesk/{}", env!("CARGO_PKG_VERSION")),
        }
    }
}

/// Encabezado `Depth`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    /// Solo el recurso.
    Zero,
    /// El recurso y sus hijos directos.
    One,
}

impl Depth {
    fn as_str(self) -> &'static str {
        match self {
            Self::Zero => "0",
            Self::One => "1",
        }
    }
}

/// Respuesta HTTP leída por completo (con tope de tamaño).
#[derive(Debug, Clone)]
pub struct RawResponse {
    /// Código HTTP.
    pub status: u16,
    /// URL final (tras redirecciones).
    pub url: Url,
    /// `ETag`.
    pub etag: Option<String>,
    /// `Location`.
    pub location: Option<String>,
    /// `Content-Type`.
    pub content_type: Option<String>,
    /// Cuerpo.
    pub body: Vec<u8>,
}

impl RawResponse {
    /// ¿2xx?
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// Cliente DAV. Barato de clonar (comparte el pool de conexiones).
#[derive(Clone)]
pub struct DavClient {
    http: reqwest::Client,
    auth: SharedAuth,
    pub(crate) opts: DavOptions,
}

impl std::fmt::Debug for DavClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DavClient").field("opts", &self.opts).finish_non_exhaustive()
    }
}

impl DavClient {
    /// Cliente con la autenticación y las opciones dadas.
    pub fn new(auth: impl AuthProvider + 'static, opts: DavOptions) -> Result<Self> {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(opts.timeout)
            .connect_timeout(opts.connect_timeout)
            .user_agent(opts.user_agent.clone())
            .build()
            .map_err(|e| Error::Network(e.to_string()))?;
        Ok(Self { http, auth: Arc::new(auth), opts })
    }

    /// Petición cruda. Sigue redirecciones conservando método y cuerpo; la credencial solo
    /// viaja a destinos del mismo sitio ([`quirks::same_site`]). Ante 401 pide al proveedor
    /// invalidar el token y reintenta una vez.
    pub async fn request(
        &self,
        method: &str,
        url: &Url,
        depth: Option<Depth>,
        headers: &[(&str, String)],
        body: Option<(&str, Vec<u8>)>,
    ) -> Result<RawResponse> {
        let method = Method::from_bytes(method.as_bytes()).map_err(|_| Error::Url(format!("método {method}")))?;
        let mut current = url.clone();
        let mut send_auth = true;
        let mut retried_auth = false;
        let mut hops = 0usize;
        loop {
            let mut map = HeaderMap::new();
            if let Some(d) = depth {
                map.insert("Depth", HeaderValue::from_static(d.as_str()));
            }
            for (k, v) in headers {
                let name = HeaderName::from_bytes(k.as_bytes()).map_err(|e| Error::Url(e.to_string()))?;
                let value = HeaderValue::from_str(v).map_err(|e| Error::Url(e.to_string()))?;
                map.insert(name, value);
            }
            if send_auth {
                let header = self.auth.header().await?;
                if !header.is_empty() {
                    let mut value = HeaderValue::from_str(&header).map_err(|e| Error::Auth(e.to_string()))?;
                    value.set_sensitive(true);
                    map.insert(reqwest::header::AUTHORIZATION, value);
                }
            }
            let mut req = self.http.request(method.clone(), current.clone()).headers(map);
            if let Some((ct, bytes)) = &body {
                req = req.header(reqwest::header::CONTENT_TYPE, *ct).body(bytes.clone());
            }
            let resp = req.send().await.map_err(net_error)?;
            let status = resp.status();

            if status.is_redirection() && status != StatusCode::NOT_MODIFIED {
                let Some(loc) = resp.headers().get(reqwest::header::LOCATION).and_then(|v| v.to_str().ok()) else {
                    return Err(Error::Http { status: status.as_u16(), detail: "redirección sin destino".into() });
                };
                hops = hops.saturating_add(1);
                if hops > self.opts.max_redirects {
                    return Err(Error::Http { status: status.as_u16(), detail: "demasiadas redirecciones".into() });
                }
                let next = current.join(loc).map_err(|e| Error::Url(e.to_string()))?;
                if current.scheme() == "https" && next.scheme() != "https" {
                    return Err(Error::Url("el servidor redirigió de HTTPS a HTTP".into()));
                }
                send_auth = send_auth && quirks::same_site(&current, &next);
                current = next;
                continue;
            }
            if status == StatusCode::UNAUTHORIZED {
                if !retried_auth && send_auth && self.auth.invalidate() {
                    retried_auth = true;
                    continue;
                }
                return Err(Error::Unauthorized);
            }
            return self.read(resp, current).await;
        }
    }

    async fn read(&self, mut resp: reqwest::Response, url: Url) -> Result<RawResponse> {
        let max = self.opts.max_response_bytes;
        if resp.content_length().is_some_and(|l| l > max as u64) {
            return Err(Error::Limit(format!("respuesta de más de {max} bytes")));
        }
        let header = |name: reqwest::header::HeaderName, r: &reqwest::Response| {
            r.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_string)
        };
        let etag = header(reqwest::header::ETAG, &resp);
        let location = header(reqwest::header::LOCATION, &resp);
        let content_type = header(reqwest::header::CONTENT_TYPE, &resp);
        let status = resp.status().as_u16();
        let mut body = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(net_error)? {
            if body.len().saturating_add(chunk.len()) > max {
                return Err(Error::Limit(format!("respuesta de más de {max} bytes")));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(RawResponse { status, url, etag, location, content_type, body })
    }

    /// `PROPFIND` con el cuerpo dado; devuelve la URL final y el `multistatus`.
    pub async fn propfind(&self, url: &Url, depth: Depth, body: &str) -> Result<(Url, Multistatus)> {
        let resp = self.request("PROPFIND", url, Some(depth), &[], Some((XML, body.as_bytes().to_vec()))).await?;
        Ok((resp.url.clone(), expect_multistatus(&resp, "PROPFIND")?))
    }

    /// `REPORT` con el cuerpo dado.
    pub async fn report(&self, url: &Url, depth: Depth, body: &str) -> Result<(Url, Multistatus)> {
        let resp = self.request("REPORT", url, Some(depth), &[], Some((XML, body.as_bytes().to_vec()))).await?;
        Ok((resp.url.clone(), expect_multistatus(&resp, "REPORT")?))
    }
}

pub(crate) const XML: &str = "application/xml; charset=utf-8";

fn net_error(e: reqwest::Error) -> Error {
    if e.is_timeout() { Error::Network("tiempo de espera agotado".into()) } else { Error::Network(e.to_string()) }
}

/// Convierte un estado de error en el [`Error`] adecuado.
pub(crate) fn status_error(resp: &RawResponse, what: &str) -> Error {
    match resp.status {
        401 => Error::Unauthorized,
        404 | 410 => Error::NotFound,
        412 => Error::PreconditionFailed,
        s => Error::Http { status: s, detail: format!("{what} {}", resp.url) },
    }
}

fn expect_multistatus(resp: &RawResponse, what: &str) -> Result<Multistatus> {
    // Algunos servidores responden 200 con el multistatus.
    if resp.status == 207 || (resp.status == 200 && !resp.body.is_empty()) {
        xml::parse_multistatus(&resp.body)
    } else {
        Err(status_error(resp, what))
    }
}
