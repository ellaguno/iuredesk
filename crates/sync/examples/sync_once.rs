//! Sincroniza una cuenta IMAP real en un almacén temporal e imprime carpetas y tiempos.
//!
//! ```text
//! IUREDESK_IMAP_PASS=… cargo run -p iuredesk-sync --example sync_once -- ana@example.com
//! ```
//!
//! Variables: `IUREDESK_IMAP_PASS` (obligatoria; nunca la guardes en el repositorio),
//! `IUREDESK_IMAP_HOST` (por omisión `imap.<dominio>`), `IUREDESK_IMAP_USER` (por omisión el
//! correo), `IUREDESK_IMAP_PORT` (993, TLS), `IUREDESK_FULL=1` para bajar también todo el
//! historial antiguo.

use std::sync::Arc;
use std::time::{Duration, Instant};

use iuredesk_accounts::{MemoryStore, SecretPurpose, SecretStore};
use iuredesk_model::account::{Account, AccountKind, AuthMethod, Protocol, Security, ServerConfig};
use iuredesk_store::Store;
use iuredesk_sync::{SyncEngine, SyncEvent, SyncOptions};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let email = std::env::args().nth(1).ok_or("uso: sync_once <correo>")?;
    let pass = std::env::var("IUREDESK_IMAP_PASS").map_err(|_| "falta IUREDESK_IMAP_PASS")?;
    let domain = email.rsplit('@').next().unwrap_or("").to_string();
    let host = std::env::var("IUREDESK_IMAP_HOST").unwrap_or_else(|_| format!("imap.{domain}"));
    let user = std::env::var("IUREDESK_IMAP_USER").unwrap_or_else(|_| email.clone());
    let port = std::env::var("IUREDESK_IMAP_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(993);
    let full = std::env::var("IUREDESK_FULL").is_ok_and(|v| v == "1");
    let server = |protocol, port, security| ServerConfig {
        protocol,
        host: host.clone(),
        port,
        security,
        auth: AuthMethod::Password,
        username: user.clone(),
    };
    let account = Account {
        id: "example".into(),
        display_name: email.clone(),
        email: email.clone(),
        kind: AccountKind::Generic,
        incoming: server(Protocol::Imap, port, if port == 143 { Security::StartTls } else { Security::Tls }),
        outgoing: server(Protocol::Smtp, 465, Security::Tls),
        caldav_url: None,
        carddav_url: None,
        identities: vec![],
    };
    let secrets = MemoryStore::default();
    secrets.set("example", SecretPurpose::Incoming, &pass)?;
    let dir = std::env::temp_dir().join(format!("iuredesk-sync-once-{}", std::process::id()));
    let store = Arc::new(Store::open(&dir)?);
    println!("almacén temporal: {}", dir.display());
    let opts = SyncOptions { idle: false, poll_interval: Duration::from_secs(24 * 3600), ..Default::default() };
    let started = Instant::now();
    let handle = SyncEngine::start_with(Arc::clone(&store), vec![account], None, Arc::new(secrets), opts);
    let mut events = handle.events();
    let r = handle.sync_now_wait("example").await;
    let first = started.elapsed();
    if let Err(e) = &r {
        eprintln!("error: {}", e.message(true));
    }
    if full && r.is_ok() {
        // Espera a que termine el historial antiguo.
        loop {
            match tokio::time::timeout(Duration::from_secs(600), events.recv()).await {
                Ok(Ok(SyncEvent::Synced { backfill_pending: false, .. })) => break,
                Ok(Ok(SyncEvent::Error(f))) => eprintln!("aviso: {}", f.es),
                Ok(Ok(_)) | Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
                _ => break,
            }
            if store.folders(store.account_labels()?.first().map(|a| a.id).unwrap_or_default())?.iter().all(|f| {
                let k = iuredesk_sync::state::backfill_key(f.account, f.id);
                iuredesk_sync::state::load::<iuredesk_sync::state::BackfillState>(&store, &k).is_none_or(|b| b.done)
            }) {
                break;
            }
        }
    }
    let total = started.elapsed();
    if let Some(acct) = store.account_labels()?.first().map(|a| a.id) {
        let mut sum = 0u64;
        for f in store.folders(acct)? {
            let c = store.count(f.id)?;
            sum = sum.saturating_add(c.total);
            println!("{:>8} mensajes {:>6} sin leer  {} ({:?})", c.total, c.unread, f.remote_path, f.role);
        }
        println!("total: {sum} mensajes");
    }
    println!("primera pasada: {first:.2?}; total: {total:.2?}");
    handle.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
    r.map_err(|e| e.into())
}
