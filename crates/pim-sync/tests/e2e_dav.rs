//! Extremo a extremo contra un CalDAV/CardDAV falso: descubrimiento, primera sincronización,
//! ediciones locales que conservan lo desconocido, cambios del servidor, conflictos 412 (fusión y
//! gana el servidor), altas y bajas, RSVP con iMIP y recurrencias con cambio de horario.

mod fake_dav;

use std::sync::Arc;
use std::time::Duration;

use chrono::{TimeZone, Utc};
use fake_dav::{BOOK, CAL, State};
use iuredesk_accounts::{MemoryStore, SecretPurpose, SecretStore};
use iuredesk_model::account::{Account, AccountKind, AuthMethod, Protocol, Security, ServerConfig};
use iuredesk_model::{Event, EventTime, Labeled, PartStat, TaskStatus};
use iuredesk_pim_sync::{
    ConflictResolution, DavAuth, OP_IMIP_REPLY, PimAccount, PimEvent, PimSource, PimSyncEngine, PimSyncHandle,
    PimSyncOptions, occurrences_between,
};
use iuredesk_store::{PimRef, Store};

const SERIES: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Apple Inc.//macOS 15//EN\r\n\
BEGIN:VTIMEZONE\r\nTZID:Europe/Madrid\r\nX-LIC-LOCATION:Europe/Madrid\r\nEND:VTIMEZONE\r\n\
BEGIN:VEVENT\r\nUID:serie-1\r\nDTSTAMP:20261001T000000Z\r\nSEQUENCE:1\r\n\
DTSTART;TZID=Europe/Madrid:20261005T090000\r\nDTEND;TZID=Europe/Madrid:20261005T100000\r\n\
RRULE:FREQ=WEEKLY;BYDAY=MO;COUNT=6\r\nEXDATE;TZID=Europe/Madrid:20261012T090000\r\n\
SUMMARY:Reunión semanal\r\nLOCATION:Sala 2\r\nX-APPLE-TRAVEL-ADVISORY-BEHAVIOR:AUTOMATIC\r\n\
BEGIN:VALARM\r\nACTION:AUDIO\r\nTRIGGER:-PT15M\r\nX-WR-ALARMUID:abc\r\nEND:VALARM\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:serie-1\r\nRECURRENCE-ID;TZID=Europe/Madrid:20261019T090000\r\nDTSTAMP:20261001T000000Z\r\n\
DTSTART;TZID=Europe/Madrid:20261019T113000\r\nDTEND;TZID=Europe/Madrid:20261019T123000\r\n\
SUMMARY:Reunión movida\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

const INVITE: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Fake//EN\r\nBEGIN:VEVENT\r\nUID:inv-1\r\n\
DTSTAMP:20261020T000000Z\r\nDTSTART:20261110T150000Z\r\nDTEND:20261110T160000Z\r\nSUMMARY:Presupuesto\r\n\
ORGANIZER;CN=Jefa:mailto:jefa@example.com\r\n\
ATTENDEE;CN=Ana;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:ana@example.com\r\n\
ATTENDEE;CN=Jefa;PARTSTAT=ACCEPTED:mailto:jefa@example.com\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

const TODO: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Fake//EN\r\nBEGIN:VTODO\r\nUID:todo-1\r\n\
DTSTAMP:20261020T000000Z\r\nSUMMARY:Pagar luz\r\nDUE;VALUE=DATE:20261103\r\nSTATUS:NEEDS-ACTION\r\n\
X-APPLE-SORT-ORDER:7\r\nEND:VTODO\r\nEND:VCALENDAR\r\n";

const CARD: &str = "BEGIN:VCARD\r\nVERSION:3.0\r\nPRODID:-//Apple Inc.//iPhone OS 18//EN\r\nN:Pérez;Ana;;;\r\n\
FN:Ana Pérez\r\nitem1.EMAIL;type=INTERNET;type=WORK:ana@acme.com\r\nitem1.X-ABLabel:_$!<Work>!$_\r\n\
PHOTO;ENCODING=b;TYPE=JPEG:/9j/4AAQSkZJRgABAQ==\r\nX-SOCIALPROFILE;type=twitter:x.com/ana\r\nUID:card-1\r\nEND:VCARD\r\n";

fn account() -> Account {
    let srv = |p| ServerConfig {
        protocol: p,
        host: "127.0.0.1".into(),
        port: 993,
        security: Security::Tls,
        auth: AuthMethod::AppPassword,
        username: "ana".into(),
    };
    Account {
        id: "ana-icloud".into(),
        display_name: "Ana".into(),
        email: "ana@example.com".into(),
        kind: AccountKind::Generic,
        incoming: srv(Protocol::Imap),
        outgoing: srv(Protocol::Smtp),
        caldav_url: None,
        carddav_url: None,
        identities: vec![],
    }
}

struct Env {
    _dir: tempfile::TempDir,
    store: Arc<Store>,
    handle: PimSyncHandle,
    server: Arc<std::sync::Mutex<State>>,
    events: tokio::sync::broadcast::Receiver<PimEvent>,
    tz: chrono_tz::Tz,
}

async fn env(state: State) -> Env {
    let (base, server) = fake_dav::start(state).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    let secrets = Arc::new(MemoryStore::default());
    secrets.set("ana-icloud", SecretPurpose::Incoming, "secreto").unwrap();
    let pim = PimAccount {
        account: account(),
        source: PimSource::Dav {
            caldav: Some(format!("{base}/dav/")),
            carddav: Some(format!("{base}/dav/")),
            auth: DavAuth::Password { username: "ana".into() },
        },
        tasks_api: None,
    };
    let tz: chrono_tz::Tz = "Europe/Madrid".parse().unwrap();
    let opts = PimSyncOptions {
        poll_interval: Duration::from_secs(3600),
        sync_on_start: false,
        user_tz: tz,
        ..Default::default()
    };
    let handle = PimSyncEngine::start(Arc::clone(&store), vec![pim], None, secrets, opts);
    let events = handle.events();
    Env { _dir: dir, store, handle, server, events, tz }
}

fn seeded() -> State {
    let mut st = State::default();
    st.change(&format!("{CAL}serie-1.ics"), Some(SERIES.into()));
    st.change(&format!("{CAL}inv-1.ics"), Some(INVITE.into()));
    st.change(&format!("{CAL}todo-1.ics"), Some(TODO.into()));
    st.change(&format!("{BOOK}card-1.vcf"), Some(CARD.into()));
    st
}

fn event_by_uid(store: &Store, uid: &str) -> Event {
    let cal = store.calendars().unwrap().into_iter().next().unwrap();
    store.events_in_calendar(cal.id).unwrap().into_iter().find(|e| e.uid == uid).unwrap()
}

fn drain(rx: &mut tokio::sync::broadcast::Receiver<PimEvent>) -> Vec<PimEvent> {
    let mut v = Vec::new();
    while let Ok(e) = rx.try_recv() {
        v.push(e);
    }
    v
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_cycle_against_fake_server() {
    let mut e = env(seeded()).await;
    e.handle.sync_now_wait("ana-icloud").await.unwrap();

    // --- primera sincronización
    let cals = e.store.calendars().unwrap();
    assert_eq!(cals.len(), 1);
    assert_eq!(cals[0].name, "Trabajo");
    assert_eq!(cals[0].color, Some(0x0B5FFF));
    let lists = e.store.task_lists().unwrap();
    assert_eq!(lists.len(), 1, "el calendario admite VTODO: también es lista de tareas");
    let books = e.store.address_books().unwrap();
    assert_eq!(books[0].name, "Contactos");
    let series = event_by_uid(&e.store, "serie-1");
    assert_eq!(series.rrule.as_deref(), Some("FREQ=WEEKLY;BYDAY=MO;COUNT=6"));
    assert!(e.store.raw_object(PimRef::Event(series.id)).unwrap().is_some());
    let tasks = e.store.tasks(lists[0].id).unwrap();
    assert_eq!(tasks[0].summary, "Pagar luz");
    let contacts = e.store.contacts(books[0].id).unwrap();
    assert_eq!(contacts[0].emails, vec![Labeled::new("work", "ana@acme.com")]);
    let photo = contacts[0].photo.expect("foto en blobs");
    assert_eq!(e.store.blobs().get(&photo).unwrap()[..2], [0xff, 0xd8]);

    // --- recurrencia con cambio de horario (25-oct-2026), EXDATE y excepción movida
    let from = Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap();
    let to = Utc.with_ymd_and_hms(2026, 12, 1, 0, 0, 0).unwrap();
    let occ: Vec<_> = occurrences_between(&e.store, &[], from, to, e.tz)
        .unwrap()
        .into_iter()
        .filter(|o| o.event == series.id)
        .collect();
    let starts: Vec<String> = occ.iter().map(|o| o.start.to_utc().format("%m-%d %H:%M").to_string()).collect();
    // 5-oct y 19-oct (movida) en CEST (UTC+2); 26-oct, 2-nov, 9-nov en CET (UTC+1); 12-oct excluida.
    assert_eq!(starts, vec!["10-05 07:00", "10-19 09:30", "10-26 08:00", "11-02 08:00", "11-09 08:00"]);
    assert!(occ[1].overridden);
    assert_eq!(occ[1].summary, "Reunión movida");
    assert!(occ.iter().all(|o| o.recurring));

    // --- edición local: solo cambia SUMMARY; lo desconocido y la excepción se conservan
    let mut s = series.clone();
    s.summary = "Reunión semanal (nueva)".into();
    e.handle.save_event(&s).unwrap();
    e.handle.sync_now_wait("ana-icloud").await.unwrap();
    let on_server = e.server.lock().unwrap().data(&format!("{CAL}serie-1.ics")).unwrap();
    assert!(on_server.contains("SUMMARY:Reunión semanal (nueva)"));
    assert!(on_server.contains("X-APPLE-TRAVEL-ADVISORY-BEHAVIOR:AUTOMATIC"));
    assert!(on_server.contains("X-WR-ALARMUID:abc"));
    assert!(on_server.contains("RECURRENCE-ID;TZID=Europe/Madrid:20261019T090000"));
    assert!(on_server.contains("BEGIN:VTIMEZONE"));
    assert_eq!(e.store.ops_len().unwrap(), 0);
    let etag_now = event_by_uid(&e.store, "serie-1").etag;
    assert!(etag_now.is_some());

    // --- cambio en el servidor (otro cliente) → llega por sync-collection incremental
    {
        let mut st = e.server.lock().unwrap();
        let d = st.data(&format!("{CAL}serie-1.ics")).unwrap().replace("LOCATION:Sala 2", "LOCATION:Sala 9");
        st.change(&format!("{CAL}serie-1.ics"), Some(d));
    }
    e.handle.sync_now_wait("ana-icloud").await.unwrap();
    assert_eq!(event_by_uid(&e.store, "serie-1").location, "Sala 9");

    // --- conflicto 412 con cambios en campos distintos → fusión
    let _ = drain(&mut e.events);
    // (El servidor cambia antes de guardar: `save_event` dispara la subida enseguida.)
    let mut s = event_by_uid(&e.store, "serie-1");
    {
        let mut st = e.server.lock().unwrap();
        let d = st.data(&format!("{CAL}serie-1.ics")).unwrap().replace("LOCATION:Sala 9", "LOCATION:Sala remota");
        st.change(&format!("{CAL}serie-1.ics"), Some(d));
    }
    s.summary = "Local".into();
    e.handle.save_event(&s).unwrap();
    e.handle.sync_now_wait("ana-icloud").await.unwrap();
    let on_server = e.server.lock().unwrap().data(&format!("{CAL}serie-1.ics")).unwrap();
    assert!(on_server.contains("SUMMARY:Local"), "{on_server}");
    assert!(on_server.contains("LOCATION:Sala remota"));
    let local = event_by_uid(&e.store, "serie-1");
    assert_eq!((local.summary.as_str(), local.location.as_str()), ("Local", "Sala remota"));
    assert!(
        drain(&mut e.events)
            .iter()
            .any(|ev| matches!(ev, PimEvent::Conflict { resolution: ConflictResolution::Merged, .. }))
    );

    // --- conflicto en el mismo campo → gana el servidor
    let mut s = event_by_uid(&e.store, "serie-1");
    {
        let mut st = e.server.lock().unwrap();
        let d = st.data(&format!("{CAL}serie-1.ics")).unwrap().replace("SUMMARY:Local", "SUMMARY:Del servidor");
        st.change(&format!("{CAL}serie-1.ics"), Some(d));
    }
    s.summary = "Mío".into();
    e.handle.save_event(&s).unwrap();
    e.handle.sync_now_wait("ana-icloud").await.unwrap();
    assert_eq!(event_by_uid(&e.store, "serie-1").summary, "Del servidor");
    assert!(e.server.lock().unwrap().data(&format!("{CAL}serie-1.ics")).unwrap().contains("SUMMARY:Del servidor"));
    assert!(
        drain(&mut e.events)
            .iter()
            .any(|ev| matches!(ev, PimEvent::Conflict { resolution: ConflictResolution::ServerWon, .. }))
    );
    assert_eq!(e.store.ops_len().unwrap(), 0);

    // --- alta local → PUT con If-None-Match en la colección
    let cal = e.store.calendars().unwrap()[0].id;
    let start = Utc.with_ymd_and_hms(2026, 11, 20, 10, 0, 0).unwrap();
    let id = e
        .handle
        .save_event(&Event {
            calendar: cal,
            summary: "Dentista".into(),
            start: EventTime::DateTime { utc: start, tzid: Some("Europe/Madrid".into()) },
            end: EventTime::DateTime { utc: start + chrono::Duration::hours(1), tzid: Some("Europe/Madrid".into()) },
            ..Default::default()
        })
        .unwrap();
    e.handle.sync_now_wait("ana-icloud").await.unwrap();
    let created = e.store.get_event(id).unwrap().unwrap();
    let href = created.remote_id.clone().unwrap();
    assert!(href.contains(CAL) && href.ends_with(".ics"));
    let path = &href[href.find(CAL).unwrap()..];
    let body = e.server.lock().unwrap().data(path).unwrap();
    assert!(body.contains("SUMMARY:Dentista"));
    assert!(body.contains("DTSTART;TZID=Europe/Madrid:20261120T110000"));
    // Una pasada más no duplica nada (el ETag ya es conocido).
    e.handle.sync_now_wait("ana-icloud").await.unwrap();
    assert_eq!(e.store.events_in_calendar(cal).unwrap().len(), 3);

    // --- baja local → DELETE
    e.handle.delete_event(id).unwrap();
    e.handle.sync_now_wait("ana-icloud").await.unwrap();
    assert!(e.server.lock().unwrap().data(path).is_none());
    assert_eq!(e.store.events_in_calendar(cal).unwrap().len(), 2);

    // --- RSVP: PARTSTAT al servidor + correo iMIP para la bandeja de salida
    let inv = event_by_uid(&e.store, "inv-1");
    let reply = e.handle.rsvp(inv.id, PartStat::Accepted, Some("Ahí estaré")).unwrap().expect("respuesta iMIP");
    assert_eq!(reply.to.addr, "jefa@example.com");
    assert!(reply.ics.contains("METHOD:REPLY"));
    e.handle.sync_now_wait("ana-icloud").await.unwrap();
    let on_server = e.server.lock().unwrap().data(&format!("{CAL}inv-1.ics")).unwrap();
    assert!(on_server.contains("PARTSTAT=ACCEPTED:mailto:ana@example.com"), "{on_server}");
    assert!(on_server.contains("PARTSTAT=ACCEPTED:mailto:jefa@example.com"));
    let ops = e.store.peek_ops(10).unwrap();
    assert_eq!(ops.len(), 1, "queda solo el correo para la bandeja de salida");
    assert_eq!(ops[0].kind, OP_IMIP_REPLY);
    let payload = iuredesk_pim_sync::ImipReply::decode(&ops[0].payload).unwrap();
    assert_eq!(payload.subject, "Aceptado: Presupuesto");
    assert!(payload.ics.contains("ATTENDEE;CN=Ana;PARTSTAT=ACCEPTED:mailto:ana@example.com"));

    // --- tarea completada y contacto editado: parche que conserva lo desconocido
    let list = e.store.task_lists().unwrap()[0].id;
    let mut t = e.store.tasks(list).unwrap().remove(0);
    t.status = TaskStatus::Completed;
    t.completed = Some(Utc::now());
    e.handle.save_task(&t).unwrap();
    let book = e.store.address_books().unwrap()[0].id;
    let mut c = e.store.contacts(book).unwrap().remove(0);
    c.title = "Directora".into();
    e.handle.save_contact(&c, None).unwrap();
    e.handle.sync_now_wait("ana-icloud").await.unwrap();
    let todo = e.server.lock().unwrap().data(&format!("{CAL}todo-1.ics")).unwrap();
    assert!(todo.contains("STATUS:COMPLETED") && todo.contains("X-APPLE-SORT-ORDER:7"));
    let card = e.server.lock().unwrap().data(&format!("{BOOK}card-1.vcf")).unwrap();
    assert!(card.contains("TITLE:Directora"));
    assert!(card.contains("item1.X-ABLABEL") && card.contains("X-SOCIALPROFILE") && card.contains("PHOTO;ENCODING=b"));

    // --- borrado en el servidor → desaparece en local
    e.server.lock().unwrap().change(&format!("{BOOK}card-1.vcf"), None);
    e.handle.sync_now_wait("ana-icloud").await.unwrap();
    assert!(e.store.contacts(book).unwrap().is_empty());

    // Descubrimiento una sola vez.
    let principal_hits = e.server.lock().unwrap().requests.iter().filter(|r| r.contains("/principals/")).count();
    assert_eq!(principal_hits, 2, "CalDAV y CardDAV, solo en la primera pasada");
    e.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_scheduling_skips_imip_and_bad_password_needs_reauth() {
    let mut st = seeded();
    st.outbox = true;
    let mut e = env(st).await;
    e.handle.sync_now_wait("ana-icloud").await.unwrap();
    let inv = event_by_uid(&e.store, "inv-1");
    assert!(e.handle.rsvp(inv.id, PartStat::Declined, None).unwrap().is_none(), "el servidor manda la respuesta");
    e.handle.sync_now_wait("ana-icloud").await.unwrap();
    assert_eq!(e.store.ops_len().unwrap(), 0);
    e.handle.shutdown().await;

    // Contraseña equivocada: error que pide volver a iniciar sesión.
    let (base, _server) = fake_dav::start(State::default()).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    let secrets = Arc::new(MemoryStore::default());
    secrets.set("ana-icloud", SecretPurpose::Dav, "mala").unwrap();
    let pim = PimAccount {
        account: account(),
        source: PimSource::Dav {
            caldav: Some(format!("{base}/dav/")),
            carddav: None,
            auth: DavAuth::Password { username: "ana".into() },
        },
        tasks_api: None,
    };
    let h = PimSyncEngine::start(store, vec![pim], None, secrets, PimSyncOptions::default());
    let err = h.sync_now_wait("ana-icloud").await.unwrap_err();
    assert!(err.needs_reauth(), "{err}");
    let _ = drain(&mut e.events);
    h.shutdown().await;
}
