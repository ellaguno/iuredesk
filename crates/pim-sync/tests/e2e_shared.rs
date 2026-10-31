//! BUG-DUP: dos cuentas configuradas ven la misma colección CalDAV (Ana es dueña; Bob la tiene
//! compartida en su casa de calendarios). Cada evento debe aparecer **una vez**.

mod fake_dav;

use std::sync::Arc;
use std::time::Duration;

use chrono::{TimeZone, Utc};
use fake_dav::{CAL, State};
use iuredesk_accounts::{MemoryStore, SecretPurpose, SecretStore};
use iuredesk_model::account::{Account, AccountKind, AuthMethod, Protocol, Security, ServerConfig};
use iuredesk_pim_sync::{DavAuth, PimAccount, PimSource, PimSyncEngine, PimSyncOptions, occurrences_between};
use iuredesk_store::Store;

const EVENT: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Fake//EN\r\nBEGIN:VEVENT\r\n\
UID:047d27cd72mhnajrmgegn11jjo@google.com\r\nDTSTAMP:20261020T000000Z\r\nDTSTART:20261030T170000Z\r\n\
DTEND:20261030T180000Z\r\nSUMMARY:Técnico de TotalPly\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

const SERIES: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Fake//EN\r\nBEGIN:VEVENT\r\nUID:diaria\r\n\
DTSTAMP:20261020T000000Z\r\nDTSTART:20261029T080000Z\r\nDTEND:20261029T083000Z\r\nRRULE:FREQ=DAILY;COUNT=5\r\n\
SUMMARY:Diaria\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

fn account(user: &str) -> Account {
    let srv = |p| ServerConfig {
        protocol: p,
        host: "127.0.0.1".into(),
        port: 993,
        security: Security::Tls,
        auth: AuthMethod::AppPassword,
        username: user.into(),
    };
    Account {
        id: format!("{user}-dav"),
        display_name: user.into(),
        email: format!("{user}@example.com"),
        kind: AccountKind::Generic,
        incoming: srv(Protocol::Imap),
        outgoing: srv(Protocol::Smtp),
        caldav_url: None,
        carddav_url: None,
        identities: vec![],
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_collection_shows_each_event_once() {
    let mut st = State { bob: true, ..State::default() };
    st.change(&format!("{CAL}totalply.ics"), Some(EVENT.into()));
    st.change(&format!("{CAL}diaria.ics"), Some(SERIES.into()));
    let (base, _server) = fake_dav::start(st).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    let secrets = Arc::new(MemoryStore::default());
    // Bob primero: es la cuenta configurada antes, pero la colección no lleva su dirección.
    let users = ["bob", "ana"];
    let mut pims = Vec::new();
    for u in users {
        secrets.set(&format!("{u}-dav"), SecretPurpose::Incoming, "secreto").unwrap();
        pims.push(PimAccount {
            account: account(u),
            source: PimSource::Dav {
                caldav: Some(format!("{base}/dav/")),
                carddav: None,
                auth: DavAuth::Password { username: u.into() },
            },
            tasks_api: None,
        });
    }
    let tz = chrono_tz::UTC;
    let opts = PimSyncOptions {
        poll_interval: Duration::from_secs(3600),
        sync_on_start: false,
        user_tz: tz,
        ..Default::default()
    };
    let handle = PimSyncEngine::start(Arc::clone(&store), pims, None, secrets, opts);
    for u in users {
        handle.sync_now_wait(&format!("{u}-dav")).await.unwrap();
    }

    let cals = store.calendars().unwrap();
    assert_eq!(cals.len(), 2, "cada cuenta tiene su copia local: {cals:?}");
    let shared = store.shared_calendars().unwrap();
    let ana_cal = cals.iter().find(|c| c.name == "Trabajo").unwrap();
    let bob_cal = cals.iter().find(|c| c.name == "Trabajo de Ana").unwrap();
    // La ruta no lleva la dirección de nadie: manda la cuenta configurada primero (Bob).
    assert_eq!(shared.primary_of(ana_cal.id), Some(bob_cal.id));
    assert!(!shared.is_duplicate(bob_cal.id));

    let from = Utc.with_ymd_and_hms(2026, 10, 30, 0, 0, 0).unwrap();
    let to = Utc.with_ymd_and_hms(2026, 10, 31, 0, 0, 0).unwrap();
    let occ = occurrences_between(&store, &[], from, to, tz).unwrap();
    let names: Vec<&str> = occ.iter().map(|o| o.summary.as_str()).collect();
    assert_eq!(names, ["Diaria", "Técnico de TotalPly"], "una sola vez cada uno");
    assert!(occ.iter().all(|o| o.calendar == bob_cal.id), "las apariciones son del primario");
    // Toda la serie, una vez por repetición.
    let week =
        occurrences_between(&store, &[], from - chrono::Duration::days(2), to + chrono::Duration::days(5), tz).unwrap();
    assert_eq!(week.iter().filter(|o| o.uid == "diaria").count(), 5);
    // Pedido explícitamente, el duplicado sigue dando sus apariciones.
    let only_dup = occurrences_between(&store, &[ana_cal.id], from, to, tz).unwrap();
    assert_eq!(only_dup.len(), 2);
}
