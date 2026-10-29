//! F7.1 en la sesión, sobre el buzón de ejemplo: reabrir un borrador en el redactor (y que
//! guardarlo reemplace al anterior), notas (crear, editar, buscar, borrar con deshacer), tareas
//! (alta, borrado y deshacer; las de una cuenta van a la cola de `pim-sync`) y carpetas propias
//! (crear, renombrar, borrar con confirmación; las del sistema no).

use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{TimeZone, Utc};
use iuredesk_accounts::MemoryStore;
use iuredesk_engine::{FolderSel, Module, OfflineTransport, Outcome, Session, SessionConfig, Zone};
use iuredesk_model::{FolderRole, Mailbox, TaskList};
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
    s.set_conversations(false);
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

#[test]
fn saved_draft_reopens_and_saving_replaces_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(dir.path());
    let account = s.account_labels()[0].id;
    let mut cd = s.compose_new(Some(account));
    cd.draft.to = vec![Mailbox::new("Ana", "ana@cliente.example")];
    cd.draft.bcc = vec![Mailbox::addr("socio@despacho.example")];
    cd.draft.subject = "Propuesta revisada".into();
    cd.draft.body = iuredesk_compose::RichBody::from_markdown("Hola **Ana**,\n\n- punto uno");
    cd.draft.in_reply_to = Some("<orig-1@cliente.example>".into());
    cd.draft.attachments.push(iuredesk_compose::Attachment::bytes("anexo.pdf", "application/pdf", b"%PDF x".to_vec()));
    // Al cerrar el redactor se guarda con adjuntos (el guardado automático, sin ellos).
    let first = s.save_draft(&mut cd, true).unwrap();
    assert!(s.is_draft(first));

    // Desde «Borradores»: «Editar borrador» (doble clic / Intro en la UI).
    s.select_folder(FolderSel::Unified(FolderRole::Drafts));
    settle(&mut s);
    s.set_selection(vec![first], Some(first));
    assert!(s.is_enabled("mail.edit_draft"));
    let Outcome::Compose(mut again) = s.run("mail.edit_draft").unwrap() else { panic!("redactor") };
    assert_eq!(again.saved, Some(first));
    assert_eq!(again.draft.subject, "Propuesta revisada");
    assert_eq!(again.draft.to[0].addr, "ana@cliente.example");
    assert_eq!(again.draft.bcc[0].addr, "socio@despacho.example", "el CCO se conserva en el borrador");
    assert_eq!(again.draft.in_reply_to.as_deref(), Some("<orig-1@cliente.example>"));
    assert_eq!(again.draft.attachments.len(), 1);
    assert!(again.draft.body.to_markdown().contains("**Ana**"));

    // Guardar de nuevo reemplaza al anterior (no se duplica).
    again.draft.subject = "Propuesta revisada v2".into();
    let second = s.save_draft(&mut again, true).unwrap();
    assert_ne!(first, second);
    assert!(s.store().get_envelope(first).unwrap().is_none(), "el borrador viejo se borra");
    assert_eq!(s.store().get_envelope(second).unwrap().unwrap().subject, "Propuesta revisada v2");

    // Un mensaje normal no se «edita».
    let inbox = s.store().folders(account).unwrap().into_iter().find(|f| f.role == FolderRole::Inbox).unwrap();
    let normal = s.store().list_envelopes(inbox.id, 0, 1).unwrap()[0].id;
    assert!(!s.is_draft(normal));
    assert!(s.edit_draft(normal).is_err());
}

#[test]
fn notes_create_edit_search_delete_undo() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(dir.path());
    let before = s.notes().len();
    s.module = Module::Notes;
    // «Nuevo» en Notas crea una nota (como «Nuevo» en el calendario crea un evento).
    s.run("mail.new").unwrap();
    let id = s.note_focus().unwrap();
    assert_eq!(s.notes().len(), before + 1);
    s.save_note(id, "Minuta con Ramírez", "# Acuerdos\n- Enviar **contrato**").unwrap();
    let n = s.notes().into_iter().find(|n| n.id == id).unwrap();
    assert_eq!(n.title, "Minuta con Ramírez");
    assert!(s.notes_matching("ramirez contrato").iter().any(|n| n.id == id));
    assert!(s.notes_matching("ramirez demanda").iter().all(|n| n.id != id));
    s.toggle_note_pin(id).unwrap();
    assert!(s.notes().iter().take_while(|n| n.pinned).any(|n| n.id == id), "fijada arriba");

    let Outcome::Notice(n) = s.run("notes.delete").unwrap() else { panic!("aviso") };
    assert!(n.undo);
    assert!(s.notes().iter().all(|n| n.id != id));
    s.run("undo").unwrap();
    let back = s.notes().into_iter().find(|n| n.title == "Minuta con Ramírez").unwrap();
    assert!(back.body.contains("contrato"));
    // Deshacer la creación la quita.
    s.run("notes.new").unwrap();
    assert_eq!(s.notes().len(), before + 2);
    s.run("undo").unwrap();
    assert_eq!(s.notes().len(), before + 1);
}

#[test]
fn tasks_add_delete_undo_and_account_lists_are_queued() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(dir.path());
    let account = s.account_labels()[0].id;
    let store = Arc::clone(s.store());
    let list = store
        .put_task_list(&TaskList {
            account: Some(account),
            name: "Mis tareas".into(),
            remote_path: format!("{}L1", iuredesk_pim_sync::GOOGLE_TASKS_LIST_PREFIX),
            ..Default::default()
        })
        .unwrap();
    s.reload_accounts().unwrap();
    assert!(s.task_list_badge(list).unwrap().starts_with("Google · "));
    let ops0 = store.ops_len().unwrap();
    s.add_task(list, "Pagar la renta").unwrap();
    let t = s.tasks(list).into_iter().find(|t| t.summary == "Pagar la renta").unwrap();
    assert_eq!(store.ops_len().unwrap(), ops0 + 1, "alta local primero y en cola (pim.put_task)");
    // Completar también va a la cola (antes se guardaba solo en local).
    s.toggle_task(t.id, &s.task_lists()).unwrap();
    assert!(s.tasks(list).iter().any(|x| x.id == t.id && x.is_done()));
    s.delete_task(t.id).unwrap();
    assert!(s.tasks(list).is_empty());
    s.run("undo").unwrap();
    assert!(s.tasks(list).iter().any(|x| x.summary == "Pagar la renta"));
    // Lista solo local: nada va a la cola.
    let local = store.put_task_list(&TaskList { name: "Personal".into(), ..Default::default() }).unwrap();
    let ops1 = store.ops_len().unwrap();
    s.add_task(local, "Comprar pan").unwrap();
    assert_eq!(store.ops_len().unwrap(), ops1);
    assert_eq!(s.task_list_badge(local), None);
}

#[test]
fn folders_create_rename_delete_with_confirmation() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(dir.path());
    let account = s.account_labels()[0].id;
    assert!(s.can_manage_folders(account));
    let f = s.create_folder(account, None, "Expedientes").unwrap();
    assert!(s.folders_of(account).iter().any(|x| x.id == f && x.name == "Expedientes"));
    assert!(s.create_folder(account, None, "expedientes").is_err(), "nombre repetido");
    assert!(s.create_folder(account, None, "  ").is_err());
    let sub = s.create_folder(account, Some(f), "2026").unwrap();
    assert_eq!(s.folder(sub).unwrap().parent, Some(f));
    s.rename_folder(f, "Expedientes VIP").unwrap();
    assert_eq!(s.folder(f).unwrap().name, "Expedientes VIP");
    // Sistema: no.
    let inbox = s.folders_of(account).iter().find(|x| x.role == FolderRole::Inbox).unwrap().id;
    assert!(!s.is_custom_folder(inbox));
    assert!(s.rename_folder(inbox, "X").is_err());
    // Borrar pide confirmación y se lleva las subcarpetas.
    let Outcome::Confirm { command, .. } = s.run(&format!("mail.delete_folder:{}", f.get())).unwrap() else {
        panic!("confirmación")
    };
    assert!(s.folder(f).is_some());
    s.run(&command).unwrap();
    assert!(s.folder(f).is_none() && s.folder(sub).is_none());
    // Por comando (CLI/MCP), sin diálogos.
    s.run(&format!("mail.new_folder:{}:Pospuestos", account.get())).unwrap();
    assert!(s.folders_of(account).iter().any(|x| x.name == "Pospuestos"));
}
