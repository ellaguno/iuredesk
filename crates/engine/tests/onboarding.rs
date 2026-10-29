//! Asistente de alta sin callejones sin salida (UX-2): cuenta repetida (sin distinguir
//! mayúsculas), fallos del navegador contra un proveedor OAuth falso (permiso negado, usuario de
//! prueba, bloqueo del administrador, `org_internal`, tiempo agotado, error del servidor de
//! tokens, sin *client id*) y «Configurar manualmente» contra IMAP y SMTP falsos.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use iuredesk_accounts::{AccountStore, MemoryStore, SecretPurpose, SecretStore};
use iuredesk_engine::onboarding::{OAuthIssue, ProbeStatus, WizardStep, known_provider_discovery, manual_setup};
use iuredesk_engine::{OfflineTransport, Session, SessionConfig, Zone};
use iuredesk_model::account::{Account, AccountKind, AuthMethod, OAuthProvider, Protocol, Security, ServerConfig};
use iuredesk_oauth::{ClientCredentials, ProviderConfig};

fn account(id: &str, email: &str) -> Account {
    let server = |protocol, host: &str, port| ServerConfig {
        protocol,
        host: host.into(),
        port,
        security: Security::Tls,
        auth: AuthMethod::OAuth2 { provider: OAuthProvider::Google },
        username: email.into(),
    };
    Account {
        id: id.into(),
        display_name: "Eduardo".into(),
        email: email.into(),
        kind: AccountKind::Gmail,
        incoming: server(Protocol::Imap, "imap.gmail.com", 993),
        outgoing: server(Protocol::Smtp, "smtp.gmail.com", 465),
        caldav_url: None,
        carddav_url: None,
        identities: Vec::new(),
    }
}

fn session(dir: &std::path::Path, existing: &[Account]) -> (Session, Arc<MemoryStore>) {
    let accounts = AccountStore::at(dir.join("accounts.json"));
    for a in existing {
        accounts.upsert(a.clone()).unwrap();
    }
    let secrets = Arc::new(MemoryStore::default());
    let store = Arc::new(iuredesk_store::Store::open(dir.join("store")).unwrap());
    let s = Session::new(SessionConfig {
        store,
        accounts: Some(accounts),
        secrets: secrets.clone(),
        prefs_path: None,
        now: None,
        zone: Zone::utc(),
        sample: false,
        sync: None,
        transport: Some(Arc::new(OfflineTransport)),
    })
    .unwrap();
    (s, secrets)
}

fn wait(s: &mut Session, what: &str, mut f: impl FnMut(&Session) -> bool) {
    let start = Instant::now();
    while !f(s) {
        assert!(start.elapsed() < Duration::from_secs(30), "tiempo agotado: {what}");
        s.poll();
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn step(s: &Session) -> Option<WizardStep> {
    s.wizard.as_ref().map(|w| w.step)
}

#[test]
fn an_existing_account_is_explained_not_a_dead_end() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, _) = session(dir.path(), &[account("acct1-x", "eduardo@llaguno.com")]);
    s.wizard_begin();
    s.wizard.as_mut().unwrap().email = "  Eduardo@LLAGUNO.com ".into();
    s.wizard_detect();
    assert_eq!(step(&s), Some(WizardStep::Exists));
    assert_eq!(s.wizard.as_ref().and_then(|w| w.existing.as_deref()), Some("acct1-x"));
    // «Volver a iniciar sesión» lleva a la tarjeta de esa cuenta.
    s.reauth_account("acct1-x");
    let w = s.wizard.as_ref().unwrap();
    assert_eq!(w.step, WizardStep::Provider);
    assert!(w.reauth.as_ref().is_some_and(|a| a.id == "acct1-x"));
    // Al volver a iniciar sesión no se considera repetida.
    assert!(!s.wizard_check_existing());
    // Otra dirección sí sigue adelante.
    s.wizard_close();
    s.wizard_begin();
    s.wizard.as_mut().unwrap().email = "otra@llaguno.com".into();
    assert!(!s.wizard_check_existing());
}

// ------------------------------------------------------------------ proveedor OAuth falso

/// Saca un parámetro de la URL de autorización (solo los que manda la app: sin `+`).
fn query_param(url: &str, key: &str) -> String {
    let q = url.split_once('?').map_or("", |(_, q)| q);
    let raw = q.split('&').find_map(|kv| kv.strip_prefix(&format!("{key}="))).unwrap_or("");
    let mut out = Vec::new();
    let b = raw.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            out.push(u8::from_str_radix(&raw[i + 1..i + 3], 16).unwrap());
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap()
}

/// «Navegador» que vuelve de la página del proveedor con `query` (más el `state` correcto).
fn browser_returning(query: &'static str) -> Arc<dyn Fn(&str) + Send + Sync> {
    Arc::new(move |authorize: &str| {
        let redirect = query_param(authorize, "redirect_uri");
        let state = query_param(authorize, "state");
        let addr = redirect.trim_start_matches("http://").trim_end_matches('/').to_string();
        std::thread::spawn(move || {
            let mut c = TcpStream::connect(addr).unwrap();
            write!(c, "GET /?{query}&state={state} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").unwrap();
            let mut sink = Vec::new();
            let _ = c.read_to_end(&mut sink);
        });
    })
}

/// Servidor de tokens falso que contesta siempre `status` con `body`.
fn token_server(status: u16, body: &'static str) -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for c in l.incoming().take(4) {
            let Ok(mut c) = c else { continue };
            let mut r = BufReader::new(c.try_clone().unwrap());
            let mut len = 0usize;
            loop {
                let mut line = String::new();
                if r.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap_or(0);
                }
            }
            let mut form = vec![0u8; len];
            let _ = r.read_exact(&mut form);
            let _ = write!(
                c,
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    format!("http://127.0.0.1:{port}/token")
}

fn fake_google(token_url: &str) -> ProviderConfig {
    ProviderConfig::custom(
        "google",
        "Google",
        "http://127.0.0.1:9/authorize",
        token_url,
        ClientCredentials::new("cid", None),
        &["openid", "email"],
    )
}

/// Asistente en la tarjeta de Google con el OAuth listo.
fn at_google_card(s: &mut Session) {
    s.wizard_begin();
    s.wizard.as_mut().unwrap().email = "ana@gmail.com".into();
    s.wizard_apply_discovery(known_provider_discovery("ana@gmail.com").unwrap());
    s.wizard.as_mut().unwrap().oauth_ready = true;
}

/// Corre el inicio de sesión y devuelve el motivo del fallo y el mensaje en español.
fn fail_with(
    cfg: Result<ProviderConfig, iuredesk_oauth::Error>,
    browser: Arc<dyn Fn(&str) + Send + Sync>,
    timeout: Duration,
) -> (OAuthIssue, String) {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, _) = session(dir.path(), &[]);
    at_google_card(&mut s);
    s.wizard_oauth_with(cfg, Some(browser), timeout);
    wait(&mut s, "fallo del OAuth", |s| step(s) != Some(WizardStep::Working));
    let w = s.wizard.as_ref().unwrap();
    // Nunca un callejón: de vuelta en la tarjeta, con el motivo y sus botones.
    assert_eq!(w.step, WizardStep::Provider);
    let msg = w.message.as_ref().map(|n| n.text.es.clone()).unwrap_or_default();
    assert!(!msg.is_empty());
    let issue = w.oauth_issue.expect("motivo del fallo");
    assert!(issue.offers_retry() || issue.offers_app_password(), "{issue:?} sin salida");
    (issue, msg)
}

const NO_TOKEN: &str = "http://127.0.0.1:9/token";

#[test]
fn oauth_access_denied() {
    let (issue, msg) =
        fail_with(Ok(fake_google(NO_TOKEN)), browser_returning("error=access_denied"), Duration::from_secs(20));
    assert_eq!(issue, OAuthIssue::Denied);
    assert!(msg.contains("No diste permiso"), "{msg}");
}

#[test]
fn oauth_test_user_403() {
    let (issue, msg) = fail_with(
        Ok(fake_google(NO_TOKEN)),
        browser_returning(
            "error=access_denied&error_description=IureDesk%20has%20not%20completed%20the%20Google%20verification%20process",
        ),
        Duration::from_secs(20),
    );
    assert_eq!(issue, OAuthIssue::TestUser);
    assert!(msg.contains("usuarios de prueba"), "{msg}");
}

#[test]
fn oauth_admin_policy_enforced() {
    let (issue, msg) =
        fail_with(Ok(fake_google(NO_TOKEN)), browser_returning("error=admin_policy_enforced"), Duration::from_secs(20));
    assert_eq!(issue, OAuthIssue::AdminBlocked);
    assert!(msg.contains("administrador"), "{msg}");
}

#[test]
fn oauth_org_internal() {
    let (issue, msg) =
        fail_with(Ok(fake_google(NO_TOKEN)), browser_returning("error=org_internal"), Duration::from_secs(20));
    assert_eq!(issue, OAuthIssue::OrgInternal);
    assert!(!issue.offers_retry() && issue.offers_app_password());
    assert!(msg.contains("otra organización"), "{msg}");
}

#[test]
fn oauth_browser_never_comes_back() {
    let (issue, msg) = fail_with(Ok(fake_google(NO_TOKEN)), Arc::new(|_: &str| {}), Duration::from_millis(300));
    assert_eq!(issue, OAuthIssue::TimedOut);
    assert!(msg.contains("Acceso bloqueado"), "{msg}");
}

#[test]
fn oauth_token_endpoint_rejects_by_policy() {
    let token = token_server(
        403,
        r#"{"error":"access_denied","error_description":"admin_policy_enforced: blocked by your administrator"}"#,
    );
    let (issue, _) = fail_with(Ok(fake_google(&token)), browser_returning("code=the-code"), Duration::from_secs(20));
    assert_eq!(issue, OAuthIssue::AdminBlocked);
}

#[test]
fn oauth_without_client_id() {
    let (issue, msg) = fail_with(
        Err(iuredesk_oauth::Error::MissingClientId { provider: "Google".into() }),
        Arc::new(|_: &str| {}),
        Duration::from_secs(5),
    );
    assert_eq!(issue, OAuthIssue::NoClientId);
    assert!(msg.contains("contraseña de app"), "{msg}");
}

#[test]
fn cancelling_the_browser_returns_to_the_card() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, _) = session(dir.path(), &[]);
    at_google_card(&mut s);
    s.wizard_oauth_with(Ok(fake_google(NO_TOKEN)), Some(Arc::new(|_: &str| {})), Duration::from_secs(60));
    assert_eq!(step(&s), Some(WizardStep::Working));
    wait(&mut s, "enlace listo", |s| s.wizard.as_ref().and_then(|w| w.oauth_link()).is_some());
    s.wizard_cancel_working();
    assert_eq!(step(&s), Some(WizardStep::Provider));
    // El resultado tardío de la cancelación se ignora.
    for _ in 0..20 {
        s.poll();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(step(&s), Some(WizardStep::Provider));
}

// ------------------------------------------------------------------ configurar manualmente

/// SMTP mínimo: acepta AUTH con `password`.
fn fake_smtp(password: &'static str) -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for c in l.incoming().take(8) {
            let Ok(mut c) = c else { continue };
            std::thread::spawn(move || {
                let mut r = BufReader::new(c.try_clone().unwrap());
                let _ = c.write_all(b"220 fake ESMTP\r\n");
                let mut line = String::new();
                loop {
                    line.clear();
                    if r.read_line(&mut line).unwrap_or(0) == 0 {
                        return;
                    }
                    let up = line.trim_end().to_ascii_uppercase();
                    let reply: &[u8] = if up.starts_with("EHLO") {
                        b"250-fake\r\n250 AUTH PLAIN\r\n"
                    } else if up.starts_with("AUTH PLAIN ") {
                        let ok = base64_contains(line.trim_end().get(11..).unwrap_or(""), password);
                        if ok { b"235 2.7.0 OK\r\n" } else { b"535 5.7.8 bad credentials\r\n" }
                    } else if up == "QUIT" {
                        let _ = c.write_all(b"221 bye\r\n");
                        return;
                    } else {
                        b"250 OK\r\n"
                    };
                    if c.write_all(reply).is_err() {
                        return;
                    }
                }
            });
        }
    });
    port
}

/// ¿El AUTH PLAIN (base64) lleva esta contraseña al final?
fn base64_contains(b64: &str, password: &str) -> bool {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut bits = 0u32;
    let mut n = 0;
    let mut out = Vec::new();
    for ch in b64.bytes().filter(|&c| c != b'=') {
        let Some(v) = T.iter().position(|&t| t == ch) else { return false };
        bits = (bits << 6) | v as u32;
        n += 6;
        if n >= 8 {
            n -= 8;
            out.push((bits >> n) as u8);
        }
    }
    out.ends_with(password.as_bytes())
}

#[test]
fn manual_setup_tests_each_server_and_lets_you_retry() {
    use iuredesk_imap::testing::{FakeCaps, FakeImap, PASSWORD, USER};
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
    let imap = rt.block_on(FakeImap::start(FakeCaps::default())).unwrap();
    let smtp = fake_smtp("smtp-secret");
    let dir = tempfile::tempdir().unwrap();
    let (mut s, secrets) = session(dir.path(), &[]);
    s.wizard_begin();
    s.wizard.as_mut().unwrap().email = USER.into();
    // Sin descubrimiento: el formulario propone mail.<dominio>.
    s.wizard_manual();
    assert_eq!(step(&s), Some(WizardStep::Manual));
    let proposed = manual_setup(USER, None);
    assert_eq!(
        s.wizard.as_ref().and_then(|w| w.manual.as_ref()).map(|m| m.incoming.host.clone()),
        Some(proposed.incoming.host)
    );
    {
        let w = s.wizard.as_mut().unwrap();
        w.password = PASSWORD.into();
        let m = w.manual.as_mut().unwrap();
        for (srv, port) in [(&mut m.incoming, imap.port()), (&mut m.outgoing, smtp)] {
            srv.host = "127.0.0.1".into();
            srv.port = port;
            srv.security = Security::Plain;
            srv.username = USER.into();
        }
        // Primero, la contraseña de salida equivocada.
        m.same_password = false;
        m.outgoing_password = "mal".into();
    }
    assert!(s.wizard.as_ref().unwrap().can_continue());
    s.wizard_manual_test();
    wait(&mut s, "prueba manual", |s| s.wizard.as_ref().and_then(|w| w.manual.as_ref()).is_some_and(|m| !m.testing()));
    let w = s.wizard.as_ref().unwrap();
    assert_eq!(w.step, WizardStep::Manual, "se queda en el formulario para corregir");
    let m = w.manual.as_ref().unwrap();
    assert_eq!(m.incoming_test, Some(ProbeStatus::Ok));
    assert!(
        matches!(&m.outgoing_test, Some(ProbeStatus::Failed(t)) if t.es.contains("contraseña")),
        "{:?}",
        m.outgoing_test
    );
    // Se corrige y se vuelve a probar sin empezar de nuevo.
    s.wizard.as_mut().unwrap().manual.as_mut().unwrap().outgoing_password = "smtp-secret".into();
    s.wizard_manual_test();
    wait(&mut s, "cuenta guardada", |s| step(s) == Some(WizardStep::Done));
    let saved = s.wizard.as_ref().and_then(|w| w.saved.clone()).unwrap();
    assert_eq!(saved.kind, AccountKind::Generic);
    assert_eq!(saved.incoming.protocol, Protocol::Imap);
    assert_eq!(secrets.get(&saved.id, SecretPurpose::Incoming).unwrap().as_deref(), Some(PASSWORD));
    assert_eq!(secrets.get(&saved.id, SecretPurpose::Outgoing).unwrap().as_deref(), Some("smtp-secret"));
    assert!(s.account(&saved.id).is_some());
    // Ya existe: un segundo alta con la misma dirección se explica.
    s.wizard_close();
    s.wizard_begin();
    s.wizard.as_mut().unwrap().email = USER.to_uppercase();
    s.wizard_detect();
    assert_eq!(step(&s), Some(WizardStep::Exists));
    drop(rt);
}

#[test]
fn manual_back_returns_to_where_you_came_from() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, _) = session(dir.path(), &[]);
    at_google_card(&mut s);
    s.wizard_manual();
    assert_eq!(step(&s), Some(WizardStep::Manual));
    // Gmail con OAuth: el formulario no copia servidores OAuth y propone mail.<dominio>.
    assert_eq!(s.wizard.as_ref().and_then(|w| w.manual.as_ref()).map(|m| m.incoming.auth), Some(AuthMethod::Password));
    s.wizard_manual_back();
    assert_eq!(step(&s), Some(WizardStep::Provider));
    // Sin tarjeta previa, «Atrás» vuelve al correo.
    s.wizard_close();
    s.wizard_begin();
    s.wizard.as_mut().unwrap().email = "ana@nada.example".into();
    s.wizard_manual();
    s.wizard_manual_back();
    assert_eq!(step(&s), Some(WizardStep::Email));
}
