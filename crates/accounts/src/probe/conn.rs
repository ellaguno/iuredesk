//! Conexión de texto por líneas sobre TCP, con TLS (rustls) desde el inicio o tras STARTTLS.
//!
//! Lo que manda el servidor es hostil: líneas y literales acotados, y cada lectura con tiempo de
//! espera.

use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

use super::ProbeError;

/// Tope de una línea del servidor.
const MAX_LINE: usize = 16 * 1024;
/// Tope de un literal IMAP que se acepta (y se descarta).
pub(crate) const MAX_LITERAL: usize = 64 * 1024;

/// Flujo TCP, en claro o cifrado.
pub(crate) enum Stream {
    Plain(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
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

/// Configuración TLS: raíces de Mozilla (`webpki-roots`), proveedor `ring`.
pub(crate) fn tls_config() -> Result<Arc<rustls::ClientConfig>, ProbeError> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| ProbeError::Tls(e.to_string()))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(cfg))
}

/// Traduce un error de E/S (de conexión o de TLS) a uno amable.
pub(crate) fn map_io(e: io::Error, host: &str) -> ProbeError {
    if let Some(tls) = e.get_ref().and_then(|i| i.downcast_ref::<rustls::Error>()) {
        return match tls {
            rustls::Error::InvalidCertificate(c) => ProbeError::TlsCertificate(format!("{c:?}")),
            other => ProbeError::Tls(other.to_string()),
        };
    }
    match e.kind() {
        io::ErrorKind::ConnectionRefused => ProbeError::ConnectionRefused { host: host.to_string() },
        io::ErrorKind::TimedOut => ProbeError::Timeout,
        io::ErrorKind::UnexpectedEof | io::ErrorKind::ConnectionReset | io::ErrorKind::BrokenPipe => {
            ProbeError::Protocol("el servidor cerró la conexión".into())
        }
        io::ErrorKind::InvalidData => ProbeError::Tls(e.to_string()),
        _ => ProbeError::Io(e.to_string()),
    }
}

/// Conexión por líneas.
pub(crate) struct LineConn {
    reader: BufReader<Stream>,
    host: String,
    io_timeout: Duration,
}

impl LineConn {
    /// Resuelve, conecta y, si `tls`, cifra desde el inicio.
    pub(crate) async fn connect(host: &str, port: u16, tls: bool, io_timeout: Duration) -> Result<Self, ProbeError> {
        let addrs: Vec<std::net::SocketAddr> =
            match tokio::time::timeout(io_timeout, tokio::net::lookup_host((host, port))).await {
                Ok(Ok(a)) => a.take(8).collect(),
                Ok(Err(_)) => return Err(ProbeError::HostNotFound { host: host.to_string() }),
                Err(_) => return Err(ProbeError::Timeout),
            };
        if addrs.is_empty() {
            return Err(ProbeError::HostNotFound { host: host.to_string() });
        }
        let mut last = ProbeError::HostNotFound { host: host.to_string() };
        let mut tcp = None;
        for addr in addrs {
            match tokio::time::timeout(io_timeout, TcpStream::connect(addr)).await {
                Ok(Ok(s)) => {
                    tcp = Some(s);
                    break;
                }
                Ok(Err(e)) => last = map_io(e, host),
                Err(_) => last = ProbeError::Timeout,
            }
        }
        let tcp = tcp.ok_or(last)?;
        tcp.set_nodelay(true).ok();
        let stream =
            if tls { Stream::Tls(Box::new(Self::handshake(tcp, host, io_timeout).await?)) } else { Stream::Plain(tcp) };
        Ok(Self { reader: BufReader::new(stream), host: host.to_string(), io_timeout })
    }

    async fn handshake(tcp: TcpStream, host: &str, io_timeout: Duration) -> Result<TlsStream<TcpStream>, ProbeError> {
        let name = rustls::pki_types::ServerName::try_from(host.to_string())
            .map_err(|_| ProbeError::Tls(format!("nombre de servidor no válido: {host}")))?;
        let connector = TlsConnector::from(tls_config()?);
        match tokio::time::timeout(io_timeout, connector.connect(name, tcp)).await {
            Ok(Ok(s)) => Ok(s),
            Ok(Err(e)) => Err(map_io(e, host)),
            Err(_) => Err(ProbeError::Timeout),
        }
    }

    /// Cifra una conexión en claro (tras `STARTTLS`/`STLS`).
    pub(crate) async fn upgrade(self) -> Result<Self, ProbeError> {
        let Self { reader, host, io_timeout } = self;
        if !reader.buffer().is_empty() {
            // Datos en claro tras el OK de STARTTLS: posible inyección (CVE-2011-0411).
            return Err(ProbeError::Protocol("datos inesperados antes de cifrar".into()));
        }
        let tcp = match reader.into_inner() {
            Stream::Plain(t) => t,
            Stream::Tls(_) => return Err(ProbeError::Protocol("la conexión ya estaba cifrada".into())),
        };
        let tls = Self::handshake(tcp, &host, io_timeout).await?;
        Ok(Self { reader: BufReader::new(Stream::Tls(Box::new(tls))), host, io_timeout })
    }

    pub(crate) async fn write_line(&mut self, line: &str) -> Result<(), ProbeError> {
        let host = self.host.clone();
        let data = format!("{line}\r\n");
        let stream = self.reader.get_mut();
        match tokio::time::timeout(self.io_timeout, async {
            stream.write_all(data.as_bytes()).await?;
            stream.flush().await
        })
        .await
        {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(map_io(e, &host)),
            Err(_) => Err(ProbeError::Timeout),
        }
    }

    /// Lee una línea (sin `\r\n`), acotada.
    pub(crate) async fn read_line(&mut self) -> Result<String, ProbeError> {
        let mut buf: Vec<u8> = Vec::new();
        match tokio::time::timeout(self.io_timeout, async {
            loop {
                let avail = self.reader.fill_buf().await?;
                if avail.is_empty() {
                    return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
                }
                if let Some(pos) = avail.iter().position(|b| *b == b'\n') {
                    buf.extend_from_slice(avail.get(..=pos).unwrap_or_default());
                    self.reader.consume(pos.saturating_add(1));
                    return Ok(());
                }
                let n = avail.len();
                buf.extend_from_slice(avail);
                self.reader.consume(n);
                if buf.len() > MAX_LINE {
                    return Err(io::Error::new(io::ErrorKind::InvalidInput, "línea demasiado larga"));
                }
            }
        })
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(e)) if e.kind() == io::ErrorKind::InvalidInput => {
                return Err(ProbeError::Protocol("línea demasiado larga".into()));
            }
            Ok(Err(e)) => return Err(map_io(e, &self.host)),
            Err(_) => return Err(ProbeError::Timeout),
        }
        while matches!(buf.last(), Some(b'\n' | b'\r')) {
            buf.pop();
        }
        Ok(String::from_utf8_lossy(&buf).into_owned())
    }

    /// Lee `n` bytes como texto (literal IMAP), acotado.
    pub(crate) async fn read_bytes(&mut self, n: usize) -> Result<String, ProbeError> {
        if n > MAX_LITERAL {
            return Err(ProbeError::Protocol("literal demasiado grande".into()));
        }
        let mut data = vec![0u8; n];
        match tokio::time::timeout(self.io_timeout, self.reader.read_exact(&mut data)).await {
            Ok(Ok(_)) => Ok(String::from_utf8_lossy(&data).into_owned()),
            Ok(Err(e)) => Err(map_io(e, &self.host)),
            Err(_) => Err(ProbeError::Timeout),
        }
    }
}
