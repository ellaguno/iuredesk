//! Proveedor Iurefficient: la IA de la instancia con la sesión compartida de la suite.
//!
//! - Sin caso: `POST /api/chat/global/chat` (`message`, `system_prompt`, `document_ids`), igual
//!   que `iurefficient_connect::api::global_chat` (v0.7.1), pero llamado con `Session::post_json`
//!   para distinguir cuota, términos, 429 y sesión caducada.
//! - Con caso (`ChatRequest::case_id`): `POST /api/chat/cases/<id>/chat` (contexto RAG del caso).
//!
//! No hay flujo: la respuesta llega entera y se entrega como un único fragmento. (La variante
//! asíncrona `/global/chat/async` + SSE queda pendiente hasta que connect la exponga.)

use std::sync::Arc;

use iurefficient_connect::rest::Session;
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::config::ProviderKind;
use crate::error::{AiError, Result};
use crate::provider::{AiProvider, BoxFuture, ChatRequest, Completion, Limits, StreamEvent, TextSink};

/// Proveedor Iurefficient.
pub struct IurefficientProvider {
    sess: Arc<Session>,
}

impl std::fmt::Debug for IurefficientProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IurefficientProvider").field("host", &self.sess.account().host()).finish()
    }
}

/// Traduce un error de connect (`anyhow`, «<estado>: <mensaje>») a [`AiError`].
pub(crate) fn map_connect_error(e: &dyn std::fmt::Display) -> AiError {
    let m = e.to_string();
    let lower = m.to_lowercase();
    if lower.contains("quota_exceeded") {
        AiError::Quota(m)
    } else if lower.contains("terms_acceptance_required") {
        AiError::NotAvailable(
            "abre Iurefficient en el navegador y acepta los términos vigentes / open Iurefficient in the browser and accept the current terms"
                .into(),
        )
    } else if lower.starts_with("429") {
        AiError::RateLimited { retry_after: None }
    } else if lower.starts_with("401") || lower.starts_with("403") || lower.contains("sesión") {
        AiError::Auth(m)
    } else if let Some(code) =
        m.split(':').next().and_then(|c| c.split_whitespace().next()).and_then(|c| c.parse::<u16>().ok())
    {
        AiError::Http { status: code, message: m.chars().take(300).collect() }
    } else {
        AiError::Network(m)
    }
}

impl IurefficientProvider {
    pub fn new(sess: Arc<Session>) -> Self {
        Self { sess }
    }

    async fn run(
        &self,
        req: &ChatRequest,
        limits: &Limits,
        tx: Option<&UnboundedSender<StreamEvent>>,
    ) -> Result<Completion> {
        let message = req.flatten_messages();
        let (path, body) = match req.case_id.as_deref().filter(|c| !c.trim().is_empty()) {
            Some(case) => {
                if !case.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
                    return Err(AiError::Action(format!("identificador de caso no válido: {case}")));
                }
                // El chat del caso no admite `system_prompt`: las instrucciones van delante.
                let msg = if req.system.is_empty() { message } else { format!("{}\n\n{message}", req.system) };
                (format!("/api/chat/cases/{case}/chat"), json!({"message": msg}))
            }
            None => {
                let mut b = json!({"message": message, "document_ids": [], "enable_url_extraction": false});
                if !req.system.trim().is_empty() {
                    b["system_prompt"] = json!(req.system);
                }
                ("/api/chat/global/chat".to_string(), b)
            }
        };
        let v = self.sess.post_json(&path, &body).await.map_err(|e| map_connect_error(&e))?;
        let content = v
            .pointer("/assistant_message/content")
            .or_else(|| v.get("response"))
            .or_else(|| v.get("content"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();
        if content.is_empty() {
            return Err(AiError::InvalidOutput("la instancia no devolvió respuesta".into()));
        }
        let mut sink = TextSink::default();
        sink.push(&content, limits, tx)?;
        Ok(Completion { text: sink.text, stop_reason: Some("end_turn".into()), usage: None })
    }
}

impl AiProvider for IurefficientProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Iurefficient
    }

    fn label(&self) -> String {
        format!("Iurefficient · {}", self.sess.account().host())
    }

    fn is_external(&self) -> bool {
        false
    }

    fn chat<'a>(
        &'a self,
        req: &'a ChatRequest,
        limits: &'a Limits,
        tx: Option<&'a UnboundedSender<StreamEvent>>,
    ) -> BoxFuture<'a, Result<Completion>> {
        Box::pin(self.run(req, limits, tx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_errors_are_classified() {
        assert!(matches!(map_connect_error(&"403 Forbidden: quota_exceeded"), AiError::Quota(_)));
        assert!(matches!(map_connect_error(&"403 Forbidden: terms_acceptance_required"), AiError::NotAvailable(_)));
        assert!(matches!(map_connect_error(&"429 Too Many Requests: slow"), AiError::RateLimited { .. }));
        assert!(matches!(map_connect_error(&"401 Unauthorized: x"), AiError::Auth(_)));
        assert!(matches!(map_connect_error(&"500 Internal Server Error: boom"), AiError::Http { status: 500, .. }));
        assert!(matches!(map_connect_error(&"No se pudo conectar con https://x"), AiError::Network(_)));
    }
}
