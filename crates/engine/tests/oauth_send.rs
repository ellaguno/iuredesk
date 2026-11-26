//! BUG-OAUTHSEND: responder desde una cuenta de Google (dueño 2026-11-26) fallaba con «esta copia
//! de IureDesk no puede pedir el permiso». Al arrancar, la bandeja de salida armaba los transportes
//! antes de que la sincronización creara el gestor de tokens OAuth, y nadie los rehacía: toda
//! cuenta de Google o Microsoft se quedaba sin poder enviar hasta agregar o quitar una cuenta.
//!
//! La prueba arranca como la aplicación (sincronización encendida, sin inyectar tokens) y envía:
//! sin token de refresco guardado, lo correcto es pedir volver a iniciar sesión, nunca
//! `OAuthUnavailable`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use iuredesk_accounts::{AccountStore, MemoryStore};
use iuredesk_engine::{Event, Session, SessionConfig, Zone};
use iuredesk_model::account::{Account, AccountKind, AuthMethod, OAuthProvider, Protocol, Security, ServerConfig};

#[test]
fn oauth_account_can_ask_for_a_token_when_sending_after_startup() {
    let dir = tempfile::tempdir().unwrap();
    // Solo salida (el entrante no es de recepción): no hace falta IMAP ni red.
    let server = ServerConfig {
        protocol: Protocol::Smtp,
        host: "127.0.0.1".into(),
        port: 9,
        security: Security::Tls,
        auth: AuthMethod::OAuth2 { provider: OAuthProvider::Google },
        username: "ana@gmail.com".into(),
    };
    let account = Account {
        id: "acct-gmail".into(),
        display_name: "Ana".into(),
        email: "ana@gmail.com".into(),
        kind: AccountKind::Gmail,
        incoming: server.clone(),
        outgoing: server,
        caldav_url: None,
        carddav_url: None,
        identities: vec![],
        shared_of: None,
    };
    let accounts = AccountStore::at(dir.path().join("accounts.json"));
    accounts.upsert(account).unwrap();
    let store = Arc::new(iuredesk_store::Store::open(dir.path().join("store")).unwrap());
    let mut s = Session::new(SessionConfig {
        accounts: Some(accounts),
        secrets: Arc::new(MemoryStore::default()),
        zone: Zone::utc(),
        sync: Some(iuredesk_engine::iuredesk_sync::SyncOptions::default()),
        ..SessionConfig::offline(store)
    })
    .unwrap();
    s.prefs.send_undo_secs = 0;
    s.apply_send_prefs();

    let mut d = s.compose_new(None);
    d.draft.set_to("bob@example.com").unwrap();
    d.draft.subject = "Respuesta".into();
    d.draft.body = iuredesk_engine::iuredesk_compose::RichBody::from_plain("Hola");
    s.send(&d).unwrap();

    let start = Instant::now();
    let error = loop {
        let err = s.poll().into_iter().find_map(|e| match e {
            Event::Notice(n) if n.text.es.contains("No se pudo enviar") || n.text.es.contains("Aún no se envió") => {
                Some(n.text.es)
            }
            _ => None,
        });
        if let Some(e) = err {
            break e;
        }
        assert!(start.elapsed() < Duration::from_secs(20), "el envío no terminó");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(!error.contains("no puede pedir el permiso"), "sin gestor de tokens: {error}");
    s.shutdown();
}
