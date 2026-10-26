//! Flujo OAuth completo contra un servidor de autorización falso (HTTP en 127.0.0.1): canje con
//! PKCE, `state` que no coincide, `access_denied`, tiempo de espera, cancelación, refresco con
//! rotación, caché e `invalid_grant`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use iuredesk_accounts::{MemoryStore, SecretPurpose, SecretStore};
use iuredesk_model::account::{Account, AuthMethod, OAuthProvider, Protocol, Security, ServerConfig};
use iuredesk_oauth::{
    CancellationToken, ClientCredentials, Error, ProviderConfig, TokenAudience, TokenManager, TokenProvider,
    http_client, start_login,
};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Default)]
struct FakeState {
    /// `code_challenge` que mandó la app en la URL de autorización.
    challenge: Option<String>,
    /// Formularios recibidos en `/token`.
    requests: Vec<HashMap<String, String>>,
    invalid_grant: bool,
    /// Rotar el token de refresco en cada refresco (como Microsoft).
    rotate: bool,
    expires_in: i64,
    counter: u32,
}

type Shared = Arc<Mutex<FakeState>>;

fn id_token(email: &str) -> String {
    format!(
        "{}.{}.x",
        URL_SAFE_NO_PAD.encode("{\"alg\":\"none\"}"),
        URL_SAFE_NO_PAD.encode(format!("{{\"sub\":\"42\",\"email\":\"{email}\"}}"))
    )
}

fn token_reply(st: &mut FakeState, form: &HashMap<String, String>) -> (u16, String) {
    let get = |k: &str| form.get(k).map(String::as_str).unwrap_or("");
    if get("client_id") != "cid" {
        return (401, r#"{"error":"invalid_client"}"#.into());
    }
    match get("grant_type") {
        "authorization_code" => {
            let expected = st.challenge.clone().unwrap_or_default();
            let got = URL_SAFE_NO_PAD.encode(Sha256::digest(get("code_verifier").as_bytes()));
            if get("code") != "the-code" || got != expected || get("redirect_uri").is_empty() {
                return (400, r#"{"error":"invalid_grant","error_description":"bad code or verifier"}"#.into());
            }
            (
                200,
                format!(
                    r#"{{"access_token":"at-0","token_type":"Bearer","expires_in":{},"refresh_token":"rt-0","scope":"openid email mail","id_token":"{}"}}"#,
                    st.expires_in,
                    id_token("ana@example.com")
                ),
            )
        }
        "refresh_token" => {
            if st.invalid_grant {
                return (
                    400,
                    r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#.into(),
                );
            }
            st.counter += 1;
            let n = st.counter;
            let rt = if st.rotate { format!(r#","refresh_token":"rt-{n}""#) } else { String::new() };
            (200, format!(r#"{{"access_token":"at-{n}","token_type":"bearer","expires_in":3600{rt}}}"#))
        }
        _ => (400, r#"{"error":"unsupported_grant_type"}"#.into()),
    }
}

/// Servidor de tokens falso: atiende `POST /token` con `application/x-www-form-urlencoded`.
async fn fake_server() -> (u16, Shared) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let shared: Shared = Arc::new(Mutex::new(FakeState { expires_in: 3600, ..Default::default() }));
    let st = shared.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = listener.accept().await else { return };
            let st = st.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let (head_end, len) = loop {
                    let n = s.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..i]).to_ascii_lowercase();
                        let len = head
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        break (i + 4, len);
                    }
                };
                while buf.len() < head_end + len {
                    let n = s.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                let first = String::from_utf8_lossy(&buf).lines().next().unwrap_or("").to_string();
                let body = &buf[head_end..];
                let form: HashMap<String, String> = url::form_urlencoded::parse(body).into_owned().collect();
                let (status, json) = if first.starts_with("POST /token ") {
                    let mut g = st.lock().unwrap();
                    g.requests.push(form.clone());
                    token_reply(&mut g, &form)
                } else {
                    (404, "{}".into())
                };
                let resp = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
                    json.len()
                );
                let _ = s.write_all(resp.as_bytes()).await;
                let _ = s.shutdown().await;
            });
        }
    });
    (port, shared)
}

fn provider(port: u16) -> ProviderConfig {
    ProviderConfig::custom(
        "fake",
        "Fake",
        format!("http://127.0.0.1:{port}/authorize"),
        format!("http://127.0.0.1:{port}/token"),
        ClientCredentials::new("cid", None),
        &["openid", "email", "mail"],
    )
}

fn query(url: &str) -> HashMap<String, String> {
    url::Url::parse(url).unwrap().query_pairs().into_owned().collect()
}

#[tokio::test]
async fn full_flow_exchanges_code_with_pkce() {
    let (port, shared) = fake_server().await;
    let pending = start_login(&provider(port), None).await.unwrap();
    let http = http_client().unwrap();
    let cancel = CancellationToken::new();
    let q = query(pending.authorize_url());
    shared.lock().unwrap().challenge = Some(q["code_challenge"].clone());
    let redirect = pending.redirect_uri().to_string();
    let finish = tokio::spawn({
        let http = http.clone();
        async move { pending.finish(&http, Duration::from_secs(10), &cancel).await }
    });

    // Ruido que el receptor debe ignorar: una conexión muda y el favicon.
    let _idle = tokio::net::TcpStream::connect(redirect.trim_start_matches("http://")).await.unwrap();
    let favicon = reqwest::get(format!("{redirect}/favicon.ico")).await.unwrap();
    assert_eq!(favicon.status(), 404);

    let resp = reqwest::get(format!("{redirect}/?code=the-code&state={}&scope=x", q["state"])).await.unwrap();
    assert_eq!(resp.status(), 200);
    assert!(resp.text().await.unwrap().contains("Ya puedes volver a IureDesk"));

    let tokens = finish.await.unwrap().unwrap();
    assert_eq!(tokens.access_token, "at-0");
    assert_eq!(tokens.refresh_token.as_deref(), Some("rt-0"));
    assert_eq!(tokens.id_claims.as_ref().and_then(|c| c.best_email()), Some("ana@example.com"));
    assert!(tokens.has_scope("mail"));
    let req = shared.lock().unwrap().requests[0].clone();
    assert_eq!(req["redirect_uri"], redirect);
    assert!(!req.contains_key("client_secret"));
    assert!(!req.contains_key("scope"));

    // El puerto del receptor ya está cerrado.
    assert!(tokio::net::TcpStream::connect(redirect.trim_start_matches("http://")).await.is_err());

    // Lo guarda el gestor: refresco en el llavero, acceso en memoria.
    let secrets = Arc::new(MemoryStore::default());
    let mgr = TokenManager::new(http, secrets.clone()).with_provider(OAuthProvider::Google, provider(port));
    mgr.store_login("acc1", &tokens).await.unwrap();
    assert_eq!(secrets.get("acc1", SecretPurpose::OAuthRefresh).unwrap().as_deref(), Some("rt-0"));
    assert_eq!(mgr.access_token(&account(), TokenAudience::Mail).await.unwrap(), "at-0");
    assert_eq!(shared.lock().unwrap().requests.len(), 1, "el token vigente sale de la caché");
}

#[tokio::test]
async fn state_mismatch_is_rejected() {
    let (port, shared) = fake_server().await;
    let pending = start_login(&provider(port), None).await.unwrap();
    let redirect = pending.redirect_uri().to_string();
    let finish = tokio::spawn(async move {
        pending.finish(&http_client().unwrap(), Duration::from_secs(10), &CancellationToken::new()).await
    });
    let resp = reqwest::get(format!("{redirect}/?code=the-code&state=forjado")).await.unwrap();
    assert_eq!(resp.status(), 400);
    assert_eq!(finish.await.unwrap().unwrap_err(), Error::StateMismatch);
    assert!(shared.lock().unwrap().requests.is_empty(), "no se canjea nada");
}

#[tokio::test]
async fn access_denied_is_reported() {
    let (port, _shared) = fake_server().await;
    let pending = start_login(&provider(port), None).await.unwrap();
    let q = query(pending.authorize_url());
    let redirect = pending.redirect_uri().to_string();
    let finish = tokio::spawn(async move {
        pending.finish(&http_client().unwrap(), Duration::from_secs(10), &CancellationToken::new()).await
    });
    let resp = reqwest::get(format!(
        "{redirect}/?error=access_denied&error_description=The%20user%20denied&state={}",
        q["state"]
    ))
    .await
    .unwrap();
    assert_eq!(resp.status(), 400);
    let err = finish.await.unwrap().unwrap_err();
    assert_eq!(err, Error::AccessDenied("The user denied".into()));
    assert!(err.message(true).contains("No diste permiso"));
}

#[tokio::test]
async fn bad_verifier_needs_reconsent() {
    let (port, shared) = fake_server().await;
    let pending = start_login(&provider(port), None).await.unwrap();
    let q = query(pending.authorize_url());
    shared.lock().unwrap().challenge = Some("otro-desafio".into());
    let redirect = pending.redirect_uri().to_string();
    let finish = tokio::spawn(async move {
        pending.finish(&http_client().unwrap(), Duration::from_secs(10), &CancellationToken::new()).await
    });
    reqwest::get(format!("{redirect}/?code=the-code&state={}", q["state"])).await.unwrap();
    assert!(matches!(finish.await.unwrap(), Err(Error::NeedsReconsent { .. })));
}

#[tokio::test]
async fn timeout_and_cancel() {
    let (port, _shared) = fake_server().await;
    let http = http_client().unwrap();
    let pending = start_login(&provider(port), None).await.unwrap();
    let r = pending.finish(&http, Duration::from_millis(200), &CancellationToken::new()).await;
    assert_eq!(r.unwrap_err(), Error::Timeout);

    let pending = start_login(&provider(port), None).await.unwrap();
    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        c2.cancel();
    });
    let r = pending.finish(&http, Duration::from_secs(30), &cancel).await;
    assert_eq!(r.unwrap_err(), Error::Cancelled);
}

fn account() -> Account {
    let server = |protocol| ServerConfig {
        protocol,
        host: "imap.example.com".into(),
        port: 993,
        security: Security::Tls,
        auth: AuthMethod::OAuth2 { provider: OAuthProvider::Google },
        username: "ana@example.com".into(),
    };
    Account {
        id: "acc1".into(),
        display_name: "Ana".into(),
        email: "ana@example.com".into(),
        kind: Default::default(),
        incoming: server(Protocol::Imap),
        outgoing: server(Protocol::Smtp),
        caldav_url: None,
        carddav_url: None,
        identities: vec![],
    }
}

#[tokio::test]
async fn refresh_rotates_and_caches_per_audience() {
    let (port, shared) = fake_server().await;
    shared.lock().unwrap().rotate = true;
    let mut cfg = provider(port);
    cfg.mail_scopes = vec!["https://outlook.office.com/IMAP.AccessAsUser.All".into(), "offline_access".into()];
    cfg.api_scopes = vec!["https://graph.microsoft.com/Mail.ReadWrite".into(), "offline_access".into()];
    let secrets = Arc::new(MemoryStore::default());
    secrets.set("acc1", SecretPurpose::OAuthRefresh, "rt-0").unwrap();
    let mgr = TokenManager::new(http_client().unwrap(), secrets.clone()).with_provider(OAuthProvider::Google, cfg);
    let acc = account();

    let mail = mgr.access_token(&acc, TokenAudience::Mail).await.unwrap();
    assert_eq!(mail, "at-1");
    assert_eq!(secrets.get("acc1", SecretPurpose::OAuthRefresh).unwrap().as_deref(), Some("rt-1"));
    // Misma audiencia: caché.
    assert_eq!(mgr.access_token(&acc, TokenAudience::Mail).await.unwrap(), "at-1");
    // Otra audiencia: otro token con otros alcances, usando el refresco rotado.
    assert_eq!(mgr.access_token(&acc, TokenAudience::Api).await.unwrap(), "at-2");
    {
        let st = shared.lock().unwrap();
        assert_eq!(st.requests.len(), 2);
        assert!(st.requests[0]["scope"].contains("IMAP.AccessAsUser.All"));
        assert_eq!(st.requests[1]["refresh_token"], "rt-1");
        assert!(st.requests[1]["scope"].contains("graph.microsoft.com"));
    }
    // Tras un 401 del servidor de correo: se invalida y se pide otro.
    mgr.invalidate("acc1");
    assert_eq!(mgr.access_token(&acc, TokenAudience::Mail).await.unwrap(), "at-3");

    // Peticiones simultáneas tras invalidar: un solo refresco.
    mgr.invalidate("acc1");
    let (a, b) = tokio::join!(mgr.access_token(&acc, TokenAudience::Mail), mgr.access_token(&acc, TokenAudience::Mail));
    assert_eq!(a.unwrap(), b.unwrap());
    assert_eq!(shared.lock().unwrap().requests.len(), 4);
}

#[tokio::test]
async fn expiring_token_is_refreshed_with_skew() {
    let (port, shared) = fake_server().await;
    shared.lock().unwrap().expires_in = 30; // menos que el margen por omisión (120 s)
    let pending = start_login(&provider(port), None).await.unwrap();
    let q = query(pending.authorize_url());
    shared.lock().unwrap().challenge = Some(q["code_challenge"].clone());
    let redirect = pending.redirect_uri().to_string();
    let http = http_client().unwrap();
    let h2 = http.clone();
    let finish =
        tokio::spawn(async move { pending.finish(&h2, Duration::from_secs(10), &CancellationToken::new()).await });
    reqwest::get(format!("{redirect}/?code=the-code&state={}", q["state"])).await.unwrap();
    let tokens = finish.await.unwrap().unwrap();

    let mgr =
        TokenManager::new(http, Arc::new(MemoryStore::default())).with_provider(OAuthProvider::Google, provider(port));
    mgr.store_login("acc1", &tokens).await.unwrap();
    // Caduca dentro del margen: se refresca (sin rotación se conserva el refresco anterior).
    assert_eq!(mgr.access_token(&account(), TokenAudience::Mail).await.unwrap(), "at-1");
    assert_eq!(shared.lock().unwrap().requests[1]["refresh_token"], "rt-0");
}

#[tokio::test]
async fn invalid_grant_needs_reconsent_and_clears_refresh_token() {
    let (port, shared) = fake_server().await;
    shared.lock().unwrap().invalid_grant = true;
    let secrets = Arc::new(MemoryStore::default());
    secrets.set("acc1", SecretPurpose::OAuthRefresh, "revocado").unwrap();
    let mgr =
        TokenManager::new(http_client().unwrap(), secrets.clone()).with_provider(OAuthProvider::Google, provider(port));

    let err = mgr.access_token(&account(), TokenAudience::Mail).await.unwrap_err();
    assert_eq!(err, Error::NeedsReconsent { provider: "Fake".into() });
    assert!(err.needs_reconsent());
    assert!(err.message(true).contains("Vuelve a iniciar sesión"));
    assert!(err.message(false).contains("Sign in again"));
    assert_eq!(secrets.get("acc1", SecretPurpose::OAuthRefresh).unwrap(), None);

    // Siguiente intento: ni siquiera llama al servidor.
    let again = mgr.access_token(&account(), TokenAudience::Mail).await.unwrap_err();
    assert_eq!(again, Error::NoRefreshToken);
    assert!(again.needs_reconsent());
    assert_eq!(shared.lock().unwrap().requests.len(), 1);
}

#[tokio::test]
async fn non_oauth_and_unknown_provider() {
    let mgr = TokenManager::new(http_client().unwrap(), Arc::new(MemoryStore::default()));
    let mut acc = account();
    assert!(matches!(mgr.access_token(&acc, TokenAudience::Mail).await, Err(Error::UnknownProvider(_))));
    acc.incoming.auth = AuthMethod::Password;
    acc.outgoing.auth = AuthMethod::AppPassword;
    assert_eq!(mgr.access_token(&acc, TokenAudience::Mail).await.unwrap_err(), Error::NotOAuthAccount);
}

#[tokio::test]
async fn hostile_callbacks_do_not_break_the_listener() {
    let (port, shared) = fake_server().await;
    let pending = start_login(&provider(port), None).await.unwrap();
    let q = query(pending.authorize_url());
    shared.lock().unwrap().challenge = Some(q["code_challenge"].clone());
    let addr = pending.redirect_uri().trim_start_matches("http://").to_string();
    let redirect = pending.redirect_uri().to_string();
    let finish = tokio::spawn(async move {
        pending.finish(&http_client().unwrap(), Duration::from_secs(10), &CancellationToken::new()).await
    });
    // Basura binaria y una cabecera enorme: se ignoran.
    let mut s = tokio::net::TcpStream::connect(&addr).await.unwrap();
    s.write_all(&[0xff; 64]).await.unwrap();
    s.write_all(b"\r\n\r\n").await.unwrap();
    let mut s2 = tokio::net::TcpStream::connect(&addr).await.unwrap();
    let _ = s2.write_all(&vec![b'a'; 20_000]).await;
    let resp = reqwest::get(format!("{redirect}/?code=the-code&state={}", q["state"])).await.unwrap();
    assert_eq!(resp.status(), 200);
    assert!(finish.await.unwrap().is_ok());
}
