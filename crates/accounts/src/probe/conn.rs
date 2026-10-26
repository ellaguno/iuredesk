//! Conexión de texto por líneas sobre la red de `iuredesk-net` (TLS desde el inicio o tras STARTTLS).
//!
//! Lo que manda el servidor es hostil: líneas y literales acotados, y cada lectura con tiempo de
//! espera.

use std::io;
use std::time::Duration;

use iuredesk_net::{NetError, Stream};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

use super::ProbeError;

/// Tope de una línea del servidor.
const MAX_LINE: usize = 16 * 1024;
/// Tope de un literal IMAP que se acepta (y se descarta).
pub(crate) const MAX_LITERAL: usize = 64 * 1024;

/// Traduce un fallo de red de `iuredesk-net` al error de la prueba.
pub(crate) fn from_net(e: NetError) -> ProbeError {
    match e {
        NetError::HostNotFound { host } => ProbeError::HostNotFound { host },
        NetError::ConnectionRefused { host } => ProbeError::ConnectionRefused { host },
        NetError::Timeout => ProbeError::Timeout,
        NetError::TlsCertificate(c) => ProbeError::TlsCertificate(c),
        NetError::Tls(t) => ProbeError::Tls(t),
        NetError::Closed => ProbeError::Protocol("el servidor cerró la conexión".into()),
        NetError::Protocol(p) => ProbeError::Protocol(p),
        NetError::Io(i) => ProbeError::Io(i),
    }
}

/// Traduce un error de E/S (de conexión o de TLS) a uno amable.
pub(crate) fn map_io(e: io::Error, host: &str) -> ProbeError {
    from_net(iuredesk_net::map_io(&e, host))
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
        let stream = iuredesk_net::connect(host, port, tls, io_timeout).await.map_err(from_net)?;
        Ok(Self { reader: BufReader::new(stream), host: host.to_string(), io_timeout })
    }

    /// Cifra una conexión en claro (tras `STARTTLS`/`STLS`).
    pub(crate) async fn upgrade(self) -> Result<Self, ProbeError> {
        let Self { reader, host, io_timeout } = self;
        let buffered = reader.buffer().len();
        let stream =
            iuredesk_net::starttls(reader.into_inner(), buffered, &host, io_timeout, None).await.map_err(from_net)?;
        Ok(Self { reader: BufReader::new(stream), host, io_timeout })
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
