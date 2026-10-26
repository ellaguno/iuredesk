//! Prueba de conexión contra servidores IMAP, POP3 y SMTP falsos (texto plano en 127.0.0.1).

use std::time::Duration;

use base64::Engine;
use iuredesk_accounts::probe::{ProbeError, ProbeOptions, test_incoming_with, test_outgoing_with};
use iuredesk_model::account::{AuthMethod, Protocol, Security, ServerConfig};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

const USER: &str = "ana@example.com";
const PASS: &str = "correcta \"con\" comillas";

fn cfg(protocol: Protocol, port: u16, security: Security) -> ServerConfig {
    ServerConfig {
        protocol,
        host: "127.0.0.1".into(),
        port,
        security,
        auth: AuthMethod::Password,
        username: USER.into(),
    }
}

fn opts() -> ProbeOptions {
    ProbeOptions { io_timeout: Duration::from_secs(3), total_timeout: Duration::from_secs(10) }
}

fn decode_plain(b64: &str) -> (String, String) {
    let raw = base64::engine::general_purpose::STANDARD.decode(b64.trim()).unwrap_or_default();
    let s = String::from_utf8_lossy(&raw).to_string();
    let mut parts = s.split('\0').skip(1);
    (parts.next().unwrap_or_default().to_string(), parts.next().unwrap_or_default().to_string())
}

/// Desentrecomilla una cadena IMAP sencilla y devuelve el resto.
fn unquote(s: &str) -> (String, &str) {
    let mut out = String::new();
    let mut chars = s.trim_start().strip_prefix('"').unwrap_or("").char_indices();
    let body = s.trim_start().strip_prefix('"').unwrap_or("");
    while let Some((i, c)) = chars.next() {
        match c {
            '\\' => {
                if let Some((_, n)) = chars.next() {
                    out.push(n);
                }
            }
            '"' => return (out, &body[i + 1..]),
            c => out.push(c),
        }
    }
    (out, "")
}

#[derive(Clone, Copy)]
enum Imap {
    /// Con AUTH=PLAIN y SASL-IR.
    Modern,
    /// Solo LOGIN.
    Legacy,
    /// Rechaza con el texto de Gmail para contraseña de app.
    AppPassword,
    /// Saludo BYE por exceso de conexiones.
    Busy,
    /// Manda una línea infinita.
    Hostile,
    /// Acepta y no dice nada.
    Silent,
}

async fn spawn(handler: impl Fn(TcpStream) -> tokio::task::JoinHandle<()> + Send + 'static) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            handler(sock);
        }
    });
    port
}

async fn imap_server(kind: Imap) -> u16 {
    spawn(move |sock| tokio::spawn(imap_session(sock, kind))).await
}

async fn imap_session(sock: TcpStream, kind: Imap) {
    let (r, mut w) = sock.into_split();
    let mut lines = BufReader::new(r).lines();
    let caps = match kind {
        Imap::Legacy => "IMAP4rev1 ID",
        _ => "IMAP4rev1 AUTH=PLAIN SASL-IR",
    };
    let greeting = match kind {
        Imap::Busy => "* BYE [UNAVAILABLE] Too many simultaneous connections. (Failure)\r\n".to_string(),
        Imap::Silent => {
            tokio::time::sleep(Duration::from_secs(60)).await;
            return;
        }
        Imap::Hostile => {
            let chunk = vec![b'x'; 64 * 1024];
            for _ in 0..64 {
                if w.write_all(&chunk).await.is_err() {
                    return;
                }
            }
            return;
        }
        _ => format!("* OK [CAPABILITY {caps}] Fake IMAP ready\r\n"),
    };
    if w.write_all(greeting.as_bytes()).await.is_err() {
        return;
    }
    while let Ok(Some(line)) = lines.next_line().await {
        let (tag, rest) = line.split_once(' ').unwrap_or((&line, ""));
        let (cmd, args) = rest.split_once(' ').unwrap_or((rest, ""));
        let reply = match cmd.to_ascii_uppercase().as_str() {
            "CAPABILITY" => format!("* CAPABILITY {caps}\r\n{tag} OK done\r\n"),
            "AUTHENTICATE" => {
                let b64 = args.strip_prefix("PLAIN ").unwrap_or("");
                let (u, p) = decode_plain(b64);
                match kind {
                    Imap::AppPassword => format!(
                        "{tag} NO [ALERT] Application-specific password required: https://support.google.com/accounts/answer/185833 (Failure)\r\n"
                    ),
                    _ if u == USER && p == PASS => format!("{tag} OK [CAPABILITY IMAP4rev1 MOVE] Logged in\r\n"),
                    _ => format!("{tag} NO [AUTHENTICATIONFAILED] Invalid credentials (Failure)\r\n"),
                }
            }
            "LOGIN" => {
                let (u, rest) = unquote(args);
                let (p, _) = unquote(rest);
                if u == USER && p == PASS {
                    format!("{tag} OK Logged in\r\n")
                } else {
                    format!("{tag} NO [AUTHENTICATIONFAILED] Invalid credentials\r\n")
                }
            }
            "LIST" => format!(
                "* LIST (\\HasNoChildren) \"/\" \"INBOX\"\r\n* LIST (\\HasNoChildren \\Sent) \"/\" \"Enviados\"\r\n* LIST (\\HasNoChildren) \"/\" {{5}}\r\nCosas\r\n{tag} OK LIST done\r\n"
            ),
            "LOGOUT" => format!("* BYE adiós\r\n{tag} OK LOGOUT\r\n"),
            _ => format!("{tag} BAD unknown\r\n"),
        };
        if w.write_all(reply.as_bytes()).await.is_err() {
            return;
        }
    }
}

async fn pop3_server() -> u16 {
    spawn(|sock| {
        tokio::spawn(async move {
            let (r, mut w) = sock.into_split();
            let mut lines = BufReader::new(r).lines();
            if w.write_all(b"+OK Fake POP3 <1896.697170952@fake.example>\r\n").await.is_err() {
                return;
            }
            let mut user = String::new();
            while let Ok(Some(line)) = lines.next_line().await {
                let (cmd, arg) = line.split_once(' ').unwrap_or((&line, ""));
                let reply = match cmd.to_ascii_uppercase().as_str() {
                    "CAPA" => "+OK\r\nUSER\r\nUIDL\r\n.\r\n".to_string(),
                    "USER" => {
                        user = arg.to_string();
                        "+OK\r\n".to_string()
                    }
                    "PASS" if user == USER && arg == PASS => "+OK logged in\r\n".to_string(),
                    "PASS" => "-ERR [AUTH] Authentication failed.\r\n".to_string(),
                    "STAT" => "+OK 3 1200\r\n".to_string(),
                    "QUIT" => "+OK bye\r\n".to_string(),
                    _ => "-ERR unknown\r\n".to_string(),
                };
                if w.write_all(reply.as_bytes()).await.is_err() {
                    return;
                }
            }
        })
    })
    .await
}

async fn smtp_server() -> u16 {
    spawn(|sock| {
        tokio::spawn(async move {
            let (r, mut w) = sock.into_split();
            let mut lines = BufReader::new(r).lines();
            if w.write_all(b"220 fake.example ESMTP ready\r\n").await.is_err() {
                return;
            }
            while let Ok(Some(line)) = lines.next_line().await {
                let upper = line.to_ascii_uppercase();
                let reply = if upper.starts_with("EHLO") {
                    "250-fake.example\r\n250-AUTH PLAIN LOGIN\r\n250 8BITMIME\r\n".to_string()
                } else if let Some(b64) = line.strip_prefix("AUTH PLAIN ") {
                    let (u, p) = decode_plain(b64);
                    if u == USER && p == PASS {
                        "235 2.7.0 Accepted\r\n".to_string()
                    } else {
                        "535 5.7.8 Username and Password not accepted\r\n".to_string()
                    }
                } else if upper.starts_with("NOOP") {
                    "250 2.0.0 OK\r\n".to_string()
                } else if upper.starts_with("QUIT") {
                    let _ = w.write_all(b"221 2.0.0 Bye\r\n").await;
                    return;
                } else {
                    "502 5.5.2 Unknown\r\n".to_string()
                };
                if w.write_all(reply.as_bytes()).await.is_err() {
                    return;
                }
            }
        })
    })
    .await
}

#[tokio::test]
async fn imap_modern_ok() {
    let port = imap_server(Imap::Modern).await;
    let r = test_incoming_with(&cfg(Protocol::Imap, port, Security::Plain), PASS, opts()).await.unwrap();
    assert_eq!(r.folder_count, 3);
    assert_eq!(r.folders, vec!["INBOX", "Enviados", "Cosas"]);
    assert!(r.capabilities.contains(&"MOVE".to_string()));
    assert!(r.greeting.contains("Fake IMAP"));
}

#[tokio::test]
async fn imap_legacy_login_ok() {
    let port = imap_server(Imap::Legacy).await;
    let r = test_incoming_with(&cfg(Protocol::Imap, port, Security::Plain), PASS, opts()).await.unwrap();
    assert_eq!(r.folder_count, 3);
}

#[tokio::test]
async fn imap_wrong_password() {
    let port = imap_server(Imap::Modern).await;
    let e = test_incoming_with(&cfg(Protocol::Imap, port, Security::Plain), "mala", opts()).await.unwrap_err();
    assert!(matches!(e, ProbeError::AuthFailed(_)), "{e:?}");
    assert_eq!(e.message(true), "Usuario o contraseña incorrectos.");
    let port = imap_server(Imap::Legacy).await;
    let e = test_incoming_with(&cfg(Protocol::Imap, port, Security::Plain), "mala", opts()).await.unwrap_err();
    assert!(matches!(e, ProbeError::AuthFailed(_)), "{e:?}");
}

#[tokio::test]
async fn imap_app_password_required() {
    let port = imap_server(Imap::AppPassword).await;
    let e = test_incoming_with(&cfg(Protocol::Imap, port, Security::Plain), PASS, opts()).await.unwrap_err();
    assert!(matches!(e, ProbeError::AppPasswordRequired(_)), "{e:?}");
}

#[tokio::test]
async fn imap_too_many_connections() {
    let port = imap_server(Imap::Busy).await;
    let e = test_incoming_with(&cfg(Protocol::Imap, port, Security::Plain), PASS, opts()).await.unwrap_err();
    assert!(matches!(e, ProbeError::TooManyConnections(_)), "{e:?}");
}

#[tokio::test]
async fn imap_starttls_missing() {
    let port = imap_server(Imap::Modern).await;
    let e = test_incoming_with(&cfg(Protocol::Imap, port, Security::StartTls), PASS, opts()).await.unwrap_err();
    assert_eq!(e, ProbeError::StartTlsUnavailable);
}

#[tokio::test]
async fn imap_hostile_line_is_bounded() {
    let port = imap_server(Imap::Hostile).await;
    let e = test_incoming_with(&cfg(Protocol::Imap, port, Security::Plain), PASS, opts()).await.unwrap_err();
    assert!(matches!(e, ProbeError::Protocol(_)), "{e:?}");
}

#[tokio::test]
async fn imap_silent_server_times_out() {
    let port = imap_server(Imap::Silent).await;
    let o = ProbeOptions { io_timeout: Duration::from_millis(300), total_timeout: Duration::from_secs(5) };
    let e = test_incoming_with(&cfg(Protocol::Imap, port, Security::Plain), PASS, o).await.unwrap_err();
    assert_eq!(e, ProbeError::Timeout);
}

#[tokio::test]
async fn tls_against_plain_server_fails_cleanly() {
    let port = imap_server(Imap::Modern).await;
    let e = test_incoming_with(&cfg(Protocol::Imap, port, Security::Tls), PASS, opts()).await.unwrap_err();
    assert!(matches!(e, ProbeError::Tls(_) | ProbeError::Protocol(_) | ProbeError::Timeout), "{e:?}");
}

#[tokio::test]
async fn pop3_ok_and_wrong_password() {
    let port = pop3_server().await;
    let r = test_incoming_with(&cfg(Protocol::Pop3, port, Security::Plain), PASS, opts()).await.unwrap();
    assert_eq!(r.message_count, Some(3));
    let e = test_incoming_with(&cfg(Protocol::Pop3, port, Security::Plain), "mala", opts()).await.unwrap_err();
    assert!(matches!(e, ProbeError::AuthFailed(_)), "{e:?}");
    let e = test_incoming_with(&cfg(Protocol::Pop3, port, Security::StartTls), PASS, opts()).await.unwrap_err();
    assert_eq!(e, ProbeError::StartTlsUnavailable);
}

#[tokio::test]
async fn smtp_ok_and_wrong_password() {
    let port = smtp_server().await;
    test_outgoing_with(&cfg(Protocol::Smtp, port, Security::Plain), PASS, opts()).await.unwrap();
    let e = test_outgoing_with(&cfg(Protocol::Smtp, port, Security::Plain), "mala", opts()).await.unwrap_err();
    assert!(matches!(e, ProbeError::AuthFailed(_)), "{e:?}");
}

#[tokio::test]
async fn wrong_protocol_and_bad_tokens() {
    let e = test_outgoing_with(&cfg(Protocol::Imap, 1, Security::Plain), PASS, opts()).await.unwrap_err();
    assert_eq!(e, ProbeError::WrongProtocol);
    let e = test_incoming_with(&cfg(Protocol::Smtp, 1, Security::Plain), PASS, opts()).await.unwrap_err();
    assert_eq!(e, ProbeError::WrongProtocol);
    let e = test_incoming_with(&cfg(Protocol::Imap, 1, Security::Plain), "a\r\nA1 LOGOUT", opts()).await.unwrap_err();
    assert!(matches!(e, ProbeError::Protocol(_)));
}

/// Con red: un certificado caducado o de otro nombre se reporta como tal, sin llegar a mandar la
/// contraseña. `cargo test -p iuredesk-accounts --test fake_servers -- --ignored`
#[tokio::test]
#[ignore]
async fn bad_certificates_are_reported() {
    let mut c = cfg(Protocol::Imap, 443, Security::Tls);
    for host in ["expired.badssl.com", "wrong.host.badssl.com", "self-signed.badssl.com"] {
        c.host = host.into();
        let e = test_incoming_with(&c, PASS, opts()).await.unwrap_err();
        assert!(matches!(e, ProbeError::TlsCertificate(_)), "{host}: {e:?}");
    }
}

#[tokio::test]
async fn host_not_found_and_refused() {
    let mut c = cfg(Protocol::Imap, 993, Security::Tls);
    c.host = "no-existe.invalid".into();
    let e = test_incoming_with(&c, PASS, opts()).await.unwrap_err();
    assert!(matches!(e, ProbeError::HostNotFound { .. }), "{e:?}");
    c.protocol = Protocol::Smtp;
    let e = test_outgoing_with(&c, PASS, opts()).await.unwrap_err();
    assert!(matches!(e, ProbeError::HostNotFound { .. }), "{e:?}");

    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l);
    let e = test_incoming_with(&cfg(Protocol::Imap, port, Security::Plain), PASS, opts()).await.unwrap_err();
    assert!(matches!(e, ProbeError::ConnectionRefused { .. }), "{e:?}");
    let e = test_outgoing_with(&cfg(Protocol::Smtp, port, Security::Plain), PASS, opts()).await.unwrap_err();
    assert!(matches!(e, ProbeError::ConnectionRefused { .. }), "{e:?}");
}
