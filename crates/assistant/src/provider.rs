//! El rasgo [`AiProvider`] y la ejecución acotada de una conversación ([`run_chat`]): tiempo
//! máximo, cancelación, topes de tamaño y reintentos ante 429 con `Retry-After`.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::config::ProviderKind;
use crate::error::{AiError, Result};

/// Futuro en caja (sin `async-trait`).
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Quién habla en un mensaje.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

/// Mensaje de la conversación.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

impl ChatMessage {
    pub fn user(content: impl Into<String>) -> Self {
        Self { role: Role::User, content: content.into() }
    }
}

/// Petición a un proveedor: instrucciones de sistema + mensajes.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ChatRequest {
    pub system: String,
    pub messages: Vec<ChatMessage>,
    /// Tope de tokens de salida que se le pide al modelo.
    pub max_output_tokens: u32,
    /// Temperatura (0 = determinista). Los proveedores que no la admiten la ignoran.
    pub temperature: f32,
    /// Se espera JSON (los proveedores que lo admiten activan el modo JSON).
    pub json: bool,
    /// Caso de Iurefficient para dar contexto (sólo lo usa el proveedor Iurefficient).
    pub case_id: Option<String>,
}

impl ChatRequest {
    /// Caracteres totales de entrada (sistema + mensajes).
    pub fn input_chars(&self) -> usize {
        self.messages.iter().fold(self.system.chars().count(), |acc, m| acc.saturating_add(m.content.chars().count()))
    }

    /// Todo en un solo texto, para proveedores sin roles (Iurefficient, Copilot).
    pub fn flatten_messages(&self) -> String {
        let mut out = String::new();
        for (i, m) in self.messages.iter().enumerate() {
            if i > 0 {
                out.push_str("\n\n");
            }
            if self.messages.len() > 1 {
                out.push_str(match m.role {
                    Role::User => "[user]\n",
                    Role::Assistant => "[assistant]\n",
                });
            }
            out.push_str(&m.content);
        }
        out
    }
}

/// Evento del flujo de respuesta (para mostrar el texto mientras llega).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEvent {
    /// Fragmento de texto nuevo.
    Delta(String),
    /// El proveedor pidió esperar; se reintenta en `wait`.
    Retrying { attempt: u32, wait: Duration },
}

/// Uso de tokens, si el proveedor lo informa.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// Respuesta completa.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Completion {
    pub text: String,
    /// `end_turn`, `stop`, `max_tokens`, `length`…
    pub stop_reason: Option<String>,
    pub usage: Option<Usage>,
}

impl Completion {
    /// ¿Se cortó por el tope de tokens?
    pub fn truncated(&self) -> bool {
        matches!(self.stop_reason.as_deref(), Some("max_tokens" | "length"))
    }
}

/// Topes de una llamada.
#[derive(Debug, Clone, PartialEq)]
pub struct Limits {
    /// Tiempo total máximo (incluidos los reintentos).
    pub timeout: Duration,
    /// Caracteres máximos de entrada.
    pub max_input_chars: usize,
    /// Bytes máximos de la respuesta (cuerpo HTTP o texto acumulado).
    pub max_response_bytes: usize,
    /// Reintentos ante 429/503/529.
    pub max_retries: u32,
    /// Espera máxima entre reintentos (aunque `Retry-After` pida más).
    pub max_retry_wait: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(120),
            max_input_chars: 200_000,
            max_response_bytes: 2 * 1024 * 1024,
            max_retries: 2,
            max_retry_wait: Duration::from_secs(30),
        }
    }
}

/// Un proveedor de IA. Nunca se llama solo: lo invoca una acción explícita del usuario.
pub trait AiProvider: Send + Sync {
    /// Qué proveedor es.
    fn kind(&self) -> ProviderKind;

    /// Nombre para mostrar («Anthropic · claude-sonnet-5-5»).
    fn label(&self) -> String;

    /// `true` si los datos salen a un tercero (se sugiere la redacción). Iurefficient y un
    /// servidor local (`localhost`) no son externos.
    fn is_external(&self) -> bool;

    /// Envía la petición. Cada fragmento de texto va a `tx` (si hay); al final devuelve la
    /// respuesta completa. Debe respetar `limits.max_response_bytes`. El tiempo total, la
    /// cancelación y los reintentos los pone [`run_chat`].
    fn chat<'a>(
        &'a self,
        req: &'a ChatRequest,
        limits: &'a Limits,
        tx: Option<&'a UnboundedSender<StreamEvent>>,
    ) -> BoxFuture<'a, Result<Completion>>;
}

/// Ejecuta una conversación con tiempo total, cancelación, topes y reintentos.
pub async fn run_chat(
    provider: &dyn AiProvider,
    req: &ChatRequest,
    limits: &Limits,
    tx: Option<&UnboundedSender<StreamEvent>>,
    cancel: &CancellationToken,
) -> Result<Completion> {
    let chars = req.input_chars();
    if chars > limits.max_input_chars {
        return Err(AiError::TooLarge(format!("{chars} caracteres de entrada (máximo {})", limits.max_input_chars)));
    }
    let work = async {
        let mut attempt = 0u32;
        loop {
            match provider.chat(req, limits, tx).await {
                Err(AiError::RateLimited { retry_after }) if attempt < limits.max_retries => {
                    attempt = attempt.saturating_add(1);
                    let backoff = Duration::from_secs(2u64.saturating_pow(attempt));
                    let wait = retry_after.unwrap_or(backoff).min(limits.max_retry_wait);
                    if let Some(tx) = tx {
                        tx.send(StreamEvent::Retrying { attempt, wait }).ok();
                    }
                    tokio::time::sleep(wait).await;
                }
                other => return other,
            }
        }
    };
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(AiError::Cancelled),
        r = tokio::time::timeout(limits.timeout, work) => r.unwrap_or(Err(AiError::Timeout)),
    }
}

/// Acumula texto con tope de bytes.
#[derive(Debug, Default)]
pub(crate) struct TextSink {
    pub text: String,
}

impl TextSink {
    pub fn push(&mut self, delta: &str, limits: &Limits, tx: Option<&UnboundedSender<StreamEvent>>) -> Result<()> {
        if delta.is_empty() {
            return Ok(());
        }
        if self.text.len().saturating_add(delta.len()) > limits.max_response_bytes {
            return Err(AiError::TooLarge(format!("respuesta de más de {} bytes", limits.max_response_bytes)));
        }
        self.text.push_str(delta);
        if let Some(tx) = tx {
            tx.send(StreamEvent::Delta(delta.to_string())).ok();
        }
        Ok(())
    }
}
