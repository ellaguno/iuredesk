//! Pruebas de la sesión sobre el buzón de ejemplo: comandos, deshacer real (archivar, eliminar,
//! banderas, leído), cola de operaciones, bandeja unificada, conversaciones y búsqueda.

use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{TimeZone, Utc};
use iuredesk_accounts::MemoryStore;
use iuredesk_engine::{Event, FolderSel, Module, OfflineTransport, Outcome, Session, SessionConfig, Zone};
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
        sync: None,
        transport: Some(Arc::new(OfflineTransport)),
    })
    .unwrap();
    s.prefs.mark_read_delay_ms = 0;
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
    // Como si ya estuviera sincronizado: con UID, la acción se encola para el servidor.
    s.store().set_remote_id(id, Some("7".into())).unwrap();
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
    assert_eq!(
        s.store().get_envelope(id).unwrap().unwrap().remote_id.as_deref(),
        Some("7"),
        "vuelve con su UID: la próxima sincronización no lo duplica"
    );
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
    let mut d = s.compose_mailto("mailto:ana@example.com?subject=Hola&body=Cuerpo%20del%20borrador").unwrap();
    let id = s.save_draft(&mut d, false).unwrap();
    assert_eq!(d.saved, Some(id));
    // Volver a guardar reemplaza la versión anterior.
    let again = s.save_draft(&mut d, true).unwrap();
    assert!(s.store().get_envelope(id).unwrap().is_none());
    let id = again;
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

fn wait_for(s: &mut Session, what: &str, mut f: impl FnMut(&mut Session) -> bool) {
    let start = Instant::now();
    while !f(s) {
        assert!(start.elapsed() < Duration::from_secs(20), "tiempo agotado: {what}");
        s.poll();
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn mark_read_waits_for_the_delay() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(dir.path(), false);
    s.prefs.mark_read_delay_ms = 150;
    let id =
        s.rows().iter().map(|r| r.id).find(|&id| !s.store().get_envelope(id).unwrap().unwrap().flags.seen).unwrap();
    select(&mut s, id);
    assert!(!s.store().get_envelope(id).unwrap().unwrap().flags.seen, "aún no");
    assert!(s.next_deadline().is_some(), "la UI sabe cuándo volver");
    wait_for(&mut s, "leído tras el retraso", |s| s.store().get_envelope(id).unwrap().unwrap().flags.seen);
    // Pasar de largo no marca nada. Retraso largo: en Windows de la CI, ir y volver entre dos
    // correos podía tardar más de 150 ms y marcarlo «de verdad».
    s.prefs.mark_read_delay_ms = 3_000;
    let other = s
        .rows()
        .iter()
        .map(|r| r.id)
        .filter(|&x| x != id)
        .find(|&x| !s.store().get_envelope(x).unwrap().unwrap().flags.seen)
        .unwrap();
    select(&mut s, other);
    select(&mut s, id);
    std::thread::sleep(Duration::from_millis(200));
    s.poll();
    assert!(!s.store().get_envelope(other).unwrap().unwrap().flags.seen);
}

#[test]
fn reply_uses_the_compose_builders_with_quote_and_identity() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(dir.path(), false);
    let id = s
        .rows()
        .iter()
        .map(|r| r.id)
        .find(|&id| s.store().get_envelope(id).unwrap().unwrap().from.first().is_some_and(|m| m.name.is_some()))
        .unwrap();
    let env = s.envelope(id).unwrap().clone();
    select(&mut s, id);
    let Outcome::Compose(d) = s.run("mail.reply").unwrap() else { panic!("redactor") };
    assert_eq!(d.kind, iuredesk_engine::ComposeKind::Reply);
    assert_eq!(d.account, Some(env.account));
    assert!(d.draft.subject.to_lowercase().starts_with("re:"), "{}", d.draft.subject);
    assert_eq!(d.draft.to[0].addr, env.from[0].addr);
    assert!(d.draft.in_reply_to.is_some());
    assert!(d.draft.body.blocks.iter().any(|b| matches!(b, iuredesk_engine::iuredesk_compose::Block::Quote { .. })));
    assert_eq!(d.draft.original.as_ref().and_then(|o| o.message), Some(id));
    let Outcome::Compose(f) = s.run("mail.forward").unwrap() else { panic!("redactor") };
    assert!(f.draft.to.is_empty());
    assert!(f.draft.subject.to_lowercase().starts_with("fwd:") || f.draft.subject.starts_with("RV:"));
}

#[test]
fn send_goes_through_the_outbox_with_undo() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(dir.path(), false);
    let mut d = s.compose_new(None);
    d.draft.set_to("ana@example.com").unwrap();
    d.draft.subject = "Prueba de envío".into();
    d.draft.body = iuredesk_engine::iuredesk_compose::RichBody::from_plain("Hola");
    // Con ventana para deshacer: queda en la bandeja de salida y «Deshacer» lo devuelve.
    let Outcome::Notice(n) = s.send(&d).unwrap() else { panic!("aviso") };
    assert!(n.undo);
    assert_eq!(n.text.es, "Enviando…");
    assert_eq!(s.outbox_entries().len(), 1);
    let Outcome::Compose(back) = s.run("undo").unwrap() else { panic!("vuelve al redactor") };
    assert_eq!(back.draft.subject, "Prueba de envío");
    assert!(s.outbox_entries().is_empty());
    // Sin ventana: sale enseguida y queda la copia en «Enviados».
    s.prefs.send_undo_secs = 0;
    s.apply_send_prefs();
    s.send(&d).unwrap();
    let mut sent = false;
    wait_for(&mut s, "enviado", |s| {
        let evs = s.poll();
        sent |= evs.iter().any(|e| matches!(e, Event::Notice(n) if n.text.es == "Mensaje enviado"));
        sent
    });
    assert!(s.outbox_entries().is_empty());
    let hits = s.store().search("asunto:prueba", 50).unwrap();
    let copy = hits.iter().find(|e| s.folder(e.folder).is_some_and(|f| f.role == FolderRole::Sent));
    assert!(copy.is_some(), "copia local en Enviados");
}

#[test]
fn invalid_recipients_are_explained() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(dir.path(), false);
    let d = s.compose_new(None);
    let e = s.send(&d).unwrap_err();
    assert!(e.text().es.to_lowercase().contains("destinatario"), "{}", e.text().es);
}

#[test]
fn sync_events_update_status_and_announce_new_mail() {
    use iuredesk_engine::iuredesk_sync::{FriendlyError, NewMailSample, Phase, SyncEvent};
    use iuredesk_engine::{AccountSync, SyncStatus};
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(dir.path(), false);
    assert_eq!(s.sync_status(), SyncStatus::Off, "sin sincronización en el buzón de ejemplo");
    let acct = s.account_labels()[0].id;
    let ev = s.feed_sync_event(SyncEvent::Progress {
        account: acct,
        phase: Phase::Headers,
        folder: None,
        done: 3,
        total: 10,
    });
    assert!(ev.is_empty());
    assert!(matches!(s.account_sync(acct), Some(AccountSync::Working { done: 3, total: 10, .. })));
    assert!(s.account_sync(acct).unwrap().label().contains("3/10"));
    let ev = s.feed_sync_event(SyncEvent::NewMail {
        account: acct,
        count: 2,
        sample: vec![NewMailSample {
            message: iuredesk_model::MessageId(1),
            from: "Ana".into(),
            subject: "Hola".into(),
        }],
    });
    // F5.3: con categorías, el aviso espera a que lleguen los mensajes (y se aplican reglas).
    assert!(ev.is_empty());
    let ev =
        s.feed_sync_event(SyncEvent::Arrived { account: acct, folder: iuredesk_model::FolderId(1), messages: vec![] });
    assert!(matches!(&ev[..], [Event::NewMail { detail, .. }] if detail.contains("Ana — Hola")));
    let ev = s.feed_sync_event(SyncEvent::Error(FriendlyError {
        account: Some(acct),
        es: "Sin conexión".into(),
        en: "Offline".into(),
        transient: true,
        needs_reauth: false,
        detail: String::new(),
    }));
    assert!(ev.is_empty());
    assert!(matches!(s.account_sync(acct), Some(AccountSync::Offline(_))));
    s.feed_sync_event(SyncEvent::Error(FriendlyError {
        account: Some(acct),
        es: "La contraseña ya no funciona".into(),
        en: "Password rejected".into(),
        transient: false,
        needs_reauth: true,
        detail: String::new(),
    }));
    assert!(matches!(s.account_sync(acct), Some(AccountSync::Error { reauth: true, .. })));
    s.feed_sync_event(SyncEvent::Synced { account: acct, took: Duration::from_secs(1), backfill_pending: false });
    assert!(matches!(s.account_sync(acct), Some(AccountSync::Ready { .. })));
    // Los cambios refrescan la lista (con retraso acotado) sin perder la selección.
    let id = s.rows()[0].id;
    select(&mut s, id);
    s.feed_sync_event(SyncEvent::FolderUpdated { account: acct, folder: iuredesk_model::FolderId(1) });
    wait_for(&mut s, "refresco", |s| s.next_deadline().is_none());
    settle(&mut s);
    assert_eq!(s.focused(), Some(id));
}

#[test]
fn imip_replies_from_pim_sync_go_to_the_outbox() {
    use iuredesk_pim_sync::{ImipReply, OP_IMIP_REPLY};
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(dir.path(), false);
    let acct = s.account_labels()[0].id;
    let r = ImipReply {
        event: iuredesk_model::EventId(1),
        uid: "inv-1@example.com".into(),
        from: iuredesk_model::Mailbox::new("Laura", "laura@despacho.example.org"),
        to: iuredesk_model::Mailbox::new("Ana", "ana@example.com"),
        subject: "Aceptado: Comité".into(),
        body: "Ahí estaré.".into(),
        partstat: iuredesk_model::PartStat::Accepted,
        ics: "BEGIN:VCALENDAR\r\nMETHOD:REPLY\r\nEND:VCALENDAR\r\n".into(),
    };
    s.store().enqueue_op(acct, OP_IMIP_REPLY, &r.encode()).unwrap();
    s.poll();
    let entries = s.outbox_entries();
    assert_eq!(entries.len(), 1, "la respuesta queda en la bandeja de salida");
    assert_eq!(entries[0].subject, "Aceptado: Comité");
    assert!(!s.store().peek_ops(100).unwrap().iter().any(|o| o.kind == OP_IMIP_REPLY), "y sale de la cola");
}

#[test]
fn pim_events_refresh_views_and_explain_conflicts() {
    use iuredesk_pim_sync::{ConflictResolution, PimEvent};
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(dir.path(), false);
    let acct = s.account_labels()[0].id;
    let before = s.data_generation();
    assert!(s.feed_pim_event(PimEvent::CollectionsChanged { account: acct }));
    assert_ne!(s.data_generation(), before);
    s.feed_pim_event(PimEvent::Conflict {
        account: acct,
        item: iuredesk_store::PimRef::Event(iuredesk_model::EventId(1)),
        summary: "Comité".into(),
        resolution: ConflictResolution::ServerWon,
    });
    let evs = s.poll();
    assert!(evs.iter().any(|e| matches!(e, Event::Notice(n) if n.text.es.contains("«Comité»"))));
}

/// R2.1: un refresco de la sincronización no cambia la lista agrupada por una de mensajes sueltos
/// mientras se recalculan las conversaciones (antes parpadeaba y saltaba el desplazamiento).
#[test]
fn sync_refresh_keeps_grouped_rows_until_regrouped() {
    use iuredesk_engine::iuredesk_sync::SyncEvent;
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(dir.path(), true);
    let grouped = s.rows().to_vec();
    assert!(grouped.iter().any(|r| r.count > 1), "el ejemplo tiene conversaciones");
    let account = s.account_labels()[0].id;
    s.feed_sync_event(SyncEvent::Synced { account, took: Duration::from_millis(5), backfill_pending: false });
    // Este `poll` relee carpetas y lanza el recálculo en segundo plano.
    s.poll();
    assert_eq!(s.rows(), grouped.as_slice());
    settle(&mut s);
    assert!(!s.is_loading());
    assert_eq!(s.rows(), grouped.as_slice());
}
