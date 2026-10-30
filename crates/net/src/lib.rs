//! Conexiones de red para los protocolos de correo (capa L2): TCP con tiempos de espera, TLS con
//! `rustls` (raíces de Mozilla, proveedor `ring`) desde el primer byte o tras STARTTLS/STLS.
//!
//! Lo comparten la prueba de conexión de `iuredesk-accounts` y los clientes `iuredesk-imap` e
//! `iuredesk-pop3`. Aquí no se interpreta ningún protocolo: solo se abre, cifra y clasifica el
//! fallo en un [`NetError`] que cada capa traduce a un mensaje humano.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

pub use rustls::ClientConfig;

/// Direcciones que se intentan como mucho al resolver un nombre.
const MAX_ADDRS: usize = 8;

/// Fallo de red, ya clasificado.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NetError {
    /// El nombre no resuelve.
    #[error("no se encontró el servidor {host}")]
    HostNotFound {
        /// Servidor.
        host: String,
    },
    /// Conexión rechazada (puerto cerrado).
    #[error("el servidor {host} rechazó la conexión")]
    ConnectionRefused {
        /// Servidor.
        host: String,
    },
    /// Tiempo de espera agotado.
    #[error("tiempo de espera agotado")]
    Timeout,
    /// Certificado no válido (caducado, autofirmado, de otro nombre).
    #[error("certificado TLS no válido: {0}")]
    TlsCertificate(String),
    /// Otro fallo de TLS.
    #[error("error de TLS: {0}")]
    Tls(String),
    /// El servidor cerró la conexión.
    #[error("el servidor cerró la conexión")]
    Closed,
    /// Respuesta que viola el protocolo (p. ej. datos en claro tras STARTTLS).
    #[error("respuesta inesperada del servidor: {0}")]
    Protocol(String),
    /// Otro error de E/S.
    #[error("error de red: {0}")]
    Io(String),
}

/// Flujo TCP, en claro o cifrado.
pub enum Stream {
    /// Sin cifrar (solo servidores locales, pruebas, o antes de STARTTLS).
    Plain(TcpStream),
    /// Cifrado con TLS.
    Tls(Box<TlsStream<TcpStream>>),
}

impl std::fmt::Debug for Stream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.is_tls() { "Stream::Tls" } else { "Stream::Plain" })
    }
}

impl Stream {
    /// `true` si la conexión está cifrada.
    pub fn is_tls(&self) -> bool {
        matches!(self, Stream::Tls(_))
    }
}

impl AsyncRead for Stream {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_read(cx, buf),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Stream {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_write(cx, buf),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_flush(cx),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_shutdown(cx),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_shutdown(cx),
        }
    }
}

/// Configuración TLS por omisión: raíces de Mozilla (`webpki-roots`), proveedor `ring`, TLS 1.2+.
pub fn tls_config() -> Result<Arc<ClientConfig>, NetError> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let cfg = ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| NetError::Tls(e.to_string()))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(cfg))
}

/// Traduce un error de E/S (de conexión o de TLS) a un [`NetError`].
pub fn map_io(e: &io::Error, host: &str) -> NetError {
    if let Some(tls) = e.get_ref().and_then(|i| i.downcast_ref::<rustls::Error>()) {
        return match tls {
            rustls::Error::InvalidCertificate(c) => NetError::TlsCertificate(format!("{c:?}")),
            other => NetError::Tls(other.to_string()),
        };
    }
    match e.kind() {
        io::ErrorKind::ConnectionRefused => NetError::ConnectionRefused { host: host.to_string() },
        io::ErrorKind::TimedOut => NetError::Timeout,
        io::ErrorKind::UnexpectedEof | io::ErrorKind::ConnectionReset | io::ErrorKind::BrokenPipe => NetError::Closed,
        io::ErrorKind::InvalidData => NetError::Tls(e.to_string()),
        _ => NetError::Io(e.to_string()),
    }
}

/// Resuelve `host`, abre TCP (probando hasta 8 direcciones) y, si `tls`, cifra desde el inicio.
/// Cada paso tiene `timeout`.
pub async fn connect(host: &str, port: u16, tls: bool, timeout: Duration) -> Result<Stream, NetError> {
    connect_with(host, port, tls, timeout, None).await
}

/// Como [`connect`], con una configuración TLS propia (p. ej. una raíz de pruebas).
pub async fn connect_with(
    host: &str,
    port: u16,
    tls: bool,
    timeout: Duration,
    config: Option<Arc<ClientConfig>>,
) -> Result<Stream, NetError> {
    let tcp = connect_tcp(host, port, timeout).await?;
    if tls { Ok(Stream::Tls(Box::new(handshake(tcp, host, timeout, config).await?))) } else { Ok(Stream::Plain(tcp)) }
}

/// Abre la conexión TCP en claro.
pub async fn connect_tcp(host: &str, port: u16, timeout: Duration) -> Result<TcpStream, NetError> {
    let addrs: Vec<std::net::SocketAddr> =
        match tokio::time::timeout(timeout, tokio::net::lookup_host((host, port))).await {
            Ok(Ok(a)) => a.take(MAX_ADDRS).collect(),
            Ok(Err(_)) => return Err(NetError::HostNotFound { host: host.to_string() }),
            Err(_) => return Err(NetError::Timeout),
        };
    let mut last = NetError::HostNotFound { host: host.to_string() };
    for addr in addrs {
        match tokio::time::timeout(timeout, TcpStream::connect(addr)).await {
            Ok(Ok(s)) => {
                s.set_nodelay(true).ok();
                return Ok(s);
            }
            Ok(Err(e)) => last = map_io(&e, host),
            Err(_) => last = NetError::Timeout,
        }
    }
    Err(last)
}

/// Negocia TLS sobre una conexión TCP abierta.
pub async fn handshake(
    tcp: TcpStream,
    host: &str,
    timeout: Duration,
    config: Option<Arc<ClientConfig>>,
) -> Result<TlsStream<TcpStream>, NetError> {
    let name = rustls::pki_types::ServerName::try_from(host.to_string())
        .map_err(|_| NetError::Tls(format!("nombre de servidor no válido: {host}")))?;
    let cfg = match config {
        Some(c) => c,
        None => tls_config()?,
    };
    let connector = TlsConnector::from(cfg);
    match tokio::time::timeout(timeout, connector.connect(name, tcp)).await {
        Ok(Ok(s)) => Ok(s),
        Ok(Err(e)) => Err(map_io(&e, host)),
        Err(_) => Err(NetError::Timeout),
    }
}

/// Cifra una conexión en claro tras `STARTTLS`/`STLS`. `buffered` son los bytes que el lector ya
/// tenía sin consumir: si hay alguno, el servidor (o alguien en medio) inyectó datos en claro
/// tras el OK (CVE-2011-0411) y se aborta.
pub async fn starttls(
    stream: Stream,
    buffered: usize,
    host: &str,
    timeout: Duration,
    config: Option<Arc<ClientConfig>>,
) -> Result<Stream, NetError> {
    if buffered > 0 {
        return Err(NetError::Protocol("datos inesperados antes de cifrar".into()));
    }
    match stream {
        Stream::Plain(tcp) => Ok(Stream::Tls(Box::new(handshake(tcp, host, timeout, config).await?))),
        Stream::Tls(_) => Err(NetError::Protocol("la conexión ya estaba cifrada".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn refused_and_not_found_are_classified() {
        // Un puerto recién liberado casi seguro está cerrado.
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        drop(l);
        // Windows reintenta el SYN a un puerto cerrado unos 2 s antes de dar «rechazada».
        let e = connect("127.0.0.1", port, false, Duration::from_secs(10)).await.unwrap_err();
        assert!(matches!(e, NetError::ConnectionRefused { .. }), "{e:?}");
        let e = connect("no-existe.invalid", 993, true, Duration::from_secs(5)).await.unwrap_err();
        assert!(matches!(e, NetError::HostNotFound { .. } | NetError::Timeout), "{e:?}");
    }

    #[tokio::test]
    async fn starttls_rejects_injected_bytes() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let s = connect("127.0.0.1", port, false, Duration::from_secs(2)).await.unwrap();
        let e = starttls(s, 3, "127.0.0.1", Duration::from_secs(1), None).await.unwrap_err();
        assert!(matches!(e, NetError::Protocol(_)));
    }

    #[test]
    fn tls_config_builds() {
        assert!(tls_config().is_ok());
    }
}
