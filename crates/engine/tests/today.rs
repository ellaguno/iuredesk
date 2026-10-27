//! «Mi día» (F6.1) en la sesión: el resumen se arma en segundo plano y sigue a los cambios; el
//! módulo de arranque obedece a la preferencia (Mi día / Donde lo dejé).

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{TimeZone, Utc};
use iuredesk_accounts::MemoryStore;
use iuredesk_engine::{FolderSel, Module, OfflineTransport, Session, SessionConfig, StartPref, Zone};
use iuredesk_store::{Store, sample};

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 21, 11, 20, 0).unwrap()
}

fn open(store: Arc<Store>, prefs: Option<&Path>) -> Session {
    Session::new(SessionConfig {
        store,
        accounts: None,
        secrets: Arc::new(MemoryStore::default()),
        prefs_path: prefs.map(Path::to_path_buf),
        now: Some(now()),
        zone: Zone::utc(),
        sample: true,
        sync: None,
        transport: Some(Arc::new(OfflineTransport)),
    })
    .unwrap()
}

fn sample_store(dir: &Path) -> Arc<Store> {
    let store = Store::open(dir).unwrap();
    sample::populate(&store, sample::DEFAULT_SEED, now()).unwrap();
    Arc::new(store)
}

fn settle(s: &mut Session) {
    let start = Instant::now();
    loop {
        s.poll();
        if s.is_idle() {
            s.poll();
            return;
        }
        assert!(start.elapsed() < Duration::from_secs(30), "los trabajos de fondo no terminan");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Pide el resumen y espera al trabajo de fondo.
fn summary(s: &mut Session) -> Arc<iuredesk_engine::today::DaySummary> {
    let _ = s.day_summary();
    settle(s);
    s.day_summary().expect("resumen listo")
}

#[test]
fn day_summary_is_built_in_the_background_and_follows_changes() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = open(sample_store(dir.path()), None);
    settle(&mut s);
    let day = summary(&mut s);
    assert!(!day.events.is_empty());
    assert!(day.events.iter().any(|e| e.iurefficient && e.join.as_ref().is_some_and(|j| j.provider == "Teams")));
    let all: Vec<_> = day.tasks.overdue.iter().chain(&day.tasks.today).chain(&day.tasks.upcoming).collect();
    assert!(all.iter().any(|t| t.badge == "Iurefficient"), "tareas de Iurefficient con su insignia");
    assert!(all.iter().any(|t| t.badge == "Despacho"), "y las locales con su lista");
    assert!(day.iure.is_some(), "el ejemplo tiene sesión de Iurefficient");
    assert_eq!(day.birthdays.first().map(|b| b.in_days), Some(0), "cumpleaños de hoy en el ejemplo");
    assert!(!day.needs_reply.is_empty());
    // Terminar una tarea de hoy: el resumen nuevo ya no la tiene.
    let t = day.tasks.today.iter().find(|t| t.badge != "Iurefficient").unwrap().task.id;
    let lists = s.task_lists();
    s.toggle_task(t, &lists).unwrap();
    let day2 = summary(&mut s);
    assert!(!day2.tasks.today.iter().any(|x| x.task.id == t));
    // Una de Iurefficient también se puede terminar en el ejemplo (sin red).
    assert!(s.iure_can_write());
}

#[test]
fn start_module_follows_the_preference() {
    let dir = tempfile::tempdir().unwrap();
    let prefs = dir.path().join("prefs.json");
    let store = sample_store(&dir.path().join("store"));
    // Usuario nuevo: «Mi día».
    let mut s = open(Arc::clone(&store), Some(&prefs));
    s.open_at_start();
    assert_eq!(s.module, Module::Today);
    // Va a «Marcados» y luego a Tareas; elige «Donde lo dejé».
    s.select_folder(FolderSel::Flagged);
    s.run("nav.tasks").unwrap();
    s.remember_place();
    s.prefs.start = StartPref::LastPlace;
    s.save_prefs();
    s.shutdown();
    drop(s);
    let mut s = open(Arc::clone(&store), Some(&prefs));
    s.open_at_start();
    assert_eq!(s.module, Module::Tasks);
    assert_eq!(s.folder_sel(), FolderSel::Flagged);
    // Con «Mi día» se ignora el último lugar.
    s.prefs.start = StartPref::Today;
    s.open_at_start();
    assert_eq!(s.module, Module::Today);
    // `nav.today` lleva a «Mi día».
    s.run("nav.mail").unwrap();
    s.run("nav.today").unwrap();
    assert_eq!(s.module, Module::Today);
    s.shutdown();
}

#[test]
fn a_damaged_last_place_falls_back_to_today() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = open(sample_store(dir.path()), None);
    s.prefs.start = StartPref::LastPlace;
    s.prefs.last_module = Some("nada".into());
    s.prefs.last_folder = Some("folder:999999".into());
    s.open_at_start();
    assert_eq!(s.module, Module::Today);
    assert_eq!(s.folder_sel(), FolderSel::default());
    s.shutdown();
}
