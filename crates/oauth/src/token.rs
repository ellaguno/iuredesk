//! Tokens: [`TokenSet`], canje del código de autorización y refresco (RFC 6749 §4.1.3 y §6).

use std::time::Duration;

use base64::Engine;
use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::Error;
use crate::provider::{ProviderConfig, TokenAudience};

/// Tamaño máximo de una respuesta del servidor de tokens.
const MAX_RESPONSE: usize = 256 * 1024;
/// Tamaño máximo de un `id_token` que se intenta leer.
const MAX_ID_TOKEN: usize = 32 * 1024;
/// Vida que se supone para un token sin `expires_in` (prudente).
const DEFAULT_LIFETIME: i64 = 30 * 60;
/// Vida máxima que se acepta (un servidor hostil podría decir «para siempre»).
const MAX_LIFETIME: i64 = 24 * 60 * 60;

/// Datos del usuario tomados del `id_token` de OpenID Connect.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct IdClaims {
    #[serde(default)]
    pub sub: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub email_verified: Option<bool>,
    #[serde(default)]
    pub name: Option<String>,
    /// Microsoft: UPN o correo con el que se inició sesión.
    #[serde(default)]
    pub preferred_username: Option<String>,
    /// Microsoft: inquilino.
    #[serde(default)]
    pub tid: Option<String>,
}

impl IdClaims {
    /// Mejor correo disponible: `email` o, en Microsoft, `preferred_username` si parece un correo.
    pub fn best_email(&self) -> Option<&str> {
        self.email.as_deref().or_else(|| self.preferred_username.as_deref().filter(|u| u.contains('@')))
    }
}

/// Resultado de un canje o refresco. `Debug` nunca muestra los tokens.
#[derive(Clone, PartialEq, Eq)]
pub struct TokenSet {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: DateTime<Utc>,
    /// Alcances concedidos (puede ser menos de lo pedido si el usuario desmarcó casillas).
    pub scopes: Vec<String>,
    /// Datos del `id_token`, si vino uno.
    pub id_claims: Option<IdClaims>,
}

impl std::fmt::Debug for TokenSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenSet")
            .field("access_token", &"…")
            .field("refresh_token", &self.refresh_token.as_ref().map(|_| "…"))
            .field("expires_at", &self.expires_at)
            .field("scopes", &self.scopes)
            .field("id_claims", &self.id_claims)
            .finish()
    }
}

impl TokenSet {
    /// ¿Caduca dentro de `skew` (o ya caducó)?
    pub fn expires_within(&self, skew: Duration) -> bool {
        let skew = chrono::Duration::from_std(skew).unwrap_or(chrono::Duration::zero());
        Utc::now().checked_add_signed(skew).is_none_or(|t| t >= self.expires_at)
    }

    /// ¿Concedió el usuario este alcance?
    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes.iter().any(|s| s == scope)
    }
}

#[derive(Deserialize)]
struct RawToken {
    access_token: String,
    #[serde(default)]
    token_type: Option<String>,
    #[serde(default)]
    expires_in: Option<serde_json::Value>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
}

#[derive(Deserialize)]
struct RawError {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

/// Cliente HTTP para el servidor de tokens: rustls, tiempos de espera, sin redirecciones.
pub fn http_client() -> Result<reqwest::Client, Error> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("IureDesk/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| Error::Http(e.to_string()))
}

/// Canjea el código de autorización (con el verificador PKCE). En Microsoft el token sale para
/// [`TokenAudience::Mail`].
pub(crate) async fn exchange_code(
    http: &reqwest::Client,
    provider: &ProviderConfig,
    code: &str,
    redirect_uri: &str,
    verifier: &str,
) -> Result<TokenSet, Error> {
    let scope = provider.token_scopes(TokenAudience::Mail).join(" ");
    let mut form: Vec<(&str, &str)> = vec![
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("client_id", &provider.client.client_id),
        ("code_verifier", verifier),
    ];
    if let Some(s) = provider.client.client_secret.as_deref() {
        form.push(("client_secret", s));
    }
    if !scope.is_empty() {
        form.push(("scope", &scope));
    }
    post_token(http, provider, &form).await
}

/// Pide un token de acceso nuevo con el de refresco. Si el servidor no rota el de refresco, el
/// [`TokenSet`] devuelto conserva el anterior.
pub async fn refresh(
    http: &reqwest::Client,
    provider: &ProviderConfig,
    refresh_token: &str,
    audience: TokenAudience,
) -> Result<TokenSet, Error> {
    let scope = provider.token_scopes(audience).join(" ");
    let mut form: Vec<(&str, &str)> = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", &provider.client.client_id),
    ];
    if let Some(s) = provider.client.client_secret.as_deref() {
        form.push(("client_secret", s));
    }
    if !scope.is_empty() {
        form.push(("scope", &scope));
    }
    let mut set = post_token(http, provider, &form).await?;
    if set.refresh_token.is_none() {
        set.refresh_token = Some(refresh_token.to_string());
    }
    Ok(set)
}

async fn post_token(
    http: &reqwest::Client,
    provider: &ProviderConfig,
    form: &[(&str, &str)],
) -> Result<TokenSet, Error> {
    provider.validate()?;
    let mut resp = http
        .post(&provider.token_url)
        .header(reqwest::header::ACCEPT, "application/json")
        .form(form)
        .send()
        .await
        .map_err(|e| Error::Http(e.without_url().to_string()))?;
    let status = resp.status();
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| Error::Http(e.without_url().to_string()))? {
        if body.len().saturating_add(chunk.len()) > MAX_RESPONSE {
            return Err(Error::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    if !status.is_success() {
        let raw: RawError = serde_json::from_slice(&body).unwrap_or(RawError { error: None, error_description: None });
        let error = raw.error.unwrap_or_else(|| format!("http {}", status.as_u16()));
        if error == "invalid_grant" {
            return Err(Error::NeedsReconsent { provider: provider.display_name.clone() });
        }
        return Err(Error::TokenEndpoint {
            status: status.as_u16(),
            error: clip(&error),
            description: raw.error_description.map(|d| clip(&d)),
        });
    }
    parse_token_response(&body)
}

fn clip(s: &str) -> String {
    s.chars().take(300).collect()
}

/// Interpreta la respuesta JSON del servidor de tokens.
pub(crate) fn parse_token_response(body: &[u8]) -> Result<TokenSet, Error> {
    let raw: RawToken = serde_json::from_slice(body).map_err(|e| Error::InvalidResponse(e.to_string()))?;
    if raw.access_token.is_empty() {
        return Err(Error::InvalidResponse("access_token vacío".into()));
    }
    if let Some(t) = raw.token_type.as_deref()
        && !t.eq_ignore_ascii_case("bearer")
    {
        return Err(Error::InvalidResponse(format!("token_type no admitido: {}", clip(t))));
    }
    // Algunos servidores mandan `expires_in` como cadena.
    let secs = match raw.expires_in {
        Some(serde_json::Value::Number(n)) => n.as_i64(),
        Some(serde_json::Value::String(s)) => s.trim().parse().ok(),
        _ => None,
    }
    .unwrap_or(DEFAULT_LIFETIME)
    .clamp(0, MAX_LIFETIME);
    let now = Utc::now();
    let expires_at = now.checked_add_signed(chrono::Duration::seconds(secs)).unwrap_or(now);
    Ok(TokenSet {
        access_token: raw.access_token,
        refresh_token: raw.refresh_token.filter(|r| !r.is_empty()),
        expires_at,
        scopes: raw.scope.unwrap_or_default().split_whitespace().take(64).map(String::from).collect(),
        id_claims: raw.id_token.as_deref().and_then(decode_id_token),
    })
}

/// Lee los datos de un `id_token` (JWT) **sin verificar la firma**: llegó directamente del
/// servidor de tokens por TLS, lo que OpenID Connect Core §3.1.3.7 admite en lugar de la firma.
/// Solo se usa para mostrar y comprobar el correo, nunca para autorizar nada.
pub fn decode_id_token(jwt: &str) -> Option<IdClaims> {
    if jwt.len() > MAX_ID_TOKEN {
        return None;
    }
    let payload = jwt.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    serde_json::from_slice(&bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jwt(claims: &str) -> String {
        let e = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        format!("{}.{}.firma", e.encode("{\"alg\":\"RS256\"}"), e.encode(claims))
    }

    #[test]
    fn parses_full_response() {
        let body = format!(
            r#"{{"access_token":"at","token_type":"Bearer","expires_in":"3599","refresh_token":"rt","scope":"a b","id_token":"{}"}}"#,
            jwt(r#"{"sub":"1","email":"ana@example.com","email_verified":true}"#)
        );
        let t = parse_token_response(body.as_bytes()).unwrap();
        assert_eq!(t.refresh_token.as_deref(), Some("rt"));
        assert!(t.has_scope("b"));
        assert_eq!(t.id_claims.as_ref().and_then(IdClaims::best_email), Some("ana@example.com"));
        assert!(!t.expires_within(Duration::from_secs(60)));
        assert!(t.expires_within(Duration::from_secs(7200)));
        assert!(!format!("{t:?}").contains("\"at\""));
    }

    #[test]
    fn rejects_bad_responses() {
        assert!(parse_token_response(b"{}").is_err());
        assert!(parse_token_response(br#"{"access_token":""}"#).is_err());
        assert!(parse_token_response(br#"{"access_token":"x","token_type":"mac"}"#).is_err());
        let t = parse_token_response(br#"{"access_token":"x","expires_in":-5}"#).unwrap();
        assert!(t.expires_within(Duration::ZERO));
    }

    #[test]
    fn microsoft_username_fallback() {
        let c = decode_id_token(&jwt(r#"{"preferred_username":"ana@contoso.com","tid":"t"}"#)).unwrap();
        assert_eq!(c.best_email(), Some("ana@contoso.com"));
        assert_eq!(decode_id_token("basura"), None);
        assert_eq!(decode_id_token("a.%%%.c"), None);
    }

    proptest::proptest! {
        #[test]
        fn hostile_input_never_panics(s in ".{0,400}") {
            let _ = parse_token_response(s.as_bytes());
            let _ = decode_id_token(&s);
        }
    }
}
