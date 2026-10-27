//! Configuración del asistente en `<config>/assistant.json` (sin secretos) y claves de API en el
//! llavero del sistema (servicio `iuredesk`, usuario `ai-<id>:api-key`).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use iuredesk_accounts::{SecretPurpose, SecretStore};
use iuredesk_model::account::Account;
use iuredesk_oauth::TokenProvider;
use iurefficient_connect::rest::Session;
use serde::{Deserialize, Serialize};

use crate::error::{AiError, Result};
use crate::provider::AiProvider;
use crate::providers::{anthropic, copilot, iurefficient, openai};

/// Modelo de Anthropic por omisión.
pub const DEFAULT_ANTHROPIC_MODEL: &str = "claude-sonnet-5-5";
/// Tope del archivo de configuración.
const MAX_FILE: u64 = 1024 * 1024;
/// Versión del formato.
const FORMAT_VERSION: u32 = 1;

/// Tipo de proveedor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProviderKind {
    /// La instancia de Iurefficient (`global_chat`, contexto de casos y CRM). Por omisión.
    Iurefficient,
    /// Microsoft 365 Copilot Chat API (Graph `beta`; experimental, sólo si el inquilino lo tiene).
    Copilot,
    /// Cualquier API compatible con OpenAI `/v1/chat/completions` (OpenAI, OpenRouter, Ollama,
    /// llama.cpp). `base_url` incluye `/v1`.
    OpenAiCompatible { base_url: String, model: String },
    /// Anthropic Messages API.
    Anthropic { model: String },
}

impl ProviderKind {
    /// Etiqueta corta para la interfaz.
    pub fn label(&self) -> String {
        match self {
            ProviderKind::Iurefficient => "Iurefficient".into(),
            ProviderKind::Copilot => "Microsoft 365 Copilot".into(),
            ProviderKind::OpenAiCompatible { base_url, model } => format!("{model} · {}", host_of(base_url)),
            ProviderKind::Anthropic { model } => format!("Anthropic · {model}"),
        }
    }

    /// ¿Necesita clave de API? (en el compatible con OpenAI es opcional: Ollama no la pide).
    pub fn uses_api_key(&self) -> bool {
        matches!(self, ProviderKind::OpenAiCompatible { .. } | ProviderKind::Anthropic { .. })
    }
}

/// Host de una URL (para etiquetas y para saber si es local).
pub fn host_of(url: &str) -> String {
    reqwest::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_else(|| url.to_string())
}

/// ¿La URL apunta a este equipo?
pub fn is_local_url(url: &str) -> bool {
    let h = host_of(url);
    h == "localhost" || h == "127.0.0.1" || h == "[::1]" || h == "::1" || h.ends_with(".localhost")
}

/// Un proveedor configurado.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Identificador estable (clave del llavero: `ai-<id>`). Sin `:`.
    pub id: String,
    pub kind: ProviderKind,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Enmascarar correos y teléfonos antes de enviar. Por omisión, sí en proveedores externos.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redact: Option<bool>,
    /// URL base alternativa (proxy; para Anthropic, en lugar de `https://api.anthropic.com`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// Tiempo máximo por llamada, en segundos.
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    /// Tope de tokens de salida.
    #[serde(default = "default_max_tokens")]
    pub max_output_tokens: u32,
}

fn yes() -> bool {
    true
}
fn default_timeout() -> u64 {
    120
}
fn default_max_tokens() -> u32 {
    2048
}

impl ProviderConfig {
    pub fn new(id: impl Into<String>, kind: ProviderKind) -> Self {
        Self {
            id: id.into(),
            kind,
            enabled: true,
            redact: None,
            endpoint: None,
            timeout_secs: default_timeout(),
            max_output_tokens: default_max_tokens(),
        }
    }

    /// Clave del llavero de este proveedor.
    pub fn secret_id(&self) -> String {
        format!("ai-{}", self.id)
    }

    fn validate(&self) -> Result<()> {
        if self.id.is_empty() || self.id.contains(':') || self.id.len() > 64 {
            return Err(AiError::Config(format!("identificador de proveedor no válido: {:?}", self.id)));
        }
        match &self.kind {
            ProviderKind::OpenAiCompatible { base_url, model } => {
                let u = reqwest::Url::parse(base_url).map_err(|e| AiError::Config(format!("{base_url}: {e}")))?;
                if !matches!(u.scheme(), "http" | "https") || model.trim().is_empty() {
                    return Err(AiError::Config(format!("URL o modelo no válidos: {base_url} {model}")));
                }
            }
            ProviderKind::Anthropic { model } if model.trim().is_empty() => {
                return Err(AiError::Config("falta el modelo de Anthropic".into()));
            }
            _ => {}
        }
        Ok(())
    }
}

/// Configuración completa.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssistantConfig {
    #[serde(default = "format_version")]
    pub version: u32,
    /// Interruptor general (PLAN.md §8: se puede desactivar).
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Proveedor por omisión (id). `None` → Iurefficient si existe, si no el primero activo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_provider: Option<String>,
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
    /// Cuentas de correo en las que el asistente está desactivado.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled_accounts: Vec<String>,
    /// Zona horaria IANA para Copilot (`locationHint`) y fechas relativas.
    #[serde(default = "default_tz")]
    pub time_zone: String,
}

fn format_version() -> u32 {
    FORMAT_VERSION
}
fn default_tz() -> String {
    "UTC".into()
}

impl Default for AssistantConfig {
    fn default() -> Self {
        Self {
            version: FORMAT_VERSION,
            enabled: true,
            default_provider: None,
            providers: vec![ProviderConfig::new("iurefficient", ProviderKind::Iurefficient)],
            disabled_accounts: Vec::new(),
            time_zone: default_tz(),
        }
    }
}

impl AssistantConfig {
    /// Ruta por omisión (`<config>/iuredesk/assistant.json`).
    pub fn default_path() -> Option<PathBuf> {
        iuredesk_accounts::store::config_dir().map(|d| d.join("assistant.json"))
    }

    /// Lee la configuración; si no existe, la de omisión.
    pub fn load(path: &Path) -> Result<Self> {
        let meta = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(AiError::Config(e.to_string())),
        };
        if meta.len() > MAX_FILE {
            return Err(AiError::Config("archivo demasiado grande".into()));
        }
        let data = std::fs::read(path).map_err(|e| AiError::Config(e.to_string()))?;
        let cfg: Self = serde_json::from_slice(&data).map_err(|e| AiError::Config(e.to_string()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Guarda con escritura atómica.
    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let data = serde_json::to_vec_pretty(self).map_err(|e| AiError::Config(e.to_string()))?;
        iuredesk_accounts::store::write_atomic(path, &data).map_err(|e| AiError::Config(e.to_string()))
    }

    fn validate(&self) -> Result<()> {
        let mut seen = std::collections::HashSet::new();
        for p in &self.providers {
            p.validate()?;
            if !seen.insert(p.id.as_str()) {
                return Err(AiError::Config(format!("proveedor repetido: {}", p.id)));
            }
        }
        Ok(())
    }

    pub fn provider(&self, id: &str) -> Option<&ProviderConfig> {
        self.providers.iter().find(|p| p.id == id)
    }

    /// El proveedor a usar si el usuario no eligió otro.
    pub fn default_provider(&self) -> Option<&ProviderConfig> {
        let active = |p: &&ProviderConfig| p.enabled;
        self.default_provider
            .as_deref()
            .and_then(|id| self.provider(id))
            .filter(active)
            .or_else(|| self.providers.iter().filter(active).find(|p| p.kind == ProviderKind::Iurefficient))
            .or_else(|| self.providers.iter().find(active))
    }

    /// ¿Está activo el asistente para esta cuenta de correo?
    pub fn enabled_for(&self, account_id: &str) -> bool {
        self.enabled && !self.disabled_accounts.iter().any(|a| a == account_id)
    }
}

/// Guarda la clave de API de un proveedor en el llavero.
pub fn set_api_key(secrets: &dyn SecretStore, provider: &ProviderConfig, key: &str) -> Result<()> {
    secrets.set(&provider.secret_id(), SecretPurpose::ApiKey, key.trim()).map_err(|e| AiError::Secret(e.to_string()))
}

/// Lee la clave de API de un proveedor.
pub fn api_key(secrets: &dyn SecretStore, provider: &ProviderConfig) -> Result<Option<String>> {
    secrets.get(&provider.secret_id(), SecretPurpose::ApiKey).map_err(|e| AiError::Secret(e.to_string()))
}

/// Borra la clave de API (al quitar el proveedor).
pub fn delete_api_key(secrets: &dyn SecretStore, provider: &ProviderConfig) -> Result<()> {
    secrets.delete(&provider.secret_id(), SecretPurpose::ApiKey).map_err(|e| AiError::Secret(e.to_string()))
}

/// Lo que hace falta para construir los proveedores que no usan clave propia.
#[derive(Clone, Default)]
pub struct ProviderContext {
    /// Sesión de la suite (de `iuredesk_accounts::iurefficient_session`).
    pub iurefficient: Option<Arc<Session>>,
    /// Cuenta Microsoft 365 y su proveedor de tokens (audiencia `Api`).
    pub microsoft: Option<(Account, Arc<dyn TokenProvider>)>,
    /// Zona horaria IANA.
    pub time_zone: String,
}

/// Construye el proveedor de una configuración. Copilot exige además
/// [`copilot::CopilotProvider::probe`] antes de ofrecerlo en la interfaz.
pub fn build_provider(
    cfg: &ProviderConfig,
    secrets: &dyn SecretStore,
    ctx: &ProviderContext,
) -> Result<Box<dyn AiProvider>> {
    cfg.validate()?;
    if !cfg.enabled {
        return Err(AiError::NotAvailable(format!("{} está desactivado", cfg.kind.label())));
    }
    match &cfg.kind {
        ProviderKind::Iurefficient => {
            let sess = ctx
                .iurefficient
                .clone()
                .ok_or_else(|| AiError::NotAvailable("no hay sesión de Iurefficient".into()))?;
            Ok(Box::new(iurefficient::IurefficientProvider::new(sess)))
        }
        ProviderKind::Copilot => {
            let (acc, tokens) =
                ctx.microsoft.clone().ok_or_else(|| AiError::NotAvailable("no hay cuenta de Microsoft 365".into()))?;
            let base = cfg.endpoint.clone().unwrap_or_else(|| copilot::DEFAULT_BASE.into());
            Ok(Box::new(copilot::CopilotProvider::new(acc, tokens, &base, &ctx.time_zone)?))
        }
        ProviderKind::OpenAiCompatible { base_url, model } => {
            let key = api_key(secrets, cfg)?;
            Ok(Box::new(openai::OpenAiProvider::new(base_url, model, key)?))
        }
        ProviderKind::Anthropic { model } => {
            let key = api_key(secrets, cfg)?
                .filter(|k| !k.is_empty())
                .ok_or_else(|| AiError::NotAvailable("falta la clave de API de Anthropic".into()))?;
            let base = cfg.endpoint.clone().unwrap_or_else(|| anthropic::DEFAULT_BASE.into());
            Ok(Box::new(anthropic::AnthropicProvider::new(&base, model, key)?))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iuredesk_accounts::MemoryStore;

    #[test]
    fn default_is_iurefficient_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("assistant.json");
        let cfg = AssistantConfig::load(&path).unwrap();
        assert_eq!(cfg.default_provider().unwrap().kind, ProviderKind::Iurefficient);

        let mut cfg = cfg;
        cfg.providers.push(ProviderConfig::new(
            "ollama",
            ProviderKind::OpenAiCompatible { base_url: "http://localhost:11434/v1".into(), model: "llama3".into() },
        ));
        cfg.providers
            .push(ProviderConfig::new("claude", ProviderKind::Anthropic { model: DEFAULT_ANTHROPIC_MODEL.into() }));
        cfg.default_provider = Some("claude".into());
        cfg.save(&path).unwrap();
        let back = AssistantConfig::load(&path).unwrap();
        assert_eq!(back, cfg);
        assert_eq!(back.default_provider().unwrap().id, "claude");
        // El JSON no lleva secretos.
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.to_lowercase().contains("key"));
    }

    #[test]
    fn invalid_configs_are_rejected() {
        let mut cfg = AssistantConfig::default();
        cfg.providers.push(ProviderConfig::new("iurefficient", ProviderKind::Iurefficient));
        let dir = tempfile::tempdir().unwrap();
        assert!(cfg.save(&dir.path().join("a.json")).is_err());
        let bad = ProviderConfig::new(
            "x",
            ProviderKind::OpenAiCompatible { base_url: "ftp://nope".into(), model: "m".into() },
        );
        assert!(bad.validate().is_err());
        assert!(ProviderConfig::new("a:b", ProviderKind::Iurefficient).validate().is_err());
    }

    #[test]
    fn api_keys_live_in_the_secret_store() {
        let secrets = MemoryStore::default();
        let p = ProviderConfig::new("claude", ProviderKind::Anthropic { model: "m".into() });
        assert!(matches!(
            build_provider(&p, &secrets, &ProviderContext::default()).err(),
            Some(AiError::NotAvailable(_))
        ));
        set_api_key(&secrets, &p, " sk-test ").unwrap();
        assert_eq!(api_key(&secrets, &p).unwrap().as_deref(), Some("sk-test"));
        assert_eq!(secrets.get("ai-claude", SecretPurpose::ApiKey).unwrap().as_deref(), Some("sk-test"));
        let built = build_provider(&p, &secrets, &ProviderContext::default()).unwrap();
        assert!(built.is_external());
        delete_api_key(&secrets, &p).unwrap();
        assert_eq!(api_key(&secrets, &p).unwrap(), None);
    }

    #[test]
    fn missing_contexts_are_not_available() {
        let secrets = MemoryStore::default();
        for kind in [ProviderKind::Iurefficient, ProviderKind::Copilot] {
            let p = ProviderConfig::new("p", kind);
            assert!(matches!(
                build_provider(&p, &secrets, &ProviderContext::default()).err(),
                Some(AiError::NotAvailable(_))
            ));
        }
        assert!(is_local_url("http://localhost:11434/v1"));
        assert!(is_local_url("http://127.0.0.1:8080/v1"));
        assert!(!is_local_url("https://openrouter.ai/api/v1"));
    }
}
