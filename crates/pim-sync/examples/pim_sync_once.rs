//! Una pasada de sincronización PIM contra un servidor real, a un almacén temporal o al indicado.
//!
//! CalDAV/CardDAV con contraseña (iCloud con contraseña de app, Fastmail, Nextcloud…):
//! ```text
//! IUREDESK_DAV_USER=ana@icloud.com IUREDESK_DAV_PASSWORD=xxxx-xxxx-xxxx-xxxx \
//! IUREDESK_CALDAV_URL=https://caldav.icloud.com/ IUREDESK_CARDDAV_URL=https://contacts.icloud.com/ \
//! cargo run -p iuredesk-pim-sync --example pim_sync_once
//! ```
//! Microsoft Graph o Google con un token de acceso ya obtenido (audiencia API):
//! ```text
//! IUREDESK_GRAPH_TOKEN=eyJ… cargo run -p iuredesk-pim-sync --example pim_sync_once
//! IUREDESK_BEARER_TOKEN=ya29… IUREDESK_DAV_USER=ana@gmail.com IUREDESK_CALDAV_URL=… cargo run …
//! ```
//! Opcionales: `IUREDESK_STORE` (directorio del almacén), `IUREDESK_TZ` (zona, p. ej.
//! `Europe/Madrid`). Los secretos solo se leen del entorno: nunca se escriben en disco.

use std::sync::Arc;
use std::time::Duration;

use chrono::{Duration as Days, Utc};
use iuredesk_accounts::{MemoryStore, SecretPurpose, SecretStore};
use iuredesk_model::account::{Account, AccountKind, AuthMethod, OAuthProvider, Protocol, Security, ServerConfig};
use iuredesk_oauth::{BoxFuture, TokenAudience, TokenProvider};
use iuredesk_pim_sync::{
    DavAuth, GRAPH_BASE, PimAccount, PimEvent, PimSource, PimSyncEngine, PimSyncOptions, occurrences_between,
};
use iuredesk_store::Store;

/// Token fijo del entorno.
struct EnvToken(String);

impl TokenProvider for EnvToken {
    fn access_token<'a>(
        &'a self,
        _: &'a Account,
        _: TokenAudience,
    ) -> BoxFuture<'a, Result<String, iuredesk_oauth::Error>> {
        let t = self.0.clone();
        Box::pin(async move { Ok(t) })
    }
    fn invalidate(&self, _: &str) {}
}

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.trim().is_empty())
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> anyhow_like::Result {
    let user = env("IUREDESK_DAV_USER").unwrap_or_else(|| "user@example.com".into());
    let graph = env("IUREDESK_GRAPH_TOKEN");
    let bearer = env("IUREDESK_BEARER_TOKEN");
    let oauth = graph.is_some() || bearer.is_some();
    let srv = |p| ServerConfig {
        protocol: p,
        host: "example.invalid".into(),
        port: 993,
        security: Security::Tls,
        auth: if oauth { AuthMethod::OAuth2 { provider: OAuthProvider::Microsoft } } else { AuthMethod::Password },
        username: user.clone(),
    };
    let account = Account {
        id: "pim-sync-once".into(),
        display_name: user.clone(),
        email: user.clone(),
        kind: if graph.is_some() { AccountKind::Microsoft365 } else { AccountKind::Generic },
        incoming: srv(Protocol::Imap),
        outgoing: srv(Protocol::Smtp),
        caldav_url: env("IUREDESK_CALDAV_URL"),
        carddav_url: env("IUREDESK_CARDDAV_URL"),
        identities: vec![],
    };
    let source = if graph.is_some() {
        PimSource::Graph { base_url: env("IUREDESK_GRAPH_BASE").unwrap_or_else(|| GRAPH_BASE.into()) }
    } else {
        if account.caldav_url.is_none() && account.carddav_url.is_none() {
            eprintln!(
                "Falta IUREDESK_CALDAV_URL o IUREDESK_CARDDAV_URL (o IUREDESK_GRAPH_TOKEN). Ver la cabecera del ejemplo."
            );
            std::process::exit(2);
        }
        PimSource::Dav {
            caldav: account.caldav_url.clone(),
            carddav: account.carddav_url.clone(),
            auth: if bearer.is_some() { DavAuth::OAuth } else { DavAuth::Password { username: user.clone() } },
        }
    };
    let secrets = Arc::new(MemoryStore::default());
    if let Some(pw) = env("IUREDESK_DAV_PASSWORD") {
        secrets.set(&account.id, SecretPurpose::Dav, &pw).map_err(|e| e.to_string())?;
    }
    let tokens: Option<Arc<dyn TokenProvider>> =
        graph.or(bearer).map(|t| Arc::new(EnvToken(t)) as Arc<dyn TokenProvider>);
    let tmp = std::env::temp_dir().join(format!("iuredesk-pim-sync-once-{}", std::process::id()));
    let root = env("IUREDESK_STORE").map(std::path::PathBuf::from).unwrap_or(tmp);
    let store = Arc::new(Store::open(&root).map_err(|e| e.to_string())?);
    let tz: chrono_tz::Tz = env("IUREDESK_TZ").and_then(|t| t.parse().ok()).unwrap_or(chrono_tz::UTC);
    let opts = PimSyncOptions {
        user_tz: tz,
        sync_on_start: false,
        poll_interval: Duration::from_secs(3600),
        ..Default::default()
    };
    let handle = PimSyncEngine::start(Arc::clone(&store), vec![PimAccount { account, source }], tokens, secrets, opts);
    let mut events = handle.events();
    let started = std::time::Instant::now();
    let result = handle.sync_now_wait("pim-sync-once").await;
    while let Ok(e) = events.try_recv() {
        match e {
            PimEvent::Error(f) => eprintln!("aviso: {} ({})", f.es, f.detail),
            PimEvent::Conflict { summary, resolution, .. } => println!("conflicto en «{summary}»: {resolution:?}"),
            _ => {}
        }
    }
    if let Err(e) = result {
        eprintln!("error: {} — {e}", e.message(true));
        std::process::exit(1);
    }
    println!("almacén: {}  ({:.1} s)", root.display(), started.elapsed().as_secs_f32());
    for c in store.calendars().map_err(|e| e.to_string())? {
        println!(
            "calendario «{}»: {} eventos",
            c.name,
            store.events_in_calendar(c.id).map_err(|e| e.to_string())?.len()
        );
    }
    for b in store.address_books().map_err(|e| e.to_string())? {
        println!("libreta «{}»: {} contactos", b.name, store.contacts_in_book(b.id).map_err(|e| e.to_string())?.len());
    }
    for l in store.task_lists().map_err(|e| e.to_string())? {
        println!("lista «{}»: {} tareas", l.name, store.tasks(l.id).map_err(|e| e.to_string())?.len());
    }
    let now = Utc::now();
    let occ = occurrences_between(&store, &[], now, now + Days::days(14), tz).map_err(|e| e.to_string())?;
    println!("próximos 14 días: {} apariciones", occ.len());
    for o in occ.iter().take(15) {
        println!("  {}  {}", o.start.to_utc().with_timezone(&tz).format("%a %d %b %H:%M"), o.summary);
    }
    handle.shutdown().await;
    Ok(())
}

/// Resultado mínimo para `main` sin dependencias extra.
mod anyhow_like {
    pub type Result = std::result::Result<(), String>;
}
