//! Microsoft 365 Copilot Chat API (**experimental**: Microsoft Graph `beta`, documentación de
//! Microsoft Learn revisada el 2026-10-26).
//!
//! - `POST /beta/copilot/conversations` con `{}` → `copilotConversation` (`id`).
//! - `POST /beta/copilot/conversations/{id}/chat` con
//!   `{"message":{"text"}, "locationHint":{"timeZone"}, "contextualResources":{"webContext":{"isWebEnabled":false}}}`
//!   → `copilotConversation` con `messages[]` (el último es la respuesta: `text`, `attributions`).
//! - Hay `chatOverStream` (SSE con la conversación completa en cada evento), pero su forma aún
//!   cambia; aquí se usa la llamada síncrona y la respuesta se entrega como un único fragmento.
//!
//! Requisitos: cuenta profesional con licencia de Microsoft 365 Copilot y los permisos delegados
//! `Sites.Read.All, Mail.Read, People.Read.All, OnlineMeetingTranscript.Read.All, Chat.Read,
//! ChannelMessage.Read.All, ExternalItem.Read.All` (todos). El token es de
//! [`TokenAudience::Api`]. Como no hay forma barata de saber si el inquilino tiene licencia,
//! [`CopilotProvider::probe`] crea una conversación vacía: sólo si responde 2xx se ofrece.

use std::sync::Arc;

use iuredesk_model::account::Account;
use iuredesk_oauth::{TokenAudience, TokenProvider};
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::config::ProviderKind;
use crate::error::{AiError, Result};
use crate::http;
use crate::provider::{AiProvider, BoxFuture, ChatRequest, Completion, Limits, StreamEvent, TextSink};

/// Raíz de Graph `beta`.
pub const DEFAULT_BASE: &str = "https://graph.microsoft.com/beta";

/// Proveedor Copilot.
pub struct CopilotProvider {
    http: reqwest::Client,
    base: String,
    account: Account,
    tokens: Arc<dyn TokenProvider>,
    time_zone: String,
}

impl std::fmt::Debug for CopilotProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CopilotProvider")
            .field("base", &self.base)
            .field("account", &self.account.id)
            .finish_non_exhaustive()
    }
}

/// Resultado del sondeo de capacidad.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopilotCapability {
    /// El inquilino tiene Copilot y el token los permisos.
    Available,
    /// 401/403/404 o licencia ausente: no se ofrece en la interfaz.
    Unavailable(String),
}

impl CopilotProvider {
    pub fn new(account: Account, tokens: Arc<dyn TokenProvider>, base: &str, time_zone: &str) -> Result<Self> {
        let tz = if time_zone.trim().is_empty() { "UTC" } else { time_zone.trim() };
        Ok(Self {
            http: http::client()?,
            base: base.trim_end_matches('/').to_string(),
            account,
            tokens,
            time_zone: tz.to_string(),
        })
    }

    async fn post(&self, path: &str, body: &Value, max: usize) -> Result<Value> {
        let mut retried = false;
        loop {
            let token = self
                .tokens
                .access_token(&self.account, TokenAudience::Api)
                .await
                .map_err(|e| AiError::Auth(e.to_string()))?;
            let resp = self
                .http
                .post(format!("{}{path}", self.base))
                .bearer_auth(&token)
                .json(body)
                .send()
                .await
                .map_err(|e| AiError::from_reqwest(&e))?;
            if resp.status() == reqwest::StatusCode::UNAUTHORIZED && !retried {
                // El token pudo caducar entre medias: se olvida y se pide otro una vez.
                self.tokens.invalidate(&self.account.id);
                retried = true;
                continue;
            }
            if !resp.status().is_success() {
                return Err(http::status_error(resp).await);
            }
            let bytes = http::read_body(resp, max).await?;
            return serde_json::from_slice(&bytes).map_err(|e| AiError::Protocol(e.to_string()));
        }
    }

    async fn create_conversation(&self) -> Result<String> {
        let v = self.post("/copilot/conversations", &json!({}), 256 * 1024).await?;
        v.get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
            .map(str::to_string)
            .ok_or_else(|| AiError::Protocol("la conversación no trae id".into()))
    }

    /// ¿Tiene este inquilino Copilot Chat API? Crea una conversación vacía (no envía datos).
    pub async fn probe(&self) -> CopilotCapability {
        match self.create_conversation().await {
            Ok(_) => CopilotCapability::Available,
            Err(e) => CopilotCapability::Unavailable(e.to_string()),
        }
    }

    async fn run(
        &self,
        req: &ChatRequest,
        limits: &Limits,
        tx: Option<&UnboundedSender<StreamEvent>>,
    ) -> Result<Completion> {
        let id = match self.create_conversation().await {
            Ok(id) => id,
            Err(AiError::Auth(m) | AiError::Http { message: m, .. }) => {
                return Err(AiError::NotAvailable(format!("Microsoft 365 Copilot: {m}")));
            }
            Err(e) => return Err(e),
        };
        // Copilot no tiene instrucciones de sistema: van delante del mensaje.
        let text = if req.system.is_empty() {
            req.flatten_messages()
        } else {
            format!("{}\n\n{}", req.system, req.flatten_messages())
        };
        let body = json!({
            "message": {"text": text},
            "locationHint": {"timeZone": self.time_zone},
            "contextualResources": {"webContext": {"isWebEnabled": false}},
        });
        let v = self.post(&format!("/copilot/conversations/{id}/chat"), &body, limits.max_response_bytes).await?;
        let answer = v
            .get("messages")
            .and_then(Value::as_array)
            .and_then(|m| m.iter().rev().find_map(|x| x.get("text").and_then(Value::as_str).filter(|t| *t != text)))
            .unwrap_or_default()
            .trim()
            .to_string();
        if answer.is_empty() {
            return Err(AiError::InvalidOutput("Copilot no devolvió respuesta".into()));
        }
        let mut sink = TextSink::default();
        sink.push(&answer, limits, tx)?;
        Ok(Completion { text: sink.text, stop_reason: Some("end_turn".into()), usage: None })
    }
}

impl AiProvider for CopilotProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Copilot
    }

    fn label(&self) -> String {
        format!("Microsoft 365 Copilot · {}", self.account.email)
    }

    /// Los datos se quedan en el inquilino del usuario (límite de servicio de Microsoft 365).
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
