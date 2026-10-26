//! Sincronización de punta a punta contra los servidores IMAP y POP3 falsos.

use std::sync::Arc;
use std::time::Duration;

use chrono::{TimeZone, Utc};
use iuredesk_accounts::{MemoryStore, SecretPurpose, SecretStore};
use iuredesk_imap::testing::{FakeCaps, FakeImap, PASSWORD, USER, sample_message};
use iuredesk_model::account::{Account, AccountKind, AuthMethod, Protocol, Security, ServerConfig};
use iuredesk_model::{Flags, FolderId, FolderRole, MessageId};
use iuredesk_store::Store;
use iuredesk_sync::{Phase, Pop3Policy, SyncEngine, SyncEvent, SyncHandle, SyncOptions};
use tokio::sync::broadcast::Receiver;

fn account(id: &str, protocol: Protocol, port: u16) -> Account {
    let server = |protocol| ServerConfig {
        protocol,
        host: "127.0.0.1".into(),
        port,
        security: Security::Plain,
        auth: AuthMethod::Password,
        username: USER.into(),
    };
    Account {
        id: id.into(),
        display_name: "Ana".into(),
        email: USER.into(),
        kind: AccountKind::Generic,
        incoming: server(protocol),
        outgoing: server(Protocol::Smtp),
        caldav_url: None,
        carddav_url: None,
        identities: vec![],
    }
}

fn secrets(id: &str, pass: &str) -> Arc<dyn SecretStore> {
    let s = MemoryStore::default();
    s.set(id, SecretPurpose::Incoming, pass).unwrap();
    Arc::new(s)
}

fn opts() -> SyncOptions {
    SyncOptions {
        poll_interval: Duration::from_secs(3600),
        idle: false,
        io_timeout: Duration::from_secs(20),
        ..Default::default()
    }
}

fn msg(i: u32, days_ago: i64) -> Vec<u8> {
    let date = Utc::now().fixed_offset() - chrono::Duration::days(days_ago) - chrono::Duration::minutes(i.into());
    let root = i - i % 5;
    let reply = !i.is_multiple_of(5);
    let subject = if reply { format!("Re: Tema hilo{root}") } else { format!("Tema hilo{root}") };
    let parent = format!("m{}@example.com", i.saturating_sub(1));
    sample_message(
        &format!("Remitente {} <r{}@example.com>", i % 7, i % 7),
        "Yo <yo@example.org>",
        &subject,
        &format!("m{i}@example.com"),
        reply.then_some(parent.as_str()),
        date,
        &format!("Cuerpo del mensaje numero {i} con la palabra zanahoria{i}"),
        i.is_multiple_of(50).then_some("factura.pdf"),
    )
}

struct Env {
    _dir: tempfile::TempDir,
    store: Arc<Store>,
    handle: SyncHandle,
    events: Receiver<SyncEvent>,
}

fn start(server_port: u16, protocol: Protocol, o: SyncOptions, pass: &str) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    let handle = SyncEngine::start_with(
        Arc::clone(&store),
        vec![account("t1", protocol, server_port)],
        None,
        secrets("t1", pass),
        o,
    );
    let events = handle.events();
    Env { _dir: dir, store, handle, events }
}

fn inbox(store: &Store) -> FolderId {
    let acct = store.account_labels().unwrap()[0].id;
    store.folders(acct).unwrap().into_iter().find(|f| f.role == FolderRole::Inbox).unwrap().id
}

fn folder(store: &Store, role: FolderRole) -> FolderId {
    let acct = store.account_labels().unwrap()[0].id;
    store.folders(acct).unwrap().into_iter().find(|f| f.role == role).unwrap().id
}

async fn until(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    while !f() {
        assert!(tokio::time::Instant::now() < deadline, "tiempo agotado esperando: {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn by_uid(store: &Store, f: FolderId, uid: u32) -> Option<MessageId> {
    store.message_by_remote(f, &uid.to_string()).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn initial_sync_of_5000_messages_threads_and_search() {
    let s = FakeImap::start(FakeCaps::default()).await.unwrap();
    // 5 000 mensajes repartidos en 200 días: la ventana inicial (30 días) trae ~750 y el resto
    // llega en segundo plano.
    let msgs = (0..5000u32)
        .rev()
        .map(|i| {
            let days = i64::from(i) / 25;
            let date = Utc::now().fixed_offset() - chrono::Duration::days(days) - chrono::Duration::minutes(i.into());
            (msg(i, days), if i % 3 == 0 { vec!["\\Seen".to_string()] } else { vec![] }, date)
        })
        .collect();
    s.deliver_many("INBOX", msgs);
    let started = std::time::Instant::now();
    let mut e = start(s.port(), Protocol::Imap, opts(), PASSWORD);
    e.handle.sync_now_wait("t1").await.unwrap();
    let ib = inbox(&e.store);
    let after_first = e.store.count(ib).unwrap().total;
    assert!((700..5000).contains(&after_first), "ventana inicial: {after_first}");
    until("historial completo", 180, || e.store.count(ib).unwrap().total == 5000).await;
    let took = started.elapsed();
    eprintln!("5 000 mensajes sincronizados en {took:?}");

    // Hilos: cada hilo tiene 5 mensajes.
    let id = by_uid(&e.store, ib, 1).unwrap();
    let env = e.store.get_envelope(id).unwrap().unwrap();
    let thread = e.store.thread_envelopes(env.thread.unwrap()).unwrap();
    assert_eq!(thread.len(), 5, "{:?}", thread.iter().map(|t| &t.subject).collect::<Vec<_>>());
    // Búsqueda por asunto.
    let hits = e.store.search("hilo4235", 50).unwrap();
    assert_eq!(hits.len(), 5);
    // Precarga: los más recientes de la bandeja tienen cuerpo e índice de texto.
    let newest = e.store.list_envelopes(ib, 0, 10).unwrap();
    assert!(newest.iter().all(|m| m.blob.is_some() && !m.preview.is_empty()));
    let word = newest[0].preview.split_whitespace().last().unwrap().to_string();
    assert_eq!(e.store.search(&word, 5).unwrap().len(), 1, "texto del cuerpo indexado ({word})");
    // No leídos según el servidor.
    let unread = e.store.count(ib).unwrap().unread;
    assert_eq!(unread, 5000 - 1667);
    // Adjuntos desde BODYSTRUCTURE.
    let att = e.store.get_envelope(by_uid(&e.store, ib, 5000).unwrap()).unwrap().unwrap();
    assert!(att.has_attachments && att.attachments[0].filename == "factura.pdf");
    assert!(s.max_connections() <= 4);
    // Hubo fase de historial antiguo.
    let mut saw_backfill = false;
    while let Ok(ev) = e.events.try_recv() {
        saw_backfill |= matches!(ev, SyncEvent::Progress { phase: Phase::Backfill, .. });
        assert!(!matches!(ev, SyncEvent::NewMail { .. }), "la primera sincronización no notifica");
    }
    assert!(saw_backfill);
    e.handle.shutdown().await;
}

async fn incremental(caps: FakeCaps) {
    let s = FakeImap::start(caps).await.unwrap();
    for i in 0..20 {
        s.deliver("INBOX", &msg(i, 1), &[], None);
    }
    let mut e = start(s.port(), Protocol::Imap, opts(), PASSWORD);
    e.handle.sync_now_wait("t1").await.unwrap();
    let ib = inbox(&e.store);
    assert_eq!(e.store.count(ib).unwrap().total, 20);
    while e.events.try_recv().is_ok() {}

    s.set_flags("INBOX", 3, &["\\Seen", "\\Flagged"]);
    s.expunge("INBOX", 4);
    s.expunge("INBOX", 18);
    s.deliver("INBOX", &msg(100, 0), &[], None);
    e.handle.sync_now_wait("t1").await.unwrap();

    assert_eq!(e.store.count(ib).unwrap().total, 19);
    let m3 = e.store.get_envelope(by_uid(&e.store, ib, 3).unwrap()).unwrap().unwrap();
    assert!(m3.flags.seen && m3.flags.flagged);
    assert!(by_uid(&e.store, ib, 4).is_none());
    assert!(by_uid(&e.store, ib, 18).is_none());
    assert!(by_uid(&e.store, ib, 21).is_some());
    let mut new_mail = None;
    while let Ok(ev) = e.events.try_recv() {
        if let SyncEvent::NewMail { count, sample, .. } = ev {
            new_mail = Some((count, sample));
        }
    }
    let (count, sample) = new_mail.expect("aviso de correo nuevo");
    assert_eq!(count, 1);
    assert_eq!(sample[0].subject, "Tema hilo100");
    e.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn incremental_with_qresync() {
    incremental(FakeCaps::default()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn incremental_without_extensions() {
    incremental(FakeCaps::bare()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn idle_brings_new_mail() {
    let s = FakeImap::start(FakeCaps::default()).await.unwrap();
    s.deliver("INBOX", &msg(1, 0), &[], None);
    let mut e = start(s.port(), Protocol::Imap, SyncOptions { idle: true, ..opts() }, PASSWORD);
    e.handle.sync_now_wait("t1").await.unwrap();
    // Da tiempo a que IDLE se instale.
    until("IDLE", 10, || s.log().iter().any(|l| l.contains(" IDLE"))).await;
    s.deliver("INBOX", &msg(2, 0), &[], None);
    let got = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            match e.events.recv().await {
                Ok(SyncEvent::NewMail { count, .. }) => return count,
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(_) => return 0,
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(got, 1);
    e.handle.shutdown().await;
}

async fn ops_roundtrip(caps: FakeCaps) {
    let s = FakeImap::start(caps.clone()).await.unwrap();
    for i in 0..5 {
        s.deliver("INBOX", &msg(i, 1), &[], None);
    }
    let e = start(s.port(), Protocol::Imap, opts(), PASSWORD);
    e.handle.sync_now_wait("t1").await.unwrap();
    let ib = inbox(&e.store);
    let archive = folder(&e.store, FolderRole::Archive);
    let drafts = folder(&e.store, FolderRole::Drafts);

    // Banderas: local al instante, servidor después.
    let m1 = by_uid(&e.store, ib, 1).unwrap();
    e.handle.set_flags(m1, Flags { seen: true, flagged: true, ..Default::default() }).unwrap();
    assert!(e.store.get_envelope(m1).unwrap().unwrap().flags.flagged);
    until("banderas en el servidor", 10, || {
        s.messages("INBOX").iter().any(|(u, f)| *u == 1 && f.contains(&"\\Flagged".to_string()))
    })
    .await;

    // Mover.
    let m2 = by_uid(&e.store, ib, 2).unwrap();
    e.handle.move_message(m2, archive).unwrap();
    until("movido en el servidor", 10, || s.messages("Archive").len() == 1).await;
    assert!(!s.messages("INBOX").iter().any(|(u, _)| *u == 2));
    e.handle.sync_now_wait("t1").await.unwrap();
    assert_eq!(e.store.count(archive).unwrap().total, 1, "sin duplicados tras mover");
    let moved = e.store.get_envelope(m2).unwrap().unwrap();
    assert_eq!(moved.folder, archive);
    assert_eq!(moved.remote_id.as_deref(), Some("1"), "UID del destino (COPYUID o adopción)");

    // Borrar.
    let m3 = by_uid(&e.store, ib, 3).unwrap();
    e.handle.delete_message(m3).unwrap();
    until("borrado en el servidor", 10, || !s.messages("INBOX").iter().any(|(u, _)| *u == 3)).await;

    // Subir un borrador.
    let raw = sample_message(
        "Yo <yo@example.org>",
        "Bo <bo@example.org>",
        "Borrador",
        "draft1@example.org",
        None,
        Utc.with_ymd_and_hms(2026, 10, 1, 9, 0, 0).unwrap().fixed_offset(),
        "texto",
        None,
    );
    let d = e.handle.append_message(drafts, raw, Flags { draft: true, ..Default::default() }).unwrap();
    until("borrador en el servidor", 10, || s.messages("Drafts").len() == 1).await;
    e.handle.sync_now_wait("t1").await.unwrap();
    assert_eq!(e.store.count(drafts).unwrap().total, 1, "sin duplicados tras subir");
    assert!(e.store.get_envelope(d).unwrap().unwrap().remote_id.is_some());
    assert_eq!(e.store.ops_len().unwrap(), 0);
    if !caps.move_ {
        assert!(s.log().iter().any(|l| l.contains("UID COPY")), "sin MOVE se usa COPY + EXPUNGE");
    }
    e.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ops_replay_with_move_and_uidplus() {
    ops_roundtrip(FakeCaps::default()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ops_replay_with_fallbacks() {
    ops_roundtrip(FakeCaps::bare()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn consumes_compose_ops_and_leaves_outbox() {
    use iuredesk_sync::ops::{OP_SEND, OP_SYNC, SyncOp};
    let s = FakeImap::start(FakeCaps::default()).await.unwrap();
    s.deliver("INBOX", &msg(1, 1), &[], None);
    let e = start(s.port(), Protocol::Imap, opts(), PASSWORD);
    e.handle.sync_now_wait("t1").await.unwrap();
    let acct = e.store.account_labels().unwrap()[0].id;
    let original = by_uid(&e.store, inbox(&e.store), 1).unwrap();
    let raw = sample_message(
        "Yo <yo@example.org>",
        "Ana <ana@example.com>",
        "Re: Tema hilo0",
        "sent1@example.org",
        Some("m1@example.com"),
        Utc.with_ymd_and_hms(2026, 10, 2, 9, 0, 0).unwrap().fixed_offset(),
        "respuesta",
        None,
    );
    let hash = e.store.blobs().put(&raw).unwrap();
    let append = SyncOp::AppendToSent { raw: hash, size: raw.len() as u64, message_id: "sent1@example.org".into() };
    e.store.enqueue_op(acct, OP_SEND, b"{\"de la bandeja de salida\":1}").unwrap();
    e.store.enqueue_op(acct, OP_SYNC, &append.encode()).unwrap();
    e.store.enqueue_op(acct, OP_SYNC, &append.encode()).unwrap();
    e.store.enqueue_op(acct, OP_SYNC, &SyncOp::MarkAnswered { message: original }.encode()).unwrap();
    e.handle.sync_now_wait("t1").await.unwrap();
    let sent = s.messages("Sent");
    assert_eq!(sent.len(), 1, "sin duplicar por Message-ID");
    assert!(sent[0].1.contains(&"\\Seen".to_string()));
    assert!(s.messages("INBOX")[0].1.contains(&"\\Answered".to_string()));
    let left = e.store.peek_ops(10).unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].kind, OP_SEND, "outbox.send no se toca");
    e.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pending_local_change_wins_over_server() {
    let s = FakeImap::start(FakeCaps::bare()).await.unwrap();
    for i in 0..3 {
        s.deliver("INBOX", &msg(i, 1), &[], None);
    }
    let e = start(s.port(), Protocol::Imap, opts(), PASSWORD);
    e.handle.sync_now_wait("t1").await.unwrap();
    let ib = inbox(&e.store);
    let m1 = by_uid(&e.store, ib, 1).unwrap();
    // El servidor rechazará la réplica: la operación queda pendiente.
    s.fail_next("UID STORE");
    e.handle.set_flags(m1, Flags { seen: true, ..Default::default() }).unwrap();
    until("intento fallido", 10, || e.store.peek_ops(10).unwrap().first().is_some_and(|o| o.attempts >= 1)).await;
    // Otro cliente cambia algo; la pasada no pisa la bandera local pendiente.
    s.set_flags("INBOX", 2, &["\\Flagged"]);
    e.handle.sync_now_wait("t1").await.unwrap();
    assert!(e.store.get_envelope(m1).unwrap().unwrap().flags.seen, "gana la operación local pendiente");
    let m2 = e.store.get_envelope(by_uid(&e.store, ib, 2).unwrap()).unwrap().unwrap();
    assert!(m2.flags.flagged, "en lo demás gana el servidor");
    e.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn uidvalidity_change_resyncs_folder() {
    let s = FakeImap::start(FakeCaps::default()).await.unwrap();
    for i in 0..10 {
        s.deliver("INBOX", &msg(i, 1), &[], None);
    }
    let e = start(s.port(), Protocol::Imap, opts(), PASSWORD);
    e.handle.sync_now_wait("t1").await.unwrap();
    let ib = inbox(&e.store);
    s.expunge("INBOX", 1);
    s.reset_uid_validity("INBOX");
    e.handle.sync_now_wait("t1").await.unwrap();
    assert_eq!(inbox(&e.store), ib, "la carpeta conserva su id");
    assert_eq!(e.store.count(ib).unwrap().total, 9);
    for u in 1..=9 {
        assert!(by_uid(&e.store, ib, u).is_some(), "uid {u}");
    }
    assert!(by_uid(&e.store, ib, 10).is_none());
    e.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn body_on_demand() {
    let s = FakeImap::start(FakeCaps::default()).await.unwrap();
    s.deliver("INBOX", &msg(7, 1), &[], None);
    let e = start(s.port(), Protocol::Imap, SyncOptions { prefetch_recent: 0, ..opts() }, PASSWORD);
    e.handle.sync_now_wait("t1").await.unwrap();
    let id = by_uid(&e.store, inbox(&e.store), 1).unwrap();
    assert!(e.store.get_envelope(id).unwrap().unwrap().blob.is_none());
    assert!(e.store.search("zanahoria7", 5).unwrap().is_empty());
    e.handle.fetch_body(id).await.unwrap();
    let env = e.store.get_envelope(id).unwrap().unwrap();
    assert!(env.blob.is_some());
    assert!(env.preview.contains("zanahoria7"));
    assert_eq!(e.store.search("zanahoria7", 5).unwrap().len(), 1);
    assert_eq!(e.store.get_raw(id).unwrap(), s.raw("INBOX", 1).unwrap());
    e.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wrong_password_is_reported_humanely() {
    let s = FakeImap::start(FakeCaps::default()).await.unwrap();
    let mut e = start(s.port(), Protocol::Imap, opts(), "mala");
    let r = e.handle.sync_now_wait("t1").await;
    assert!(r.is_err());
    let mut found = None;
    while let Ok(ev) = e.events.try_recv() {
        if let SyncEvent::Error(f) = ev {
            found = Some(f);
        }
    }
    let f = found.expect("evento de error");
    assert!(f.needs_reauth);
    assert!(f.es.contains("contraseña"), "{}", f.es);
    e.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pop3_incremental_with_delete_policy() {
    use iuredesk_pop3::testing::{FakePop3, PASSWORD as POP_PASS};
    let p = FakePop3::start().await.unwrap();
    for i in 0..3 {
        p.deliver(&format!("u{i}"), &msg(i, 1));
    }
    let mut o = opts();
    o.pop3_policies.insert("t1".into(), Pop3Policy { leave_on_server: false, delete_after_days: None });
    let mut e = start(p.config().port, Protocol::Pop3, o, POP_PASS);
    e.handle.sync_now_wait("t1").await.unwrap();
    let ib = inbox(&e.store);
    assert_eq!(e.store.count(ib).unwrap().total, 3);
    assert!(p.uidls().is_empty(), "sin copia en el servidor");
    while e.events.try_recv().is_ok() {}
    p.deliver("u9", &msg(9, 0));
    p.deliver("u10", &msg(10, 0));
    e.handle.sync_now_wait("t1").await.unwrap();
    assert_eq!(e.store.count(ib).unwrap().total, 5);
    let mut count = 0;
    while let Ok(ev) = e.events.try_recv() {
        if let SyncEvent::NewMail { count: c, .. } = ev {
            count = c;
        }
    }
    assert_eq!(count, 2);
    // Cuerpos completos e hilos.
    let any = e.store.list_envelopes(ib, 0, 1).unwrap();
    assert!(any[0].blob.is_some());
    e.handle.shutdown().await;
}

/// Prueba contra un servidor real: `IUREDESK_IMAP_HOST`, `IUREDESK_IMAP_USER`,
/// `IUREDESK_IMAP_PASS` (y opcional `IUREDESK_IMAP_PORT`, por omisión 993 con TLS).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "necesita un servidor IMAP real (variables IUREDESK_IMAP_*)"]
async fn live_imap() {
    let (Ok(host), Ok(user), Ok(pass)) =
        (std::env::var("IUREDESK_IMAP_HOST"), std::env::var("IUREDESK_IMAP_USER"), std::env::var("IUREDESK_IMAP_PASS"))
    else {
        eprintln!("sin variables IUREDESK_IMAP_*: se omite");
        return;
    };
    let port = std::env::var("IUREDESK_IMAP_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(993);
    let mut a = account("live", Protocol::Imap, port);
    a.incoming.host = host;
    a.incoming.username = user.clone();
    a.incoming.security = Security::Tls;
    a.email = user;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    let o = SyncOptions { backfill: false, ..opts() };
    let h = SyncEngine::start_with(Arc::clone(&store), vec![a], None, secrets("live", &pass), o);
    h.sync_now_wait("live").await.unwrap();
    let acct = store.account_labels().unwrap()[0].id;
    for f in store.folders(acct).unwrap() {
        eprintln!("{:>8}  {}", store.count(f.id).unwrap().total, f.remote_path);
    }
    h.shutdown().await;
}
