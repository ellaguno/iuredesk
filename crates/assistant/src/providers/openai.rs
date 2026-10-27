//! API compatible con OpenAI: `POST {base}/chat/completions` con `stream: true` (OpenAI,
//! OpenRouter, Ollama, llama.cpp `server`). Clave opcional (`Authorization: Bearer`).
//!
//! Eventos: `data: {"choices":[{"delta":{"content":"…"},"finish_reason":null}]}` y `data: [DONE]`.
//! Algunos servidores mandan un `data: {"error":{…}}` dentro del flujo.

use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::config::{ProviderKind, is_local_url};
use crate::error::{AiError, Result};
use crate::http;
use crate::provider::{AiProvider, BoxFuture, ChatRequest, Completion, Limits, Role, StreamEvent, TextSink, Usage};

/// Proveedor compatible con OpenAI.
pub struct OpenAiProvider {
    http: reqwest::Client,
    base: String,
    model: String,
    key: Option<String>,
}

impl std::fmt::Debug for OpenAiProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiProvider").field("base", &self.base).field("model", &self.model).finish_non_exhaustive()
    }
}

impl OpenAiProvider {
    /// `base` incluye `/v1` (`https://openrouter.ai/api/v1`, `http://localhost:11434/v1`).
    pub fn new(base: &str, model: &str, key: Option<String>) -> Result<Self> {
        Ok(Self {
            http: http::client()?,
            base: base.trim_end_matches('/').to_string(),
            model: model.to_string(),
            key: key.filter(|k| !k.trim().is_empty()),
        })
    }

    fn body(&self, req: &ChatRequest) -> Value {
        let mut messages = Vec::with_capacity(req.messages.len().saturating_add(1));
        if !req.system.is_empty() {
            messages.push(json!({"role": "system", "content": req.system}));
        }
        for m in &req.messages {
            let role = match m.role {
                Role::User => "user",
                Role::Assistant => "assistant",
            };
            messages.push(json!({"role": role, "content": m.content}));
        }
        json!({
            "model": self.model,
            "messages": messages,
            "max_tokens": req.max_output_tokens.max(1),
            "temperature": req.temperature,
            "stream": true,
            "stream_options": {"include_usage": true},
        })
    }

    async fn run(
        &self,
        req: &ChatRequest,
        limits: &Limits,
        tx: Option<&UnboundedSender<StreamEvent>>,
    ) -> Result<Completion> {
        let mut rb = self
            .http
            .post(format!("{}/chat/completions", self.base))
            .header("accept", "text/event-stream")
            .json(&self.body(req));
        if let Some(k) = &self.key {
            rb = rb.bearer_auth(k);
        }
        let resp = rb.send().await.map_err(|e| AiError::from_reqwest(&e))?;
        if !resp.status().is_success() {
            return Err(http::status_error(resp).await);
        }
        let mut sink = TextSink::default();
        let mut stop_reason = None;
        let mut usage = None;
        let mut done = false;
        http::for_each_sse(resp, limits.max_response_bytes, |ev| {
            let data = ev.data.trim();
            if data == "[DONE]" {
                done = true;
                return Ok(false);
            }
            if data.is_empty() {
                return Ok(true);
            }
            let Ok(v) = serde_json::from_str::<Value>(data) else {
                return Err(AiError::Protocol(format!(
                    "evento no es JSON: {}",
                    data.chars().take(80).collect::<String>()
                )));
            };
            if let Some(err) = v.get("error") {
                let msg = err.get("message").and_then(Value::as_str).unwrap_or("error").to_string();
                return Err(http::classify(
                    reqwest::StatusCode::from_u16(
                        u16::try_from(err.get("code").and_then(Value::as_u64).unwrap_or(500)).unwrap_or(500),
                    )
                    .unwrap_or(reqwest::StatusCode::INTERNAL_SERVER_ERROR),
                    None,
                    msg,
                ));
            }
            if let Some(choice) = v.pointer("/choices/0") {
                if let Some(t) = choice.pointer("/delta/content").and_then(Value::as_str) {
                    sink.push(t, limits, tx)?;
                }
                if let Some(r) = choice.get("finish_reason").and_then(Value::as_str) {
                    stop_reason = Some(r.to_string());
                }
            }
            if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
                usage = Some(Usage {
                    input_tokens: u.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0),
                    output_tokens: u.get("completion_tokens").and_then(Value::as_u64).unwrap_or(0),
                });
            }
            Ok(true)
        })
        .await?;
        // Algunos servidores locales cierran sin `[DONE]` pero con `finish_reason`.
        if !done && stop_reason.is_none() {
            return Err(AiError::Protocol("el flujo terminó sin [DONE]".into()));
        }
        Ok(Completion { text: sink.text, stop_reason, usage })
    }
}

impl AiProvider for OpenAiProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::OpenAiCompatible { base_url: self.base.clone(), model: self.model.clone() }
    }

    fn label(&self) -> String {
        self.kind().label()
    }

    fn is_external(&self) -> bool {
        !is_local_url(&self.base)
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
