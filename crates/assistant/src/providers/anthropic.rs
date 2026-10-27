//! Anthropic Messages API (`POST /v1/messages`, `stream: true`).
//!
//! Eventos SSE: `message_start` (uso de entrada), `content_block_delta` con `text_delta`,
//! `message_delta` (`stop_reason`, uso de salida), `message_stop`, `ping` y `error`.

use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::config::{ProviderKind, is_local_url};
use crate::error::{AiError, Result};
use crate::http;
use crate::provider::{AiProvider, BoxFuture, ChatRequest, Completion, Limits, Role, StreamEvent, TextSink, Usage};

/// Raíz de la API.
pub const DEFAULT_BASE: &str = "https://api.anthropic.com";
/// Versión de la API.
const API_VERSION: &str = "2023-06-01";

/// Proveedor Anthropic.
pub struct AnthropicProvider {
    http: reqwest::Client,
    base: String,
    model: String,
    key: String,
}

impl std::fmt::Debug for AnthropicProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnthropicProvider")
            .field("base", &self.base)
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

impl AnthropicProvider {
    pub fn new(base: &str, model: &str, key: String) -> Result<Self> {
        Ok(Self { http: http::client()?, base: base.trim_end_matches('/').to_string(), model: model.to_string(), key })
    }

    fn body(&self, req: &ChatRequest) -> Value {
        let messages: Vec<Value> = req
            .messages
            .iter()
            .map(|m| {
                json!({
                    "role": match m.role { Role::User => "user", Role::Assistant => "assistant" },
                    "content": m.content,
                })
            })
            .collect();
        let mut body = json!({
            "model": self.model,
            "max_tokens": req.max_output_tokens.max(1),
            "messages": messages,
            "stream": true,
        });
        if !req.system.is_empty() {
            body["system"] = json!(req.system);
        }
        if req.temperature > 0.0 {
            body["temperature"] = json!(req.temperature);
        }
        body
    }

    async fn run(
        &self,
        req: &ChatRequest,
        limits: &Limits,
        tx: Option<&UnboundedSender<StreamEvent>>,
    ) -> Result<Completion> {
        let resp = self
            .http
            .post(format!("{}/v1/messages", self.base))
            .header("x-api-key", &self.key)
            .header("anthropic-version", API_VERSION)
            .header("accept", "text/event-stream")
            .json(&self.body(req))
            .send()
            .await
            .map_err(|e| AiError::from_reqwest(&e))?;
        if !resp.status().is_success() {
            return Err(http::status_error(resp).await);
        }
        let mut sink = TextSink::default();
        let mut usage = Usage::default();
        let mut stop_reason = None;
        let mut finished = false;
        http::for_each_sse(resp, limits.max_response_bytes, |ev| {
            let Ok(v) = serde_json::from_str::<Value>(&ev.data) else {
                return Err(AiError::Protocol(format!(
                    "evento no es JSON: {}",
                    ev.data.chars().take(80).collect::<String>()
                )));
            };
            match v.get("type").and_then(Value::as_str).unwrap_or_default() {
                "message_start" => {
                    usage.input_tokens = v.pointer("/message/usage/input_tokens").and_then(Value::as_u64).unwrap_or(0);
                }
                "content_block_delta" => {
                    if v.pointer("/delta/type").and_then(Value::as_str) == Some("text_delta") {
                        let t = v.pointer("/delta/text").and_then(Value::as_str).unwrap_or_default();
                        sink.push(t, limits, tx)?;
                    }
                }
                "message_delta" => {
                    if let Some(r) = v.pointer("/delta/stop_reason").and_then(Value::as_str) {
                        stop_reason = Some(r.to_string());
                    }
                    if let Some(o) = v.pointer("/usage/output_tokens").and_then(Value::as_u64) {
                        usage.output_tokens = o;
                    }
                }
                "message_stop" => {
                    finished = true;
                    return Ok(false);
                }
                "error" => {
                    let kind = v.pointer("/error/type").and_then(Value::as_str).unwrap_or_default();
                    let msg = v.pointer("/error/message").and_then(Value::as_str).unwrap_or_default().to_string();
                    return Err(match kind {
                        "overloaded_error" | "rate_limit_error" if sink.text.is_empty() => {
                            AiError::RateLimited { retry_after: None }
                        }
                        _ => AiError::Http { status: 500, message: msg },
                    });
                }
                _ => {}
            }
            Ok(true)
        })
        .await?;
        if !finished {
            return Err(AiError::Protocol("el flujo terminó sin message_stop".into()));
        }
        Ok(Completion { text: sink.text, stop_reason, usage: Some(usage) })
    }
}

impl AiProvider for AnthropicProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Anthropic { model: self.model.clone() }
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
