//! BUG-GCAL: una cuenta Gmail con OAuth, como la guarda la aplicación (tipo `Gmail`, raíz CalDAV
//! genérica `…/caldav/v2/`), contra un servidor falso con el comportamiento de Google: CalDAV
//! descubre principal y calendarios; CardDAV responde 403 `ACCESS_TOKEN_SCOPE_INSUFFICIENT`
//! porque el token no tiene el alcance `carddav`.
//!
//! Antes, el fallo de CardDAV cortaba el descubrimiento y la cuenta se quedaba sin calendarios
//! (y el error solo salía como aviso pasajero). Ahora los calendarios llegan y la cuenta muestra
//! «Vuelve a iniciar sesión para dar acceso a tus contactos» con la acción de volver a entrar.

#[path = "../../dav/tests/common/mod.rs"]
mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{TimeZone, Utc};
use common::{Req, Resp, between, multistatus, serve};
use iuredesk_accounts::{AccountStore, MemoryStore};
use iuredesk_engine::{AccountSync, Session, SessionConfig, Zone};
use iuredesk_model::account::{Account, AccountKind, AuthMethod, OAuthProvider, Protocol, Security, ServerConfig};
use iuredesk_oauth::{BoxFuture, TokenAudience, TokenProvider};

const EMAIL: &str = "ana@gmail.com";
const HOME: &str = "/caldav/v2/ana@gmail.com/";
const CAL: &str = "/caldav/v2/ana@gmail.com/events/";
const TOKEN: &str = "ya29.prueba";

const GOOGLE_403: &str = r#"{"error":{"code":403,"message":"Request had insufficient authentication scopes.","status":"PERMISSION_DENIED","details":[{"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"ACCESS_TOKEN_SCOPE_INSUFFICIENT","domain":"googleapis.com","metadata":{"service":"carddav.googleapis.com","method":"google.carddav.v1.CardDav.Propfind"}}]}}"#;

/// Token fijo; cuenta cuántas veces se pide para la API.
struct FakeTokens(Mutex<Vec<TokenAudience>>);

impl TokenProvider for FakeTokens {
    fn access_token<'a>(
        &'a self,
        _: &'a Account,
        audience: TokenAudience,
    ) -> BoxFuture<'a, Result<String, iuredesk_oauth::Error>> {
        self.0.lock().unwrap().push(audience);
        Box::pin(async { Ok(TOKEN.to_string()) })
    }
    fn invalidate(&self, _: &str) {}
}

fn ok_prop(inner: &str) -> String {
    format!("<d:propstat><d:prop>{inner}</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>")
}

fn ics() -> String {
    // Un evento con hora y un cumpleaños anual de día completo, como los de Google.
    "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Google Inc//Google Calendar 70.9054//EN\r\nBEGIN:VEVENT\r\n\
DTSTART:20261027T170000Z\r\nDTEND:20261027T180000Z\r\nUID:junta@google.com\r\nSUMMARY:Junta\r\nEND:VEVENT\r\n\
END:VCALENDAR\r\n"
        .into()
}

fn birthday() -> String {
    "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Google Inc//Google Calendar 70.9054//EN\r\nBEGIN:VEVENT\r\n\
DTSTART;VALUE=DATE:20001028\r\nRRULE:FREQ=YEARLY\r\nUID:cumple@google.com\r\nSUMMARY:Cumpleaños de Luis\r\n\
END:VEVENT\r\nEND:VCALENDAR\r\n"
        .into()
}

fn handle(log: &Mutex<Vec<String>>, req: Req) -> Resp {
    log.lock().unwrap().push(format!("{} {}", req.method, req.path));
    if req.header("Authorization") != Some(&format!("Bearer {TOKEN}")) {
        return Resp::new(401, "");
    }
    let path = req.path.split('?').next().unwrap_or_default().to_string();
    if path.starts_with("/carddav/") {
        return Resp::new(403, GOOGLE_403).with("Content-Type", "application/json; charset=UTF-8");
    }
    let items = [(format!("{CAL}junta.ics"), ics()), (format!("{CAL}cumple.ics"), birthday())];
    match (req.method.as_str(), path.as_str()) {
        ("PROPFIND", "/caldav/v2/") => Resp::xml(
            207,
            multistatus(&format!(
                "<d:response><d:href>/caldav/v2/</d:href>{}</d:response>",
                ok_prop(&format!(
                    "<d:current-user-principal><d:href>/caldav/v2/{EMAIL}/user</d:href></d:current-user-principal>"
                ))
            )),
        ),
        ("PROPFIND", p) if p == format!("/caldav/v2/{EMAIL}/user") => Resp::xml(
            207,
            multistatus(&format!(
                "<d:response><d:href>{p}</d:href>{}</d:response>",
                ok_prop(&format!("<c:calendar-home-set><d:href>{HOME}</d:href></c:calendar-home-set>"))
            )),
        ),
        ("PROPFIND", HOME) => Resp::xml(
            207,
            multistatus(&format!(
                "<d:response><d:href>{HOME}</d:href>{}</d:response><d:response><d:href>{CAL}</d:href>{}</d:response>",
                ok_prop("<d:resourcetype><d:collection/></d:resourcetype>"),
                ok_prop(
                    "<d:resourcetype><d:collection/><c:calendar/></d:resourcetype><d:displayname>ana@gmail.com</d:displayname>\
<d:sync-token>tok-1</d:sync-token>\
<c:supported-calendar-component-set><c:comp name=\"VEVENT\"/></c:supported-calendar-component-set>\
<d:current-user-privilege-set><d:privilege><d:read/></d:privilege><d:privilege><d:write/></d:privilege></d:current-user-privilege-set>"
                )
            )),
        ),
        ("REPORT", CAL) if req.body.contains("sync-collection") => {
            let mut inner = String::new();
            for (href, _) in &items {
                inner.push_str(&format!(
                    "<d:response><d:href>{href}</d:href>{}</d:response>",
                    ok_prop("<d:getetag>\"e1\"</d:getetag>")
                ));
            }
            inner.push_str("<d:sync-token>tok-1</d:sync-token>");
            Resp::xml(207, multistatus(&inner))
        }
        ("REPORT", CAL) if req.body.contains("multiget") => {
            let mut inner = String::new();
            for (href, data) in &items {
                if req.body.contains(&format!("<d:href>{href}</d:href>")) {
                    let data = data.replace('&', "&amp;").replace('<', "&lt;");
                    inner.push_str(&format!(
                        "<d:response><d:href>{href}</d:href>{}</d:response>",
                        ok_prop(&format!("<d:getetag>\"e1\"</d:getetag><c:calendar-data>{data}</c:calendar-data>"))
                    ));
                }
            }
            let _ = between(&req.body, "<d:href>", "</d:href>");
            Resp::xml(207, multistatus(&inner))
        }
        _ => Resp::new(405, ""),
    }
}

#[test]
fn gmail_oauth_account_gets_calendars_even_without_contacts_scope() {
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let log = Arc::new(Mutex::new(Vec::new()));
    let l = Arc::clone(&log);
    let base = rt.block_on(serve(move |req| handle(&l, req)));

    let dir = tempfile::tempdir().unwrap();
    // Sin IMAP real: el correo no interviene (el entrante no es de recepción).
    let server = |protocol| ServerConfig {
        protocol,
        host: "127.0.0.1".into(),
        port: 9,
        security: Security::Tls,
        auth: AuthMethod::OAuth2 { provider: OAuthProvider::Google },
        username: EMAIL.into(),
    };
    let account = Account {
        id: "acct-gmail".into(),
        display_name: "Ana".into(),
        email: EMAIL.into(),
        kind: AccountKind::Gmail,
        incoming: server(Protocol::Smtp),
        outgoing: server(Protocol::Smtp),
        // Lo que guarda el autodescubrimiento: raíces genéricas.
        caldav_url: Some(format!("{base}/caldav/v2/")),
        carddav_url: Some(format!("{base}/carddav/v1/principals/{EMAIL}/lists/default/")),
        identities: vec![],
    };
    let accounts = AccountStore::at(dir.path().join("accounts.json"));
    accounts.upsert(account).unwrap();
    let store = Arc::new(iuredesk_store::Store::open(dir.path().join("store")).unwrap());
    // Ciudad de México (UTC-6, sin horario de verano).
    let mexico = Zone::Fixed(chrono::FixedOffset::west_opt(6 * 3600).unwrap());
    let mut s = Session::new(SessionConfig {
        accounts: Some(accounts),
        secrets: Arc::new(MemoryStore::default()),
        zone: mexico,
        now: Some(Utc.with_ymd_and_hms(2026, 10, 27, 15, 0, 0).unwrap()),
        sync: Some(iuredesk_engine::iuredesk_sync::SyncOptions::default()),
        ..SessionConfig::offline(store)
    })
    .unwrap();
    let tokens = Arc::new(FakeTokens(Mutex::new(Vec::new())));
    s.set_token_provider(tokens.clone());

    let aid = s.accounts_overview().iter().find(|r| r.id == "acct-gmail").and_then(|r| r.aid);
    let start = Instant::now();
    loop {
        s.poll();
        let has_cal = !s.calendars().is_empty();
        let st = aid.and_then(|a| s.account_sync(a).cloned());
        if has_cal && matches!(st, Some(AccountSync::Error { reauth: true, .. })) {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "sin calendarios o sin aviso: calendarios={:?} estado={st:?} peticiones={:?}",
            s.calendars(),
            log.lock().unwrap()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(tokens.0.lock().unwrap().iter().all(|a| *a == TokenAudience::Api));

    // El calendario de Google, con sus eventos.
    assert_eq!(s.calendars().len(), 1);
    let start = Instant::now();
    loop {
        s.poll();
        if s.store().events_in_calendar(s.calendars()[0].id).unwrap().len() == 2 {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(30), "eventos sin llegar");
        std::thread::sleep(Duration::from_millis(20));
    }

    // El estado de la cuenta pide volver a iniciar sesión para los contactos.
    let row = s.accounts_overview().into_iter().find(|r| r.id == "acct-gmail").unwrap();
    let Some(AccountSync::Error { text, reauth: true }) = row.status else { panic!("estado {:?}", row.status) };
    assert!(text.es.contains("Vuelve a iniciar sesión para dar acceso a tus contactos"), "{}", text.es);

    // Cumpleaños de día completo: el 28 de octubre, no «27 a las 18:00».
    let from = Utc.with_ymd_and_hms(2026, 10, 27, 6, 0, 0).unwrap();
    let to = Utc.with_ymd_and_hms(2026, 10, 29, 6, 0, 0).unwrap();
    let occ = s.occurrences(from, to).to_vec();
    let b = occ.iter().find(|o| o.summary.contains("Cumpleaños")).expect("cumpleaños");
    assert!(b.all_day);
    assert_eq!(b.days.map(|d| d.0), chrono::NaiveDate::from_ymd_opt(2026, 10, 28));
    assert_eq!(b.start, Utc.with_ymd_and_hms(2026, 10, 28, 6, 0, 0).unwrap(), "medianoche local");

    s.shutdown();
    drop(rt);
}
