//! Prueba SMTP con `lettre` (rustls + tokio): conecta, cifra, se autentica, NOOP y QUIT.

use iuredesk_model::account::{Security, ServerConfig};
use lettre::transport::smtp::authentication::{Credentials, Mechanism};
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::transport::smtp::extension::ClientId;
use lettre::{AsyncSmtpTransport, Tokio1Executor};

use super::{ProbeError, ProbeOptions, ProbeReport, classify_auth_failure, clip};

/// Traduce un error de lettre a uno amable.
pub(crate) fn map_error(e: &lettre::transport::smtp::Error, host: &str) -> ProbeError {
    let text = clip(&e.to_string());
    let lower = text.to_ascii_lowercase();
    if e.is_timeout() {
        return ProbeError::Timeout;
    }
    if e.is_tls() || lower.contains("certificate") || lower.contains("tls") {
        return if lower.contains("certificate") || lower.contains("unknownissuer") || lower.contains("notvalidforname")
        {
            ProbeError::TlsCertificate(text)
        } else {
            ProbeError::Tls(text)
        };
    }
    if lower.contains("no compatible authentication mechanism") {
        return ProbeError::AuthUnsupported;
    }
    if lower.contains("connection refused") {
        return ProbeError::ConnectionRefused { host: host.to_string() };
    }
    if lower.contains("failed to lookup")
        || lower.contains("name or service not known")
        || lower.contains("no such host")
    {
        return ProbeError::HostNotFound { host: host.to_string() };
    }
    match e.status().map(u16::from) {
        // 530/535/534/454: rechazo de credenciales o de autenticación; 421: servicio no disponible.
        Some(421 | 454 | 530 | 534 | 535 | 538) => classify_auth_failure(&text),
        _ if e.is_permanent() || e.is_transient() => classify_auth_failure(&text),
        _ => ProbeError::Protocol(text),
    }
}

pub(super) async fn probe(
    cfg: &ServerConfig,
    secret: &str,
    oauth: bool,
    opts: ProbeOptions,
) -> Result<ProbeReport, ProbeError> {
    // Resolver antes da un «servidor no encontrado» claro, igual que en IMAP/POP3.
    match tokio::time::timeout(opts.io_timeout, tokio::net::lookup_host((cfg.host.as_str(), cfg.port))).await {
        Ok(Ok(mut a)) => {
            if a.next().is_none() {
                return Err(ProbeError::HostNotFound { host: cfg.host.clone() });
            }
        }
        Ok(Err(_)) => return Err(ProbeError::HostNotFound { host: cfg.host.clone() }),
        Err(_) => return Err(ProbeError::Timeout),
    }
    let tls = match cfg.security {
        Security::Plain => Tls::None,
        Security::Tls | Security::StartTls => {
            let params = TlsParameters::new(cfg.host.clone()).map_err(|e| ProbeError::Tls(clip(&e.to_string())))?;
            if cfg.security == Security::Tls { Tls::Wrapper(params) } else { Tls::Required(params) }
        }
    };
    let mechanisms = if oauth { vec![Mechanism::Xoauth2] } else { vec![Mechanism::Plain, Mechanism::Login] };
    let transport = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(cfg.host.as_str())
        .port(cfg.port)
        .tls(tls)
        .hello_name(ClientId::Domain("localhost".into()))
        .credentials(Credentials::new(cfg.username.clone(), secret.to_string()))
        .authentication(mechanisms)
        .timeout(Some(opts.io_timeout))
        .build::<Tokio1Executor>();
    match transport.test_connection().await {
        Ok(true) => Ok(ProbeReport::default()),
        Ok(false) => Err(ProbeError::Protocol("el servidor no respondió a NOOP".into())),
        Err(e) => Err(map_error(&e, &cfg.host)),
    }
}
