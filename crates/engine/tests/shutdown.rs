//! Regresión (dueño 2026-11-09): al cerrar la app con una cuenta sincronizando salía
//! «thread 'iuredesk-shutdown' panicked … A Tokio 1.x context was found, but it is being
//! shutdown»: `Session::shutdown` esperaba el apagado en un hilo suelto con `block_on` y el runtime
//! de fondo se destruía mientras tanto. Cerrar no debe dejar ningún pánico.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use iuredesk_accounts::{AccountStore, MemoryStore};
use iuredesk_engine::{Session, SessionConfig, SyncStatus};
use iuredesk_model::account::{Account, AccountKind, AuthMethod, Protocol, Security, ServerConfig};

static PANICS: AtomicUsize = AtomicUsize::new(0);

fn session(dir: &std::path::Path, port: u16) -> Session {
    let server = |protocol| ServerConfig {
        protocol,
        host: "127.0.0.1".into(),
        port,
        security: Security::Plain,
        auth: AuthMethod::Password,
        username: "ana@example.com".into(),
    };
    let accounts = AccountStore::at(dir.join("accounts.json"));
    accounts
        .upsert(Account {
            id: "acct-imap".into(),
            display_name: "Ana".into(),
            email: "ana@example.com".into(),
            kind: AccountKind::Generic,
            incoming: server(Protocol::Imap),
            outgoing: server(Protocol::Smtp),
            caldav_url: None,
            carddav_url: None,
            identities: vec![],
            shared_of: None,
        })
        .unwrap();
    let store = Arc::new(iuredesk_store::Store::open(dir.join("store")).unwrap());
    Session::new(SessionConfig {
        accounts: Some(accounts),
        secrets: Arc::new(MemoryStore::default()),
        sync: Some(iuredesk_engine::iuredesk_sync::SyncOptions::default()),
        ..SessionConfig::offline(store)
    })
    .unwrap()
}

#[test]
fn closing_while_syncing_does_not_panic() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        PANICS.fetch_add(1, Ordering::SeqCst);
        previous(info);
    }));
    // Un servidor que acepta la conexión y no contesta: la sincronización queda a medias y su
    // apagado tarda, como con una cuenta real.
    let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = silent.local_addr().unwrap().port();
    for _ in 0..5 {
        let dir = tempfile::tempdir().unwrap();
        let mut s = session(dir.path(), port);
        assert_ne!(s.sync_status(), SyncStatus::Off, "la cuenta debe estar sincronizando");
        std::thread::sleep(Duration::from_millis(200));
        // Lo que hace la app al cerrar: apagar y soltar la sesión (y con ella el runtime).
        s.shutdown();
        drop(s);
    }
    // Un hilo suelto que siguiera vivo reventaría aquí.
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(PANICS.load(Ordering::SeqCst), 0, "pánico al cerrar");
    drop(silent);
}
