//! Cliente IMAP contra el servidor falso (texto plano en 127.0.0.1).

use std::time::Duration;

use chrono::{TimeZone, Utc};
use iuredesk_imap::testing::{FakeCaps, FakeImap, PASSWORD, TOKEN, USER, sample_message};
use iuredesk_imap::{
    IdleEvent, IdleOutcome, ImapClient, ImapError, NetError, Qresync, SelectOptions, SeqSet, StoreMode, fetch_items,
};
use iuredesk_model::FolderRole;
use tokio::io::AsyncWriteExt;

fn msg(n: u32, attachment: bool) -> Vec<u8> {
    let date = Utc.with_ymd_and_hms(2026, 10, 1, 9, 0, 0).unwrap().fixed_offset() + chrono::Duration::hours(n.into());
    sample_message(
        "Ana Pérez <ana@example.com>",
        "Yo <yo@example.org>",
        &format!("Asunto {n}"),
        &format!("m{n}@example.com"),
        if n > 1 { Some("m1@example.com") } else { None },
        date,
        &format!("Cuerpo del mensaje {n}"),
        attachment.then_some("factura.pdf"),
    )
}

async fn server(caps: FakeCaps, n: u32) -> FakeImap {
    let s = FakeImap::start(caps).await.unwrap();
    for i in 1..=n {
        s.deliver("INBOX", &msg(i, i == 2), if i == 1 { &["\\Seen"] } else { &[] }, None);
    }
    s
}

async fn login(s: &FakeImap) -> ImapClient {
    let mut c = ImapClient::connect(s.config()).await.unwrap();
    c.login(USER, PASSWORD).await.unwrap();
    c
}

#[tokio::test]
async fn login_list_select_fetch() {
    let s = server(FakeCaps::default(), 3).await;
    let mut c = login(&s).await;
    assert!(c.is_authenticated());
    c.id(&[("name", "IureDesk")]).await.unwrap();
    assert!(c.enable(&["QRESYNC"]).await.unwrap().contains(&"QRESYNC".to_string()));
    assert!(c.compress().await.unwrap());
    assert!(c.is_compressed());

    let list = c.list().await.unwrap();
    let role = |p: &str| list.iter().find(|m| m.path == p).map(|m| m.role());
    assert_eq!(role("INBOX"), Some(FolderRole::Inbox));
    assert_eq!(role("Sent"), Some(FolderRole::Sent));
    assert_eq!(role("Trash"), Some(FolderRole::Trash));

    let sel = c.select("INBOX", &SelectOptions { condstore: true, ..Default::default() }).await.unwrap();
    assert_eq!(sel.mailbox.exists, 3);
    assert_eq!(sel.mailbox.uid_next, Some(4));
    assert!(sel.mailbox.uid_validity.is_some());
    assert!(sel.mailbox.highest_modseq.is_some());

    let r = c.uid_fetch(&SeqSet::range(1, 3), &fetch_items::headers(true), None, false).await.unwrap();
    assert_eq!(r.messages.len(), 3);
    let m2 = r.messages.iter().find(|m| m.uid == Some(2)).unwrap();
    let env = m2.envelope.as_ref().unwrap();
    assert_eq!(env.subject.as_deref(), Some(&b"Asunto 2"[..]));
    assert_eq!(env.from[0].email(), "ana@example.com");
    assert_eq!(env.message_id.as_deref(), Some(&b"<m2@example.com>"[..]));
    assert!(m2.body_structure.as_ref().unwrap().has_attachments());
    assert!(m2.section("BODY[HEADER.FIELDS").unwrap().starts_with(b"In-Reply-To:"));
    assert!(m2.size.unwrap() > 100);
    assert!(m2.modseq.is_some());
    let m1 = r.messages.iter().find(|m| m.uid == Some(1)).unwrap();
    assert_eq!(m1.flags.as_deref(), Some(&["\\Seen".to_string()][..]));

    let full = c.uid_fetch(&SeqSet::single(3), &fetch_items::full(), None, false).await.unwrap();
    assert_eq!(full.messages[0].full_body().unwrap(), s.raw("INBOX", 3).unwrap().as_slice());

    let part = c.uid_fetch(&SeqSet::single(3), &fetch_items::partial("TEXT", 0, 6), None, false).await.unwrap();
    assert_eq!(part.messages[0].section("BODY[TEXT]").unwrap(), b"Cuerpo");

    // `n:*` con n > último UID devuelve el último: el llamador debe filtrar.
    let star = c.uid_fetch(&SeqSet::from_to_star(10), &fetch_items::flags(false), None, false).await.unwrap();
    assert_eq!(star.messages.len(), 1);

    let st = c.status("INBOX", &["MESSAGES", "UIDNEXT", "UNSEEN"]).await.unwrap();
    assert_eq!((st.messages, st.uid_next, st.unseen), (Some(3), Some(4), Some(2)));
    c.logout().await;
}

#[tokio::test]
async fn sasl_plain_and_xoauth2() {
    let s = server(FakeCaps::default(), 0).await;
    let mut c = ImapClient::connect(s.config()).await.unwrap();
    c.authenticate_plain(USER, PASSWORD).await.unwrap();

    let ok = iuredesk_imap_xoauth2(USER, TOKEN);
    let mut c = ImapClient::connect(s.config()).await.unwrap();
    c.authenticate("XOAUTH2", &ok).await.unwrap();

    let bad = iuredesk_imap_xoauth2(USER, "caducado");
    let mut c = ImapClient::connect(s.config()).await.unwrap();
    let e = c.authenticate("XOAUTH2", &bad).await.unwrap_err();
    let ImapError::AuthFailed { sasl, .. } = &e else { panic!("{e:?}") };
    assert!(sasl.is_some(), "el reto de error SASL se conserva");
    assert!(e.is_auth());

    // Sin SASL-IR: la respuesta inicial va tras la continuación.
    let s = server(FakeCaps { sasl_ir: false, ..FakeCaps::default() }, 0).await;
    let mut c = ImapClient::connect(s.config()).await.unwrap();
    c.authenticate("XOAUTH2", &ok).await.unwrap();

    let mut c = ImapClient::connect(s.config()).await.unwrap();
    assert!(c.login(USER, "mala").await.unwrap_err().is_auth());
}

fn iuredesk_imap_xoauth2(user: &str, token: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(format!("user={user}\x01auth=Bearer {token}\x01\x01"))
}

#[tokio::test]
async fn bare_server_uses_fallbacks() {
    // Sin LITERAL+: la contraseña (no ASCII) va como literal sincronizado.
    let s = server(FakeCaps::bare(), 4).await;
    let mut c = login(&s).await;
    assert!(c.enable(&["QRESYNC"]).await.unwrap().is_empty());
    assert!(!c.compress().await.unwrap());
    c.select("INBOX", &SelectOptions::default()).await.unwrap();
    assert_eq!(c.uid_search("ALL").await.unwrap(), vec![1, 2, 3, 4]);
    assert_eq!(c.uid_search("UID 2:3").await.unwrap(), vec![2, 3]);

    // MOVE ausente: COPY + \Deleted + EXPUNGE.
    let map = c.uid_move(&SeqSet::from_numbers([1, 3]), "Archive").await.unwrap();
    assert!(map.is_none(), "sin UIDPLUS no hay COPYUID");
    assert_eq!(s.messages("INBOX").iter().map(|m| m.0).collect::<Vec<_>>(), vec![2, 4]);
    assert_eq!(s.messages("Archive").len(), 2);
    let log = s.log();
    assert!(log.iter().any(|l| l.contains("UID COPY")));
    assert!(log.iter().any(|l| l.contains("EXPUNGE")));
    assert!(!log.iter().any(|l| l.contains("UID MOVE")));
    assert!(matches!(c.idle(Duration::from_secs(1), std::future::pending()).await, Err(ImapError::Unsupported(_))));
}

#[tokio::test]
async fn move_copyuid_append_search_store() {
    let s = server(FakeCaps::default(), 5).await;
    let mut c = login(&s).await;
    c.select("INBOX", &SelectOptions::default()).await.unwrap();
    let map = c.uid_move(&SeqSet::single(2), "Archive").await.unwrap().unwrap();
    assert_eq!(map.mapping, vec![(2, 1)]);
    assert!(s.log().iter().any(|l| l.contains("UID MOVE")));

    let (fetched, _) =
        c.uid_store(&SeqSet::single(3), StoreMode::Add, &["\\Flagged".into()], false, None).await.unwrap();
    assert!(fetched[0].flags.as_ref().unwrap().contains(&"\\Flagged".to_string()));
    c.uid_store(&SeqSet::single(3), StoreMode::Remove, &["\\Flagged".into()], true, Some(1)).await.unwrap();
    assert!(s.messages("INBOX").iter().all(|m| !m.1.contains(&"\\Flagged".to_string())));
    assert!(c.uid_store(&SeqSet::single(3), StoreMode::Add, &["mal flag".into()], true, None).await.is_err());

    let appended = c.append("Drafts", &["\\Draft".into()], None, &msg(99, false)).await.unwrap();
    assert_eq!(appended.map(|a| a.1), Some(1));
    assert_eq!(s.messages("Drafts").len(), 1);

    // ESEARCH.
    assert_eq!(c.uid_search("ALL").await.unwrap(), vec![1, 3, 4, 5]);
    let since = c.uid_search("SINCE 1-Jan-2020").await.unwrap();
    assert_eq!(since.len(), 4);
    let none = c.uid_search("BEFORE 1-Jan-2020").await.unwrap();
    assert!(none.is_empty());
}

#[tokio::test]
async fn qresync_reports_vanished_and_changes() {
    let s = server(FakeCaps::default(), 6).await;
    let mut c = login(&s).await;
    c.enable(&["QRESYNC"]).await.unwrap();
    let first = c.select("INBOX", &SelectOptions { condstore: true, ..Default::default() }).await.unwrap();
    let v = first.mailbox.uid_validity.unwrap();
    let modseq = first.mailbox.highest_modseq.unwrap();
    c.unselect().await.unwrap();

    s.expunge("INBOX", 2);
    s.expunge("INBOX", 5);
    s.set_flags("INBOX", 4, &["\\Seen", "\\Flagged"]);

    let q = Qresync { uid_validity: v, modseq, known_uids: Some(SeqSet::range(1, 6)) };
    let r = c.select("INBOX", &SelectOptions { qresync: Some(q), ..Default::default() }).await.unwrap();
    assert_eq!(r.vanished, vec![2, 5]);
    assert_eq!(r.changed.len(), 1);
    assert_eq!(r.changed[0].uid, Some(4));
    assert!(r.mailbox.highest_modseq.unwrap() > modseq);

    // CHANGEDSINCE … VANISHED en FETCH.
    s.expunge("INBOX", 6);
    let f = c.uid_fetch(&SeqSet::range(1, 6), &fetch_items::flags(true), Some(modseq), true).await.unwrap();
    assert!(f.vanished.contains(&6));
    assert!(f.messages.iter().any(|m| m.uid == Some(4)));
}

#[tokio::test]
async fn uidvalidity_change_is_visible() {
    let s = server(FakeCaps::default(), 3).await;
    let mut c = login(&s).await;
    let a = c.select("INBOX", &SelectOptions::default()).await.unwrap().mailbox.uid_validity;
    s.reset_uid_validity("INBOX");
    let b = c.select("INBOX", &SelectOptions::default()).await.unwrap().mailbox.uid_validity;
    assert_ne!(a, b);
}

#[tokio::test]
async fn idle_notifies_new_mail_and_stops_on_demand() {
    let s = server(FakeCaps::default(), 1).await;
    let mut c = login(&s).await;
    c.select("INBOX", &SelectOptions { read_only: true, ..Default::default() }).await.unwrap();
    let s2 = s.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        s2.deliver("INBOX", &msg(2, false), &[], None);
    });
    let out = c.idle(Duration::from_secs(10), std::future::pending()).await.unwrap();
    let IdleOutcome::Changed(ev) = out else { panic!("{out:?}") };
    assert!(ev.contains(&IdleEvent::Exists(2)));
    assert_eq!(c.selected().unwrap().exists, 2);

    // DONE a petición.
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let _ = tx.send(());
    });
    let out = c
        .idle(Duration::from_secs(10), async {
            let _ = rx.await;
        })
        .await
        .unwrap();
    assert_eq!(out, IdleOutcome::Stopped);

    // Plazo cumplido (reemitir).
    let out = c.idle(Duration::from_millis(200), std::future::pending()).await.unwrap();
    assert_eq!(out, IdleOutcome::Timeout);
    // La conexión sigue sana.
    c.noop().await.unwrap();
}

/// Servidor hostil: manda `script` y luego calla o cierra.
async fn hostile(script: Vec<u8>, close: bool) -> u16 {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = l.accept().await {
            let script = script.clone();
            tokio::spawn(async move {
                let _ = sock.write_all(&script).await;
                if !close {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                }
            });
        }
    });
    port
}

fn cfg(port: u16) -> iuredesk_imap::ImapConfig {
    let mut c = iuredesk_imap::ImapConfig::new("127.0.0.1", port, iuredesk_model::account::Security::Plain);
    c.io_timeout = Duration::from_millis(800);
    c
}

#[tokio::test]
async fn hostile_servers_fail_cleanly() {
    // Línea infinita.
    let mut long = b"* OK ".to_vec();
    long.extend(std::iter::repeat_n(b'a', 9 * 1024 * 1024));
    let e = ImapClient::connect(cfg(hostile(long, false).await)).await.unwrap_err();
    assert!(matches!(e, ImapError::TooLarge(_)), "{e:?}");
    // Literal gigante anunciado.
    let e = ImapClient::connect(cfg(hostile(b"* OK {999999999999}\r\n".to_vec(), false).await)).await.unwrap_err();
    assert!(matches!(e, ImapError::TooLarge(_)), "{e:?}");
    // Silencio.
    let e = ImapClient::connect(cfg(hostile(Vec::new(), false).await)).await.unwrap_err();
    assert_eq!(e, ImapError::Net(NetError::Timeout));
    // Cierre tras saludo BYE.
    let e = ImapClient::connect(cfg(hostile(b"* BYE [UNAVAILABLE] Too many connections\r\n".to_vec(), true).await))
        .await
        .unwrap_err();
    assert!(matches!(e, ImapError::Bye(_)), "{e:?}");
    // Basura binaria.
    let e = ImapClient::connect(cfg(hostile(vec![0xff, 0x00, 0x13, b'\n', b'\n'], true).await)).await.unwrap_err();
    assert!(!matches!(e, ImapError::AuthFailed { .. }), "{e:?}");
    // STARTTLS pedido y no ofrecido.
    let mut c = cfg(hostile(b"* OK [CAPABILITY IMAP4rev1] hola\r\n".to_vec(), false).await);
    c.security = iuredesk_model::account::Security::StartTls;
    assert_eq!(ImapClient::connect(c).await.unwrap_err(), ImapError::StartTlsUnavailable);
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig { cases: 48, ..Default::default() })]
    #[test]
    fn random_greeting_never_panics(bytes in proptest::collection::vec(proptest::num::u8::ANY, 0..300)) {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let port = hostile(bytes, true).await;
            let mut c = cfg(port);
            c.io_timeout = Duration::from_millis(300);
            if let Ok(mut cl) = ImapClient::connect(c).await {
                let _ = cl.login("u", "p").await;
                let _ = cl.list().await;
            }
        });
    }
}
