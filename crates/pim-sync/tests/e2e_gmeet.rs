//! UX-11 de extremo a extremo: un evento de Google que pide videollamada se sube por CalDAV y
//! luego la API de Calendar le crea la sala de Meet (`conferenceData.createRequest`,
//! `sendUpdates=all`); el enlace queda en el evento local. Si Google lo rechaza, la cita se queda
//! sin videollamada y se avisa en palabras.

mod fake_dav;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{TimeZone, Utc};
use fake_dav::common::{Req, Resp, serve};
use fake_dav::{CAL, State};
use iuredesk_accounts::{MemoryStore, SecretPurpose, SecretStore};
use iuredesk_model::account::{Account, AccountKind, AuthMethod, Protocol, Security, ServerConfig};
use iuredesk_model::{Event, EventTime, MeetingProvider, OnlineMeeting};
use iuredesk_oauth::{BoxFuture, TokenAudience, TokenProvider};
use iuredesk_pim_sync::{DavAuth, PimAccount, PimEvent, PimSource, PimSyncEngine, PimSyncOptions};
use iuredesk_store::Store;
use serde_json::{Value, json};

struct FakeTokens;

impl TokenProvider for FakeTokens {
    fn access_token<'a>(
        &'a self,
        _a: &'a Account,
        _aud: TokenAudience,
    ) -> BoxFuture<'a, Result<String, iuredesk_oauth::Error>> {
        Box::pin(async move { Ok("tok-google".to_string()) })
    }
    fn invalidate(&self, _account_id: &str) {}
}

#[derive(Default)]
struct G {
    /// Responder la creación con este estado (200 = bien).
    status: u16,
    requests: Vec<String>,
    patches: Vec<Value>,
}

fn route(st: &Mutex<G>, req: Req) -> Resp {
    let mut g = st.lock().unwrap();
    assert_eq!(req.header("authorization"), Some("Bearer tok-google"));
    g.requests.push(format!("{} {}", req.method, req.path));
    let path = req.path.split('?').next().unwrap_or_default().to_string();
    let json_resp = |code: u16, v: Value| Resp::new(code, v.to_string()).with("Content-Type", "application/json");
    match (req.method.as_str(), path.as_str()) {
        ("GET", p) if p.starts_with("/tasks/v1/users/") => json_resp(200, json!({ "items": [] })),
        ("GET", "/calendar/v3/calendars/work/events") => {
            assert!(req.path.contains("iCalUID=junta-1"), "{}", req.path);
            json_resp(200, json!({ "items": [{ "id": "g1", "iCalUID": "junta-1" }] }))
        }
        ("PATCH", "/calendar/v3/calendars/work/events/g1") => {
            g.patches.push(serde_json::from_str(&req.body).unwrap());
            if g.status != 200 {
                return json_resp(g.status, json!({ "error": { "code": g.status, "message": "Forbidden" } }));
            }
            json_resp(
                200,
                json!({ "id": "g1", "hangoutLink": "https://meet.google.com/abc-defg-hij",
                        "conferenceData": { "createRequest": { "status": { "statusCode": "success" } } } }),
            )
        }
        _ => Resp::new(404, "{}"),
    }
}

fn account() -> Account {
    let srv = |p| ServerConfig {
        protocol: p,
        host: "imap.gmail.com".into(),
        port: 993,
        security: Security::Tls,
        auth: AuthMethod::Password,
        username: "ana".into(),
    };
    Account {
        id: "ana-gmail".into(),
        display_name: "Ana".into(),
        email: "ana@example.com".into(),
        kind: AccountKind::Gmail,
        incoming: srv(Protocol::Imap),
        outgoing: srv(Protocol::Smtp),
        caldav_url: None,
        carddav_url: None,
        identities: vec![],
    }
}

fn meeting_event(cal: iuredesk_model::CalendarId) -> Event {
    let at = Utc.with_ymd_and_hms(2026, 11, 10, 16, 0, 0).unwrap();
    Event {
        calendar: cal,
        uid: "junta-1".into(),
        summary: "Junta con Altamira".into(),
        start: EventTime::DateTime { utc: at, tzid: None },
        end: EventTime::DateTime { utc: at + chrono::Duration::hours(1), tzid: None },
        online_meeting: Some(OnlineMeeting { provider: MeetingProvider::GoogleMeet, join_url: String::new() }),
        ..Default::default()
    }
}

async fn run(status: u16) -> (Arc<Store>, Arc<Mutex<G>>, Arc<Mutex<State>>, Vec<PimEvent>) {
    let (dav, server) = fake_dav::start(State::default()).await;
    let g = Arc::new(Mutex::new(G { status, ..Default::default() }));
    let h = Arc::clone(&g);
    let google = serve(move |req| route(&h, req)).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    let secrets = Arc::new(MemoryStore::default());
    secrets.set("ana-gmail", SecretPurpose::Incoming, "secreto").unwrap();
    let pim = PimAccount {
        account: account(),
        source: PimSource::Dav {
            caldav: Some(format!("{dav}/dav/")),
            carddav: None,
            auth: DavAuth::Password { username: "ana".into() },
        },
        tasks_api: Some(format!("{google}/tasks/v1")),
        calendar_api: Some(format!("{google}/calendar/v3")),
    };
    let opts = PimSyncOptions { poll_interval: Duration::from_secs(3600), sync_on_start: false, ..Default::default() };
    let tokens: Arc<dyn TokenProvider> = Arc::new(FakeTokens);
    let handle = PimSyncEngine::start(Arc::clone(&store), vec![pim], Some(tokens), secrets, opts);
    let mut events = handle.events();
    handle.sync_now_wait("ana-gmail").await.unwrap();
    let cal = store.calendars().unwrap().into_iter().next().unwrap();
    assert!(cal.remote_path.ends_with(CAL), "{}", cal.remote_path);
    handle.save_event(&meeting_event(cal.id)).unwrap();
    let _ = handle.sync_now_wait("ana-gmail").await;
    let mut seen = Vec::new();
    while let Ok(e) = events.try_recv() {
        seen.push(e);
    }
    handle.shutdown().await;
    std::mem::forget(dir);
    (store, g, server, seen)
}

fn the_event(store: &Store) -> Event {
    let cal = store.calendars().unwrap().into_iter().next().unwrap();
    store.events_in_calendar(cal.id).unwrap().into_iter().find(|e| e.uid == "junta-1").unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn google_event_gets_a_meet_room() {
    let (store, g, server, _) = run(200).await;
    // Subido por CalDAV…
    assert!(server.lock().unwrap().puts.iter().any(|(p, b)| p.starts_with(CAL) && b.contains("UID:junta-1")));
    // …y la sala pedida a la API de Calendar, avisando a los invitados.
    let g = g.lock().unwrap();
    let patch = g.requests.iter().find(|r| r.starts_with("PATCH")).unwrap();
    assert!(patch.contains("conferenceDataVersion=1") && patch.contains("sendUpdates=all"), "{patch}");
    let req = &g.patches[0]["conferenceData"]["createRequest"];
    assert_eq!(req["conferenceSolutionKey"]["type"], json!("hangoutsMeet"));
    assert!(req["requestId"].as_str().unwrap().starts_with("iuredesk"));
    let ev = the_event(&store);
    assert_eq!(
        ev.online_meeting,
        Some(OnlineMeeting {
            provider: MeetingProvider::GoogleMeet,
            join_url: "https://meet.google.com/abc-defg-hij".into()
        })
    );
    assert!(store.peek_ops(100).unwrap().is_empty(), "nada pendiente");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejected_meet_leaves_the_event_and_says_why() {
    let (store, g, _, seen) = run(403).await;
    assert_eq!(g.lock().unwrap().patches.len(), 1, "no se reintenta un 403");
    let ev = the_event(&store);
    assert_eq!(ev.online_meeting, None, "la cita queda sin videollamada");
    assert!(
        seen.iter().any(|e| matches!(e, PimEvent::Error(f) if f.es.contains("Google Meet") && !f.transient)),
        "aviso en palabras"
    );
    assert!(store.peek_ops(100).unwrap().is_empty());
}
