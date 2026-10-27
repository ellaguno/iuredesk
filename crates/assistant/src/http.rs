//! HTTP común a los proveedores: cliente con tiempos, errores por estado, `Retry-After`, lectura
//! acotada del cuerpo y un lector SSE (`text/event-stream`) con topes.

use std::time::Duration;

use reqwest::StatusCode;
use reqwest::header::HeaderMap;
use serde_json::Value;

use crate::error::{AiError, Result};

/// Tope de una línea/evento SSE.
const MAX_EVENT: usize = 1024 * 1024;

/// Cliente HTTP con tiempo de conexión acotado (el total lo pone `run_chat`).
pub(crate) fn client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .user_agent(concat!("IureDesk/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| AiError::Config(e.to_string()))
}

/// `Retry-After` en segundos (la forma de fecha HTTP se ignora: se usa la espera exponencial).
pub(crate) fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let v = headers.get("retry-after")?.to_str().ok()?.trim();
    if let Ok(secs) = v.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    v.parse::<f64>().ok().filter(|s| s.is_finite() && *s >= 0.0 && *s < 86_400.0).map(Duration::from_secs_f64)
}

/// Lee el cuerpo entero con tope.
pub(crate) async fn read_body(mut resp: reqwest::Response, max: usize) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| AiError::from_reqwest(&e))? {
        if out.len().saturating_add(chunk.len()) > max {
            return Err(AiError::TooLarge(format!("respuesta de más de {max} bytes")));
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

/// Mensaje de error del cuerpo (`{"error":{"message"}}`, `{"error":"…"}`, `{"message"}`) o el
/// texto recortado.
pub(crate) fn error_message(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    if let Ok(v) = serde_json::from_str::<Value>(&text) {
        let candidates = [
            v.pointer("/error/message"),
            v.get("error").filter(|e| e.is_string()),
            v.get("message"),
            v.pointer("/error/code"),
        ];
        if let Some(m) = candidates.into_iter().flatten().find_map(Value::as_str) {
            return m.chars().take(300).collect();
        }
    }
    text.chars().take(300).collect()
}

/// Convierte una respuesta no exitosa en error (lee el cuerpo con tope pequeño).
pub(crate) async fn status_error(resp: reqwest::Response) -> AiError {
    let status = resp.status();
    let wait = retry_after(resp.headers());
    let body = read_body(resp, 64 * 1024).await.unwrap_or_default();
    let message = error_message(&body);
    classify(status, wait, message)
}

pub(crate) fn classify(status: StatusCode, wait: Option<Duration>, message: String) -> AiError {
    let lower = message.to_lowercase();
    if lower.contains("quota_exceeded") || lower.contains("insufficient_quota") {
        return AiError::Quota(message);
    }
    match status.as_u16() {
        401 | 403 => AiError::Auth(message),
        429 | 503 | 529 => AiError::RateLimited { retry_after: wait },
        s => AiError::Http { status: s, message },
    }
}

/// Un evento SSE.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

/// Lector incremental de `text/event-stream` (sólo `event:` y `data:`; comentarios fuera).
#[derive(Debug, Default)]
pub(crate) struct SseParser {
    buf: Vec<u8>,
    current: SseEvent,
    has_data: bool,
}

impl SseParser {
    /// Añade bytes y devuelve los eventos completos.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<SseEvent>> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some(pos) = self.buf.iter().position(|b| *b == b'\n') {
            let mut line: Vec<u8> = self.buf.drain(..=pos).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            self.line(&line, &mut out)?;
        }
        if self.buf.len() > MAX_EVENT {
            return Err(AiError::Protocol("línea SSE demasiado larga".into()));
        }
        Ok(out)
    }

    /// Fin del flujo: entrega el último evento si quedó sin línea en blanco.
    pub fn finish(&mut self) -> Result<Vec<SseEvent>> {
        let mut out = Vec::new();
        if !self.buf.is_empty() {
            let line = std::mem::take(&mut self.buf);
            self.line(&line, &mut out)?;
        }
        self.dispatch(&mut out);
        Ok(out)
    }

    fn dispatch(&mut self, out: &mut Vec<SseEvent>) {
        if self.has_data {
            out.push(std::mem::take(&mut self.current));
        }
        self.current = SseEvent::default();
        self.has_data = false;
    }

    fn line(&mut self, line: &[u8], out: &mut Vec<SseEvent>) -> Result<()> {
        if line.is_empty() {
            self.dispatch(out);
            return Ok(());
        }
        let text = String::from_utf8_lossy(line);
        if text.starts_with(':') {
            return Ok(());
        }
        let (field, value) = match text.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (text.as_ref(), ""),
        };
        match field {
            "event" => self.current.event = Some(value.to_string()),
            "data" => {
                if self.has_data {
                    self.current.data.push('\n');
                }
                self.current.data.push_str(value);
                self.has_data = true;
                if self.current.data.len() > MAX_EVENT {
                    return Err(AiError::Protocol("evento SSE demasiado grande".into()));
                }
            }
            _ => {}
        }
        Ok(())
    }
}

/// Recorre un cuerpo SSE llamando a `on_event` por cada evento; `on_event` devuelve `false` para
/// terminar antes (p. ej. `[DONE]`). El cuerpo total está acotado a `max` bytes.
pub(crate) async fn for_each_sse(
    mut resp: reqwest::Response,
    max: usize,
    mut on_event: impl FnMut(SseEvent) -> Result<bool>,
) -> Result<()> {
    let mut parser = SseParser::default();
    let mut total = 0usize;
    while let Some(chunk) = resp.chunk().await.map_err(|e| AiError::from_reqwest(&e))? {
        total = total.saturating_add(chunk.len());
        if total > max.saturating_mul(4) {
            return Err(AiError::TooLarge(format!("flujo de más de {} bytes", max.saturating_mul(4))));
        }
        for ev in parser.feed(&chunk)? {
            if !on_event(ev)? {
                return Ok(());
            }
        }
    }
    for ev in parser.finish()? {
        if !on_event(ev)? {
            return Ok(());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_events_split_across_chunks() {
        let mut p = SseParser::default();
        let mut evs = p.feed(b"event: a\r\ndata: {\"x\"").unwrap();
        assert!(evs.is_empty());
        evs.extend(p.feed(b":1}\r\n\r\n: comentario\n\ndata: uno\ndata: dos\n\ndata: fin").unwrap());
        evs.extend(p.finish().unwrap());
        assert_eq!(
            evs,
            vec![
                SseEvent { event: Some("a".into()), data: "{\"x\":1}".into() },
                SseEvent { event: None, data: "uno\ndos".into() },
                SseEvent { event: None, data: "fin".into() },
            ]
        );
    }

    #[test]
    fn sse_line_too_long_is_an_error() {
        let mut p = SseParser::default();
        let big = vec![b'a'; MAX_EVENT + 10];
        assert!(p.feed(&big).is_err());
    }

    #[test]
    fn retry_after_forms() {
        let mut h = HeaderMap::new();
        assert_eq!(retry_after(&h), None);
        h.insert("retry-after", "3".parse().unwrap());
        assert_eq!(retry_after(&h), Some(Duration::from_secs(3)));
        h.insert("retry-after", "0.5".parse().unwrap());
        assert_eq!(retry_after(&h), Some(Duration::from_millis(500)));
        h.insert("retry-after", "Wed, 21 Oct 2026 07:28:00 GMT".parse().unwrap());
        assert_eq!(retry_after(&h), None);
        h.insert("retry-after", "-4".parse().unwrap());
        assert_eq!(retry_after(&h), None);
    }

    #[test]
    fn error_messages() {
        assert_eq!(error_message(br#"{"error":{"message":"bad key","type":"x"}}"#), "bad key");
        assert_eq!(error_message(br#"{"error":"quota_exceeded"}"#), "quota_exceeded");
        assert_eq!(error_message(b"plain"), "plain");
        assert!(matches!(classify(StatusCode::FORBIDDEN, None, "quota_exceeded".into()), AiError::Quota(_)));
        assert!(matches!(
            classify(StatusCode::from_u16(529).unwrap(), None, String::new()),
            AiError::RateLimited { .. }
        ));
        assert!(matches!(classify(StatusCode::UNAUTHORIZED, None, String::new()), AiError::Auth(_)));
    }
}
