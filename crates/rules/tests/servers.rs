//! Con servidores falsos en 127.0.0.1: ManageSieve (RFC 5804) y la baja en un clic (RFC 8058).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use iuredesk_model::AccountId;
use iuredesk_rules::managesieve::{self, Auth, Config, Security};
use iuredesk_rules::rule::{Action, Condition, Field, Rule, TextOp};
use iuredesk_rules::sieve::{self, GenCtx, ReadCtx};
use iuredesk_rules::unsubscribe::{self, HttpOptions};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

#[derive(Default)]
struct Fake {
    scripts: Vec<(String, String)>,
    active: Option<String>,
    log: Vec<String>,
}

async fn read_literal(r: &mut BufReader<tokio::net::tcp::OwnedReadHalf>, line: &str) -> String {
    let n: usize = line.rsplit('{').next().unwrap().trim_end_matches(['}', '+']).parse().unwrap();
    let mut buf = vec![0u8; n];
    r.read_exact(&mut buf).await.unwrap();
    let mut rest = String::new();
    r.read_line(&mut rest).await.unwrap();
    String::from_utf8(buf).unwrap()
}

fn arg(line: &str, i: usize) -> String {
    line.split('"').nth(i * 2 + 1).unwrap_or("").to_string()
}

/// Servidor ManageSieve mínimo, sin TLS.
async fn fake_server(state: Arc<Mutex<Fake>>, other_active: Option<&'static str>) -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    if let Some(o) = other_active {
        let mut s = state.lock().unwrap();
        s.scripts.push((o.into(), "keep;".into()));
        s.active = Some(o.into());
    }
    tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = l.accept().await else { return };
            let state = Arc::clone(&state);
            tokio::spawn(async move {
                let (rd, mut wr) = sock.into_split();
                let mut r = BufReader::new(rd);
                wr.write_all(b"\"IMPLEMENTATION\" \"Falso 1.0\"\r\n\"SIEVE\" \"fileinto imap4flags copy vacation regex body date relational\"\r\n\"SASL\" \"PLAIN\"\r\n\"VERSION\" \"1.0\"\r\nOK \"Listo\"\r\n").await.unwrap();
                loop {
                    let mut line = String::new();
                    if r.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    let line = line.trim_end().to_string();
                    state.lock().unwrap().log.push(line.split(' ').next().unwrap_or("").to_string());
                    let reply: String = if line.starts_with("AUTHENTICATE") {
                        let b64 = arg(&line, 1);
                        // "\0laura\0secreto"
                        if b64 == "AGxhdXJhAHNlY3JldG8=" { "OK\r\n".into() } else { "NO \"Credenciales\"\r\n".into() }
                    } else if line == "LISTSCRIPTS" {
                        let s = state.lock().unwrap();
                        let mut out = String::new();
                        for (n, _) in &s.scripts {
                            out.push_str(&format!(
                                "\"{n}\"{}\r\n",
                                if s.active.as_deref() == Some(n) { " ACTIVE" } else { "" }
                            ));
                        }
                        out + "OK\r\n"
                    } else if line.starts_with("PUTSCRIPT") || line.starts_with("CHECKSCRIPT") {
                        let name = arg(&line, 0);
                        let body = read_literal(&mut r, &line).await;
                        if sieve::check(&body).is_err() {
                            "NO \"error de sintaxis\"\r\n".into()
                        } else {
                            if line.starts_with("PUTSCRIPT") {
                                let mut s = state.lock().unwrap();
                                s.scripts.retain(|(n, _)| *n != name);
                                s.scripts.push((name, body));
                            }
                            "OK\r\n".into()
                        }
                    } else if line.starts_with("SETACTIVE") {
                        state.lock().unwrap().active = Some(arg(&line, 0));
                        "OK\r\n".into()
                    } else if line.starts_with("GETSCRIPT") {
                        let name = arg(&line, 0);
                        let s = state.lock().unwrap();
                        match s.scripts.iter().find(|(n, _)| *n == name) {
                            Some((_, b)) => format!("{{{}}}\r\n{b}\r\nOK\r\n", b.len()),
                            None => "NO \"no existe\"\r\n".into(),
                        }
                    } else if line == "LOGOUT" {
                        let _ = wr.write_all(b"OK \"Adios\"\r\n").await;
                        return;
                    } else {
                        "NO \"desconocido\"\r\n".into()
                    };
                    wr.write_all(reply.as_bytes()).await.unwrap();
                }
            });
        }
    });
    port
}

fn cfg(port: u16) -> Config {
    Config { port, security: Security::PlainLoopback, timeout: Duration::from_secs(5), ..Config::new("127.0.0.1") }
}

fn rules() -> Vec<Rule> {
    vec![
        Rule {
            id: 1,
            name: "Clientes".into(),
            conditions: vec![Condition::Text {
                field: Field::From,
                op: TextOp::Contains,
                value: "cliente.example".into(),
            }],
            actions: vec![Action::Flag, Action::Stop],
            ..Rule::default()
        },
        Rule { id: 2, conditions: vec![Condition::HasAttachment], actions: vec![Action::MarkRead], ..Rule::default() },
    ]
}

#[tokio::test]
async fn upload_and_read_back() {
    let state = Arc::new(Mutex::new(Fake::default()));
    let port = fake_server(Arc::clone(&state), None).await;
    let caps = managesieve::probe(&cfg(port)).await.unwrap();
    assert_eq!(caps.implementation, "Falso 1.0");
    assert!(caps.sieve.contains(&"vacation".to_string()));
    let ctx = GenCtx { account: AccountId(1), extensions: caps.sieve.iter().cloned().collect(), ..Default::default() };
    let script = sieve::to_sieve(&rules(), None, &ctx);
    assert_eq!(script.server, vec![1]);
    assert_eq!(script.local.len(), 1, "los adjuntos no se pueden en el servidor");
    let auth = Auth::Password { user: "laura".into(), password: "secreto".into() };
    let up = managesieve::upload(&cfg(port), &auth, &script.text).await.unwrap();
    assert_eq!(up.other_active, None);
    assert_eq!(state.lock().unwrap().active.as_deref(), Some(sieve::SCRIPT_NAME));

    let mut c = managesieve::Client::connect(&cfg(port), &auth).await.unwrap();
    assert_eq!(c.list_scripts().await.unwrap(), vec![(sieve::SCRIPT_NAME.to_string(), true)]);
    let text = c.get_script(sieve::SCRIPT_NAME).await.unwrap();
    assert!(c.check_script("if true {").await.is_err());
    c.logout().await;
    let resolve = |_: &str| None;
    let back =
        sieve::from_sieve(&text, &ReadCtx { account: AccountId(1), trash_path: None, resolve: &resolve }).unwrap();
    assert_eq!(back.rules, vec![rules()[0].clone()]);
}

#[tokio::test]
async fn bad_password_and_foreign_active_script() {
    let state = Arc::new(Mutex::new(Fake::default()));
    let port = fake_server(Arc::clone(&state), Some("de-mano")).await;
    let bad = Auth::Password { user: "laura".into(), password: "mal".into() };
    assert!(matches!(managesieve::Client::connect(&cfg(port), &bad).await, Err(iuredesk_rules::RulesError::Auth)));
    let auth = Auth::Password { user: "laura".into(), password: "secreto".into() };
    let up = managesieve::upload(&cfg(port), &auth, "keep;\n").await.unwrap();
    assert_eq!(up.other_active.as_deref(), Some("de-mano"), "no se pisa el script del usuario");
    assert_eq!(state.lock().unwrap().active.as_deref(), Some("de-mano"));
    assert!(!state.lock().unwrap().log.contains(&"SETACTIVE".to_string()));
}

#[tokio::test]
async fn starttls_required_when_not_offered() {
    let state = Arc::new(Mutex::new(Fake::default()));
    let port = fake_server(state, None).await;
    let c = Config { security: Security::StartTls, ..cfg(port) };
    assert!(matches!(managesieve::probe(&c).await, Err(iuredesk_rules::RulesError::Net(_))));
}

/// Servidor HTTP mínimo: guarda la petición y responde `status`.
async fn http_server(status: u16, seen: Arc<Mutex<String>>) -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        let Ok((mut sock, _)) = l.accept().await else { return };
        let mut buf = vec![0u8; 8192];
        let mut got = Vec::new();
        loop {
            let n = sock.read(&mut buf).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            got.extend_from_slice(&buf[..n]);
            let s = String::from_utf8_lossy(&got);
            if s.contains("\r\n\r\n") && s.ends_with("One-Click") {
                break;
            }
        }
        *seen.lock().unwrap() = String::from_utf8_lossy(&got).into_owned();
        let _ = sock
            .write_all(format!("HTTP/1.1 {status} X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes())
            .await;
    });
    port
}

#[tokio::test]
async fn one_click_posts_rfc8058_body() {
    let seen = Arc::new(Mutex::new(String::new()));
    let port = http_server(200, Arc::clone(&seen)).await;
    let opts = HttpOptions { allow_loopback_http: true, timeout: Duration::from_secs(5) };
    let st = unsubscribe::one_click(&format!("http://127.0.0.1:{port}/u/abc?x=1"), &opts).await.unwrap();
    assert_eq!(st, 200);
    let req = seen.lock().unwrap().clone();
    assert!(req.starts_with("POST /u/abc?x=1 HTTP/1.1"), "{req}");
    assert!(req.to_lowercase().contains("content-type: application/x-www-form-urlencoded"));
    assert!(req.ends_with("List-Unsubscribe=One-Click"));
    assert!(!req.to_lowercase().contains("cookie"));

    let seen = Arc::new(Mutex::new(String::new()));
    let port = http_server(500, seen).await;
    assert!(unsubscribe::one_click(&format!("http://127.0.0.1:{port}/u"), &opts).await.is_err());
}
