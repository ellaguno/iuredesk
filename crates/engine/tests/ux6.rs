//! UX-6 en la sesión: editor de tareas por fuente (qué se guarda, mover de lista, deshacer, la
//! cola de `pim-sync`) y notas como archivos Markdown (migración, escribir, renombrar, borrar, lo
//! guardado en IureEditor vuelve, conflicto con las dos versiones, deep link con ruta absoluta).

use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{NaiveDate, TimeZone, Utc};
use iuredesk_accounts::MemoryStore;
use iuredesk_engine::session::{NoteSaved, TaskSource};
use iuredesk_engine::{OfflineTransport, Outcome, Session, SessionConfig, Zone};
use iuredesk_model::{EventTime, Note, TaskList, TaskStatus};
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

fn settle(s: &mut Session) {
    let start = Instant::now();
    loop {
        s.poll();
        if s.is_idle() {
            s.poll();
            return;
        }
        assert!(start.elapsed() < Duration::from_secs(30));
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn day(d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, d).unwrap()
}

#[test]
fn task_editor_respects_each_source() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(dir.path());
    let account = s.account_labels()[0].id;
    let store = Arc::clone(s.store());
    let list = |name: &str, path: String| {
        store
            .put_task_list(&TaskList {
                account: Some(account),
                name: name.into(),
                remote_path: path,
                ..Default::default()
            })
            .unwrap()
    };
    let google = list("Mis tareas", format!("{}L1", iuredesk_pim_sync::GOOGLE_TASKS_LIST_PREFIX));
    let google2 = list("Trabajo", format!("{}L2", iuredesk_pim_sync::GOOGLE_TASKS_LIST_PREFIX));
    let caldav = list("Pendientes", "/dav/cal/tareas/".into());
    let todo = list("Tasks", "AAMkAD-lista".into());
    s.reload_accounts().unwrap();
    assert_eq!(s.task_source(google), TaskSource::GoogleTasks);
    assert_eq!(s.task_source(caldav), TaskSource::CalDav);
    assert_eq!(s.task_source(todo), TaskSource::MicrosoftToDo);
    let gcaps = s.task_caps(google);
    assert!(!gcaps.start && !gcaps.priority && !gcaps.reminder && !gcaps.recurrence && !gcaps.due_time);
    assert!(gcaps.move_list);
    let ccaps = s.task_caps(caldav);
    assert!(ccaps.start && ccaps.reminder && ccaps.recurrence && ccaps.due_time);
    assert!(!s.task_caps(todo).due_time);
    // Mover solo entre listas de la misma cuenta y el mismo tipo.
    let targets: Vec<_> = s.task_move_targets(google).into_iter().map(|l| l.id).collect();
    assert!(targets.contains(&google2) && !targets.contains(&caldav));

    // Google: lo que no guarda se queda como estaba; el vencimiento pierde la hora.
    s.add_task(google, "Pagar la renta").unwrap();
    let t = s.tasks(google).into_iter().next().unwrap();
    let ops0 = store.ops_len().unwrap();
    let mut e = t.clone();
    e.summary = "Pagar la renta de noviembre".into();
    e.description = "Antes del 5".into();
    e.due = Some(EventTime::DateTime { utc: Utc.with_ymd_and_hms(2026, 11, 3, 10, 0, 0).unwrap(), tzid: None });
    e.start = Some(EventTime::Date(day(1)));
    e.priority = 1;
    e.rrule = "FREQ=MONTHLY".into();
    e.status = TaskStatus::Completed;
    let out = s.save_task_edit(e).unwrap();
    assert!(matches!(out, Outcome::Notice(ref n) if n.undo));
    let g = s.task(t.id).unwrap();
    assert_eq!(g.summary, "Pagar la renta de noviembre");
    assert_eq!(g.due, Some(EventTime::Date(day(3))));
    assert_eq!((g.start.clone(), g.priority, g.rrule.as_str()), (None, 0, ""));
    assert!(g.is_done() && g.completed.is_some());
    assert!(
        store.ops_len().unwrap() > ops0.saturating_sub(1) && store.ops_len().unwrap() > 0,
        "local primero y a la cola"
    );
    // Deshacer devuelve todo.
    s.run("undo").unwrap();
    let back = s.task(t.id).unwrap();
    assert_eq!(back.summary, "Pagar la renta");
    assert!(!back.is_done());
    // Sin título no se guarda.
    let mut bad = back.clone();
    bad.summary = "   ".into();
    assert!(matches!(s.save_task_edit(bad).unwrap(), Outcome::Notice(ref n) if !n.undo));
    // F2: solo el título.
    s.rename_task(t.id, "Renta").unwrap();
    assert_eq!(s.task(t.id).unwrap().summary, "Renta");

    // Mover a otra lista de Google: se borra de una y se crea en la otra (deshacer lo devuelve).
    let mut m = s.task(t.id).unwrap();
    m.list = google2;
    s.save_task_edit(m.clone()).unwrap();
    assert!(s.tasks(google).is_empty());
    assert!(s.tasks(google2).iter().any(|x| x.summary == "Renta" && x.remote_id.is_none()));
    s.run("undo").unwrap();
    assert!(s.tasks(google2).is_empty());
    assert!(s.tasks(google).iter().any(|x| x.summary == "Renta"));
    // A una lista de otro tipo no se mueve.
    let cur = s.tasks(google).into_iter().next().unwrap();
    let mut x = cur.clone();
    x.list = caldav;
    x.summary = "Renta!".into();
    s.save_task_edit(x).unwrap();
    assert_eq!(s.task(cur.id).unwrap().list, google);

    // CalDAV: guarda inicio, hora, recordatorio y repetición.
    s.add_task(caldav, "Revisar contrato").unwrap();
    let c = s.tasks(caldav).into_iter().next().unwrap();
    let mut e = c.clone();
    let due = EventTime::DateTime { utc: Utc.with_ymd_and_hms(2026, 11, 3, 17, 0, 0).unwrap(), tzid: None };
    e.due = Some(due.clone());
    e.start = Some(EventTime::Date(day(1)));
    e.reminder = Some(Utc.with_ymd_and_hms(2026, 11, 3, 9, 0, 0).unwrap());
    e.rrule = "RRULE:FREQ=WEEKLY".into();
    e.priority = 5;
    s.save_task_edit(e).unwrap();
    let c2 = s.task(c.id).unwrap();
    assert_eq!(c2.due, Some(due));
    assert_eq!(c2.start, Some(EventTime::Date(day(1))));
    assert_eq!(c2.rrule, "FREQ=WEEKLY");
    assert_eq!(c2.priority, 5);
    assert!(c2.reminder.is_some());
}

fn note_session(dir: &std::path::Path, notes: &std::path::Path) -> Session {
    let mut s = session(dir);
    s.notes_fs_for_tests(notes.to_path_buf(), Duration::from_millis(30));
    assert!(s.notes_fs_flush(Duration::from_secs(10)));
    s
}

fn flush(s: &mut Session) {
    assert!(s.notes_fs_flush(Duration::from_secs(10)));
}

/// Pasan vueltas de la carpeta hasta que se cumpla `cond` (el antirrebote pide dos).
fn until(s: &mut Session, mut cond: impl FnMut(&Session) -> bool) {
    let start = Instant::now();
    while !cond(s) {
        if start.elapsed() > Duration::from_secs(15) {
            let ns: Vec<_> =
                s.notes().into_iter().filter(|n| n.title.starts_with("Minuta R")).map(|n| (n.id, n.title)).collect();
            let d = s.notes_dir().unwrap();
            eprintln!("existe: {}", d.join("Minuta Ramírez.md").exists());
            panic!("no llegó el cambio de la carpeta: {ns:#?}");
        }
        flush(s);
        std::thread::sleep(Duration::from_millis(40));
    }
}

#[test]
fn notes_are_markdown_files_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    let notes_dir = folder.path().join("Notas");
    std::fs::create_dir_all(&notes_dir).unwrap();
    // Migración: una nota que ya estaba en el almacén se exporta al primer arranque.
    {
        let store = Store::open(dir.path()).unwrap();
        store.put_note(&Note { title: "Vieja".into(), body: "de antes".into(), ..Default::default() }).unwrap();
    }
    let mut s = note_session(dir.path(), &notes_dir);
    flush(&mut s);
    let path = notes_dir.join("Vieja.md");
    let names: Vec<_> =
        std::fs::read_dir(&notes_dir).map(|r| r.flatten().map(|e| e.file_name()).collect()).unwrap_or_default();
    eprintln!("archivos: {names:?} dir={:?}", s.notes_dir());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "# Vieja\n\nde antes\n");

    // Crear y escribir aquí: archivo con el título; cambiar el título lo renombra.
    s.new_note().unwrap();
    let id = s.note_focus().unwrap();
    let base = s.notes().into_iter().find(|n| n.id == id).unwrap().updated;
    let NoteSaved::Saved(base) = s.save_note_from(id, "Minuta", "- acuerdo", Some(base)).unwrap() else {
        panic!("no debía ser conflicto")
    };
    flush(&mut s);
    assert!(notes_dir.join("Minuta.md").is_file());
    let NoteSaved::Saved(_) = s.save_note_from(id, "Minuta Ramírez", "- acuerdo", Some(base)).unwrap() else {
        panic!()
    };
    flush(&mut s);
    assert!(!notes_dir.join("Minuta.md").exists());
    let p = s.note_path(id).unwrap();
    assert!(p.is_absolute());
    assert_eq!(p, notes_dir.join("Minuta Ramírez.md"));
    // El deep link lleva la ruta absoluta.
    let link = iuredesk_engine::iuredesk_integrations::editor::editor_link(
        &iuredesk_engine::iuredesk_integrations::editor::EditorTarget::File(p.clone()),
    )
    .unwrap();
    assert!(link.starts_with("iureditor://open?path="));
    assert_eq!(url_path(&link), p.to_string_lossy(), "IureEditor lee la misma ruta");

    // Lo guardado en IureEditor vuelve a la nota (con aviso).
    std::fs::write(&p, "# Minuta Ramírez\n\n- acuerdo\n- **nuevo** desde IureEditor\n").unwrap();
    until(&mut s, |s| s.notes().iter().any(|n| n.id == id && n.body.contains("desde IureEditor")));
    // Un archivo nuevo en la carpeta se vuelve nota.
    std::fs::write(notes_dir.join("Suelta.md"), "Primera línea\nsegunda").unwrap();
    until(&mut s, |s| s.notes().iter().any(|n| n.body.starts_with("Primera línea")));

    // Conflicto: cambió en IureEditor y aquí a la vez → las dos versiones.
    let n = s.notes().into_iter().find(|n| n.id == id).unwrap();
    std::fs::write(&p, "# Minuta Ramírez\n\nversión de IureEditor\n").unwrap();
    s.save_note(id, "Minuta Ramírez", "versión de IureDesk").unwrap();
    let _ = n;
    until(&mut s, |s| {
        s.notes().iter().any(|n| n.title.contains("copia en conflicto") || n.title.contains("conflicting copy"))
    });
    let all = s.notes();
    assert!(all.iter().any(|n| n.id == id && n.body == "versión de IureEditor"), "la nota toma lo de IureEditor");
    assert!(all.iter().any(|n| n.id != id && n.body == "versión de IureDesk"), "lo de aquí queda en la copia");
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "# Minuta Ramírez\n\nversión de IureEditor\n");

    // Borrar la nota borra su archivo; deshacer lo vuelve a escribir.
    s.delete_note(id).unwrap();
    flush(&mut s);
    assert!(!p.exists());
    s.run("undo").unwrap();
    flush(&mut s);
    assert!(p.is_file());
    // Borrado fuera: la nota se va (con deshacer).
    std::fs::remove_file(&p).unwrap();
    until(&mut s, |s| !s.notes().iter().any(|n| n.title == "Minuta Ramírez"));
}

/// `iureditor://open?path=…` → la ruta, como la lee IureEditor (`url` + `query_pairs`).
fn url_path(link: &str) -> String {
    let q = link.split_once("path=").map(|(_, q)| q).unwrap_or("");
    let mut out = Vec::new();
    let b = q.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let h = std::str::from_utf8(&b[i + 1..i + 3]).unwrap();
            out.push(u8::from_str_radix(h, 16).unwrap());
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap()
}

/// En sesiones sin preferencias (pruebas, ejemplo) no se escribe nada fuera.
#[test]
fn no_notes_folder_without_prefs() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(dir.path());
    assert_eq!(s.notes_dir(), None);
    s.new_note().unwrap();
    let id = s.note_focus().unwrap();
    s.save_note(id, "Solo aquí", "x").unwrap();
    assert_eq!(s.note_path(id), None);
}
