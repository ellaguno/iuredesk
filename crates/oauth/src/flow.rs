//! Inicio de sesión interactivo: código de autorización + PKCE con receptor *loopback*
//! (RFC 8252, RFC 7636).
//!
//! ```text
//! let pending = start_login(&provider, Some("ana@gmail.com")).await?;
//! if pending.open_browser().is_err() {
//!     // la UI muestra pending.authorize_url() para copiarla
//! }
//! let tokens = pending.finish(&http, DEFAULT_LOGIN_TIMEOUT, &cancel).await?;
//! ```
//!
//! [`start_login`] abre el receptor en `127.0.0.1` con un puerto efímero y arma la URL; [`PendingLogin::finish`]
//! espera la redirección (con tiempo de espera y [`CancellationToken`]), comprueba `state` y canjea
//! el código. Si se suelta el `PendingLogin`, el puerto se cierra.

use std::time::Duration;

use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use crate::pkce::{Pkce, random_token};
use crate::provider::ProviderConfig;
use crate::token::{TokenSet, exchange_code};
use crate::{Error, loopback};

/// Tiempo que se espera por omisión a que el usuario termine en el navegador.
pub const DEFAULT_LOGIN_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Inicio de sesión en curso: el receptor ya escucha y la URL está lista.
pub struct PendingLogin {
    provider: ProviderConfig,
    listener: TcpListener,
    redirect_uri: String,
    state: String,
    verifier: String,
    authorize_url: String,
}

impl std::fmt::Debug for PendingLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingLogin")
            .field("provider", &self.provider.id)
            .field("redirect_uri", &self.redirect_uri)
            .finish_non_exhaustive()
    }
}

/// Prepara un inicio de sesión: abre el receptor *loopback* y arma la URL de autorización.
/// `login_hint` preselecciona la cuenta en la página del proveedor.
pub async fn start_login(provider: &ProviderConfig, login_hint: Option<&str>) -> Result<PendingLogin, Error> {
    provider.validate()?;
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.map_err(|e| Error::Listener(e.to_string()))?;
    let port = listener.local_addr().map_err(|e| Error::Listener(e.to_string()))?.port();
    let redirect_uri = format!("http://{}:{port}", provider.redirect_host.as_str());
    let state = random_token(24)?;
    let pkce = Pkce::new()?;
    let authorize_url = authorize_url(provider, &redirect_uri, &state, &pkce.challenge, login_hint)?;
    Ok(PendingLogin {
        provider: provider.clone(),
        listener,
        redirect_uri,
        state,
        verifier: pkce.verifier,
        authorize_url,
    })
}

/// URL de autorización (RFC 6749 §4.1.1 + PKCE).
fn authorize_url(
    provider: &ProviderConfig,
    redirect_uri: &str,
    state: &str,
    challenge: &str,
    login_hint: Option<&str>,
) -> Result<String, Error> {
    let mut url = url::Url::parse(&provider.authorize_url).map_err(|e| Error::Config(e.to_string()))?;
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("response_type", "code")
            .append_pair("client_id", &provider.client.client_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("scope", &provider.consent_scopes.join(" "))
            .append_pair("state", state)
            .append_pair("code_challenge", challenge)
            .append_pair("code_challenge_method", "S256");
        if let Some(h) = login_hint.map(str::trim).filter(|h| !h.is_empty()) {
            q.append_pair("login_hint", h);
        }
        for (k, v) in &provider.extra_authorize_params {
            q.append_pair(k, v);
        }
    }
    Ok(url.into())
}

impl PendingLogin {
    /// URL que hay que abrir en el navegador (la UI la ofrece para copiar si no se pudo abrir).
    pub fn authorize_url(&self) -> &str {
        &self.authorize_url
    }

    /// `redirect_uri` en uso (`http://127.0.0.1:<puerto>` o `http://localhost:<puerto>`).
    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    /// Abre la URL en el navegador del sistema sin esperar a que se cierre.
    pub fn open_browser(&self) -> Result<(), Error> {
        open::that_detached(&self.authorize_url).map_err(|e| Error::Browser(e.to_string()))
    }

    /// Espera la redirección del navegador y canjea el código por tokens.
    pub async fn finish(
        self,
        http: &reqwest::Client,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> Result<TokenSet, Error> {
        let code = tokio::select! {
            _ = cancel.cancelled() => return Err(Error::Cancelled),
            r = tokio::time::timeout(timeout, loopback::wait_for_code(&self.listener, &self.state)) => {
                r.map_err(|_| Error::Timeout)??
            }
        };
        // Ya no hace falta el puerto.
        drop(self.listener);
        tokio::select! {
            _ = cancel.cancelled() => Err(Error::Cancelled),
            r = exchange_code(http, &self.provider, &code, &self.redirect_uri, &self.verifier) => r,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::ClientCredentials;

    #[tokio::test]
    async fn builds_authorize_url() {
        let p = ProviderConfig::google(ClientCredentials::new("cid.apps.googleusercontent.com", None));
        let pending = start_login(&p, Some("ana@gmail.com")).await.unwrap();
        let u = url::Url::parse(pending.authorize_url()).unwrap();
        let q: std::collections::HashMap<_, _> = u.query_pairs().into_owned().collect();
        assert_eq!(q.get("response_type").map(String::as_str), Some("code"));
        assert_eq!(q.get("code_challenge_method").map(String::as_str), Some("S256"));
        assert_eq!(q.get("access_type").map(String::as_str), Some("offline"));
        assert_eq!(q.get("login_hint").map(String::as_str), Some("ana@gmail.com"));
        assert_eq!(q.get("redirect_uri").map(String::as_str), Some(pending.redirect_uri()));
        assert!(pending.redirect_uri().starts_with("http://127.0.0.1:"));
        assert!(q.get("scope").is_some_and(|s| s.contains("https://mail.google.com/")));
    }
}
