//! Pruebas de la sesión sobre el buzón de ejemplo: comandos, deshacer real (archivar, eliminar,
//! banderas, leído), cola de operaciones, bandeja unificada, conversaciones y búsqueda.

use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{TimeZone, Utc};
use iuredesk_accounts::MemoryStore;
use iuredesk_engine::{FolderSel, Module, Outcome, Session, SessionConfig, Zone};
use iuredesk_model::{FolderRole, MessageId};
use iuredesk_store::{Store, sample};

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 21, 15, 0, 0).unwrap()
}

fn session(dir: &std::path::Path, conversations: bool) -> Session {
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
    })
    .unwrap();
    s.set_conversations(conversations);
    settle(&mut s);
    s
}

/// Espera a que terminen los trabajos de fondo.
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

fn select(s: &mut Session, id: MessageId) {
    s.set_selection(vec![id], Some(id));
    settle(s);
}

fn folder_of(s: &Session, id: MessageId) -> iuredesk_model::FolderId {
    s.store().get_envelope(id).unwrap().unwrap().folder
}

#[test]
fn unified_inbox_merges_accounts_by_date() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(dir.path(), false);
    assert_eq!(s.folder_sel(), FolderSel::Unified(FolderRole::Inbox));
    let rows: Vec<MessageId> = s.rows().iter().map(|r| r.id).collect();
    assert!(rows.len() > 300);
    let dates: Vec<_> = rows.iter().map(|&id| s.envelope(id).unwrap().date).collect();
    assert!(dates.windows(2).all(|w| w[0] >= w[1]), "orden por fecha descendente");
    let accounts: std::collections::HashSet<_> = rows.iter().map(|&id| s.envelope(id).unwrap().account).collect();
    assert_eq!(accounts.len(), 3, "las tres cuentas");
}

#[test]
fn archive_and_undo_restore_folder_and_cancel_the_queued_op() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(dir.path(), false);
    s.prefs.mark_read_on_open = false;
    let id = s.rows()[3].id;
    let next = s.rows()[4].id;
    let inbox = folder_of(&s, id);
    let ops_before = s.store().ops_len().unwrap();
    select(&mut s, id);
    assert!(s.is_enabled("mail.archive"));
    let out = s.run("mail.archive").unwrap();
    let Outcome::Notice(n) = out else { panic!("se esperaba un aviso: {out:?}") };
    assert!(n.undo);
    assert_eq!(n.text.es, "Mensaje archivado");
    let archived_to = folder_of(&s, id);
    assert_eq!(s.folder(archived_to).unwrap().role, FolderRole::Archive);
    assert_eq!(s.store().ops_len().unwrap(), ops_before + 1, "la acción se encola");
    settle(&mut s);
    assert!(!s.rows().iter().any(|r| r.id == id), "sale de la bandeja");
    assert_eq!(s.take_focus_request().map(|(_, m)| m), Some(next), "el foco pasa al siguiente");

    s.run("undo").unwrap();
    settle(&mut s);
    assert_eq!(folder_of(&s, id), inbox);
    assert_eq!(s.store().ops_len().unwrap(), ops_before, "la operación pendiente se cancela");
    assert!(s.rows().iter().any(|r| r.id == id));
    assert_eq!(s.take_focus_request().map(|(_, m)| m), Some(id));
    assert!(!s.is_enabled("undo"));
}

#[test]
fn flag_and_read_toggles_are_undoable() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(dir.path(), false);
    s.prefs.mark_read_on_open = false;
    let id = s.rows().iter().map(|r| r.id).find(|&id| s.store().get_envelope(id).unwrap().unwrap().flags.seen).unwrap();
    select(&mut s, id);
    let before = s.envelope(id).unwrap().flags;
    assert!(!before.flagged);

    s.run("mail.toggle_flag").unwrap();
    assert!(s.store().get_envelope(id).unwrap().unwrap().flags.flagged);
    s.run("mail.toggle_read").unwrap();
    assert!(!s.store().get_envelope(id).unwrap().unwrap().flags.seen, "leído → no leído");

    s.run("undo").unwrap();
    assert!(s.store().get_envelope(id).unwrap().unwrap().flags.seen);
    s.run("undo").unwrap();
    assert_eq!(s.store().get_envelope(id).unwrap().unwrap().flags, before);
}

#[test]
fn opening_marks_read_and_delete_goes_to_trash() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(dir.path(), false);
    let id =
        s.rows().iter().map(|r| r.id).find(|&id| !s.store().get_envelope(id).unwrap().unwrap().flags.seen).unwrap();
    let unread_before = s.unified_unread(FolderRole::Inbox);
    select(&mut s, id);
    assert!(s.store().get_envelope(id).unwrap().unwrap().flags.seen);
    assert_eq!(s.unified_unread(FolderRole::Inbox), unread_before - 1);
    s.run("mail.delete").unwrap();
    let f = folder_of(&s, id);
    assert_eq!(s.folder(f).unwrap().role, FolderRole::Trash);
    s.run("undo").unwrap();
    assert_eq!(s.folder(folder_of(&s, id)).unwrap().role, FolderRole::Inbox);
}

#[test]
fn commands_without_selection_explain_instead_of_failing() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(dir.path(), false);
    assert!(!s.is_enabled("mail.reply"));
    assert!(matches!(s.run("mail.archive").unwrap(), Outcome::Notice(_)));
    assert!(s.run("no.existe").is_err());
    assert!(matches!(s.run("mail.new").unwrap(), Outcome::Compose(_)));
    assert_eq!(s.run("app.palette").unwrap(), Outcome::OpenPalette);
    s.run("nav.calendar").unwrap();
    assert_eq!(s.module, Module::Calendar);
    assert_eq!(s.run("view.density.dense").unwrap(), Outcome::Restyle);
    assert_eq!(s.prefs.density, iuredesk_engine::DensityPref::Dense);
    s.run("view.scheme.gmail").unwrap();
    assert_eq!(s.scheme(), iuredesk_engine::ShortcutScheme::Gmail);
}

#[test]
fn every_command_runs_or_explains() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(dir.path(), true);
    let id = s.rows()[0].id;
    select(&mut s, id);
    for c in iuredesk_engine::COMMANDS {
        let r = s.run(c.id);
        assert!(r.is_ok(), "{}: {r:?}", c.id);
        settle(&mut s);
        if s.focused().is_none()
            && let Some(r) = s.rows().first().map(|r| r.id)
        {
            select(&mut s, r);
        }
        s.module = Module::Mail;
    }
}

#[test]
fn conversations_group_threads_and_archive_whole_conversation() {
    let dir = tempfile::tempdir().unwrap();
    let flat = session(dir.path(), false);
    let flat_n = flat.rows().len();
    drop(flat);
    let dir2 = tempfile::tempdir().unwrap();
    let mut s = session(dir2.path(), true);
    assert!(s.rows().len() < flat_n, "{} < {flat_n}", s.rows().len());
    let row = *s.rows().iter().find(|r| r.count > 1).unwrap();
    let conv = s.conversation(row.id);
    assert!(conv.len() > 1);
    assert!(conv.windows(2).all(|w| w[0].date <= w[1].date), "orden cronológico");
    select(&mut s, row.id);
    s.run("mail.archive").unwrap();
    settle(&mut s);
    assert!(!s.rows().iter().any(|r| r.id == row.id));
    s.run("undo").unwrap();
    settle(&mut s);
    assert!(s.rows().iter().any(|r| r.id == row.id));
}

#[test]
fn search_runs_in_background() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(dir.path(), false);
    s.set_search("tiene:adjunto");
    assert!(s.is_loading() || !s.rows().is_empty());
    settle(&mut s);
    assert!(!s.rows().is_empty());
    let ids: Vec<MessageId> = s.rows().iter().map(|r| r.id).collect();
    for id in ids.into_iter().take(20) {
        assert!(s.envelope(id).unwrap().has_attachments);
    }
    s.set_search("");
    settle(&mut s);
    assert!(s.rows().len() > 300);
}

#[test]
fn move_and_flagged_view() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(dir.path(), false);
    s.select_folder(FolderSel::Flagged);
    settle(&mut s);
    let flagged = s.rows().len();
    assert!(flagged > 0);
    s.select_folder(FolderSel::Unified(FolderRole::Inbox));
    settle(&mut s);
    let id = s.rows()[0].id;
    select(&mut s, id);
    let account = s.envelope(id).unwrap().account;
    let custom = s.folders_of(account).into_iter().find(|f| f.role == FolderRole::Custom).unwrap().id;
    assert_eq!(s.run("mail.move").unwrap(), Outcome::PickFolder);
    s.run(&format!("mail.move:{}", custom.get())).unwrap();
    assert_eq!(folder_of(&s, id), custom);
    s.run("undo").unwrap();
    assert_eq!(s.folder(folder_of(&s, id)).unwrap().role, FolderRole::Inbox);
}

#[test]
fn drafts_are_saved_and_bodies_load() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(dir.path(), false);
    let mut d = iuredesk_engine::ComposeDraft::from_mailto("mailto:ana@example.com?subject=Hola").unwrap();
    d.body = "Cuerpo del borrador".into();
    let id = s.save_draft(&d).unwrap();
    let env = s.store().get_envelope(id).unwrap().unwrap();
    assert!(env.flags.draft);
    assert_eq!(s.folder(env.folder).unwrap().role, FolderRole::Drafts);
    assert!(s.body(id).is_none(), "se pide en segundo plano");
    settle(&mut s);
    assert!(s.body(id).unwrap().text.contains("Cuerpo del borrador"));
}

#[test]
fn calendar_today_and_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(dir.path(), false);
    let monday = Utc.with_ymd_and_hms(2026, 10, 19, 0, 0, 0).unwrap();
    let week = s.occurrences(monday, monday + chrono::Duration::days(7)).to_vec();
    assert!(week.len() >= 15);
    assert!(week.iter().any(|o| o.recurring), "la junta semanal se expande");
    assert!(week.iter().any(|o| o.all_day));
    assert!(!s.today_events().is_empty());
    let due = s.due_tasks();
    assert!(!due.is_empty());
    let lists = s.task_lists();
    let t = due[0].id;
    s.toggle_task(t, &lists).unwrap();
    assert!(!s.due_tasks().iter().any(|x| x.id == t));
    s.run("undo").unwrap();
    assert!(s.due_tasks().iter().any(|x| x.id == t));
    assert_eq!(s.contacts().len(), 50);
}
