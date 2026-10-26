//! Cliente POP3 contra el servidor falso.

use std::collections::HashMap;
use std::time::Duration;

use base64::Engine;
use iuredesk_pop3::testing::{FakePop3, PASSWORD, TOKEN, USER};
use iuredesk_pop3::{Pop3Client, Pop3Error, Pop3Policy, plan_download};
use tokio::io::AsyncWriteExt;

const MSG: &[u8] = b"From: a@b.c\r\nSubject: Hola\r\n\r\nLinea 1\r\n.linea con punto\r\nfin\r\n";

#[tokio::test]
async fn login_uidl_retr_dele() {
    let s = FakePop3::start().await.unwrap();
    s.deliver("u1", MSG);
    s.deliver("u2", b"Subject: dos\r\n\r\nx\r\n");
    let mut c = Pop3Client::connect(s.config()).await.unwrap();
    assert!(c.has_capa("UIDL"));
    assert!(c.has_sasl("XOAUTH2"));
    assert!(c.has_apop());
    c.login(USER, PASSWORD).await.unwrap();
    assert_eq!(c.stat().await.unwrap().0, 2);
    assert_eq!(c.uidl().await.unwrap(), vec![(1, "u1".to_string()), (2, "u2".to_string())]);
    assert_eq!(c.list().await.unwrap().len(), 2);
    assert_eq!(c.retr(1).await.unwrap(), MSG);
    let top = c.top(1, 0).await.unwrap();
    assert!(top.starts_with(b"From: a@b.c\r\nSubject: Hola\r\n\r\n"));
    assert!(!top.windows(5).any(|w| w == b"Linea"));
    c.dele(1).await.unwrap();
    assert!(matches!(c.retr(1).await, Err(Pop3Error::Err(_))));
    c.quit().await.unwrap();
    assert_eq!(s.uidls(), vec!["u2".to_string()]);
}

#[tokio::test]
async fn apop_and_sasl() {
    let s = FakePop3::start().await.unwrap();
    let mut c = Pop3Client::connect(s.config()).await.unwrap();
    c.apop(USER, PASSWORD).await.unwrap();

    let ir = base64::engine::general_purpose::STANDARD.encode(format!("user={USER}\x01auth=Bearer {TOKEN}\x01\x01"));
    let mut c = Pop3Client::connect(s.config()).await.unwrap();
    c.auth_sasl("XOAUTH2", &ir).await.unwrap();

    let bad = base64::engine::general_purpose::STANDARD.encode(format!("user={USER}\x01auth=Bearer x\x01\x01"));
    let mut c = Pop3Client::connect(s.config()).await.unwrap();
    assert!(c.auth_sasl("XOAUTH2", &bad).await.unwrap_err().is_auth());

    let mut c = Pop3Client::connect(s.config()).await.unwrap();
    c.auth_plain(USER, PASSWORD).await.unwrap();

    let mut c = Pop3Client::connect(s.config()).await.unwrap();
    assert!(c.login(USER, "mala").await.unwrap_err().is_auth());
    assert!(s.log().iter().all(|l| !l.contains(PASSWORD)), "los secretos no se registran");
}

#[tokio::test]
async fn incremental_with_plan() {
    let s = FakePop3::start().await.unwrap();
    for i in 0..5 {
        s.deliver(&format!("id{i}"), MSG);
    }
    let mut known: HashMap<String, i64> = HashMap::new();
    let policy = Pop3Policy { leave_on_server: false, delete_after_days: None };
    for round in 0..2 {
        let mut c = Pop3Client::connect(s.config()).await.unwrap();
        c.login(USER, PASSWORD).await.unwrap();
        let plan = plan_download(&c.uidl().await.unwrap(), &known, policy, 0, 3);
        for (n, uid) in &plan.download {
            c.retr(*n).await.unwrap();
            known.insert(uid.clone(), 0);
        }
        for (n, _) in &plan.delete {
            c.dele(*n).await.unwrap();
        }
        c.quit().await.unwrap();
        if round == 0 {
            assert_eq!(known.len(), 3);
        }
    }
    assert_eq!(known.len(), 5);
    // Los 3 primeros se borraron en la segunda vuelta; los 2 últimos quedan para la siguiente.
    assert_eq!(s.uidls().len(), 2);
}

async fn hostile(script: &'static [u8]) -> u16 {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = l.accept().await {
            tokio::spawn(async move {
                let _ = sock.write_all(script).await;
                tokio::time::sleep(Duration::from_secs(20)).await;
            });
        }
    });
    port
}

#[tokio::test]
async fn hostile_pop3() {
    let cfg = |port| {
        let mut c = iuredesk_pop3::Pop3Config::new("127.0.0.1", port, iuredesk_model::account::Security::Plain);
        c.io_timeout = Duration::from_millis(500);
        c
    };
    assert!(Pop3Client::connect(cfg(hostile(b"-ERR vete\r\n").await)).await.is_err());
    assert!(Pop3Client::connect(cfg(hostile(b"\xff\xfe basura\r\n").await)).await.is_err());
    assert!(Pop3Client::connect(cfg(hostile(b"").await)).await.is_err());
    let mut st = cfg(hostile(b"+OK hola\r\n").await);
    st.security = iuredesk_model::account::Security::StartTls;
    assert!(Pop3Client::connect(st).await.is_err());
}
