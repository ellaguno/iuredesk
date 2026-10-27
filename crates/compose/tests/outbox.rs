//! Bandeja de salida (deshacer, programado, reintentos) con reloj falso, y SMTP real contra un
//! servidor falso (tokio, sin TLS): éxito, 4xx con reintento, 5xx permanente y credenciales
//! rechazadas.

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{TimeZone, Utc};
use iuredesk_accounts::{MemoryStore, SecretPurpose, SecretStore};
use iuredesk_compose::draft::{OriginalKind, OriginalRef};
use iuredesk_compose::ops::{OP_SEND, OP_SYNC};
use iuredesk_compose::outbox::Outbox;
use iuredesk_compose::{
    ComposeError, Draft, FsResolver, Lang, ManualClock, OutboxConfig, OutboxState, OutgoingMessage, RichBody,
    SendError, SendErrorKind, SendOutcome, SendReceipt, SmtpTransport, SyncOp, Transport,
};
use iuredesk_model::account::{Account, AccountKind, AuthMethod, Identity, Protocol, Security, ServerConfig};
use iuredesk_model::{AccountId, Folder, FolderRole, MessageEnvelope};
use iuredesk_oauth::BoxFuture;
use iuredesk_store::Store;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

const ACCT: AccountId = AccountId(1);

fn draft() -> Draft {
    let mut d =
        Draft::new(Identity { name: "Ana".into(), email: "ana@acme.mx".into(), signature: None, reply_to: None });
    d.set_to("bob@example.com").unwrap();
    d.set_bcc("oculto@example.com").unwrap();
    d.subject = "Hola".into();
    d.body = RichBody::from_plain("Qué tal.");
    d
}

/// Transporte falso: devuelve los resultados programados en orden (por omisión, éxito).
#[derive(Default)]
struct FakeTransport {
    results: Mutex<VecDeque<Result<(), SendError>>>,
    sent: Mutex<Vec<OutgoingMessage>>,
    saves: bool,
}

impl Transport for FakeTransport {
    fn send<'a>(&'a self, msg: &'a OutgoingMessage) -> BoxFuture<'a, Result<SendReceipt, SendError>> {
        Box::pin(async move {
            let r = self.results.lock().unwrap().pop_front().unwrap_or(Ok(()));
            if r.is_ok() {
                self.sent.lock().unwrap().push(msg.clone());
            }
            r.map(|_| SendReceipt::default())
        })
    }
    fn saves_to_sent(&self) -> bool {
        self.saves
    }
}

struct Env {
    _dir: tempfile::TempDir,
    store: Arc<Store>,
    clock: ManualClock,
    outbox: Outbox,
    transport: Arc<FakeTransport>,
    source: HashMap<AccountId, Arc<dyn Transport>>,
}

fn env(undo: u64, saves: bool) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    let clock = ManualClock::new(Utc.with_ymd_and_hms(2026, 10, 26, 16, 0, 0).unwrap());
    let cfg = OutboxConfig { undo_window: Duration::from_secs(undo), ..Default::default() };
    let outbox = Outbox::new(store.clone(), Arc::new(clock.clone()), cfg).unwrap();
    let transport = Arc::new(FakeTransport { saves, ..Default::default() });
    let mut source: HashMap<AccountId, Arc<dyn Transport>> = HashMap::new();
    source.insert(ACCT, transport.clone());
    Env { _dir: dir, store, clock, outbox, transport, source }
}

fn sync_ops(store: &Store) -> Vec<SyncOp> {
    store
        .peek_ops(100)
        .unwrap()
        .into_iter()
        .filter(|o| o.kind == OP_SYNC)
        .filter_map(|o| SyncOp::decode(&o.payload))
        .collect()
}

#[tokio::test]
async fn undo_window_and_cancel() {
    let e = env(10, false);
    let id = e.outbox.queue(ACCT, &draft(), &FsResolver).unwrap();
    assert_eq!(e.outbox.entries().unwrap().len(), 1);
    // Dentro de la ventana no sale nada.
    assert!(e.outbox.process_due(&e.source).await.unwrap().is_empty());
    e.clock.advance(Duration::from_secs(5));
    assert!(e.outbox.process_due(&e.source).await.unwrap().is_empty());
    // Deshacer devuelve el borrador.
    let back = e.outbox.cancel(id).unwrap();
    assert_eq!(back.subject, "Hola");
    assert!(e.outbox.entries().unwrap().is_empty());
    assert_eq!(e.store.ops_len().unwrap(), 0);
    e.clock.advance(Duration::from_secs(60));
    assert!(e.outbox.process_due(&e.source).await.unwrap().is_empty());
    assert!(e.transport.sent.lock().unwrap().is_empty());
    assert!(matches!(e.outbox.cancel(id), Err(ComposeError::NotCancellable)));
}

#[tokio::test]
async fn sends_after_window_and_queues_sync_ops() {
    let e = env(10, false);
    // Original en el almacén, para marcarlo como respondido.
    let folder = e
        .store
        .put_folder(&Folder { account: ACCT, name: "INBOX".into(), role: FolderRole::Inbox, ..Default::default() })
        .unwrap();
    let env_ = MessageEnvelope { account: ACCT, folder, subject: "x".into(), date: Utc::now(), ..Default::default() };
    let orig = e.store.put_message(env_, b"Subject: x\r\n\r\nx").unwrap();
    let mut d = draft();
    d.original = Some(OriginalRef { message: Some(orig), kind: OriginalKind::Reply });

    let id = e.outbox.queue(ACCT, &d, &FsResolver).unwrap();
    e.clock.advance(Duration::from_secs(10));
    let out = e.outbox.process_due(&e.source).await.unwrap();
    let mid = e.outbox.entries().unwrap().first().map(|x| x.message_id.clone());
    assert!(mid.is_none());
    assert!(matches!(&out[..], [SendOutcome::Sent { id: i, .. }] if *i == id), "{out:?}");
    let sent = e.transport.sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].recipients, vec!["bob@example.com", "oculto@example.com"]);
    assert!(!String::from_utf8_lossy(&sent[0].raw).contains("oculto"));
    // Fuera de la cola de envío; dentro, las operaciones para sync.
    assert!(e.store.peek_ops(100).unwrap().iter().all(|o| o.kind != OP_SEND));
    let ops = sync_ops(&e.store);
    assert!(matches!(&ops[0], SyncOp::AppendToSent { raw, .. } if e.store.blobs().get(raw).unwrap() == sent[0].raw));
    assert_eq!(ops[1], SyncOp::MarkAnswered { message: orig });
    assert!(e.store.get_envelope(orig).unwrap().unwrap().flags.answered);
    assert!(matches!(e.outbox.cancel(id), Err(ComposeError::NotCancellable)));
}

#[tokio::test]
async fn provider_that_saves_sent_gets_no_append() {
    let e = env(0, true);
    e.outbox.queue(ACCT, &draft(), &FsResolver).unwrap();
    let out = e.outbox.process_due(&e.source).await.unwrap();
    assert!(matches!(&out[..], [SendOutcome::Sent { .. }]));
    assert!(sync_ops(&e.store).is_empty());
}

#[tokio::test]
async fn scheduled_send() {
    let e = env(10, false);
    let mut d = draft();
    let at = e.clock.now_utc() + chrono::Duration::hours(1);
    d.send_at = Some(at);
    let id = e.outbox.queue(ACCT, &d, &FsResolver).unwrap();
    let entry = e.outbox.entry(id).unwrap().unwrap();
    assert!(entry.scheduled);
    assert_eq!(e.outbox.next_due().unwrap(), Some(at));
    e.clock.advance(Duration::from_secs(30 * 60));
    assert!(e.outbox.process_due(&e.source).await.unwrap().is_empty());
    e.clock.advance(Duration::from_secs(30 * 60));
    let out = e.outbox.process_due(&e.source).await.unwrap();
    assert!(matches!(&out[..], [SendOutcome::Sent { .. }]));
    assert_eq!(e.outbox.next_due().unwrap(), None);

    // Un programado se puede cancelar aunque haya pasado la ventana.
    let id = e.outbox.queue(ACCT, &d, &FsResolver).unwrap();
    e.clock.advance(Duration::from_secs(60));
    assert!(e.outbox.cancel(id).is_ok());
}

#[tokio::test]
async fn transient_errors_back_off_then_succeed() {
    let e = env(0, false);
    e.transport.results.lock().unwrap().extend([
        Err(SendError::new(SendErrorKind::Network, "sin red")),
        Err(SendError::new(SendErrorKind::TemporarilyRejected, "451")),
    ]);
    let id = e.outbox.queue(ACCT, &draft(), &FsResolver).unwrap();
    let out = e.outbox.process_due(&e.source).await.unwrap();
    let SendOutcome::Retrying { at, .. } = &out[0] else { panic!("{out:?}") };
    assert_eq!(*at, e.clock.now_utc() + chrono::Duration::seconds(30));
    let entry = e.outbox.entry(id).unwrap().unwrap();
    assert_eq!(entry.attempts, 1);
    assert!(entry.last_error.unwrap().message(Lang::Es).contains("reintentará"));
    e.clock.advance(Duration::from_secs(29));
    assert!(e.outbox.process_due(&e.source).await.unwrap().is_empty());
    e.clock.advance(Duration::from_secs(1));
    let out = e.outbox.process_due(&e.source).await.unwrap();
    let SendOutcome::Retrying { at, .. } = &out[0] else { panic!("{out:?}") };
    assert_eq!(*at, e.clock.now_utc() + chrono::Duration::seconds(60));
    e.clock.advance(Duration::from_secs(60));
    let out = e.outbox.process_due(&e.source).await.unwrap();
    assert!(matches!(&out[..], [SendOutcome::Sent { .. }]));
}

#[tokio::test]
async fn gives_up_after_max_attempts() {
    let mut e = env(0, false);
    e.outbox = Outbox::new(
        e.store.clone(),
        Arc::new(e.clock.clone()),
        OutboxConfig { undo_window: Duration::ZERO, max_attempts: 2, ..Default::default() },
    )
    .unwrap();
    e.transport.results.lock().unwrap().extend((0..5).map(|_| Err(SendError::new(SendErrorKind::Timeout, "t"))));
    let id = e.outbox.queue(ACCT, &draft(), &FsResolver).unwrap();
    e.outbox.process_due(&e.source).await.unwrap();
    e.clock.advance(Duration::from_secs(3600));
    let out = e.outbox.process_due(&e.source).await.unwrap();
    assert!(matches!(&out[..], [SendOutcome::Failed { .. }]));
    assert_eq!(e.outbox.entry(id).unwrap().unwrap().state, OutboxState::Failed);
}

#[tokio::test]
async fn permanent_error_keeps_message_until_retry() {
    let e = env(0, false);
    e.transport.results.lock().unwrap().push_back(Err(SendError::new(SendErrorKind::Rejected, "554 5.7.1 spam")));
    let id = e.outbox.queue(ACCT, &draft(), &FsResolver).unwrap();
    let out = e.outbox.process_due(&e.source).await.unwrap();
    assert!(matches!(&out[..], [SendOutcome::Failed { .. }]));
    let entry = e.outbox.entry(id).unwrap().unwrap();
    assert_eq!(entry.state, OutboxState::Failed);
    let err = entry.last_error.unwrap();
    assert!(err.message(Lang::Es).contains("rechazó"));
    assert!(err.message(Lang::En).contains("rejected"));
    // Sigue ahí aunque pase el tiempo…
    e.clock.advance(Duration::from_secs(86_400));
    assert!(e.outbox.process_due(&e.source).await.unwrap().is_empty());
    assert_eq!(e.outbox.entries().unwrap().len(), 1);
    // …hasta que el usuario lo reintenta.
    e.outbox.retry(id).unwrap();
    let out = e.outbox.process_due(&e.source).await.unwrap();
    assert!(matches!(&out[..], [SendOutcome::Sent { .. }]));
}

#[tokio::test]
async fn missing_transport_is_permanent_and_recovery_after_crash() {
    let e = env(0, false);
    let id = e.outbox.queue(AccountId(99), &draft(), &FsResolver).unwrap();
    let out = e.outbox.process_due(&e.source).await.unwrap();
    assert!(matches!(&out[..], [SendOutcome::Failed { error, .. }] if error.kind == SendErrorKind::Config));
    // Simula un cierre a mitad de envío.
    let mut entry = e.outbox.entry(id).unwrap().unwrap();
    entry.state = OutboxState::Sending;
    e.store.put_sync_state(&format!("outbox/{id:020}"), &serde_json::to_vec(&entry).unwrap()).unwrap();
    let reopened = Outbox::new(e.store.clone(), Arc::new(e.clock.clone()), OutboxConfig::default()).unwrap();
    assert_eq!(reopened.entry(id).unwrap().unwrap().state, OutboxState::Waiting);
}

#[tokio::test]
async fn queue_rejects_invalid_and_snapshots_files() {
    let e = env(5, false);
    let mut bad = draft();
    bad.to.clear();
    bad.bcc.clear();
    assert!(matches!(e.outbox.queue(ACCT, &bad, &FsResolver), Err(ComposeError::NoRecipients)));
    // El archivo adjunto se copia al encolar: borrarlo después no impide el envío.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nota.txt");
    std::fs::write(&path, "contenido").unwrap();
    let mut d = draft();
    d.attachments.push(iuredesk_compose::Attachment::file(&path));
    e.outbox.queue(ACCT, &d, &FsResolver).unwrap();
    std::fs::remove_file(&path).unwrap();
    e.clock.advance(Duration::from_secs(5));
    let out = e.outbox.process_due(&e.source).await.unwrap();
    assert!(matches!(&out[..], [SendOutcome::Sent { .. }]), "{out:?}");
    let raw = String::from_utf8_lossy(&e.transport.sent.lock().unwrap()[0].raw).into_owned();
    assert!(raw.contains("nota.txt"));
}

trait NowUtc {
    fn now_utc(&self) -> chrono::DateTime<Utc>;
}
impl NowUtc for ManualClock {
    fn now_utc(&self) -> chrono::DateTime<Utc> {
        iuredesk_compose::Clock::now(self)
    }
}

// ---------------------------------------------------------------------------------------------
// Servidor SMTP falso
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Ok,
    Utf8,
    TempFail,
    PermFail,
    AuthFail,
}

#[derive(Debug, Default, Clone)]
struct Received {
    auth: String,
    from: String,
    rcpts: Vec<String>,
    data: String,
}

struct FakeSmtp {
    addr: SocketAddr,
    mode: Arc<Mutex<Mode>>,
    received: Arc<Mutex<Vec<Received>>>,
}

async fn fake_smtp(mode: Mode) -> FakeSmtp {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mode = Arc::new(Mutex::new(mode));
    let received = Arc::new(Mutex::new(Vec::new()));
    let (m, r) = (mode.clone(), received.clone());
    tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = listener.accept().await else { return };
            let (m, r) = (m.clone(), r.clone());
            tokio::spawn(async move {
                let (rd, mut wr) = sock.into_split();
                let mut rd = BufReader::new(rd);
                let mut cur = Received::default();
                wr.write_all(b"220 fake.test ESMTP\r\n").await.ok();
                let mut line = String::new();
                loop {
                    line.clear();
                    if rd.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    let cmd = line.trim_end().to_string();
                    let upper = cmd.to_ascii_uppercase();
                    let mode = *m.lock().unwrap();
                    let reply: String = if upper.starts_with("EHLO") && mode == Mode::Utf8 {
                        "250-fake.test\r\n250-AUTH PLAIN LOGIN XOAUTH2\r\n250-SMTPUTF8\r\n250 8BITMIME\r\n".into()
                    } else if upper.starts_with("EHLO") {
                        "250-fake.test\r\n250-AUTH PLAIN LOGIN XOAUTH2\r\n250 8BITMIME\r\n".into()
                    } else if upper.starts_with("AUTH") {
                        cur.auth = cmd.clone();
                        if mode == Mode::AuthFail {
                            "535 5.7.8 Authentication credentials invalid\r\n".into()
                        } else {
                            "235 2.7.0 Accepted\r\n".into()
                        }
                    } else if upper.starts_with("MAIL FROM") {
                        cur.from = cmd.clone();
                        "250 2.1.0 OK\r\n".into()
                    } else if upper.starts_with("RCPT TO") {
                        cur.rcpts.push(cmd.clone());
                        "250 2.1.5 OK\r\n".into()
                    } else if upper == "DATA" {
                        wr.write_all(b"354 Go ahead\r\n").await.ok();
                        let mut data = String::new();
                        loop {
                            let mut l = String::new();
                            if rd.read_line(&mut l).await.unwrap_or(0) == 0 {
                                return;
                            }
                            if l == ".\r\n" {
                                break;
                            }
                            data.push_str(&l);
                        }
                        cur.data = data;
                        match mode {
                            Mode::TempFail => "451 4.3.0 Try again later\r\n".into(),
                            Mode::PermFail => "554 5.7.1 Message rejected as spam\r\n".into(),
                            _ => {
                                r.lock().unwrap().push(std::mem::take(&mut cur));
                                "250 2.0.0 OK queued as 42\r\n".into()
                            }
                        }
                    } else if upper == "QUIT" {
                        wr.write_all(b"221 2.0.0 Bye\r\n").await.ok();
                        return;
                    } else {
                        "250 OK\r\n".into()
                    };
                    if wr.write_all(reply.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    FakeSmtp { addr, mode, received }
}

fn smtp_account(port: u16) -> Account {
    let server = |protocol| ServerConfig {
        protocol,
        host: "127.0.0.1".into(),
        port,
        security: Security::Plain,
        auth: AuthMethod::Password,
        username: "ana@acme.mx".into(),
    };
    Account {
        id: "acct-smtp".into(),
        display_name: "Ana".into(),
        email: "ana@acme.mx".into(),
        kind: AccountKind::Generic,
        incoming: server(Protocol::Imap),
        outgoing: server(Protocol::Smtp),
        caldav_url: None,
        carddav_url: None,
        identities: vec![],
    }
}

fn smtp_transport(port: u16) -> SmtpTransport {
    let secrets = Arc::new(MemoryStore::default());
    secrets.set("acct-smtp", SecretPurpose::Outgoing, "s3creta").unwrap();
    SmtpTransport::new(smtp_account(port), secrets, None).with_timeout(Duration::from_secs(10))
}

fn built() -> OutgoingMessage {
    let b = iuredesk_compose::build_message(&draft(), &FsResolver, &iuredesk_compose::BuildOptions::now()).unwrap();
    OutgoingMessage::from(&b)
}

#[tokio::test]
async fn smtp_success() {
    let srv = fake_smtp(Mode::Ok).await;
    let t = smtp_transport(srv.addr.port());
    assert!(!t.saves_to_sent());
    let receipt = t.send(&built()).await.unwrap();
    assert!(receipt.server_reply.contains("queued as 42"));
    let got = srv.received.lock().unwrap().clone();
    assert_eq!(got.len(), 1);
    assert!(got[0].auth.starts_with("AUTH PLAIN"));
    assert!(got[0].from.contains("<ana@acme.mx>"));
    assert_eq!(got[0].rcpts.len(), 2);
    assert!(got[0].rcpts[1].contains("oculto@example.com"));
    assert!(got[0].data.contains("Subject: Hola"));
    assert!(!got[0].data.contains("oculto"));
}

/// Regresión F2.5: partes locales UTF-8 (RFC 6531) con `SMTPUTF8` si el servidor lo anuncia; si
/// no, error claro en vez de mandarlo mal.
#[tokio::test]
async fn smtputf8_local_parts() {
    let mut d = draft();
    d.to = vec![iuredesk_compose::parse_address("José <josé@acme.mx>").unwrap()];
    d.bcc.clear();
    let b = iuredesk_compose::build_message(&d, &FsResolver, &iuredesk_compose::BuildOptions::now()).unwrap();
    assert_eq!(b.recipients, vec!["josé@acme.mx".to_string()]);
    let msg = OutgoingMessage::from(&b);

    let srv = fake_smtp(Mode::Utf8).await;
    smtp_transport(srv.addr.port()).send(&msg).await.unwrap();
    let got = srv.received.lock().unwrap().clone();
    assert!(got[0].from.contains("SMTPUTF8"), "{:?}", got[0].from);
    assert!(got[0].rcpts[0].contains("josé@acme.mx"));
    assert!(got[0].data.contains("josé@acme.mx"));

    let old = fake_smtp(Mode::Ok).await;
    let err = smtp_transport(old.addr.port()).send(&msg).await.unwrap_err();
    assert_eq!(err.kind, SendErrorKind::InternationalUnsupported, "{err:?}");
    assert!(!err.is_transient());
    assert!(err.message(iuredesk_compose::Lang::Es).contains("acentos"));
}

#[tokio::test]
async fn smtp_4xx_is_transient_and_outbox_retries() {
    let srv = fake_smtp(Mode::TempFail).await;
    let err = smtp_transport(srv.addr.port()).send(&built()).await.unwrap_err();
    assert_eq!(err.kind, SendErrorKind::TemporarilyRejected, "{err:?}");
    assert_eq!(err.code, Some(451));
    assert!(err.is_transient());

    // Con la bandeja de salida: falla, espera, el servidor se recupera y sale.
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    let clock = ManualClock::new(Utc.with_ymd_and_hms(2026, 10, 26, 16, 0, 0).unwrap());
    let outbox =
        Outbox::new(store, Arc::new(clock.clone()), OutboxConfig { undo_window: Duration::ZERO, ..Default::default() })
            .unwrap();
    let mut source: HashMap<AccountId, Arc<dyn Transport>> = HashMap::new();
    source.insert(ACCT, Arc::new(smtp_transport(srv.addr.port())));
    outbox.queue(ACCT, &draft(), &FsResolver).unwrap();
    let out = outbox.process_due(&source).await.unwrap();
    assert!(matches!(&out[..], [SendOutcome::Retrying { .. }]), "{out:?}");
    *srv.mode.lock().unwrap() = Mode::Ok;
    clock.advance(Duration::from_secs(30));
    let out = outbox.process_due(&source).await.unwrap();
    assert!(matches!(&out[..], [SendOutcome::Sent { .. }]), "{out:?}");
    assert_eq!(srv.received.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn smtp_5xx_is_permanent() {
    let srv = fake_smtp(Mode::PermFail).await;
    let err = smtp_transport(srv.addr.port()).send(&built()).await.unwrap_err();
    assert_eq!(err.kind, SendErrorKind::Rejected, "{err:?}");
    assert!(!err.is_transient());
    assert!(err.message(Lang::Es).contains("spam"));
}

#[tokio::test]
async fn smtp_auth_failure() {
    let srv = fake_smtp(Mode::AuthFail).await;
    let err = smtp_transport(srv.addr.port()).send(&built()).await.unwrap_err();
    assert_eq!(err.kind, SendErrorKind::Auth, "{err:?}");
    assert!(err.message(Lang::Es).contains("contraseña"));
    assert!(srv.received.lock().unwrap().is_empty());
}

#[tokio::test]
async fn smtp_unreachable_and_missing_password() {
    // Puerto cerrado: error de red (transitorio).
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l);
    let err = smtp_transport(port).send(&built()).await.unwrap_err();
    assert!(err.is_transient(), "{err:?}");
    // Sin contraseña en el llavero: error de configuración, sin conectar.
    let t = SmtpTransport::new(smtp_account(port), Arc::new(MemoryStore::default()), None);
    let err = t.send(&built()).await.unwrap_err();
    assert_eq!(err.kind, SendErrorKind::Config);
}
