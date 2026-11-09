//! Buzones compartidos de Microsoft 365 (F8.1) contra un Graph falso: se comprueba el acceso
//! antes de guardarlo (con acceso, sin acceso, no existe, falta el permiso `.Shared`), se guarda
//! como una cuenta más que apunta a su dueño, «Volver a iniciar sesión» lleva al dueño y quitar al
//! dueño se lleva sus buzones.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iuredesk_accounts::{AccountStore, MemoryStore};
use iuredesk_engine::{OfflineTransport, Session, SessionConfig, Zone};
use iuredesk_model::account::{Account, AccountKind, AuthMethod, OAuthProvider, Protocol, Security, ServerConfig};
use iuredesk_oauth::{BoxFuture, TokenAudience, TokenProvider};

fn owner() -> Account {
    let server = |protocol, port| ServerConfig {
        protocol,
        host: "outlook.office365.com".into(),
        port,
        security: Security::Tls,
        auth: AuthMethod::OAuth2 { provider: OAuthProvider::Microsoft },
        username: "ana@contoso.com".into(),
    };
    Account {
        id: "ana".into(),
        display_name: "Ana".into(),
        email: "ana@contoso.com".into(),
        kind: AccountKind::Microsoft365,
        incoming: server(Protocol::Imap, 993),
        outgoing: server(Protocol::Smtp, 587),
        caldav_url: None,
        carddav_url: None,
        identities: Vec::new(),
        shared_of: None,
    }
}

/// Tokens falsos: al buzón `permiso@…` le falta el consentimiento `.Shared`.
struct Tokens;

impl TokenProvider for Tokens {
    fn access_token<'a>(
        &'a self,
        a: &'a Account,
        aud: TokenAudience,
    ) -> BoxFuture<'a, Result<String, iuredesk_oauth::Error>> {
        assert_eq!(aud, TokenAudience::Shared);
        assert_eq!(a.shared_of.as_deref(), Some("ana"));
        let missing = a.email.starts_with("permiso@");
        Box::pin(async move {
            if missing {
                Err(iuredesk_oauth::Error::NeedsReconsent { provider: "Microsoft".into() })
            } else {
                Ok("tok".to_string())
            }
        })
    }
    fn invalidate(&self, _account_id: &str) {}
}

/// Graph falso: `ventas@` existe y hay acceso; `ajeno@` existe sin acceso; lo demás no existe.
fn graph() -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for c in l.incoming() {
            let Ok(mut c) = c else { continue };
            let mut r = BufReader::new(c.try_clone().unwrap());
            let mut first = String::new();
            let _ = r.read_line(&mut first);
            loop {
                let mut line = String::new();
                if r.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
            }
            let path = first.split_whitespace().nth(1).unwrap_or_default().to_string();
            let (status, body) = if path.starts_with("/v1.0/users/ventas@contoso.com/mailFolders/inbox") {
                (200, r#"{"id":"F-inbox"}"#)
            } else if path.starts_with("/v1.0/users/ajeno@contoso.com/") {
                (403, r#"{"error":{"code":"ErrorAccessDenied"}}"#)
            } else {
                (404, r#"{"error":{"code":"ErrorNonExistentMailbox"}}"#)
            };
            let _ = write!(
                c,
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    format!("http://127.0.0.1:{port}/v1.0")
}

fn session(dir: &std::path::Path) -> Session {
    let accounts = AccountStore::at(dir.join("accounts.json"));
    accounts.upsert(owner()).unwrap();
    Session::new(SessionConfig {
        store: Arc::new(iuredesk_store::Store::open(dir.join("store")).unwrap()),
        accounts: Some(accounts),
        secrets: Arc::new(MemoryStore::default()),
        prefs_path: None,
        now: None,
        zone: Zone::utc(),
        sample: false,
        sync: None,
        transport: Some(Arc::new(OfflineTransport)),
    })
    .unwrap()
}

/// Agrega `address` y espera el resultado: el fallo en español, o `None` si se agregó.
fn add(s: &mut Session, base: &str, address: &str) -> Option<(String, bool)> {
    s.add_shared_mailbox_with("ana", address, Arc::new(Tokens), base).unwrap();
    let start = Instant::now();
    while s.shared_add.as_ref().is_some_and(|a| a.running()) {
        assert!(start.elapsed() < Duration::from_secs(30), "tiempo agotado: {address}");
        s.poll();
        std::thread::sleep(Duration::from_millis(10));
    }
    s.shared_add.as_ref().and_then(|a| a.failure.clone()).map(|f| (f.text.es, f.reauth_owner))
}

#[test]
fn shared_mailboxes_are_checked_saved_and_follow_their_owner() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(dir.path());
    let base = graph();
    assert!(s.can_open_shared("ana"));

    // Lo que no sirve se dice antes de ir al servidor.
    assert!(s.add_shared_mailbox_with("ana", "ventas", Arc::new(Tokens), &base).is_err());
    assert!(s.add_shared_mailbox_with("ana", "ana@contoso.com", Arc::new(Tokens), &base).is_err());
    assert!(s.add_shared_mailbox_with("nadie", "ventas@contoso.com", Arc::new(Tokens), &base).is_err());

    let (msg, reauth) = add(&mut s, &base, "ajeno@contoso.com").expect("sin acceso");
    assert!(msg.contains("Acceso total") && !reauth, "{msg}");
    let (msg, _) = add(&mut s, &base, "noexiste@contoso.com").expect("no existe");
    assert!(msg.contains("No hay un buzón"), "{msg}");
    let (msg, reauth) = add(&mut s, &base, "permiso@contoso.com").expect("falta el permiso");
    assert!(reauth && msg.contains("Vuelve a iniciar sesión"), "{msg}");
    assert_eq!(s.accounts_overview().iter().filter(|r| !r.id.is_empty()).count(), 1, "nada guardado");

    assert_eq!(add(&mut s, &base, " Ventas@Contoso.com "), None);
    assert!(s.shared_add.as_ref().is_some_and(|a| a.done()));
    s.close_shared_add();
    let shared = s.shared_mailboxes_of("ana");
    assert_eq!(shared.len(), 1);
    let v = shared[0].clone();
    assert_eq!(v.email, "ventas@contoso.com");
    assert_eq!(v.shared_of.as_deref(), Some("ana"));
    assert!(!s.can_open_shared(&v.id), "un buzón compartido no abre otros");
    // Guardado en `accounts.json`.
    let saved = AccountStore::at(dir.path().join("accounts.json")).load().unwrap();
    assert!(saved.iter().any(|a| a.id == v.id && a.shared_of.as_deref() == Some("ana")));
    // Ya está: no se agrega dos veces.
    assert!(s.add_shared_mailbox_with("ana", "ventas@contoso.com", Arc::new(Tokens), &base).is_err());

    // «Volver a iniciar sesión» en el buzón compartido es volver a entrar con Ana.
    s.reauth_account(&v.id);
    assert!(s.wizard.as_ref().and_then(|w| w.reauth.as_ref()).is_some_and(|a| a.id == "ana"));
    s.wizard_close();

    // Quitar a Ana se lleva su buzón compartido.
    s.remove_account("ana").unwrap();
    assert!(s.accounts_overview().iter().all(|r| r.id.is_empty()));
    assert!(AccountStore::at(dir.path().join("accounts.json")).load().unwrap().is_empty());
}
