//! Cliente DAV contra un servidor CalDAV falso con estado: descubrimiento con redirección,
//! colecciones, sync-collection (con páginas 507 y token caducado), caída a ctag+ETag,
//! conflictos 412, multiget, free/busy, 401 y topes de tamaño.

mod common;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use chrono::{TimeZone, Utc};
use common::{Req, Resp, between, multistatus, serve};
use iuredesk_dav::dav::{CollectionKind, Precondition, Service, SyncMethod, SyncState};
use iuredesk_dav::{AuthProvider, BasicAuth, DavClient, DavOptions, Error};
use reqwest::Url;

const CAL: &str = "/dav/calendars/ana/work/";

#[derive(Default)]
struct State {
    /// nombre → (etag, datos)
    items: BTreeMap<String, (u64, String)>,
    /// (secuencia, nombre, borrado)
    log: Vec<(u64, String, bool)>,
    seq: u64,
    sync_supported: bool,
    page_size: usize,
    requests: Vec<String>,
}

impl State {
    fn change(&mut self, name: &str, data: Option<String>) -> u64 {
        self.seq += 1;
        match data {
            Some(d) => {
                self.items.insert(name.to_string(), (self.seq, d));
            }
            None => {
                self.items.remove(name);
            }
        }
        self.log.push((self.seq, name.to_string(), !self.items.contains_key(name)));
        self.seq
    }
}

fn ics(uid: &str, summary: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Fake//EN\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20261026T000000Z\r\n\
DTSTART:20261110T150000Z\r\nDTEND:20261110T160000Z\r\nSUMMARY:{summary}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
    )
}

fn etag(n: u64) -> String {
    format!("\"e{n}\"")
}

fn handle(state: &Mutex<State>, req: Req) -> Resp {
    let mut st = state.lock().unwrap();
    st.requests.push(format!("{} {}", req.method, req.path));
    let expected = format!("Basic {}", base64::engine::general_purpose::STANDARD.encode("ana:secreto"));
    if req.header("Authorization") != Some(expected.as_str()) {
        return Resp::new(401, "").with("WWW-Authenticate", "Basic realm=\"x\"");
    }
    let path = req.path.as_str();
    match (req.method.as_str(), path) {
        ("PROPFIND", "/.well-known/caldav") => Resp::new(301, "").with("Location", "/dav/"),
        ("PROPFIND", "/dav/") => Resp::xml(
            207,
            multistatus(
                "<d:response><d:href>/dav/</d:href><d:propstat><d:prop><d:current-user-principal>\
<d:href>/dav/principals/ana/</d:href></d:current-user-principal><d:resourcetype><d:collection/></d:resourcetype>\
</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat><d:propstat><d:prop><c:calendar-home-set/></d:prop>\
<d:status>HTTP/1.1 404 Not Found</d:status></d:propstat></d:response>",
            ),
        ),
        ("PROPFIND", "/dav/principals/ana/") => Resp::xml(
            207,
            multistatus(
                "<d:response><d:href>/dav/principals/ana/</d:href><d:propstat><d:prop>\
<d:displayname>Ana López</d:displayname>\
<c:calendar-home-set><d:href>/dav/calendars/ana/</d:href></c:calendar-home-set>\
<c:schedule-outbox-URL><d:href>/dav/calendars/ana/outbox/</d:href></c:schedule-outbox-URL>\
<c:calendar-user-address-set><d:href>mailto:ana@example.com</d:href><d:href>/dav/principals/ana/</d:href></c:calendar-user-address-set>\
</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>",
            ),
        ),
        ("PROPFIND", "/dav/calendars/ana/") => {
            let token =
                if st.sync_supported { format!("<d:sync-token>tok-{}</d:sync-token>", st.seq) } else { String::new() };
            Resp::xml(
                207,
                multistatus(&format!(
                    "<d:response><d:href>/dav/calendars/ana/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype>\
</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>\
<d:response><d:href>{CAL}</d:href><d:propstat><d:prop><d:resourcetype><d:collection/><c:calendar/></d:resourcetype>\
<d:displayname>Trabajo</d:displayname><a:calendar-color>#0B5FFFFF</a:calendar-color><cs:getctag>ctag-{seq}</cs:getctag>{token}\
<c:supported-calendar-component-set><c:comp name=\"VEVENT\"/><c:comp name=\"VTODO\"/></c:supported-calendar-component-set>\
<d:current-user-privilege-set><d:privilege><d:read/></d:privilege><d:privilege><d:write/></d:privilege></d:current-user-privilege-set>\
</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>\
<d:response><d:href>/dav/calendars/ana/inbox/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/><c:schedule-inbox/></d:resourcetype>\
</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>",
                    seq = st.seq
                )),
            )
        }
        ("PROPFIND", CAL) => {
            if req.header("Depth") == Some("0") {
                return Resp::xml(
                    207,
                    multistatus(&format!(
                        "<d:response><d:href>{CAL}</d:href><d:propstat><d:prop><d:resourcetype><d:collection/><c:calendar/></d:resourcetype>\
<cs:getctag>ctag-{}</cs:getctag></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>",
                        st.seq
                    )),
                );
            }
            let mut inner = format!(
                "<d:response><d:href>{CAL}</d:href><d:propstat><d:prop><d:resourcetype><d:collection/><c:calendar/></d:resourcetype>\
</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"
            );
            for (name, (e, _)) in &st.items {
                inner.push_str(&format!(
                    "<d:response><d:href>{CAL}{name}</d:href><d:propstat><d:prop><d:getetag>{}</d:getetag><d:resourcetype/></d:prop>\
<d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>",
                    etag(*e)
                ));
            }
            Resp::xml(207, multistatus(&inner))
        }
        ("REPORT", CAL) if req.body.contains("sync-collection") => {
            if !st.sync_supported {
                return Resp::new(501, "");
            }
            let token = between(&req.body, "<d:sync-token>", "</d:sync-token>").unwrap_or_default();
            let mut inner = String::new();
            let new_token;
            if token.is_empty() {
                for (name, (e, _)) in &st.items {
                    inner.push_str(&format!(
                        "<d:response><d:href>{CAL}{name}</d:href><d:propstat><d:prop><d:getetag>{}</d:getetag></d:prop>\
<d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>",
                        etag(*e)
                    ));
                }
                new_token = st.seq;
            } else {
                let Some(since) =
                    token.strip_prefix("tok-").and_then(|n| n.parse::<u64>().ok()).filter(|n| *n <= st.seq)
                else {
                    return Resp::xml(
                        403,
                        r#"<?xml version="1.0"?><d:error xmlns:d="DAV:"><d:valid-sync-token/></d:error>"#,
                    );
                };
                let changes: Vec<&(u64, String, bool)> = st.log.iter().filter(|(s, _, _)| *s > since).collect();
                let page: Vec<_> = changes.iter().take(st.page_size).collect();
                new_token = page.last().map(|(s, _, _)| *s).unwrap_or(since);
                for (_, name, deleted) in &page {
                    if *deleted {
                        inner.push_str(&format!(
                            "<d:response><d:href>{CAL}{name}</d:href><d:status>HTTP/1.1 404 Not Found</d:status></d:response>"
                        ));
                    } else {
                        let e = st.items.get(name).map(|(e, _)| *e).unwrap_or(0);
                        inner.push_str(&format!(
                            "<d:response><d:href>{CAL}{name}</d:href><d:propstat><d:prop><d:getetag>{}</d:getetag></d:prop>\
<d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>",
                            etag(e)
                        ));
                    }
                }
                if changes.len() > page.len() {
                    inner.push_str(&format!(
                        "<d:response><d:href>{CAL}</d:href><d:status>HTTP/1.1 507 Insufficient Storage</d:status></d:response>"
                    ));
                }
            }
            inner.push_str(&format!("<d:sync-token>tok-{new_token}</d:sync-token>"));
            Resp::xml(207, multistatus(&inner))
        }
        ("REPORT", CAL) if req.body.contains("calendar-multiget") || req.body.contains("calendar-query") => {
            let mut inner = String::new();
            let wanted: Vec<String> = req
                .body
                .split("<d:href>")
                .skip(1)
                .filter_map(|s| s.split("</d:href>").next())
                .map(str::to_string)
                .collect();
            for (name, (e, data)) in &st.items {
                let href = format!("{CAL}{name}");
                if req.body.contains("calendar-multiget") && !wanted.contains(&href) {
                    continue;
                }
                inner.push_str(&format!(
                    "<d:response><d:href>{href}</d:href><d:propstat><d:prop><d:getetag>{}</d:getetag>\
<c:calendar-data>{}</c:calendar-data></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>",
                    etag(*e),
                    data.replace('&', "&amp;").replace('<', "&lt;")
                ));
            }
            for w in wanted.iter().filter(|w| st.items.keys().all(|n| format!("{CAL}{n}") != **w)) {
                inner.push_str(&format!(
                    "<d:response><d:href>{w}</d:href><d:status>HTTP/1.1 404 Not Found</d:status></d:response>"
                ));
            }
            Resp::xml(207, multistatus(&inner))
        }
        ("POST", "/dav/calendars/ana/outbox/") => Resp::xml(
            200,
            r#"<?xml version="1.0" encoding="utf-8"?><C:schedule-response xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
<C:response><C:recipient><D:href>mailto:juan@example.com</D:href></C:recipient><C:request-status>2.0;Success</C:request-status>
<C:calendar-data>BEGIN:VCALENDAR
VERSION:2.0
METHOD:REPLY
BEGIN:VFREEBUSY
UID:fb1
FREEBUSY;FBTYPE=BUSY:20261110T150000Z/20261110T160000Z
END:VFREEBUSY
END:VCALENDAR
</C:calendar-data></C:response>
<C:response><C:recipient><D:href>mailto:nadie@example.com</D:href></C:recipient><C:request-status>3.7;Invalid calendar user</C:request-status></C:response>
</C:schedule-response>"#,
        ),
        ("GET", p) if p.starts_with(CAL) => {
            let name = &p[CAL.len()..];
            match st.items.get(name) {
                Some((e, d)) => Resp::new(200, d.clone()).with("ETag", &etag(*e)),
                None => Resp::new(404, ""),
            }
        }
        ("PUT", p) if p.starts_with(CAL) => {
            let name = p[CAL.len()..].to_string();
            let current = st.items.get(&name).map(|(e, _)| etag(*e));
            if req.header("If-None-Match") == Some("*") && current.is_some() {
                return Resp::new(412, "");
            }
            if let Some(m) = req.header("If-Match")
                && current.as_deref() != Some(m)
            {
                return Resp::new(412, "");
            }
            let created = current.is_none();
            let n = st.change(&name, Some(req.body.clone()));
            Resp::new(if created { 201 } else { 204 }, "").with("ETag", &etag(n))
        }
        ("DELETE", p) if p.starts_with(CAL) => {
            let name = p[CAL.len()..].to_string();
            let Some(current) = st.items.get(&name).map(|(e, _)| etag(*e)) else { return Resp::new(404, "") };
            if let Some(m) = req.header("If-Match")
                && m != current
            {
                return Resp::new(412, "");
            }
            st.change(&name, None);
            Resp::new(204, "")
        }
        ("GET", "/huge") => Resp::new(200, vec![b'x'; 4096]),
        _ => Resp::new(404, ""),
    }
}

async fn fake(sync_supported: bool) -> (String, Arc<Mutex<State>>) {
    let state = Arc::new(Mutex::new(State { sync_supported, page_size: 2, ..State::default() }));
    {
        let mut st = state.lock().unwrap();
        for (i, s) in ["Junta", "Comida", "Cine"].iter().enumerate() {
            let name = format!("{i}.ics");
            st.change(&name, Some(ics(&format!("uid-{i}"), s)));
        }
    }
    let s2 = state.clone();
    let base = serve(move |req| handle(&s2, req)).await;
    (base, state)
}

fn client() -> DavClient {
    DavClient::new(BasicAuth::new("ana", "secreto"), DavOptions::default()).unwrap()
}

#[tokio::test]
async fn discovery_redirect_and_collections() {
    let (base, state) = fake(true).await;
    let c = client();
    // Se escribe solo el servidor: .well-known → 301 → /dav/ (el PROPFIND se conserva).
    let (d, cols) = c.discover_collections(&base, Service::CalDav).await.unwrap();
    assert_eq!(d.principal.as_ref().unwrap().path(), "/dav/principals/ana/");
    assert_eq!(d.calendar_homes[0].path(), "/dav/calendars/ana/");
    assert_eq!(d.schedule_outbox.as_ref().unwrap().path(), "/dav/calendars/ana/outbox/");
    assert_eq!(d.display_name.as_deref(), Some("Ana López"));
    assert!(d.user_addresses.contains(&"mailto:ana@example.com".to_string()));
    assert_eq!(cols.len(), 1, "el hogar y el inbox no son calendarios");
    let cal = &cols[0];
    assert_eq!(cal.kind, CollectionKind::Calendar);
    assert_eq!(cal.display_name.as_deref(), Some("Trabajo"));
    assert_eq!(cal.color.as_deref(), Some("#0B5FFFFF"));
    assert_eq!(cal.ctag.as_deref(), Some("ctag-3"));
    assert_eq!(cal.sync_token.as_deref(), Some("tok-3"));
    assert_eq!(cal.components, vec!["VEVENT", "VTODO"]);
    assert!(!cal.read_only);
    let reqs = state.lock().unwrap().requests.clone();
    assert!(reqs.contains(&"PROPFIND /.well-known/caldav".to_string()));
    assert!(reqs.contains(&"PROPFIND /dav/".to_string()));
}

#[tokio::test]
async fn wrong_password_is_unauthorized() {
    let (base, _) = fake(true).await;
    let c = DavClient::new(BasicAuth::new("ana", "mal"), DavOptions::default()).unwrap();
    assert_eq!(c.discover(&base, Service::CalDav).await.unwrap_err(), Error::Unauthorized);
}

#[tokio::test]
async fn sync_collection_incremental_with_pages() {
    let (base, _) = fake(true).await;
    let c = client();
    let url = Url::parse(&format!("{base}{CAL}")).unwrap();

    // Primera vez: todo.
    let first = c.sync(&url, &SyncState::default(), &BTreeMap::new()).await.unwrap();
    assert_eq!(first.method, SyncMethod::SyncCollectionFull);
    assert_eq!(first.changed.len(), 3);
    let hrefs: Vec<Url> = first.changed.iter().map(|i| i.url.clone()).collect();
    let res = c.multiget(&url, CollectionKind::Calendar, &hrefs).await.unwrap();
    assert_eq!(res.len(), 3);
    assert!(res.iter().all(|r| r.data.as_deref().is_some_and(|d| d.contains("BEGIN:VEVENT"))));
    let mut known: BTreeMap<Url, String> = res.iter().map(|r| (r.url.clone(), r.etag.clone().unwrap())).collect();

    // Cambios: crear, editar con If-Match, borrar.
    let new_url = url.join("nuevo.ics").unwrap();
    let put = c.put(&new_url, &ics("uid-n", "Nuevo"), CollectionKind::Calendar, Precondition::Create).await.unwrap();
    assert!(put.etag.is_some());
    let first_url = url.join("0.ics").unwrap();
    let old = known[&first_url].clone();
    c.put(&first_url, &ics("uid-0", "Junta movida"), CollectionKind::Calendar, Precondition::IfMatch(old))
        .await
        .unwrap();
    let del_url = url.join("2.ics").unwrap();
    c.delete(&del_url, Precondition::IfMatch(known[&del_url].clone())).await.unwrap();

    // Tres cambios con páginas de dos: el cliente sigue el 507.
    let second = c.sync(&url, &first.state, &known).await.unwrap();
    assert_eq!(second.method, SyncMethod::SyncCollection);
    let changed: Vec<&str> = second.changed.iter().map(|i| i.url.path()).collect();
    assert_eq!(changed, vec!["/dav/calendars/ana/work/0.ics", "/dav/calendars/ana/work/nuevo.ics"]);
    assert_eq!(second.deleted, vec![del_url.clone()]);
    assert_eq!(second.state.sync_token.as_deref(), Some("tok-6"));

    // Sin cambios: nada.
    for i in &second.changed {
        known.insert(i.url.clone(), i.etag.clone().unwrap());
    }
    known.remove(&del_url);
    let third = c.sync(&url, &second.state, &known).await.unwrap();
    assert!(third.changed.is_empty() && third.deleted.is_empty());
}

#[tokio::test]
async fn invalid_token_restarts_and_detects_deletions() {
    let (base, _) = fake(true).await;
    let c = client();
    let url = Url::parse(&format!("{base}{CAL}")).unwrap();
    let mut known = BTreeMap::new();
    known.insert(url.join("0.ics").unwrap(), etag(1));
    known.insert(url.join("viejo.ics").unwrap(), "\"x\"".to_string());
    let state = SyncState { sync_token: Some("caducado".into()), ..SyncState::default() };
    let out = c.sync(&url, &state, &known).await.unwrap();
    assert_eq!(out.method, SyncMethod::SyncCollectionFull);
    // 0.ics no cambió (mismo ETag): no se vuelve a bajar.
    assert_eq!(out.changed.len(), 2);
    assert_eq!(out.deleted, vec![url.join("viejo.ics").unwrap()]);
    assert_eq!(c.sync_collection(&url, Some("caducado")).await.unwrap_err(), Error::InvalidSyncToken);
}

#[tokio::test]
async fn fallback_to_ctag_and_etags() {
    let (base, state) = fake(false).await;
    let c = client();
    let url = Url::parse(&format!("{base}{CAL}")).unwrap();
    let out = c.sync(&url, &SyncState::default(), &BTreeMap::new()).await.unwrap();
    assert_eq!(out.method, SyncMethod::EtagDiff);
    assert_eq!(out.changed.len(), 3);
    assert!(out.state.sync_unsupported);
    assert_eq!(out.state.ctag.as_deref(), Some("ctag-3"));
    let known: BTreeMap<Url, String> = out.changed.iter().map(|i| (i.url.clone(), i.etag.clone().unwrap())).collect();

    let again = c.sync(&url, &out.state, &known).await.unwrap();
    assert_eq!(again.method, SyncMethod::CtagUnchanged);
    // Ya no se intenta sync-collection.
    let reports = state.lock().unwrap().requests.iter().filter(|r| r.starts_with("REPORT")).count();
    assert_eq!(reports, 1);

    state.lock().unwrap().change("1.ics", None);
    let after = c.sync(&url, &again.state, &known).await.unwrap();
    assert_eq!(after.method, SyncMethod::EtagDiff);
    assert!(after.changed.is_empty());
    assert_eq!(after.deleted, vec![url.join("1.ics").unwrap()]);
}

#[tokio::test]
async fn etag_conflicts_are_412() {
    let (base, _) = fake(true).await;
    let c = client();
    let url = Url::parse(&format!("{base}{CAL}0.ics")).unwrap();
    let got = c.get(&url).await.unwrap();
    assert_eq!(got.etag.as_deref(), Some("\"e1\""));
    // Otro cliente lo cambia.
    c.put(&url, &ics("uid-0", "Otro"), CollectionKind::Calendar, Precondition::IfMatch(got.etag.clone().unwrap()))
        .await
        .unwrap();
    // Nuestra edición con el ETag viejo no pisa el cambio.
    let err = c
        .put(&url, &ics("uid-0", "Mío"), CollectionKind::Calendar, Precondition::IfMatch(got.etag.clone().unwrap()))
        .await
        .unwrap_err();
    assert_eq!(err, Error::PreconditionFailed);
    let err = c.put(&url, &ics("uid-0", "Mío"), CollectionKind::Calendar, Precondition::Create).await.unwrap_err();
    assert_eq!(err, Error::PreconditionFailed);
    let err = c.delete(&url, Precondition::IfMatch(got.etag.unwrap())).await.unwrap_err();
    assert_eq!(err, Error::PreconditionFailed);
    // Borrar algo que ya no existe no es error.
    c.delete(&Url::parse(&format!("{base}{CAL}nada.ics")).unwrap(), Precondition::None).await.unwrap();
    assert_eq!(c.get(&Url::parse(&format!("{base}{CAL}nada.ics")).unwrap()).await.unwrap_err(), Error::NotFound);
}

#[tokio::test]
async fn calendar_query_and_free_busy() {
    let (base, _) = fake(true).await;
    let c = client();
    let url = Url::parse(&format!("{base}{CAL}")).unwrap();
    let start = Utc.with_ymd_and_hms(2026, 11, 1, 0, 0, 0).unwrap();
    let end = Utc.with_ymd_and_hms(2026, 12, 1, 0, 0, 0).unwrap();
    let found = c.calendar_query(&url, "VEVENT", start, end).await.unwrap();
    assert_eq!(found.len(), 3);

    let outbox = Url::parse(&format!("{base}/dav/calendars/ana/outbox/")).unwrap();
    let fb = c
        .free_busy(&outbox, "ana@example.com", &["juan@example.com", "nadie@example.com"], start, end, "fb1")
        .await
        .unwrap();
    assert_eq!(fb.len(), 2);
    assert_eq!(fb[0].recipient, "juan@example.com");
    assert_eq!(fb[0].busy.len(), 1);
    assert!(fb[1].request_status.starts_with("3.7"));
}

#[tokio::test]
async fn response_size_is_capped() {
    let (base, _) = fake(true).await;
    let opts = DavOptions { max_response_bytes: 1024, ..DavOptions::default() };
    let c = DavClient::new(BasicAuth::new("ana", "secreto"), opts).unwrap();
    let err = c.get(&Url::parse(&format!("{base}/huge")).unwrap()).await.unwrap_err();
    assert!(matches!(err, Error::Limit(_)));
}

/// Proveedor OAuth de prueba: el primer token caduca; tras `invalidate` entrega uno bueno.
struct Rotating {
    fresh: AtomicUsize,
    calls: AtomicUsize,
}

impl AuthProvider for Rotating {
    async fn auth_header(&self) -> iuredesk_dav::Result<String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(if self.fresh.load(Ordering::SeqCst) > 0 { "Bearer nuevo".into() } else { "Bearer viejo".into() })
    }

    fn invalidate(&self) -> bool {
        self.fresh.fetch_add(1, Ordering::SeqCst);
        true
    }
}

#[tokio::test]
async fn bearer_refresh_after_401() {
    let base = serve(|req: Req| {
        if req.header("Authorization") == Some("Bearer nuevo") {
            Resp::new(200, "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n").with("ETag", "\"1\"")
        } else {
            Resp::new(401, "")
        }
    })
    .await;
    let auth = Rotating { fresh: AtomicUsize::new(0), calls: AtomicUsize::new(0) };
    let c = DavClient::new(auth, DavOptions::default()).unwrap();
    let r = c.get(&Url::parse(&format!("{base}/x.ics")).unwrap()).await.unwrap();
    assert_eq!(r.etag.as_deref(), Some("\"1\""));
}

#[tokio::test]
async fn redirect_to_other_site_drops_credentials() {
    // Servidor B: registra si le llegó la credencial.
    let seen = Arc::new(Mutex::new(None::<String>));
    let s2 = seen.clone();
    let other = serve(move |req: Req| {
        *s2.lock().unwrap() = req.header("Authorization").map(str::to_string);
        Resp::new(200, "ok")
    })
    .await;
    // Servidor A redirige a B (otra IP/puerto: no es el mismo sitio).
    let other_url = other.replace("127.0.0.1", "localhost");
    let base = serve(move |_req: Req| Resp::new(302, "").with("Location", &format!("{other_url}/dest"))).await;
    let c = client();
    c.get(&Url::parse(&format!("{base}/x")).unwrap()).await.unwrap();
    assert_eq!(*seen.lock().unwrap(), None);
}
