//! Envío por Microsoft Graph (`POST /me/sendMail` con el MIME en base64). Graph guarda la copia
//! en «Elementos enviados» por su cuenta (equivale a `saveToSentItems: true`), así que la bandeja
//! de salida no encola `AppendToSent`.
//!
//! El mensaje que construye el redactor no lleva `Bcc:` (va solo en el sobre SMTP); Graph toma los
//! destinatarios de las cabeceras del MIME, así que aquí se añade un `Bcc:` con los destinatarios
//! del sobre que no estén en `To:`/`Cc:` (Graph lo quita de las copias entregadas).
//!
//! Graph no admite peticiones de más de ~4 MB: un mensaje mayor se manda por el transporte de
//! reserva (SMTP de la cuenta) si lo hay, o se informa como demasiado grande.

use std::sync::Arc;

use base64::Engine as _;
use iuredesk_compose::{OutgoingMessage, SendError, SendErrorKind, SendReceipt, Transport};
use iuredesk_oauth::BoxFuture;
use reqwest::Method;

use crate::{Body, GraphClient, GraphError};

/// MIME máximo (antes de base64) que se manda por Graph: 4 MB de petición con base64 (×4/3).
pub const MAX_MIME_BYTES: usize = 2_900_000;
/// Destinatarios máximos que se añaden en `Bcc:`.
const MAX_BCC: usize = 500;

/// Transporte Graph `sendMail`.
pub struct GraphSendTransport {
    client: GraphClient,
    fallback: Option<Arc<dyn Transport>>,
}

impl std::fmt::Debug for GraphSendTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GraphSendTransport").field("client", &self.client).finish_non_exhaustive()
    }
}

impl GraphSendTransport {
    /// Transporte sobre un cliente de Graph de la cuenta.
    pub fn new(client: GraphClient) -> Self {
        Self { client, fallback: None }
    }

    /// Transporte de reserva para mensajes que Graph no acepta por tamaño.
    pub fn with_fallback(mut self, t: Arc<dyn Transport>) -> Self {
        self.fallback = Some(t);
        self
    }
}

/// Direcciones de una cabecera de lista, en minúsculas.
fn header_addrs(m: &mail_parser::Message<'_>) -> Vec<String> {
    let mut out = Vec::new();
    for a in [m.to(), m.cc()].into_iter().flatten() {
        for x in a.iter() {
            if let Some(e) = x.address() {
                out.push(e.to_ascii_lowercase());
            }
        }
    }
    out
}

/// Añade `Bcc:` con los destinatarios del sobre que no están en `To:`/`Cc:`.
pub fn with_bcc_header(raw: &[u8], recipients: &[String]) -> Vec<u8> {
    let visible =
        mail_parser::MessageParser::default().parse_headers(raw).map(|m| header_addrs(&m)).unwrap_or_default();
    let hidden: Vec<&str> = recipients
        .iter()
        .map(String::as_str)
        .filter(|r| {
            !r.is_empty()
                && !r.bytes().any(|b| matches!(b, b'\r' | b'\n' | b',' | b'<' | b'>'))
                && !visible.contains(&r.to_ascii_lowercase())
        })
        .take(MAX_BCC)
        .collect();
    if hidden.is_empty() {
        return raw.to_vec();
    }
    let mut out = format!("Bcc: {}\r\n", hidden.join(",\r\n ")).into_bytes();
    out.extend_from_slice(raw);
    out
}

fn map_error(e: &GraphError) -> SendError {
    let kind = match e {
        GraphError::Network(_) => SendErrorKind::Network,
        GraphError::Throttled { .. } => SendErrorKind::TemporarilyRejected,
        GraphError::Unauthorized => SendErrorKind::Auth,
        GraphError::OAuth(o) if o.needs_reconsent() => SendErrorKind::NeedsReconsent,
        GraphError::OAuth(iuredesk_oauth::Error::Http(_)) => SendErrorKind::Network,
        GraphError::OAuth(_) => SendErrorKind::Auth,
        GraphError::TooLarge => SendErrorKind::TooLarge,
        GraphError::Invalid(_) => SendErrorKind::Rejected,
        GraphError::Http { status: 413, .. } => SendErrorKind::TooLarge,
        GraphError::Http { status: 403, code } if code.contains("SendAs") => SendErrorKind::Rejected,
        GraphError::Http { status: 403, .. } => SendErrorKind::NeedsReconsent,
        GraphError::Http { status, .. } if *status >= 500 => SendErrorKind::TemporarilyRejected,
        GraphError::Http { .. } => SendErrorKind::Rejected,
    };
    let mut err = SendError::new(kind, e.to_string());
    err.code = e.status();
    err.host = "Microsoft 365".to_string();
    err
}

impl Transport for GraphSendTransport {
    fn send<'a>(&'a self, msg: &'a OutgoingMessage) -> BoxFuture<'a, Result<SendReceipt, SendError>> {
        Box::pin(async move {
            if msg.raw.len() > MAX_MIME_BYTES {
                if let Some(f) = &self.fallback {
                    return f.send(msg).await;
                }
                let mut e = SendError::new(SendErrorKind::TooLarge, format!("{} bytes", msg.raw.len()));
                e.host = "Microsoft 365".to_string();
                return Err(e);
            }
            let mime = with_bcc_header(&msg.raw, &msg.recipients);
            let b64 = base64::engine::general_purpose::STANDARD.encode(&mime);
            let url = self.client.url("/me/sendMail");
            let body = Body::Raw { content_type: "text/plain", data: b64.as_bytes() };
            let r = self.client.request(Method::POST, &url, Some(body), &[]).await.map_err(|e| map_error(&e))?;
            if !r.is_success() {
                return Err(map_error(&r.error()));
            }
            Ok(SendReceipt { server_reply: format!("{} Accepted (Microsoft Graph sendMail)", r.status) })
        })
    }

    fn saves_to_sent(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_only_hidden_recipients() {
        let raw = b"From: a@x.com\r\nTo: B <b@x.com>\r\nCc: c@x.com\r\nSubject: s\r\n\r\nhola\r\n";
        let out = with_bcc_header(raw, &["b@x.com".into(), "C@X.com".into(), "d@x.com".into(), "e@x.com".into()]);
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("Bcc: d@x.com,\r\n e@x.com\r\n"));
        assert!(s.ends_with("hola\r\n"));
        // Sin ocultos, tal cual; con inyección, se descarta.
        assert_eq!(with_bcc_header(raw, &["b@x.com".into()]), raw.to_vec());
        assert_eq!(with_bcc_header(raw, &["x@y.com\r\nX-Evil: 1".into()]), raw.to_vec());
    }

    #[test]
    fn errors_are_human() {
        assert_eq!(map_error(&GraphError::Throttled { retry_after: Some(5) }).kind, SendErrorKind::TemporarilyRejected);
        assert_eq!(map_error(&GraphError::Http { status: 413, code: String::new() }).kind, SendErrorKind::TooLarge);
        assert_eq!(map_error(&GraphError::Unauthorized).kind, SendErrorKind::Auth);
        assert!(map_error(&GraphError::Network("x".into())).is_transient());
    }
}
