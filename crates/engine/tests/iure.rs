//! Iurefficient desde la sesión (F3.1) contra una instancia falsa (HTTP en tokio): restaurar la
//! sesión compartida, contexto del mensaje, vincular a un ticket (comentario interno), avisos,
//! agenda copiada al almacén, 403 de términos y sin conexión.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{TimeZone, Utc};
use iuredesk_accounts::Error as AccError;
use iuredesk_accounts::iurefficient::SharedVault;
use iuredesk_engine::iure::{IureStatus, LinkOptions, LinkTarget, TargetKind};
use iuredesk_engine::{IureSetup, Session, SessionConfig};
use iuredesk_model::MessageId;
use iuredesk_store::{EmailLink, Store, sample};
use iurefficient_connect::Account;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 21, 15, 0, 0).unwrap()
}

struct Vault(Mutex<Option<String>>);

impl SharedVault for Vault {
    fn read(&self, _: &Account) -> Result<Option<String>, AccError> {
        Ok(self.0.lock().unwrap().clone())
    }
    fn write(&self, _: &Account, s: &str) -> Result<(), AccError> {
        *self.0.lock().unwrap() = Some(s.to_string());
        Ok(())
    }
}

fn vault() -> Arc<dyn SharedVault> {
    Arc::new(Vault(Mutex::new(Some(r#"{"cookies":["refresh_token_cookie=r1","csrf_refresh_token=c1"]}"#.into()))))
}

#[derive(Default)]
struct Fake {
    terms: AtomicBool,
    log: Mutex<Vec<String>>,
}

fn param(req: &str, name: &str) -> String {
    let line = req.lines().next().unwrap_or("");
    let q = line.split_whitespace().nth(1).and_then(|p| p.split_once('?')).map(|x| x.1).unwrap_or("");
    q.split('&')
        .find_map(|kv| kv.strip_prefix(&format!("{name}=")))
        .unwrap_or("")
        .replace("%40", "@")
        .replace("%2B", "+")
        .replace("%2F", "/")
}

fn route(f: &Fake, req: &str) -> (u16, String) {
    let first = req.lines().next().unwrap_or("").to_string();
    let path = first.split_whitespace().nth(1).unwrap_or("").split('?').next().unwrap_or("").to_string();
    let post = first.starts_with("POST");
    if path == "/api/auth/refresh" {
        return (200, "{}".into());
    }
    if path == "/api/auth/me" {
        return (200, r#"{"user":{"id":7,"email":"laura@despacho.mx","name":"Laura"}}"#.into());
    }
    if f.terms.load(Ordering::SeqCst) {
        return (403, r#"{"code":"terms_acceptance_required","error":"Acepta los términos"}"#.into());
    }
    match path.as_str() {
        "/api/plugins/crm/leads" => {
            let who = param(req, "search");
            let body = if who.contains('@') {
                format!(
                    r#"{{"total":1,"items":[{{"id":"l1","name":"Persona Uno","email":"{who}","organization_name":"Acme","position":"Directora","status":"active"}}]}}"#
                )
            } else {
                r#"{"total":0,"items":[]}"#.to_string()
            };
            (200, body)
        }
        "/api/search" => (200, r#"{"results":{"cases":[{"id":"c1","case_number":"241/2026","title":"Arrendamiento"}],"clients":[],"documents":[],"tasks":[],"plugins":[]}}"#.into()),
        "/api/plugins/tickets" => {
            let who = param(req, "search");
            (200, format!(r#"{{"tickets":[{{"id":"t1","ticket_number":"TK-1","title":"Portal","status":"open","priority":"low","requester_email":"{who}"}}],"pagination":{{"total":1}}}}"#))
        }
        "/api/plugins/crm/opportunities" => (200, r#"{"items":[{"id":"o1","name":"Acme 2027","stage_name":"Propuesta","amount":120000.5,"status":"open"}]}"#.into()),
        "/api/plugins/crm/activities" if post => (201, r#"{"id":"a9"}"#.into()),
        "/api/plugins/crm/activities" => (200, r#"{"items":[{"id":"a1","activity_type":"call","subject":"Llamada","completed_at":"2026-10-19T16:00:00Z"}]}"#.into()),
        "/api/plugins/tickets/t1/comments" => (201, r#"{"comment":{"id":"k1","content":"x","is_internal":true}}"#.into()),
        "/api/notifications" => (200, r#"{"notifications":[{"id":"n1","type":"deadline","title":"Plazo","is_read":false,"created_at":"2026-10-21T10:00:00Z"}],"unread_count":1}"#.into()),
        "/api/calendar-events" => (200, r#"{"events":[{"id":"e1","title":"Audiencia","start_datetime":"2026-10-22T16:00:00Z","end_datetime":"2026-10-22T17:00:00Z","event_type":"court"}],"total":1}"#.into()),
        "/api/tasks" => (200, r#"{"tasks":[{"id":"k5","title":"Escrito","status":"pending","due_date":"2026-10-23"}],"pagination":{"total":1}}"#.into()),
        _ => (404, r#"{"error":"no existe"}"#.into()),
    }
}

async fn serve(f: Arc<Fake>) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { return };
            let f = Arc::clone(&f);
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = vec![0u8; 16_384];
                // Cabeceras y, si hay, el cuerpo completo.
                loop {
                    let Ok(n) = sock.read(&mut chunk).await else { return };
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&buf).to_string();
                    if let Some(h) = text.find("\r\n\r\n") {
                        let len = text
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().to_string())
                            })
                            .and_then(|v| v.parse::<usize>().ok())
                            .unwrap_or(0);
                        if buf.len() >= h + 4 + len {
                            break;
                        }
                    }
                }
                let req = String::from_utf8_lossy(&buf).to_string();
                f.log.lock().unwrap().push(req.clone());
                let (status, body) = route(&f, &req);
                let resp = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                sock.write_all(resp.as_bytes()).await.ok();
                sock.shutdown().await.ok();
            });
        }
    });
    port
}

fn session(dir: &std::path::Path) -> Session {
    let store = Store::open(dir).unwrap();
    sample::populate(&store, sample::DEFAULT_SEED, now()).unwrap();
    let mut cfg = SessionConfig::offline(Arc::new(store));
    cfg.now = Some(now());
    Session::new(cfg).unwrap()
}

fn wait(s: &mut Session, what: &str, mut ok: impl FnMut(&mut Session) -> bool) {
    let start = Instant::now();
    loop {
        s.poll();
        if ok(s) {
            return;
        }
        assert!(start.elapsed() < Duration::from_secs(20), "no llegó: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Un mensaje con Message-ID y remitente.
fn some_message(s: &mut Session) -> MessageId {
    let rows: Vec<_> = s.rows().iter().map(|r| r.id).take(200).collect();
    rows.into_iter().find(|id| s.envelope(*id).is_some_and(|e| e.message_id.is_some() && !e.from.is_empty())).unwrap()
}

fn start(s: &mut Session, port: u16) {
    let account = Account::new(&format!("http://127.0.0.1:{port}"), "laura@despacho.mx").unwrap();
    s.iure_start(IureSetup::With { account, vault: vault() });
    wait(s, "sesión lista", |s| s.iure_status().is_ready());
}

#[tokio::test(flavor = "multi_thread")]
async fn context_link_feed_and_agenda_against_a_fake_instance() {
    let fake = Arc::new(Fake::default());
    let port = serve(Arc::clone(&fake)).await;
    let dir = tempfile::tempdir().unwrap();
    tokio::task::block_in_place(|| {
        let mut s = session(dir.path());
        start(&mut s, port);
        let id = some_message(&mut s);
        // Contexto: persona/lead por remitente, oportunidad de la organización, caso, ticket.
        wait(&mut s, "contexto", |s| s.iure_panel(id).context.is_some_and(|c| !c.tickets.is_empty()));
        let p = s.iure_panel(id);
        assert!(p.visible() && p.linkable);
        let c = p.context.unwrap();
        assert_eq!(c.people.first().map(|p| p.name.as_str()), Some("Persona Uno"));
        assert!(c.deals.iter().any(|d| d.kind == TargetKind::Opportunity && d.amount == Some(120000.5)));
        assert_eq!(c.tickets[0].number, "TK-1");
        assert!(!c.activities.is_empty());

        // Vincular al ticket: comentario INTERNO por omisión, y el vínculo queda en el almacén.
        let t = LinkTarget { kind: TargetKind::Ticket, id: "t1".into(), label: "TK-1 · Portal".into(), detail: None };
        s.iure_link(id, t, LinkOptions::default()).unwrap();
        wait(&mut s, "vínculo", |s| !s.iure_busy());
        let posted = fake.log.lock().unwrap().iter().find(|r| r.contains("/tickets/t1/comments")).cloned().unwrap();
        assert!(posted.contains(r#""is_internal":true"#), "{posted}");
        let links: Vec<EmailLink> = s.iure_panel(id).links;
        assert!(links.iter().any(|l| l.kind == "ticket" && l.id == "t1" && l.how == "comment"), "{links:?}");

        // Avisos y agenda.
        wait(&mut s, "avisos", |s| s.iure_feed().unread >= 1);
        wait(&mut s, "agenda", |s| s.calendars().iter().any(|c| c.name == "Iurefficient"));
        let cal = s.calendars().iter().find(|c| c.name == "Iurefficient").unwrap().clone();
        assert!(cal.read_only);
        let evs = s
            .store()
            .events_between(&[cal.id], now() - chrono::Duration::days(1), now() + chrono::Duration::days(3))
            .unwrap();
        assert_eq!(evs.len(), 1);
        assert!(s.task_lists().iter().any(|l| l.name == "Iurefficient" && !s.tasks(l.id).is_empty()));
    });
}

#[tokio::test(flavor = "multi_thread")]
async fn terms_not_accepted_is_shown_and_links_stay_local() {
    let fake = Arc::new(Fake::default());
    let port = serve(Arc::clone(&fake)).await;
    let dir = tempfile::tempdir().unwrap();
    tokio::task::block_in_place(|| {
        let mut s = session(dir.path());
        start(&mut s, port);
        fake.terms.store(true, Ordering::SeqCst);
        let id = some_message(&mut s);
        let _ = s.iure_panel(id);
        wait(&mut s, "403 de términos", |s| {
            let _ = s.iure_panel(id);
            matches!(s.iure_status(), IureStatus::TermsRequired { .. })
        });
        let p = s.iure_panel(id);
        assert!(p.visible(), "el panel sigue visible con lo que se recuerda");
        assert!(p.error.is_some());
        assert!(s.iure_web_url().unwrap().starts_with("http://127.0.0.1"));
        // Aceptados los términos, «Reintentar» vuelve a Listo.
        fake.terms.store(false, Ordering::SeqCst);
        s.iure_retry();
        wait(&mut s, "de vuelta", |s| s.iure_status().is_ready());
    });
}

#[tokio::test(flavor = "multi_thread")]
async fn unreachable_instance_is_offline_and_never_blocks() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let dir = tempfile::tempdir().unwrap();
    tokio::task::block_in_place(|| {
        let mut s = session(dir.path());
        let account = Account::new(&format!("http://127.0.0.1:{port}"), "laura@despacho.mx").unwrap();
        let t0 = Instant::now();
        s.iure_start(IureSetup::With { account, vault: vault() });
        assert!(t0.elapsed() < Duration::from_millis(500), "iure_start no espera a la red");
        wait(&mut s, "sin conexión", |s| matches!(s.iure_status(), IureStatus::Offline { .. }));
        let id = some_message(&mut s);
        // Un vínculo recordado se ve aunque no haya red.
        let mid = s.envelope(id).and_then(|e| e.message_id.clone()).unwrap();
        s.store()
            .add_email_link(
                &mid,
                &EmailLink {
                    kind: "case".into(),
                    id: "c1".into(),
                    label: "241/2026".into(),
                    how: "document".into(),
                    remote_ref: None,
                    created: now(),
                },
            )
            .unwrap();
        s.iure_unlink(id, "nada", "x");
        let p = s.iure_panel(id);
        assert!(p.visible());
        assert_eq!(p.links.len(), 1);
        // Las acciones fallan con un mensaje humano, no con pánico.
        assert!(s.iure_create_task(id, Default::default()).is_err());
    });
}

#[test]
fn without_iurefficient_the_panel_is_hidden() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(dir.path());
    let id = some_message(&mut s);
    assert_eq!(*s.iure_status(), IureStatus::Disabled);
    assert!(!s.iure_panel(id).visible());
}
