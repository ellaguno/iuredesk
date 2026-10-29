//! R1.2: abrir `.eml`, `.ics` y `.vcf` desde el sistema y los enlaces `iuredesk://` sobre el
//! buzón de ejemplo: lectura, agregar con deshacer, responder a una invitación, ir a un mensaje.

use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{TimeZone, Utc};
use iuredesk_accounts::MemoryStore;
use iuredesk_engine::open::{FileKind, Opened, load};
use iuredesk_engine::{Event, Module, OfflineTransport, Outcome, Session, SessionConfig, Zone};
use iuredesk_model::PartStat;
use iuredesk_store::{Store, sample};

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 21, 15, 0, 0).unwrap()
}

fn session(dir: &std::path::Path) -> Session {
    let store = Store::open(dir).unwrap();
    sample::populate(&store, sample::DEFAULT_SEED, now()).unwrap();
    let mut s = Session::new(SessionConfig {
        store: Arc::new(store),
        accounts: None,
        secrets: Arc::new(MemoryStore::default()),
        prefs_path: None,
        now: Some(now()),
        zone: Zone::utc(),
        sample: true,
        sync: None,
        transport: Some(Arc::new(OfflineTransport)),
    })
    .unwrap();
    settle(&mut s);
    s
}

fn settle(s: &mut Session) -> Vec<Event> {
    let start = Instant::now();
    let mut out = Vec::new();
    loop {
        out.extend(s.poll());
        if s.is_idle() {
            out.extend(s.poll());
            return out;
        }
        assert!(start.elapsed() < Duration::from_secs(30), "los trabajos de fondo no terminan");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn me(s: &Session) -> String {
    s.account_labels()[0].email.clone()
}

fn ics(method: Option<&str>, uid: &str, attendee: &str) -> String {
    let m = method.map(|m| format!("METHOD:{m}\r\n")).unwrap_or_default();
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Prueba//ES\r\n{m}BEGIN:VEVENT\r\nUID:{uid}\r\n\
         DTSTAMP:20261020T100000Z\r\nDTSTART:20261028T170000Z\r\nDTEND:20261028T180000Z\r\nSUMMARY:Audiencia\r\n\
         LOCATION:Juzgado 4\r\nORGANIZER;CN=Ana:mailto:ana@juzgado.example\r\n\
         ATTENDEE;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:{attendee}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
    )
}

fn notice(out: iuredesk_engine::Result<Outcome>) -> String {
    match out.unwrap() {
        Outcome::Notice(n) => n.text.es,
        _ => String::new(),
    }
}

#[test]
fn ics_publicado_se_agrega_y_se_deshace() {
    let dir = tempfile::tempdir().unwrap();
    let files = tempfile::tempdir().unwrap();
    let mut s = session(dir.path());
    let path = files.path().join("cita.ics");
    std::fs::write(&path, ics(None, "cita-1@prueba", "otro@example.com")).unwrap();
    let Opened::Calendar(cal) = load(&path, FileKind::Ics, &[me(&s)], chrono_tz::UTC).unwrap() else {
        panic!("se esperaba un calendario")
    };
    assert_eq!(cal.total, 1);
    let inv = cal.invitation.as_ref().expect("un .ics suelto es una publicación");
    assert_eq!(inv.event.summary, "Audiencia");
    assert!(inv.me.is_none());
    assert!(notice(s.add_file_events(&cal.events)).contains("Agregado a tu calendario: Audiencia"));
    assert!(s.occurrence_by_uid("cita-1@prueba").is_some());
    // Dos veces: no se duplica.
    assert!(notice(s.add_file_events(&cal.events)).contains("ya están"));
    s.run("undo").unwrap();
    assert!(s.occurrence_by_uid("cita-1@prueba").is_none(), "deshacer quita lo agregado");
}

#[test]
fn invitacion_en_ics_se_responde() {
    let dir = tempfile::tempdir().unwrap();
    let files = tempfile::tempdir().unwrap();
    let mut s = session(dir.path());
    let path = files.path().join("invite.ics");
    std::fs::write(&path, ics(Some("REQUEST"), "junta-9@prueba", &me(&s))).unwrap();
    let Opened::Calendar(cal) = load(&path, FileKind::Ics, &[me(&s)], chrono_tz::UTC).unwrap() else {
        panic!("se esperaba un calendario")
    };
    let inv = cal.invitation.as_ref().unwrap();
    assert!(inv.me.is_some(), "estoy invitado");
    let text = notice(s.respond_file_invitation(inv, PartStat::Accepted));
    assert!(text.starts_with("Aceptad"), "{text}");
    let o = s.occurrence_by_uid("junta-9@prueba").expect("queda en mi calendario");
    assert_eq!(o.summary, "Audiencia");
}

#[test]
fn vcf_agrega_contactos_con_deshacer() {
    let dir = tempfile::tempdir().unwrap();
    let files = tempfile::tempdir().unwrap();
    let mut s = session(dir.path());
    let path = files.path().join("tarjetas.vcf");
    std::fs::write(
        &path,
        "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Zoe Quintana\r\nEMAIL:zoe@quintana.example\r\nEND:VCARD\r\n\
         BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Yago Ruiz\r\nTEL:+52 55 0000 0000\r\nEND:VCARD\r\n",
    )
    .unwrap();
    let Opened::Contacts(list) = load(&path, FileKind::Vcf, &[], chrono_tz::UTC).unwrap() else {
        panic!("se esperaban contactos")
    };
    assert_eq!(list.total, 2);
    let before = s.contacts().len();
    assert!(notice(s.add_file_contacts(&list.contacts)).contains("Se agregaron 2 contactos"));
    assert_eq!(s.contacts().len(), before + 2);
    assert!(s.contacts().iter().any(|c| c.full_name == "Zoe Quintana"));
    s.run("undo").unwrap();
    assert_eq!(s.contacts().len(), before);
}

#[test]
fn eml_de_solo_lectura_con_invitacion() {
    let files = tempfile::tempdir().unwrap();
    let path = files.path().join("mensaje.eml");
    let raw = format!(
        "From: Ana <ana@juzgado.example>\r\nTo: yo@example.org\r\nSubject: Citatorio\r\n\
         Date: Tue, 20 Oct 2026 10:00:00 +0000\r\nMessage-ID: <cit-1@juzgado.example>\r\nMIME-Version: 1.0\r\n\
         Content-Type: multipart/mixed; boundary=\"b\"\r\n\r\n--b\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n\
         Se le cita a la audiencia.\r\n--b\r\nContent-Type: text/calendar; method=REQUEST; charset=utf-8\r\n\r\n{}\r\n--b--\r\n",
        ics(Some("REQUEST"), "cit-1@prueba", "yo@example.org")
    );
    std::fs::write(&path, raw).unwrap();
    let Opened::Message(m) = load(&path, FileKind::Eml, &["yo@example.org".into()], chrono_tz::UTC).unwrap() else {
        panic!("se esperaba un mensaje")
    };
    assert_eq!(m.subject, "Citatorio");
    assert_eq!(m.from[0].addr, "ana@juzgado.example");
    assert!(m.body.text.contains("Se le cita"));
    assert!(m.invitation.as_ref().is_some_and(|i| i.uid == "cit-1@prueba"));
}

#[test]
fn enlace_a_un_mensaje_y_archivo_en_segundo_plano() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(dir.path());
    // Un mensaje del ejemplo con Message-ID.
    let inbox: Vec<_> = s.rows().iter().map(|r| r.id).collect();
    let (id, msgid) = inbox
        .iter()
        .find_map(|id| s.store().get_envelope(*id).unwrap().and_then(|e| e.message_id.map(|m| (*id, m))))
        .expect("el ejemplo trae Message-ID");
    s.module = Module::Calendar;
    assert!(s.open_message_id(&format!("<{msgid}>")));
    assert_eq!(s.module, Module::Mail);
    settle(&mut s);
    assert_eq!(s.message_by_message_id(&msgid), Some(id));
    assert!(!s.open_message_id("no-existe@x"));

    // Lectura en segundo plano: llega como `FileOpened` (y un archivo que no existe, con su error).
    let missing = dir.path().join("no.vcf");
    let job = s.open_file(missing, FileKind::Vcf);
    let evs = settle(&mut s);
    let got = evs.into_iter().find_map(|e| match e {
        Event::Custom { id, value } if id == job => value.downcast::<iuredesk_engine::session::FileOpened>().ok(),
        _ => None,
    });
    let got = got.expect("llega el resultado");
    assert!(got.result.is_err_and(|e| e.es.contains("no existe")));
}
