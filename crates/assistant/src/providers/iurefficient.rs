//! Proveedor Iurefficient: la IA de la instancia con la sesión compartida de la suite.
//!
//! Usa `iurefficient_connect::chat` (v0.9): la pregunta se encola en la instancia y la
//! respuesta llega **en streaming** (SSE), fragmento a fragmento, a `tx`. Al terminar se
//! devuelve la versión definitiva que guarda la instancia (post-procesada: avisos de
//! validación), que puede diferir un poco de la suma de los fragmentos.
//!
//! - Sin caso: chat global (`system_prompt` aparte).
//! - Con caso (`ChatRequest::case_id`): chat del caso (contexto RAG de sus documentos); las
//!   instrucciones van delante del mensaje.
//!
//! **Cada llamada usa una conversación propia**, creada y borrada aquí: sin `conversation_id`
//! la instancia tomaría como historial TODO el chat global del usuario (o todo el del caso,
//! de todos sus miembros). La petición ya trae su propio historial (`flatten_messages`).

use std::sync::Arc;

use iurefficient_connect::chat::{self, Scope};
use iurefficient_connect::rest::{ApiError, Session};
use tokio::sync::mpsc::UnboundedSender;

use crate::config::ProviderKind;
use crate::error::{AiError, Result};
use crate::provider::{AiProvider, BoxFuture, ChatRequest, Completion, Limits, StreamEvent, TextSink};

/// Título de las conversaciones (efímeras) que crea IureDesk en la instancia.
const CONVERSATION_TITLE: &str = "IureDesk";

/// Proveedor Iurefficient.
pub struct IurefficientProvider {
    sess: Arc<Session>,
}

impl std::fmt::Debug for IurefficientProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IurefficientProvider").field("host", &self.sess.account().host()).finish()
    }
}

/// Traduce un error de connect a [`AiError`]: por el [`ApiError`] de la instancia si lo
/// trae (cuota, términos, estado HTTP) y, si no, por el texto («<estado>: <mensaje>»).
pub(crate) fn map_connect_error(e: &anyhow::Error) -> AiError {
    if let Some(api) = e.downcast_ref::<ApiError>() {
        if api.is_quota_exceeded() {
            return AiError::Quota(api.message.clone());
        }
        if api.is_terms_required() {
            return terms_error();
        }
    }
    let m = format!("{e:#}");
    let lower = m.to_lowercase();
    if lower.contains("quota_exceeded") {
        AiError::Quota(m)
    } else if lower.contains("terms_acceptance_required") {
        terms_error()
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

fn terms_error() -> AiError {
    AiError::NotAvailable(
        "abre Iurefficient en el navegador y acepta los términos vigentes / open Iurefficient in the browser and accept the current terms"
            .into(),
    )
}

/// Borra la conversación al soltarse (cancelación, tiempo agotado o error), en segundo
/// plano; en el camino normal se borra en línea y se desarma.
struct ConversationGuard {
    sess: Arc<Session>,
    case_id: Option<String>,
    id: Option<String>,
}

impl Drop for ConversationGuard {
    fn drop(&mut self) {
        let Some(id) = self.id.take() else { return };
        let Ok(rt) = tokio::runtime::Handle::try_current() else { return };
        let (sess, case_id) = (self.sess.clone(), self.case_id.take());
        rt.spawn(async move {
            let scope = case_id.as_deref().map_or(Scope::Global, Scope::Case);
            chat::delete_conversation(&sess, scope, &id).await.ok();
        });
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
        let case = req.case_id.as_deref().map(str::trim).filter(|c| !c.is_empty());
        if let Some(case) = case
            && !case.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(AiError::Action(format!("identificador de caso no válido: {case}")));
        }
        let scope = case.map_or(Scope::Global, Scope::Case);
        // El chat del caso no admite `system_prompt`: las instrucciones van delante.
        let (message, system) = match case {
            Some(_) if !req.system.is_empty() => (format!("{}\n\n{message}", req.system), None),
            Some(_) => (message, None),
            None => (message, Some(req.system.as_str()).filter(|s| !s.trim().is_empty())),
        };

        let conversation =
            chat::new_conversation(&self.sess, scope, CONVERSATION_TITLE).await.map_err(|e| map_connect_error(&e))?;
        let mut guard = ConversationGuard {
            sess: self.sess.clone(),
            case_id: case.map(str::to_string),
            id: Some(conversation.clone()),
        };

        let mut sink = TextSink::default();
        let mut overflow: Option<AiError> = None;
        let request = chat::Request {
            message: &message,
            system_prompt: system,
            conversation_id: Some(&conversation),
            ..Default::default()
        };
        let answer = chat::ask_streaming(&self.sess, scope, &request, |delta| {
            if overflow.is_none()
                && let Err(e) = sink.push(delta, limits, tx)
            {
                overflow = Some(e);
            }
        })
        .await;

        // Camino normal: se borra en línea (y el guardián ya no hace nada).
        guard.id = None;
        chat::delete_conversation(&self.sess, scope, &conversation).await.ok();

        if let Some(e) = overflow {
            return Err(e);
        }
        let answer = answer.map_err(|e| map_connect_error(&e))?;
        if answer.content.len() > limits.max_response_bytes {
            return Err(AiError::TooLarge(format!("respuesta de más de {} bytes", limits.max_response_bytes)));
        }
        // Sin fragmentos (instancia sin Redis para el flujo): se entrega entera de una vez.
        if sink.text.is_empty() {
            sink.push(&answer.content, limits, tx)?;
        }
        Ok(Completion { text: answer.content, stop_reason: Some("end_turn".into()), usage: None })
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
    use anyhow::anyhow;

    #[test]
    fn connect_errors_are_classified() {
        let api = |status, code: Option<&str>, msg: &str| anyhow::Error::new(ApiError::new(status, code, msg));
        assert!(
            matches!(map_connect_error(&api(403, Some("quota_exceeded"), "Límite de IA")), AiError::Quota(m) if m == "Límite de IA")
        );
        assert!(matches!(
            map_connect_error(&api(403, Some("terms_acceptance_required"), "x")),
            AiError::NotAvailable(_)
        ));
        assert!(matches!(map_connect_error(&api(403, None, "prohibido")), AiError::Auth(_)));
        assert!(matches!(map_connect_error(&anyhow!("403 Forbidden: quota_exceeded")), AiError::Quota(_)));
        assert!(matches!(map_connect_error(&anyhow!("429 Too Many Requests: slow")), AiError::RateLimited { .. }));
        assert!(matches!(map_connect_error(&anyhow!("401 Unauthorized: x")), AiError::Auth(_)));
        assert!(matches!(
            map_connect_error(&anyhow!("500 Internal Server Error: boom")),
            AiError::Http { status: 500, .. }
        ));
        assert!(matches!(map_connect_error(&anyhow!("No se pudo conectar con https://x")), AiError::Network(_)));
    }
}
