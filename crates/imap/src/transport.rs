//! Transporte: búfer de entrada, delimitación de respuestas (líneas + literales), COMPRESS=DEFLATE
//! (RFC 4978) y escrituras con tiempo de espera.
//!
//! Las lecturas del socket se hacen en [`Transport::fill`], que es seguro ante cancelación (los
//! bytes solo entran al búfer cuando la lectura terminó); por eso IDLE puede competir con un
//! temporizador sin perder datos.

use std::time::Duration;

use flate2::{Compress, Compression, Decompress, FlushCompress, FlushDecompress, Status};
use iuredesk_net::{NetError, Stream, map_io};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::error::{ImapError, Result};

/// Tamaño de cada lectura del socket.
const CHUNK: usize = 64 * 1024;
/// Vueltas máximas de (des)compresión por bloque: cada vuelta progresa o se corta.
const MAX_ZLIB_ROUNDS: usize = 1 << 20;

/// Topes de lo que se acepta del servidor.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Línea (sin literales).
    pub max_line: usize,
    /// Literal.
    pub max_literal: usize,
    /// Respuesta completa con sus literales.
    pub max_response: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self { max_line: 8 * 1024 * 1024, max_literal: 64 * 1024 * 1024, max_response: 80 * 1024 * 1024 }
    }
}

struct Deflate {
    c: Compress,
    d: Decompress,
}

/// Conexión con búfer.
pub(crate) struct Transport {
    stream: Stream,
    buf: Vec<u8>,
    scratch: Vec<u8>,
    deflate: Option<Deflate>,
    host: String,
    /// Inicio de la línea en curso dentro de la respuesta que se está delimitando.
    frame_pos: usize,
    /// Desde dónde seguir buscando el salto de línea.
    scan: usize,
    limits: Limits,
}

/// `{123}`, `{123+}`, `{123-}` o `~{123}` al final de una línea.
pub(crate) fn trailing_literal(line: &[u8]) -> Option<usize> {
    let inner = line.strip_suffix(b"}")?;
    let open = inner.iter().rposition(|b| *b == b'{')?;
    let mut n = inner.get(open.saturating_add(1)..)?;
    if let Some(stripped) = n.strip_suffix(b"+").or_else(|| n.strip_suffix(b"-")) {
        n = stripped;
    }
    if n.is_empty() || n.len() > 12 || !n.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(n).ok()?.parse().ok()
}

fn zlib_err(e: impl std::fmt::Display) -> ImapError {
    ImapError::Protocol(format!("COMPRESS: {e}"))
}

/// Descomprime `input` añadiéndolo a `out`.
fn inflate(d: &mut Decompress, input: &[u8], out: &mut Vec<u8>, max: usize) -> Result<()> {
    let mut consumed = 0usize;
    for _ in 0..MAX_ZLIB_ROUNDS {
        out.reserve(CHUNK);
        let (in0, out0) = (d.total_in(), d.total_out());
        let rest = input.get(consumed..).unwrap_or_default();
        let st = d.decompress_vec(rest, out, FlushDecompress::Sync).map_err(zlib_err)?;
        let used = usize::try_from(d.total_in().saturating_sub(in0)).unwrap_or(usize::MAX);
        let produced = d.total_out().saturating_sub(out0);
        consumed = consumed.saturating_add(used);
        if out.len() > max {
            return Err(ImapError::TooLarge("respuesta comprimida"));
        }
        let full = out.len() == out.capacity();
        if st == Status::StreamEnd || (consumed >= input.len() && !full) || (used == 0 && produced == 0 && !full) {
            return Ok(());
        }
    }
    Err(zlib_err("sin progreso"))
}

/// Comprime `input` con vaciado de sincronización.
fn deflate(c: &mut Compress, input: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len().saturating_div(2).saturating_add(64));
    let mut consumed = 0usize;
    for _ in 0..MAX_ZLIB_ROUNDS {
        out.reserve(CHUNK);
        let in0 = c.total_in();
        let rest = input.get(consumed..).unwrap_or_default();
        c.compress_vec(rest, &mut out, FlushCompress::Sync).map_err(zlib_err)?;
        consumed = consumed.saturating_add(usize::try_from(c.total_in().saturating_sub(in0)).unwrap_or(usize::MAX));
        if consumed >= input.len() && out.len() < out.capacity() {
            return Ok(out);
        }
    }
    Err(zlib_err("sin progreso"))
}

impl Transport {
    pub(crate) fn new(stream: Stream, host: &str, limits: Limits) -> Self {
        Self {
            stream,
            buf: Vec::new(),
            scratch: vec![0u8; CHUNK],
            deflate: None,
            host: host.to_string(),
            frame_pos: 0,
            scan: 0,
            limits,
        }
    }

    /// Devuelve el flujo y cuántos bytes quedaban sin leer en el búfer.
    pub(crate) fn into_stream(self) -> (Stream, usize) {
        (self.stream, self.buf.len())
    }

    pub(crate) fn is_compressed(&self) -> bool {
        self.deflate.is_some()
    }

    /// Activa DEFLATE en ambos sentidos. Lo que ya estaba en el búfer llegó comprimido.
    pub(crate) fn enable_deflate(&mut self) -> Result<()> {
        let mut d = Deflate { c: Compress::new(Compression::default(), false), d: Decompress::new(false) };
        let pending = std::mem::take(&mut self.buf);
        let max = self.max_buffer();
        inflate(&mut d.d, &pending, &mut self.buf, max)?;
        self.frame_pos = 0;
        self.scan = 0;
        self.deflate = Some(d);
        Ok(())
    }

    fn max_buffer(&self) -> usize {
        self.limits.max_response.saturating_add(CHUNK.saturating_mul(1024))
    }

    /// Lee del socket al búfer (seguro ante cancelación). Error si el servidor cerró.
    pub(crate) async fn fill(&mut self) -> Result<()> {
        let n = match self.stream.read(&mut self.scratch).await {
            Ok(0) => return Err(ImapError::Net(NetError::Closed)),
            Ok(n) => n,
            Err(e) => return Err(ImapError::Net(map_io(&e, &self.host))),
        };
        let data = self.scratch.get(..n).unwrap_or_default();
        let max = self.max_buffer();
        match self.deflate.as_mut() {
            Some(z) => inflate(&mut z.d, data, &mut self.buf, max)?,
            None => self.buf.extend_from_slice(data),
        }
        if self.buf.len() > max {
            return Err(ImapError::TooLarge("respuesta"));
        }
        Ok(())
    }

    /// [`Transport::fill`] con tiempo de espera.
    pub(crate) async fn fill_timeout(&mut self, t: Duration) -> Result<()> {
        match tokio::time::timeout(t, self.fill()).await {
            Ok(r) => r,
            Err(_) => Err(ImapError::Net(NetError::Timeout)),
        }
    }

    /// Escribe (comprimiendo si toca) con tiempo de espera.
    pub(crate) async fn write_all(&mut self, data: &[u8], t: Duration) -> Result<()> {
        let owned;
        let bytes = match self.deflate.as_mut() {
            Some(z) => {
                owned = deflate(&mut z.c, data)?;
                owned.as_slice()
            }
            None => data,
        };
        let host = self.host.clone();
        let stream = &mut self.stream;
        match tokio::time::timeout(t, async {
            stream.write_all(bytes).await?;
            stream.flush().await
        })
        .await
        {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(ImapError::Net(map_io(&e, &host))),
            Err(_) => Err(ImapError::Net(NetError::Timeout)),
        }
    }

    /// Saca del búfer una respuesta completa (sin el CRLF final), si ya está entera.
    pub(crate) fn try_frame(&mut self) -> Result<Option<Vec<u8>>> {
        let lim = self.limits;
        let mut pos = self.frame_pos;
        let mut search = self.scan.max(pos);
        loop {
            let hay = self.buf.get(search..).unwrap_or_default();
            let Some(i) = hay.iter().position(|b| *b == b'\n') else {
                if self.buf.len().saturating_sub(pos) > lim.max_line {
                    return Err(ImapError::TooLarge("línea"));
                }
                self.frame_pos = pos;
                self.scan = self.buf.len();
                return Ok(None);
            };
            let lf = search.saturating_add(i);
            let mut line = self.buf.get(pos..lf).unwrap_or_default();
            if let Some(l) = line.strip_suffix(b"\r") {
                line = l;
            }
            if line.len() > lim.max_line {
                return Err(ImapError::TooLarge("línea"));
            }
            if let Some(n) = trailing_literal(line) {
                if n > lim.max_literal {
                    return Err(ImapError::TooLarge("literal"));
                }
                let end = lf.saturating_add(1).saturating_add(n);
                if end > lim.max_response {
                    return Err(ImapError::TooLarge("respuesta"));
                }
                if self.buf.len() < end {
                    self.frame_pos = pos;
                    self.scan = lf;
                    return Ok(None);
                }
                pos = end;
                search = end;
                continue;
            }
            let mut frame: Vec<u8> = self.buf.drain(..=lf).collect();
            while matches!(frame.last(), Some(b'\n' | b'\r')) {
                frame.pop();
            }
            self.frame_pos = 0;
            self.scan = 0;
            return Ok(Some(frame));
        }
    }

    /// Lee una respuesta completa; `t` es el tiempo máximo entre bloques recibidos.
    pub(crate) async fn read_frame(&mut self, t: Duration) -> Result<Vec<u8>> {
        loop {
            if let Some(f) = self.try_frame()? {
                return Ok(f);
            }
            self.fill_timeout(t).await?;
        }
    }

    /// Lee una línea sin interpretar literales (servidor de pruebas, continuaciones).
    #[allow(dead_code)]
    pub(crate) async fn read_line(&mut self, t: Duration) -> Result<Vec<u8>> {
        loop {
            if let Some(i) = self.buf.iter().position(|b| *b == b'\n') {
                let mut line: Vec<u8> = self.buf.drain(..=i).collect();
                while matches!(line.last(), Some(b'\n' | b'\r')) {
                    line.pop();
                }
                self.frame_pos = 0;
                self.scan = 0;
                return Ok(line);
            }
            if self.buf.len() > self.limits.max_line {
                return Err(ImapError::TooLarge("línea"));
            }
            self.fill_timeout(t).await?;
        }
    }

    /// Lee exactamente `n` bytes (servidor de pruebas: literales de órdenes).
    #[allow(dead_code)]
    pub(crate) async fn read_exact(&mut self, n: usize, t: Duration) -> Result<Vec<u8>> {
        if n > self.limits.max_literal {
            return Err(ImapError::TooLarge("literal"));
        }
        while self.buf.len() < n {
            self.fill_timeout(t).await?;
        }
        self.frame_pos = 0;
        self.scan = 0;
        Ok(self.buf.drain(..n).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_suffix() {
        assert_eq!(trailing_literal(b"* 1 FETCH (BODY[] {42}"), Some(42));
        assert_eq!(trailing_literal(b"a APPEND x {7+}"), Some(7));
        assert_eq!(trailing_literal(b"x {}"), None);
        assert_eq!(trailing_literal(b"x {a}"), None);
        assert_eq!(trailing_literal(b"x {9999999999999}"), None);
    }

    #[test]
    fn deflate_roundtrip() {
        let mut c = Compress::new(Compression::default(), false);
        let mut d = Decompress::new(false);
        let data: Vec<u8> = (0..200_000u32).flat_map(|i| (i % 251).to_le_bytes()).collect();
        let z = deflate(&mut c, &data).unwrap();
        let mut out = Vec::new();
        // En trozos, como llegarían por la red.
        for part in z.chunks(1000) {
            inflate(&mut d, part, &mut out, usize::MAX).unwrap();
        }
        assert_eq!(out, data);
        let z2 = deflate(&mut c, b"a1 OK hecho\r\n").unwrap();
        let mut out2 = Vec::new();
        inflate(&mut d, &z2, &mut out2, usize::MAX).unwrap();
        assert_eq!(out2, b"a1 OK hecho\r\n");
    }

    #[test]
    fn inflate_rejects_garbage_and_bombs() {
        let mut d = Decompress::new(false);
        let mut out = Vec::new();
        assert!(inflate(&mut d, &[0xff; 64], &mut out, 1 << 20).is_err());
        let mut c = Compress::new(Compression::best(), false);
        let z = deflate(&mut c, &vec![0u8; 4 << 20]).unwrap();
        let mut d = Decompress::new(false);
        let mut out = Vec::new();
        assert!(inflate(&mut d, &z, &mut out, 1 << 20).is_err());
    }
}
