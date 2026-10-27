//! De punta a punta contra un IMAP falso (el de `iuredesk-imap`) y un SMTP falso: la sesión
//! arranca la sincronización con una cuenta guardada, baja la bandeja, abre un cuerpo bajo demanda,
//! archiva (llega al servidor) y envía por la bandeja de salida con SMTP.
//!
//! `cargo test -p iuredesk-engine --test live -- --ignored`

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iuredesk_accounts::{AccountStore, MemoryStore, SecretPurpose, SecretStore};
use iuredesk_engine::{Event, FolderSel, Outcome, Session, SessionConfig, SyncStatus, Zone};
use iuredesk_imap::testing::{FakeCaps, FakeImap, PASSWORD, USER, sample_message};
use iuredesk_model::FolderRole;
use iuredesk_model::account::{Account, AccountKind, AuthMethod, Protocol, Security, ServerConfig};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

/// SMTP mínimo que acepta todo y guarda lo recibido.
async fn fake_smtp() -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let got = Arc::new(Mutex::new(Vec::new()));
    let g = Arc::clone(&got);
    tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            let g = Arc::clone(&g);
            tokio::spawn(async move {
                let (rd, mut wr) = sock.into_split();
                let mut rd = BufReader::new(rd);
                wr.write_all(b"220 fake ESMTP\r\n").await.ok();
                let mut line = String::new();
                loop {
                    line.clear();
                    if rd.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    let up = line.trim_end().to_ascii_uppercase();
                    let reply = if up.starts_with("EHLO") {
                        "250-fake\r\n250-AUTH PLAIN LOGIN\r\n250 8BITMIME\r\n".to_string()
                    } else if up.starts_with("AUTH") {
                        "235 2.7.0 OK\r\n".into()
                    } else if up == "DATA" {
                        wr.write_all(b"354 go\r\n").await.ok();
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
                        g.lock().unwrap().push(data);
                        "250 2.0.0 queued\r\n".into()
                    } else if up == "QUIT" {
                        wr.write_all(b"221 bye\r\n").await.ok();
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
    (addr, got)
}

fn wait(s: &mut Session, what: &str, secs: u64, mut f: impl FnMut(&mut Session, &[Event]) -> bool) {
    let start = Instant::now();
    loop {
        let evs = s.poll();
        if f(s, &evs) {
            return;
        }
        assert!(start.elapsed() < Duration::from_secs(secs), "tiempo agotado: {what} ({:?})", s.sync_status());
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
#[ignore = "levanta servidores IMAP y SMTP locales; corre con --ignored"]
fn live_account_syncs_reads_archives_and_sends() {
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let imap = rt.block_on(FakeImap::start(FakeCaps::default())).unwrap();
    imap.add_mailbox("Archive", Some("\\Archive"));
    imap.add_mailbox("Sent", Some("\\Sent"));
    imap.add_mailbox("Drafts", Some("\\Drafts"));
    let date = chrono::Utc::now().fixed_offset() - chrono::Duration::hours(2);
    for i in 0..5u32 {
        let raw = sample_message(
            &format!("Remitente {i} <r{i}@example.com>"),
            USER,
            &format!("Asunto {i}"),
            &format!("m{i}@example.com"),
            None,
            date - chrono::Duration::minutes(i.into()),
            &format!("Cuerpo completo del mensaje {i} con la palabra zanahoria{i}"),
            None,
        );
        imap.deliver("INBOX", &raw, &[], Some(date));
    }
    let (smtp, received) = rt.block_on(fake_smtp());

    let dir = tempfile::tempdir().unwrap();
    let server = |protocol, port| ServerConfig {
        protocol,
        host: "127.0.0.1".into(),
        port,
        security: Security::Plain,
        auth: AuthMethod::Password,
        username: USER.into(),
    };
    let account = Account {
        id: "acct-live-test".into(),
        display_name: "Ana".into(),
        email: USER.into(),
        kind: AccountKind::Generic,
        incoming: server(Protocol::Imap, imap.port()),
        outgoing: server(Protocol::Smtp, smtp.port()),
        caldav_url: None,
        carddav_url: None,
        identities: vec![],
    };
    let accounts = AccountStore::at(dir.path().join("accounts.json"));
    accounts.upsert(account).unwrap();
    let secrets = MemoryStore::default();
    secrets.set("acct-live-test", SecretPurpose::Incoming, PASSWORD).unwrap();
    let store = Arc::new(iuredesk_store::Store::open(dir.path().join("store")).unwrap());
    let opts = iuredesk_engine::iuredesk_sync::SyncOptions {
        idle: false,
        poll_interval: Duration::from_secs(3600),
        prefetch_recent: 0,
        io_timeout: Duration::from_secs(20),
        ..Default::default()
    };
    let mut s = Session::new(SessionConfig {
        accounts: Some(accounts),
        secrets: Arc::new(secrets),
        zone: Zone::utc(),
        sync: Some(opts),
        ..SessionConfig::offline(store)
    })
    .unwrap();
    s.prefs.mark_read_delay_ms = 0;
    s.prefs.send_undo_secs = 0;
    s.apply_send_prefs();
    assert!(s.is_live());

    // 1. La bandeja llega (sin cuerpos: precarga desactivada).
    wait(&mut s, "bandeja sincronizada", 30, |s, _| s.sync_status() == SyncStatus::Idle && s.rows().len() == 5);
    let id = s.rows()[0].id;
    assert!(s.store().get_envelope(id).unwrap().unwrap().blob.is_none(), "el cuerpo aún no está");

    // 2. Abrir baja el cuerpo bajo demanda y marca como leído en el servidor.
    s.set_selection(vec![id], Some(id));
    wait(&mut s, "cuerpo descargado", 20, |s, _| s.body(id).is_some_and(|b| b.text.contains("Cuerpo completo")));
    wait(&mut s, "\\Seen en el servidor", 20, |_, _| {
        imap.messages("INBOX").iter().any(|(_, f)| f.iter().any(|x| x == "\\Seen"))
    });

    // 3. Archivar llega al servidor.
    let Outcome::Notice(_) = s.run("mail.archive").unwrap() else { panic!("aviso") };
    wait(&mut s, "movido en el servidor", 20, |_, _| imap.messages("Archive").len() == 1);
    assert_eq!(imap.messages("INBOX").len(), 4);

    // 4. Enviar por SMTP con la bandeja de salida.
    let mut d = s.compose_new(None);
    d.draft.set_to("bob@example.com").unwrap();
    d.draft.subject = "Hola desde la prueba".into();
    d.draft.body = iuredesk_engine::iuredesk_compose::RichBody::from_plain("Va por SMTP");
    s.send(&d).unwrap();
    wait(&mut s, "enviado", 20, |_, evs| {
        evs.iter().any(|e| matches!(e, Event::Notice(n) if n.text.es == "Mensaje enviado"))
    });
    assert!(received.lock().unwrap().iter().any(|m| m.contains("Hola desde la prueba")));
    // Copia en Enviados (local al momento; el servidor la recibe por la cola).
    s.select_folder(FolderSel::Unified(FolderRole::Sent));
    wait(&mut s, "copia en Enviados", 20, |s, _| !s.rows().is_empty());
    wait(&mut s, "copia en el servidor", 20, |_, _| !imap.messages("Sent").is_empty());

    s.shutdown();
    drop(rt);
}
