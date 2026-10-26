//! Prueba contra un servidor real (Radicale, Stalwart…), apagada por omisión:
//!
//! ```sh
//! IUREDESK_DAV_URL=http://localhost:5232 IUREDESK_DAV_USER=ana IUREDESK_DAV_PASSWORD=x \
//!   cargo test -p iuredesk-dav --test live -- --ignored
//! ```

use std::collections::BTreeMap;

use iuredesk_dav::dav::{CollectionKind, Precondition, Service, SyncState};
use iuredesk_dav::{BasicAuth, DavClient, DavOptions};

#[tokio::test]
#[ignore = "necesita un servidor CalDAV en IUREDESK_DAV_URL"]
async fn live_server_round_trip() {
    let (Ok(url), Ok(user), Ok(pass)) =
        (std::env::var("IUREDESK_DAV_URL"), std::env::var("IUREDESK_DAV_USER"), std::env::var("IUREDESK_DAV_PASSWORD"))
    else {
        eprintln!("faltan IUREDESK_DAV_URL / IUREDESK_DAV_USER / IUREDESK_DAV_PASSWORD");
        return;
    };
    let c = DavClient::new(BasicAuth::new(&user, &pass), DavOptions::default()).unwrap();
    let (_, cols) = c.discover_collections(&url, Service::CalDav).await.unwrap();
    let cal =
        cols.iter().find(|c| c.kind == CollectionKind::Calendar && !c.read_only).expect("un calendario escribible");
    let first = c.sync(&cal.url, &SyncState::default(), &BTreeMap::new()).await.unwrap();

    let uid = format!("iuredesk-live-{}", std::process::id());
    let item = cal.url.join(&format!("{uid}.ics")).unwrap();
    let body = format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//IureDesk//prueba//ES\r\nBEGIN:VEVENT\r\nUID:{uid}\r\n\
DTSTAMP:20261026T000000Z\r\nDTSTART:20261110T150000Z\r\nDTEND:20261110T160000Z\r\nSUMMARY:Prueba\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
    );
    c.put(&item, &body, CollectionKind::Calendar, Precondition::Create).await.unwrap();
    let got = c.get(&item).await.unwrap();
    let known: BTreeMap<_, _> = first.changed.iter().filter_map(|i| Some((i.url.clone(), i.etag.clone()?))).collect();
    let second = c.sync(&cal.url, &first.state, &known).await.unwrap();
    assert!(second.changed.iter().any(|i| i.url.path() == item.path()));
    c.delete(&item, got.etag.map_or(Precondition::None, Precondition::IfMatch)).await.unwrap();
}
