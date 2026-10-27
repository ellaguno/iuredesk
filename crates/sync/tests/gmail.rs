//! Gmail con `X-GM-EXT-1` contra el IMAP falso en modo Gmail: etiquetas como carpetas, un solo
//! mensaje local por `X-GM-MSGID`, hilos por `X-GM-THRID`, archivar = quitar `\Inbox`, etiquetas
//! puestas/quitadas desde aquí y desde «otro cliente». También NOTIFY y extractos parciales.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use iuredesk_accounts::{MemoryStore, SecretPurpose, SecretStore};
use iuredesk_imap::testing::{FakeCaps, FakeImap, PASSWORD, USER, sample_message};
use iuredesk_model::account::{Account, AccountKind, AuthMethod, Protocol, Security, ServerConfig};
use iuredesk_model::{FolderId, MessageId};
use iuredesk_store::Store;
use iuredesk_sync::{SyncEngine, SyncEvent, SyncOptions};

fn account(kind: AccountKind, port: u16) -> Account {
    let server = |protocol| ServerConfig {
        protocol,
        host: "127.0.0.1".into(),
        port,
        security: Security::Plain,
        auth: AuthMethod::Password,
        username: USER.into(),
    };
    Account {
        id: "g1".into(),
        display_name: "Ana".into(),
        email: USER.into(),
        kind,
        incoming: server(Protocol::Imap),
        outgoing: server(Protocol::Smtp),
        caldav_url: None,
        carddav_url: None,
        identities: vec![],
    }
}

fn secrets() -> Arc<dyn SecretStore> {
    let s = MemoryStore::default();
    s.set("g1", SecretPurpose::Incoming, PASSWORD).unwrap();
    Arc::new(s)
}

fn opts() -> SyncOptions {
    SyncOptions { poll_interval: Duration::from_secs(3600), idle: false, ..Default::default() }
}

fn raw(i: u32, reply_to: Option<u32>) -> Vec<u8> {
    let parent = reply_to.map(|p| format!("g{p}@example.com"));
    sample_message(
        "Bea <bea@example.com>",
        "Ana <ana@example.com>",
        &format!("Asunto {i}"),
        &format!("g{i}@example.com"),
        parent.as_deref(),
        Utc::now().fixed_offset() - chrono::Duration::minutes(i64::from(10 - i)),
        &format!("Cuerpo {i}"),
        None,
    )
}

fn folder(store: &Store, path: &str) -> FolderId {
    let acct = store.account_labels().unwrap()[0].id;
    store.folders(acct).unwrap().into_iter().find(|f| f.remote_path == path).unwrap().id
}

fn by_mid(store: &Store, f: FolderId, mid: &str) -> Option<MessageId> {
    store.list_envelopes(f, 0, 1000).unwrap().into_iter().find(|e| e.message_id.as_deref() == Some(mid)).map(|e| e.id)
}

async fn until(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while !f() {
        assert!(tokio::time::Instant::now() < deadline, "tiempo agotado esperando: {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gmail_labels_dedupe_threads_and_label_ops() {
    let s = FakeImap::start(FakeCaps::gmail()).await.unwrap();
    s.remove_mailbox("Archive");
    s.add_mailbox("[Gmail]/All Mail", Some("\\All"));
    s.add_mailbox("Trabajo", None);
    let a = s.deliver_gmail(&raw(1, None), &["\\Inbox", "Trabajo"], &[], Some(77));
    let b = s.deliver_gmail(&raw(2, Some(1)), &["\\Inbox"], &[], Some(77));
    let c = s.deliver_gmail(&raw(3, None), &["Trabajo"], &["\\Seen"], None);

    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    let h = SyncEngine::start_with(
        Arc::clone(&store),
        vec![account(AccountKind::Gmail, s.port())],
        None,
        secrets(),
        opts(),
    );
    h.sync_now_wait("g1").await.unwrap();

    let (inbox, work, all, trash) = (
        folder(&store, "INBOX"),
        folder(&store, "Trabajo"),
        folder(&store, "[Gmail]/All Mail"),
        folder(&store, "Trash"),
    );
    // Un mensaje con varias etiquetas es UN mensaje local.
    assert_eq!(store.message_count().unwrap(), 3);
    assert_eq!(store.count(inbox).unwrap().total, 2);
    assert_eq!(store.count(work).unwrap().total, 2);
    assert_eq!(store.count(all).unwrap().total, 3);
    let ma = by_mid(&store, inbox, "g1@example.com").unwrap();
    let mb = by_mid(&store, inbox, "g2@example.com").unwrap();
    let mc = by_mid(&store, work, "g3@example.com").unwrap();
    assert_eq!(by_mid(&store, work, "g1@example.com"), Some(ma));
    assert_eq!(store.memberships(ma).unwrap().len(), 3);
    let ea = store.get_envelope(ma).unwrap().unwrap();
    assert_eq!(ea.keywords, vec!["Trabajo".to_string()], "solo etiquetas de usuario");
    assert_eq!(ea.thread, store.get_envelope(mb).unwrap().unwrap().thread, "X-GM-THRID une el hilo");
    let acct = store.account_labels().unwrap()[0].id;
    assert!(h.uses_labels(acct));

    // Archivar = quitar \Inbox (sigue en «Todos» y en «Trabajo»).
    h.move_message(ma, all).unwrap();
    assert_eq!(store.count(inbox).unwrap().total, 1);
    assert_eq!(store.count(all).unwrap().total, 3);
    until("archivado en el servidor", || !s.gmail_labels(a).contains(&"\\Inbox".to_string())).await;
    assert!(s.gmail_labels(a).contains(&"Trabajo".to_string()));

    // Poner y quitar etiquetas desde aquí.
    h.add_label(mc, inbox).unwrap();
    until("etiqueta puesta", || s.gmail_labels(c).contains(&"\\Inbox".to_string())).await;
    h.remove_label(mc, work).unwrap();
    until("etiqueta quitada", || !s.gmail_labels(c).contains(&"Trabajo".to_string())).await;
    assert!(store.get_envelope(mc).unwrap().is_some());

    // Cambios de «otro cliente»: B gana «Trabajo»; C pierde todo (queda en «Todos»).
    s.set_gmail_labels(b, &["\\Inbox", "Trabajo"]);
    s.set_gmail_labels(c, &[]);
    h.sync_now_wait("g1").await.unwrap();
    assert_eq!(store.message_count().unwrap(), 3, "sin duplicados");
    assert_eq!(by_mid(&store, work, "g2@example.com"), Some(mb));
    assert_eq!(by_mid(&store, inbox, "g3@example.com"), None);
    assert_eq!(by_mid(&store, all, "g3@example.com"), Some(mc));

    // A la Papelera: movimiento real, pierde las etiquetas.
    h.move_message(mb, trash).unwrap();
    until("en la papelera", || s.messages("INBOX").is_empty() && s.messages("Trash").len() == 1).await;
    h.sync_now_wait("g1").await.unwrap();
    let eb = store.get_envelope(mb).unwrap().unwrap();
    assert_eq!(eb.folder, trash);
    assert_eq!(store.memberships(mb).unwrap().len(), 1);
    assert_eq!(store.count(work).unwrap().total, 1);
    h.shutdown().await;
}

/// Sin `X-GM-EXT-1` se comporta como antes (regresión): una copia por carpeta.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plain_imap_keeps_one_copy_per_folder() {
    let s = FakeImap::start(FakeCaps::default()).await.unwrap();
    s.deliver("INBOX", &raw(1, None), &[], None);
    s.deliver("Archive", &raw(1, None), &[], None);
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    let h = SyncEngine::start_with(
        Arc::clone(&store),
        vec![account(AccountKind::Generic, s.port())],
        None,
        secrets(),
        opts(),
    );
    h.sync_now_wait("g1").await.unwrap();
    assert_eq!(store.message_count().unwrap(), 2);
    let acct = store.account_labels().unwrap()[0].id;
    assert!(!h.uses_labels(acct));
    h.shutdown().await;
}

/// NOTIFY: un cambio en otra carpeta llega por el IDLE de la bandeja de entrada; los mensajes sin
/// cuerpo reciben su extracto por descarga parcial.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn notify_on_other_folders_and_partial_previews() {
    let s = FakeImap::start(FakeCaps { notify: true, ..FakeCaps::default() }).await.unwrap();
    s.deliver("INBOX", &raw(1, None), &[], None);
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    let o = SyncOptions { idle: true, prefetch_recent: 0, ..opts() };
    let h =
        SyncEngine::start_with(Arc::clone(&store), vec![account(AccountKind::Generic, s.port())], None, secrets(), o);
    let mut ev = h.events();
    h.sync_now_wait("g1").await.unwrap();
    let inbox = folder(&store, "INBOX");
    let e1 = store.list_envelopes(inbox, 0, 10).unwrap().remove(0);
    assert!(e1.blob.is_none());
    assert_eq!(e1.preview, "Cuerpo 1", "extracto por BODY.PEEK[TEXT]<0.2048>");
    assert!(s.log().iter().any(|l| l.contains("NOTIFY SET")));

    let arch = folder(&store, "Archive");
    tokio::time::sleep(Duration::from_millis(300)).await;
    s.deliver("Archive", &raw(5, None), &[], None);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        assert!(tokio::time::Instant::now() < deadline, "NOTIFY no trajo el cambio de Archive");
        if let Ok(Ok(SyncEvent::FolderUpdated { folder, .. })) =
            tokio::time::timeout(Duration::from_millis(200), ev.recv()).await
            && folder == arch
            && store.count(arch).unwrap().total == 1
        {
            break;
        }
        if store.count(arch).unwrap().total == 1 {
            break;
        }
    }
    h.shutdown().await;
}
