//! Prueba de Exchange Web Services (F8.2): pide el árbol de carpetas con usuario y contraseña.

use std::sync::Arc;
use std::time::Duration;

use iuredesk_ews::{BasicAuth, EwsClient, EwsError};
use iuredesk_model::account::{Security, ServerConfig};

use super::{MAX_FOLDERS, ProbeError, ProbeOptions, ProbeReport};

/// Traduce un fallo de EWS.
pub(crate) fn classify(e: &EwsError, host: &str) -> ProbeError {
    match e {
        EwsError::Unauthorized => ProbeError::AuthFailed("401".into()),
        EwsError::Http { status: 404 } => {
            ProbeError::Protocol("no hay Exchange Web Services en esa dirección (404)".into())
        }
        EwsError::Http { status: 403 } => {
            ProbeError::Protocol("Exchange Web Services está desactivado para esta cuenta (403)".into())
        }
        EwsError::Network(s) => {
            let l = s.to_ascii_lowercase();
            if l.contains("dns") || l.contains("resolve") || l.contains("lookup") {
                ProbeError::HostNotFound { host: host.to_string() }
            } else if l.contains("certificate") || l.contains("unknownissuer") {
                ProbeError::TlsCertificate(s.clone())
            } else if l.contains("refused") {
                ProbeError::ConnectionRefused { host: host.to_string() }
            } else if l.contains("timed out") || l.contains("timeout") {
                ProbeError::Timeout
            } else {
                ProbeError::Io(s.clone())
            }
        }
        other => ProbeError::Protocol(other.to_string()),
    }
}

pub(crate) async fn probe(
    cfg: &ServerConfig,
    secret: &str,
    oauth: bool,
    opts: ProbeOptions,
) -> Result<ProbeReport, ProbeError> {
    // Exchange local con OAuth (híbrido) llega con el inicio de sesión de la organización.
    if oauth {
        return Err(ProbeError::AuthUnsupported);
    }
    let url = iuredesk_ews::server_url(&cfg.host, cfg.port, cfg.security != Security::Plain)
        .map_err(|e| ProbeError::Protocol(e.to_string()))?;
    let timeout = opts.io_timeout.saturating_mul(3).min(opts.total_timeout);
    let client = EwsClient::new(&url, Arc::new(BasicAuth::new(&cfg.username, secret)), timeout)
        .map_err(|e| ProbeError::Protocol(e.to_string()))?
        .with_retry(1, Duration::from_secs(5));
    let h = client.sync_folder_hierarchy(None).await.map_err(|e| classify(&e, &cfg.host))?;
    let mail: Vec<String> = h.changed.iter().filter(|f| f.is_mail()).map(|f| f.name.clone()).collect();
    Ok(ProbeReport {
        greeting: "Exchange Web Services".into(),
        folder_count: mail.len(),
        folders: mail.into_iter().take(MAX_FOLDERS).collect(),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_are_classified() {
        assert!(matches!(classify(&EwsError::Unauthorized, "h"), ProbeError::AuthFailed(_)));
        assert!(matches!(
            classify(&EwsError::Network("error trying to connect: dns error: failed to lookup".into()), "h"),
            ProbeError::HostNotFound { .. }
        ));
        assert!(matches!(
            classify(&EwsError::Network("invalid peer certificate: UnknownIssuer".into()), "h"),
            ProbeError::TlsCertificate(_)
        ));
        assert!(matches!(classify(&EwsError::Http { status: 404 }, "h"), ProbeError::Protocol(_)));
    }
}
