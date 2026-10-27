//! Transportes de envío.
//!
//! [`Transport`] es lo que usa la bandeja de salida; hoy hay SMTP ([`SmtpTransport`], `lettre`
//! con rustls) y la interfaz está pensada para que Microsoft Graph (`sendMail`) o la API de Gmail
//! se enchufen igual: reciben el mensaje ya construido (sobre + bytes RFC 5322) y dicen si el
//! proveedor guarda solo la copia en Enviados.
//!
//! Los errores se clasifican en transitorios (se reintenta con espera creciente) y permanentes
//! (el mensaje se queda en la bandeja de salida con un mensaje humano en español e inglés).

use std::sync::Arc;
use std::time::Duration;

use iuredesk_accounts::{SecretPurpose, SecretStore};
use iuredesk_model::account::{Account, AccountKind, AuthMethod, Protocol, Security};
use iuredesk_oauth::{BoxFuture, TokenAudience, TokenProvider};
use lettre::address::Envelope;
use lettre::transport::smtp::authentication::{Credentials, Mechanism};
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::transport::smtp::extension::ClientId;
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};
use serde::{Deserialize, Serialize};

use crate::lang::Lang;
use crate::mime::BuiltMessage;

/// Tiempo de espera por orden SMTP.
pub const DEFAULT_SMTP_TIMEOUT: Duration = Duration::from_secs(60);
/// Longitud máxima del detalle técnico que se guarda.
const MAX_DETAIL: usize = 500;

/// Mensaje listo para un transporte.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutgoingMessage {
    /// `MAIL FROM` (dominio en ASCII; la parte local puede ser UTF-8, RFC 6531).
    pub envelope_from: String,
    /// `RCPT TO` (ídem).
    pub recipients: Vec<String>,
    /// Bytes RFC 5322 (sin `Bcc:`).
    pub raw: Vec<u8>,
}

impl From<&BuiltMessage> for OutgoingMessage {
    fn from(b: &BuiltMessage) -> Self {
        Self { envelope_from: b.envelope_from.clone(), recipients: b.recipients.clone(), raw: b.raw.clone() }
    }
}

/// Respuesta del servidor al aceptar el mensaje.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SendReceipt {
    /// Texto de la respuesta (p. ej. `2.0.0 OK 1698…`).
    pub server_reply: String,
}

/// Clase de error de envío.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SendErrorKind {
    /// Sin red, servidor inalcanzable, conexión cortada. Transitorio.
    Network,
    /// El servidor no respondió a tiempo. Transitorio.
    Timeout,
    /// Fallo de TLS.
    Tls,
    /// Certificado no válido.
    Certificate,
    /// Usuario o contraseña (o token) rechazados.
    Auth,
    /// El servidor no admite el método de autenticación configurado.
    AuthUnsupported,
    /// OAuth: hay que volver a dar permiso.
    NeedsReconsent,
    /// Respuesta 4xx: rechazo temporal. Transitorio.
    TemporarilyRejected,
    /// Respuesta 5xx: rechazo definitivo.
    Rejected,
    /// El servidor dice que el mensaje es demasiado grande (552).
    TooLarge,
    /// Una dirección del sobre no es válida.
    InvalidAddress,
    /// La cuenta no tiene un SMTP utilizable o falta la contraseña.
    Config,
    /// Hay direcciones internacionalizadas (`josé@…`) y el servidor no anuncia `SMTPUTF8`.
    InternationalUnsupported,
}

/// Error de envío.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{kind:?}: {detail}")]
pub struct SendError {
    pub kind: SendErrorKind,
    /// Detalle técnico (respuesta del servidor), recortado.
    pub detail: String,
    /// Código SMTP, si lo hubo.
    #[serde(default)]
    pub code: Option<u16>,
    /// Servidor implicado (para el mensaje).
    #[serde(default)]
    pub host: String,
}

impl SendError {
    /// Crea un error.
    pub fn new(kind: SendErrorKind, detail: impl Into<String>) -> Self {
        let detail: String = detail.into().chars().filter(|c| !c.is_control()).take(MAX_DETAIL).collect();
        Self { kind, detail, code: None, host: String::new() }
    }

    fn with_host(mut self, host: &str) -> Self {
        self.host = host.to_string();
        self
    }

    fn with_code(mut self, code: Option<u16>) -> Self {
        self.code = code;
        self
    }

    /// ¿Vale la pena reintentar más tarde sin que el usuario haga nada?
    pub fn is_transient(&self) -> bool {
        matches!(self.kind, SendErrorKind::Network | SendErrorKind::Timeout | SendErrorKind::TemporarilyRejected)
    }

    /// Mensaje para el usuario.
    pub fn message(&self, lang: Lang) -> String {
        let host =
            if self.host.is_empty() { lang.pick("the mail server", "el servidor de correo") } else { &self.host };
        let d = &self.detail;
        match (self.kind, lang) {
            (SendErrorKind::Network, Lang::Es) => {
                format!("No se pudo conectar con {host}. Revisa tu conexión a internet; se reintentará solo.")
            }
            (SendErrorKind::Network, Lang::En) => {
                format!("Couldn't connect to {host}. Check your internet connection; it will retry automatically.")
            }
            (SendErrorKind::Timeout, Lang::Es) => format!("{host} tardó demasiado en responder; se reintentará solo."),
            (SendErrorKind::Timeout, Lang::En) => {
                format!("{host} took too long to answer; it will retry automatically.")
            }
            (SendErrorKind::Tls, Lang::Es) => format!("No se pudo establecer una conexión segura con {host}."),
            (SendErrorKind::Tls, Lang::En) => format!("Couldn't establish a secure connection with {host}."),
            (SendErrorKind::Certificate, Lang::Es) => {
                format!("El certificado de seguridad de {host} no es válido. No se envió nada.")
            }
            (SendErrorKind::Certificate, Lang::En) => {
                format!("The security certificate of {host} is not valid. Nothing was sent.")
            }
            (SendErrorKind::Auth, Lang::Es) => {
                "El servidor rechazó tu usuario o contraseña. Revisa la contraseña de la cuenta y vuelve a enviar."
                    .to_string()
            }
            (SendErrorKind::Auth, Lang::En) => {
                "The server rejected your username or password. Check the account password and send again.".to_string()
            }
            (SendErrorKind::AuthUnsupported, Lang::Es) => {
                format!("{host} no admite el método de inicio de sesión configurado.")
            }
            (SendErrorKind::AuthUnsupported, Lang::En) => {
                format!("{host} doesn't support the configured sign-in method.")
            }
            (SendErrorKind::NeedsReconsent, Lang::Es) => {
                "Hay que volver a iniciar sesión en la cuenta para poder enviar.".to_string()
            }
            (SendErrorKind::NeedsReconsent, Lang::En) => "Sign in to the account again to be able to send.".to_string(),
            (SendErrorKind::TemporarilyRejected, Lang::Es) => {
                format!("{host} no aceptó el mensaje por ahora; se reintentará solo.")
            }
            (SendErrorKind::TemporarilyRejected, Lang::En) => {
                format!("{host} didn't accept the message for now; it will retry automatically.")
            }
            (SendErrorKind::Rejected, Lang::Es) => format!("{host} rechazó el mensaje: «{d}»."),
            (SendErrorKind::Rejected, Lang::En) => format!("{host} rejected the message: “{d}”."),
            (SendErrorKind::TooLarge, Lang::Es) => {
                "El mensaje es demasiado grande para tu servidor. Comparte los archivos como enlace.".to_string()
            }
            (SendErrorKind::TooLarge, Lang::En) => {
                "The message is too large for your server. Share the files as a link instead.".to_string()
            }
            (SendErrorKind::InvalidAddress, Lang::Es) => format!("Una dirección no es válida: {d}."),
            (SendErrorKind::InvalidAddress, Lang::En) => format!("An address is not valid: {d}."),
            (SendErrorKind::Config, Lang::Es) => {
                "La cuenta no tiene bien configurado el servidor de salida (SMTP) o falta la contraseña.".to_string()
            }
            (SendErrorKind::Config, Lang::En) => {
                "The account's outgoing (SMTP) server isn't set up correctly, or the password is missing.".to_string()
            }
            (SendErrorKind::InternationalUnsupported, Lang::Es) => format!(
                "{host} no acepta direcciones con acentos o letras especiales antes de la @. Usa otra dirección del destinatario."
            ),
            (SendErrorKind::InternationalUnsupported, Lang::En) => format!(
                "{host} doesn't accept addresses with accents or special letters before the @. Use another address for the recipient."
            ),
        }
    }
}

/// Un medio de envío (SMTP hoy; Graph `sendMail` o la API de Gmail mañana).
pub trait Transport: Send + Sync {
    /// Entrega el mensaje.
    fn send<'a>(&'a self, msg: &'a OutgoingMessage) -> BoxFuture<'a, Result<SendReceipt, SendError>>;

    /// ¿El proveedor guarda solo la copia en Enviados? (Gmail y Microsoft 365 sí; entonces no se
    /// encola `AppendToSent`).
    fn saves_to_sent(&self) -> bool;
}

/// ¿El proveedor guarda los mensajes enviados por SMTP en Enviados sin que lo pidamos?
pub fn provider_saves_sent(account: &Account) -> bool {
    let host = account.outgoing.host.to_ascii_lowercase();
    matches!(account.kind, AccountKind::Gmail | AccountKind::Microsoft365)
        || host == "smtp.gmail.com"
        || host == "smtp.office365.com"
        || host == "smtp-mail.outlook.com"
}

/// Envío por SMTP con `lettre`: TLS implícito (465) o STARTTLS obligatorio (587) con rustls;
/// AUTH PLAIN/LOGIN con la contraseña del llavero o XOAUTH2 con un [`TokenProvider`].
pub struct SmtpTransport {
    account: Account,
    secrets: Arc<dyn SecretStore>,
    tokens: Option<Arc<dyn TokenProvider>>,
    timeout: Duration,
    saves_sent: bool,
}

impl std::fmt::Debug for SmtpTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SmtpTransport").field("account", &self.account.id).finish_non_exhaustive()
    }
}

impl SmtpTransport {
    pub fn new(account: Account, secrets: Arc<dyn SecretStore>, tokens: Option<Arc<dyn TokenProvider>>) -> Self {
        let saves_sent = provider_saves_sent(&account);
        Self { account, secrets, tokens, timeout: DEFAULT_SMTP_TIMEOUT, saves_sent }
    }

    /// Cambia el tiempo de espera por orden.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Fuerza si el proveedor guarda la copia en Enviados.
    pub fn with_saves_to_sent(mut self, saves: bool) -> Self {
        self.saves_sent = saves;
        self
    }

    async fn credentials(&self, oauth: bool) -> Result<String, SendError> {
        let host = self.account.outgoing.host.as_str();
        if oauth {
            let Some(tokens) = &self.tokens else {
                return Err(SendError::new(SendErrorKind::Config, "sin proveedor de tokens OAuth").with_host(host));
            };
            return tokens.access_token(&self.account, TokenAudience::Mail).await.map_err(|e| {
                let kind = if e.needs_reconsent() {
                    SendErrorKind::NeedsReconsent
                } else if matches!(e, iuredesk_oauth::Error::Http(_)) {
                    SendErrorKind::Network
                } else {
                    SendErrorKind::Auth
                };
                SendError::new(kind, e.to_string()).with_host(host)
            });
        }
        let get = |p| self.secrets.get(&self.account.id, p);
        let secret = match get(SecretPurpose::Outgoing) {
            Ok(Some(s)) => Some(s),
            Ok(None) => get(SecretPurpose::Incoming).ok().flatten(),
            Err(e) => return Err(SendError::new(SendErrorKind::Config, format!("llavero: {e}")).with_host(host)),
        };
        secret.ok_or_else(|| SendError::new(SendErrorKind::Config, "no hay contraseña guardada").with_host(host))
    }

    async fn attempt(&self, msg: &OutgoingMessage, oauth: bool) -> Result<SendReceipt, SendError> {
        let cfg = &self.account.outgoing;
        let host = cfg.host.as_str();
        let secret = self.credentials(oauth).await?;
        let tls = match cfg.security {
            Security::Plain => Tls::None,
            Security::Tls | Security::StartTls => {
                let params = TlsParameters::new(cfg.host.clone())
                    .map_err(|e| SendError::new(SendErrorKind::Tls, e.to_string()).with_host(host))?;
                if cfg.security == Security::Tls { Tls::Wrapper(params) } else { Tls::Required(params) }
            }
        };
        let mechanisms = if oauth { vec![Mechanism::Xoauth2] } else { vec![Mechanism::Plain, Mechanism::Login] };
        let ehlo = msg
            .envelope_from
            .rsplit_once('@')
            .map(|(_, d)| d.to_string())
            .filter(|d| !d.is_empty())
            .unwrap_or_else(|| "localhost".to_string());
        let transport = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(host)
            .port(cfg.port)
            .tls(tls)
            .hello_name(ClientId::Domain(ehlo))
            .credentials(Credentials::new(cfg.username.clone(), secret))
            .authentication(mechanisms)
            .timeout(Some(self.timeout))
            .build::<Tokio1Executor>();
        let from = msg
            .envelope_from
            .parse::<lettre::Address>()
            .map_err(|e| SendError::new(SendErrorKind::InvalidAddress, format!("{}: {e}", msg.envelope_from)))?;
        let mut to = Vec::with_capacity(msg.recipients.len());
        for r in &msg.recipients {
            to.push(
                r.parse::<lettre::Address>()
                    .map_err(|e| SendError::new(SendErrorKind::InvalidAddress, format!("{r}: {e}")))?,
            );
        }
        let envelope =
            Envelope::new(Some(from), to).map_err(|e| SendError::new(SendErrorKind::InvalidAddress, e.to_string()))?;
        match transport.send_raw(&envelope, &msg.raw).await {
            Ok(resp) => Ok(SendReceipt { server_reply: resp.message().collect::<Vec<_>>().join(" ") }),
            Err(e) => Err(map_lettre(&e, host)),
        }
    }
}

impl Transport for SmtpTransport {
    fn send<'a>(&'a self, msg: &'a OutgoingMessage) -> BoxFuture<'a, Result<SendReceipt, SendError>> {
        Box::pin(async move {
            if self.account.outgoing.protocol != Protocol::Smtp {
                return Err(SendError::new(SendErrorKind::Config, "el servidor de salida no es SMTP"));
            }
            let oauth = matches!(self.account.outgoing.auth, AuthMethod::OAuth2 { .. });
            match self.attempt(msg, oauth).await {
                // Token caducado o revocado: se olvida y se pide otro una sola vez.
                Err(e) if oauth && e.kind == SendErrorKind::Auth => {
                    if let Some(t) = &self.tokens {
                        t.invalidate(&self.account.id);
                    }
                    self.attempt(msg, oauth).await
                }
                other => other,
            }
        })
    }

    fn saves_to_sent(&self) -> bool {
        self.saves_sent
    }
}

/// Clasifica un error de `lettre`.
pub(crate) fn map_lettre(e: &lettre::transport::smtp::Error, host: &str) -> SendError {
    let text = e.to_string();
    let lower = text.to_ascii_lowercase();
    let code = e.status().map(u16::from);
    let err = |kind| SendError::new(kind, text.clone()).with_host(host).with_code(code);
    if e.is_timeout() {
        return err(SendErrorKind::Timeout);
    }
    if lower.contains("certificate") || lower.contains("unknownissuer") || lower.contains("notvalidforname") {
        return err(SendErrorKind::Certificate);
    }
    if e.is_tls() {
        return err(SendErrorKind::Tls);
    }
    if lower.contains("smtputf8") {
        return err(SendErrorKind::InternationalUnsupported);
    }
    if lower.contains("no compatible authentication mechanism") {
        return err(SendErrorKind::AuthUnsupported);
    }
    match code {
        Some(530 | 534 | 535 | 538) => err(SendErrorKind::Auth),
        Some(552) => err(SendErrorKind::TooLarge),
        Some(c) if (400..500).contains(&c) => err(SendErrorKind::TemporarilyRejected),
        Some(c) if (500..600).contains(&c) => err(SendErrorKind::Rejected),
        _ if e.is_transient() => err(SendErrorKind::TemporarilyRejected),
        _ if e.is_permanent() => err(SendErrorKind::Rejected),
        // Conexión rechazada o cortada, DNS, E/S: problemas de red.
        _ => err(SendErrorKind::Network),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_classification_and_messages() {
        for kind in [SendErrorKind::Network, SendErrorKind::Timeout, SendErrorKind::TemporarilyRejected] {
            assert!(SendError::new(kind, "x").is_transient());
        }
        for kind in [SendErrorKind::Auth, SendErrorKind::Rejected, SendErrorKind::TooLarge, SendErrorKind::Config] {
            let e = SendError::new(kind, "x");
            assert!(!e.is_transient());
            assert!(!e.message(Lang::Es).is_empty());
            assert!(!e.message(Lang::En).is_empty());
        }
        let e = SendError::new(SendErrorKind::Rejected, "550\r\n5.7.1 spam");
        assert!(!e.detail.contains('\n'));
        assert!(e.message(Lang::Es).contains("rechazó"));
    }
}
