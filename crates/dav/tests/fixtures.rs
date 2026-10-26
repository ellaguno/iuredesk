//! Respuestas grabadas (escritas a mano con la forma de cada servidor) servidas por un servidor
//! falso, y un `.ics` real de Outlook con zona de Windows.

mod common;

use chrono::{TimeZone, Timelike, Utc};
use common::{Req, Resp, serve};
use iuredesk_dav::dav::CollectionKind;
use iuredesk_dav::recur::{Limits, expand};
use iuredesk_dav::{DavClient, DavOptions, EventView, ICalendar, NoAuth};
use reqwest::Url;

async fn collections(fixture: &'static str) -> Vec<iuredesk_dav::Collection> {
    let base = serve(move |req: Req| {
        assert_eq!(req.method, "PROPFIND");
        assert_eq!(req.header("Depth"), Some("1"));
        Resp::xml(207, fixture)
    })
    .await;
    let c = DavClient::new(NoAuth, DavOptions::default()).unwrap();
    c.list_collections(&Url::parse(&format!("{base}/home/")).unwrap()).await.unwrap()
}

#[tokio::test]
async fn icloud() {
    let cols = collections(include_str!("fixtures/icloud_calendars.xml")).await;
    assert_eq!(cols.len(), 2, "inbox, outbox y notificaciones se omiten");
    assert_eq!(cols[0].display_name.as_deref(), Some("Casa"));
    assert_eq!(cols[0].color.as_deref(), Some("#FF2968FF"));
    assert!(cols[0].sync_token.is_some());
    assert!(!cols[0].read_only);
    assert_eq!(cols[1].components, vec!["VTODO"]);
    assert_eq!(cols[1].url.path(), "/1234567890/calendars/B2A6D5F0-1C9E-4C5B-9C73-6B1F1F2E3A4B/");
    // Sin current-user-privilege-set: se asume escribible.
    assert!(!cols[1].read_only);
}

#[tokio::test]
async fn nextcloud() {
    let cols = collections(include_str!("fixtures/nextcloud_calendars.xml")).await;
    assert_eq!(cols.len(), 2, "la papelera no es calendario");
    assert_eq!(cols[0].description.as_deref(), Some("Agenda & pendientes"));
    assert_eq!(cols[0].ctag.as_deref(), Some("http://sabre.io/ns/sync/42"));
    assert!(cols[1].read_only, "los cumpleaños son de solo lectura");
}

#[tokio::test]
async fn google() {
    let cols = collections(include_str!("fixtures/google_calendars.xml")).await;
    assert_eq!(cols.len(), 2);
    assert_eq!(cols[0].url.path(), "/caldav/v2/ana%40gmail.com/events/");
    assert_eq!(cols[1].display_name.as_deref(), Some("Días festivos en México"));
    assert!(cols[1].read_only);
    assert!(cols[1].sync_token.is_none());
}

#[tokio::test]
async fn fastmail_addressbooks() {
    let cols = collections(include_str!("fixtures/fastmail_addressbooks.xml")).await;
    assert_eq!(cols.len(), 2);
    assert!(cols.iter().all(|c| c.kind == CollectionKind::AddressBook));
    assert_eq!(cols[0].description.as_deref(), Some("Libreta principal"));
    assert!(!cols[0].read_only);
    assert!(cols[1].read_only);
}

#[tokio::test]
async fn radicale() {
    let cols = collections(include_str!("fixtures/radicale_calendars.xml")).await;
    assert_eq!(cols.len(), 2);
    assert_eq!(cols[0].kind, CollectionKind::Calendar);
    assert!(cols[0].ctag.is_none());
    assert_eq!(cols[0].components, vec!["VEVENT", "VJOURNAL", "VTODO"]);
    assert_eq!(cols[1].kind, CollectionKind::AddressBook);
    assert_eq!(cols[1].ctag.as_deref(), Some("\"3f2e1d\""));
}

#[tokio::test]
async fn stalwart() {
    let cols = collections(include_str!("fixtures/stalwart_calendars.xml")).await;
    assert_eq!(cols.len(), 1);
    assert_eq!(cols[0].description.as_deref(), Some("Calendario \u{201c}por defecto\u{201d}"));
    assert_eq!(cols[0].components.len(), 6);
}

#[test]
fn outlook_recurring_with_windows_zone() {
    let cal = ICalendar::parse(include_str!("fixtures/outlook_recurring.ics")).unwrap();
    let from = Utc.with_ymd_and_hms(2026, 11, 1, 0, 0, 0).unwrap();
    let to = Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap();
    let e = expand(&cal, from, to, chrono_tz::America::Mexico_City, Limits::default()).unwrap();
    assert!(e.errors.is_empty(), "{:?}", e.errors);
    // Lunes y miércoles del 2 nov al 9 dic (12), menos el EXDATE del 11 nov.
    assert_eq!(e.instances.len(), 11);
    let regular: Vec<_> = e.instances.iter().filter(|i| !i.overridden).collect();
    // 09:00 PST = 17:00 UTC.
    assert!(regular.iter().all(|i| i.start.hour() == 17));
    let moved: Vec<_> = e.instances.iter().filter(|i| i.overridden).collect();
    assert_eq!(moved.len(), 1);
    assert_eq!(moved[0].start, Utc.with_ymd_and_hms(2026, 11, 17, 22, 0, 0).unwrap());
    assert_eq!(EventView::new(moved[0].component).summary().as_deref(), Some("Seguimiento del proyecto (movida)"));
    // Lo desconocido sobrevive a la reescritura.
    let out = cal.to_ics();
    assert!(out.contains("X-MICROSOFT-CDO-OWNERAPPTID:2117236516"));
    assert!(out.contains("BEGIN:VALARM"));
    assert_eq!(ICalendar::parse(&out).unwrap(), cal);
}
