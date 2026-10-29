//! Extremo a extremo contra un Microsoft Graph falso (tokio): delta paginado de eventos,
//! contactos y To Do, borrados `@removed`, `PATCH` solo con lo cambiado, 412 con fusión, alta,
//! baja y respuesta a invitaciones por `/accept`.

#[path = "../../dav/tests/common/mod.rs"]
mod common;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{Req, Resp, serve};
use iuredesk_accounts::MemoryStore;
use iuredesk_model::account::{Account, AccountKind, AuthMethod, OAuthProvider, Protocol, Security, ServerConfig};
use iuredesk_model::{Event, EventTime, PartStat, TaskStatus};
use iuredesk_oauth::{BoxFuture, TokenAudience, TokenProvider};
use iuredesk_pim_sync::{ConflictResolution, PimAccount, PimEvent, PimSource, PimSyncEngine, PimSyncOptions};
use iuredesk_store::Store;
use serde_json::{Value, json};

struct FakeTokens;

impl TokenProvider for FakeTokens {
    fn access_token<'a>(
        &'a self,
        _a: &'a Account,
        aud: TokenAudience,
    ) -> BoxFuture<'a, Result<String, iuredesk_oauth::Error>> {
        Box::pin(async move {
            assert_eq!(aud, TokenAudience::Api);
            Ok("tok-graph".to_string())
        })
    }
    fn invalidate(&self, _account_id: &str) {}
}

#[derive(Default)]
struct G {
    base: String,
    events: BTreeMap<String, Value>,
    etag_seq: u64,
    /// Fase del delta de eventos (0: inicial; 1: tras el primer deltaLink).
    phase: u32,
    requests: Vec<String>,
    patches: Vec<(String, Value)>,
    accepted: Vec<String>,
}

impl G {
    fn bump(&mut self, id: &str) {
        self.etag_seq += 1;
        let tag = format!("W/\"{}\"", self.etag_seq);
        if let Some(Value::Object(m)) = self.events.get_mut(id) {
            m.insert("@odata.etag".into(), json!(tag));
        }
    }
}

fn ev(id: &str, subject: &str, day: u32) -> Value {
    json!({
        "id": id, "iCalUId": format!("uid-{id}"), "subject": subject,
        "start": {"dateTime": format!("2026-11-{day:02}T15:00:00.0000000"), "timeZone": "UTC"},
        "end": {"dateTime": format!("2026-11-{day:02}T16:00:00.0000000"), "timeZone": "UTC"},
        "originalStartTimeZone": "Romance Standard Time",
        "location": {"displayName": "Madrid"}, "showAs": "busy", "isAllDay": false,
        "organizer": {"emailAddress": {"name": "Jefa", "address": "jefa@contoso.com"}},
        "attendees": [{"type": "required", "status": {"response": "none"}, "emailAddress": {"name": "Ana", "address": "ana@contoso.com"}}],
    })
}

fn handle(st: &Mutex<G>, req: Req) -> Resp {
    let mut g = st.lock().unwrap();
    g.requests.push(format!("{} {}", req.method, req.path));
    if req.header("Authorization") != Some("Bearer tok-graph") {
        return Resp::new(401, "");
    }
    let base = g.base.clone();
    let json_resp = |status: u16, v: Value| Resp::new(status, v.to_string()).with("Content-Type", "application/json");
    let (path, query) = req.path.split_once('?').unwrap_or((req.path.as_str(), ""));
    let path = path.strip_prefix("/v1.0").unwrap_or(path).to_string();
    match (req.method.as_str(), path.as_str()) {
        ("GET", "/me/calendars") => json_resp(
            200,
            json!({"value": [{"id": "cal1", "name": "Calendario", "canEdit": true, "hexColor": "#112233"}]}),
        ),
        ("GET", "/me/todo/lists") => json_resp(200, json!({"value": [{"id": "l1", "displayName": "Tareas"}]})),
        ("GET", "/me/calendars/cal1/calendarView/delta") => {
            if query.contains("deltatoken=d1") {
                // Segunda pasada: un cambio y un borrado.
                let mut changed = g.events.get("e2").cloned().unwrap_or(Value::Null);
                if let Value::Object(m) = &mut changed {
                    m.insert("subject".into(), json!("Comida (cambiada)"));
                }
                g.events.insert("e2".into(), changed.clone());
                g.bump("e2");
                g.events.remove("e3");
                let e2 = g.events.get("e2").cloned().unwrap_or(Value::Null);
                g.phase = 2;
                return json_resp(
                    200,
                    json!({
                        "value": [e2, {"id": "e3", "@removed": {"reason": "deleted"}}],
                        "@odata.deltaLink": format!("{base}/v1.0/me/calendars/cal1/calendarView/delta?$deltatoken=d2"),
                    }),
                );
            }
            if query.contains("deltatoken=d2") {
                return json_resp(
                    200,
                    json!({"value": [], "@odata.deltaLink": format!("{base}/v1.0/me/calendars/cal1/calendarView/delta?$deltatoken=d2")}),
                );
            }
            assert!(query.contains("startDateTime=") && query.contains("endDateTime="), "{query}");
            let (e1, e2) = (g.events.get("e1").cloned(), g.events.get("e2").cloned());
            json_resp(
                200,
                json!({
                    "value": [e1, e2],
                    "@odata.nextLink": format!("{base}/v1.0/me/calendars/cal1/calendarView/delta?$skiptoken=p2"),
                }),
            )
        }
        ("GET", "/me/contacts/delta") => {
            if query.contains("deltatoken=c") {
                return json_resp(
                    200,
                    json!({"value": [], "@odata.deltaLink": format!("{base}/v1.0/me/contacts/delta?$deltatoken=c")}),
                );
            }
            // Como Outlook.com: sin `$select`, el delta de contactos trae solo el id (UX-1).
            let select = query.contains("select=") && query.contains("emailAddresses") && query.contains("displayName");
            let value = if select {
                json!([
                    {"id": "c1", "@odata.etag": "W/\"c1\"", "displayName": "Luis Gómez", "givenName": "Luis", "surname": "Gómez",
                     "emailAddresses": [{"address": "luis@contoso.com", "name": "Luis"}], "mobilePhone": "+34 600",
                     "companyName": "Contoso", "jobTitle": "Ventas", "businessPhones": ["+34 911"]},
                    {"id": "c2", "@odata.etag": "W/\"c2\"", "displayName": "", "givenName": "Marta", "surname": "Ruiz",
                     "emailAddresses": [{"address": "marta@contoso.com", "name": "Marta"}]}
                ])
            } else {
                json!([{"id": "c1", "@odata.etag": "W/\"c1\""}, {"id": "c2", "@odata.etag": "W/\"c2\""}])
            };
            json_resp(
                200,
                json!({"value": value, "@odata.deltaLink": format!("{base}/v1.0/me/contacts/delta?$deltatoken=c")}),
            )
        }
        ("GET", "/me/todo/lists/l1/tasks/delta") => json_resp(
            200,
            json!({
                "value": [{"id": "t1", "@odata.etag": "W/\"t1\"", "title": "Enviar informe", "importance": "high", "status": "notStarted"}],
                "@odata.deltaLink": format!("{base}/v1.0/me/todo/lists/l1/tasks/delta?$deltatoken=t"),
            }),
        ),
        ("PATCH", p) if p.starts_with("/me/events/") => {
            let id = p.trim_start_matches("/me/events/").to_string();
            let Some(cur) = g.events.get(&id).cloned() else { return Resp::new(404, "") };
            let tag = cur.get("@odata.etag").and_then(Value::as_str).unwrap_or_default().to_string();
            if req.header("If-Match").is_some_and(|m| m != tag) {
                return json_resp(412, json!({"error": {"code": "ErrorIrresolvableConflict"}}));
            }
            let patch: Value = serde_json::from_str(&req.body).unwrap();
            g.patches.push((id.clone(), patch.clone()));
            let mut merged = cur;
            if let (Value::Object(m), Value::Object(p)) = (&mut merged, patch) {
                for (k, v) in p {
                    m.insert(k, v);
                }
            }
            g.events.insert(id.clone(), merged);
            g.bump(&id);
            json_resp(200, g.events.get(&id).cloned().unwrap_or(Value::Null))
        }
        ("GET", p) if p.starts_with("/me/events/") => {
            let id = p.trim_start_matches("/me/events/");
            match g.events.get(id) {
                Some(v) => json_resp(200, v.clone()),
                None => Resp::new(404, ""),
            }
        }
        ("POST", "/me/calendars/cal1/events") => {
            let mut v: Value = serde_json::from_str(&req.body).unwrap();
            if let Value::Object(m) = &mut v {
                m.insert("id".into(), json!("new1"));
                m.insert("iCalUId".into(), json!("uid-new1"));
            }
            g.events.insert("new1".into(), v);
            g.bump("new1");
            json_resp(201, g.events.get("new1").cloned().unwrap_or(Value::Null))
        }
        ("POST", p) if p.ends_with("/accept") => {
            g.accepted.push(p.to_string());
            Resp::new(202, "")
        }
        ("DELETE", p) if p.starts_with("/me/events/") => {
            let id = p.trim_start_matches("/me/events/").to_string();
            g.events.remove(&id);
            Resp::new(204, "")
        }
        _ => Resp::new(404, json!({"error": {"code": "NotFound"}}).to_string()),
    }
}

/// La segunda página del delta inicial (`$skiptoken=p2`) va por la misma ruta: se distingue aquí.
fn route(st: &Mutex<G>, req: Req) -> Resp {
    if req.path.contains("skiptoken=p2") {
        let mut g = st.lock().unwrap();
        g.requests.push(format!("{} {}", req.method, req.path));
        let base = g.base.clone();
        let e3 = g.events.get("e3").cloned();
        return Resp::new(
            200,
            json!({
                "value": [e3],
                "@odata.deltaLink": format!("{base}/v1.0/me/calendars/cal1/calendarView/delta?$deltatoken=d1"),
            })
            .to_string(),
        );
    }
    handle(st, req)
}

fn account() -> Account {
    let srv = |p| ServerConfig {
        protocol: p,
        host: "outlook.office365.com".into(),
        port: 993,
        security: Security::Tls,
        auth: AuthMethod::OAuth2 { provider: OAuthProvider::Microsoft },
        username: "ana@contoso.com".into(),
    };
    Account {
        id: "ana-m365".into(),
        display_name: "Ana".into(),
        email: "ana@contoso.com".into(),
        kind: AccountKind::Microsoft365,
        incoming: srv(Protocol::Imap),
        outgoing: srv(Protocol::Smtp),
        caldav_url: None,
        carddav_url: None,
        identities: vec![],
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn graph_delta_edits_conflicts_rsvp() {
    let state = Arc::new(Mutex::new(G::default()));
    {
        let mut g = state.lock().unwrap();
        for (id, s, d) in [("e1", "Revisión", 5), ("e2", "Comida", 6), ("e3", "Café", 7)] {
            g.events.insert(id.into(), ev(id, s, d));
            g.bump(id);
        }
    }
    let h = Arc::clone(&state);
    let base = serve(move |req| route(&h, req)).await;
    state.lock().unwrap().base = base.clone();

    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    let pim = PimAccount {
        account: account(),
        source: PimSource::Graph { base_url: format!("{base}/v1.0") },
        tasks_api: None,
    };
    assert!(PimAccount::from_account(&account()).is_some_and(|p| matches!(p.source, PimSource::Graph { .. })));
    let opts = PimSyncOptions { poll_interval: Duration::from_secs(3600), sync_on_start: false, ..Default::default() };
    let tokens: Arc<dyn TokenProvider> = Arc::new(FakeTokens);
    let handle =
        PimSyncEngine::start(Arc::clone(&store), vec![pim], Some(tokens), Arc::new(MemoryStore::default()), opts);
    let mut events = handle.events();
    handle.sync_now_wait("ana-m365").await.unwrap();

    // --- delta inicial en dos páginas + contactos + To Do
    let cal = store.calendars().unwrap().remove(0);
    assert_eq!((cal.name.as_str(), cal.color), ("Calendario", Some(0x112233)));
    let mut evs = store.events_in_calendar(cal.id).unwrap();
    evs.sort_by_key(|e| e.start.to_utc());
    assert_eq!(evs.iter().map(|e| e.summary.as_str()).collect::<Vec<_>>(), vec!["Revisión", "Comida", "Café"]);
    assert_eq!(
        evs[0].start,
        EventTime::DateTime {
            utc: chrono::TimeZone::with_ymd_and_hms(&chrono::Utc, 2026, 11, 5, 15, 0, 0).unwrap(),
            tzid: Some("Europe/Paris".into())
        }
    );
    let book = store.address_books().unwrap().remove(0);
    // Regresión UX-1: los contactos llegan con sus datos (se piden con `$select`).
    let mut cs = store.contacts(book.id).unwrap();
    cs.sort_by(|a, b| a.full_name.cmp(&b.full_name));
    assert_eq!(cs.iter().map(|c| c.full_name.as_str()).collect::<Vec<_>>(), vec!["Luis Gómez", "Marta Ruiz"]);
    assert_eq!((cs[0].org.as_str(), cs[0].title.as_str(), cs[0].phones.len()), ("Contoso", "Ventas", 2));
    assert_eq!(cs[0].primary_email(), Some("luis@contoso.com"));
    assert_eq!(cs[1].primary_email(), Some("marta@contoso.com"));
    // Un estado de una versión anterior (deltaLink de antes de `$select`) se descarta.
    {
        let cref = iuredesk_store::CollectionRef::AddressBook(book.id);
        let mut st = store.collection_sync(cref).unwrap();
        assert_eq!(st.ctag.as_deref(), Some("graph-contacts-v2"));
        st.ctag = None;
        store.put_collection_sync(cref, &st).unwrap();
        let mut stale = cs[0].clone();
        stale.org.clear();
        store.put_contact(&stale).unwrap();
    }
    let list = store.task_lists().unwrap().remove(0);
    let task = store.tasks(list.id).unwrap().remove(0);
    assert_eq!(
        (task.summary.as_str(), task.priority, task.status.clone()),
        ("Enviar informe", 1, TaskStatus::NeedsAction)
    );

    // --- segunda pasada con deltaLink: cambio y borrado
    handle.sync_now_wait("ana-m365").await.unwrap();
    assert!(store.contacts(book.id).unwrap().iter().any(|c| c.org == "Contoso"), "re-descarga completa");
    let evs = store.events_in_calendar(cal.id).unwrap();
    assert_eq!(evs.len(), 2);
    assert!(evs.iter().any(|e| e.summary == "Comida (cambiada)"));

    // --- edición local: PATCH solo con el título
    let mut e1 = evs.iter().find(|e| e.summary == "Revisión").cloned().unwrap();
    e1.summary = "Revisión trimestral".into();
    handle.save_event(&e1).unwrap();
    handle.sync_now_wait("ana-m365").await.unwrap();
    {
        let g = state.lock().unwrap();
        let (id, patch) = g.patches.last().unwrap();
        assert_eq!(id, "e1");
        assert_eq!(patch.as_object().unwrap().keys().collect::<Vec<_>>(), vec!["subject"]);
        assert_eq!(g.events["e1"]["subject"], "Revisión trimestral");
    }

    // --- 412: el servidor cambió el lugar, nosotros el título → fusión
    let _ = drain(&mut events);
    // (El servidor cambia antes de guardar: `save_event` dispara la subida enseguida.)
    {
        let mut g = state.lock().unwrap();
        if let Some(Value::Object(m)) = g.events.get_mut("e1") {
            m.insert("location".into(), json!({"displayName": "Barcelona"}));
        }
        g.bump("e1");
    }
    let mut e1 = store.get_event(e1.id).unwrap().unwrap();
    e1.summary = "Revisión (local)".into();
    handle.save_event(&e1).unwrap();
    handle.sync_now_wait("ana-m365").await.unwrap();
    {
        let g = state.lock().unwrap();
        assert_eq!(g.events["e1"]["subject"], "Revisión (local)");
        assert_eq!(g.events["e1"]["location"]["displayName"], "Barcelona");
    }
    let local = store.get_event(e1.id).unwrap().unwrap();
    assert_eq!((local.summary.as_str(), local.location.as_str()), ("Revisión (local)", "Barcelona"));
    assert!(
        drain(&mut events)
            .iter()
            .any(|e| matches!(e, PimEvent::Conflict { resolution: ConflictResolution::Merged, .. }))
    );

    // --- alta, RSVP por /accept (sin correo iMIP) y baja
    let start = chrono::TimeZone::with_ymd_and_hms(&chrono::Utc, 2026, 11, 20, 9, 0, 0).unwrap();
    let id = handle
        .save_event(&Event {
            calendar: cal.id,
            summary: "Nueva".into(),
            start: EventTime::DateTime { utc: start, tzid: None },
            end: EventTime::DateTime { utc: start + chrono::Duration::hours(1), tzid: None },
            ..Default::default()
        })
        .unwrap();
    let rsvp = handle.rsvp(e1.id, PartStat::Accepted, None).unwrap();
    assert!(rsvp.is_none(), "Graph manda la respuesta");
    handle.sync_now_wait("ana-m365").await.unwrap();
    let created = store.get_event(id).unwrap().unwrap();
    assert_eq!(created.remote_id.as_deref(), Some("new1"));
    assert_eq!(created.uid, "uid-new1");
    assert!(state.lock().unwrap().accepted.iter().any(|p| p.contains("/me/events/e1/accept")));
    handle.delete_event(id).unwrap();
    handle.sync_now_wait("ana-m365").await.unwrap();
    assert!(!state.lock().unwrap().events.contains_key("new1"));
    assert_eq!(store.ops_len().unwrap(), 0);
    handle.shutdown().await;
}

fn drain(rx: &mut tokio::sync::broadcast::Receiver<PimEvent>) -> Vec<PimEvent> {
    let mut v = Vec::new();
    while let Ok(e) = rx.try_recv() {
        v.push(e);
    }
    v
}
