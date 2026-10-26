//! Pruebas del correo en el almacén: carpetas, mensajes, banderas, hilos y búsqueda.

use chrono::{DateTime, Duration, TimeZone, Utc};
use iuredesk_model::{AccountId, Flags, Folder, FolderId, FolderRole, Mailbox, MessageEnvelope, MessageId};
use iuredesk_store::{NewMessage, Store, StoreError};

fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 5, 1, 12, 0, 0).unwrap()
}

fn folder(s: &Store, account: u64, name: &str, role: FolderRole) -> FolderId {
    s.put_folder(&Folder {
        account: AccountId(account),
        name: name.into(),
        remote_path: name.into(),
        role,
        ..Default::default()
    })
    .unwrap()
}

fn env(folder: FolderId, n: i64, subject: &str) -> MessageEnvelope {
    MessageEnvelope {
        folder,
        remote_id: Some(format!("uid-{n}")),
        message_id: Some(format!("m{n}@test")),
        from: vec![Mailbox::new("Ana Pérez", "ana@example.com")],
        to: vec![Mailbox::addr("yo@example.org")],
        subject: subject.into(),
        date: t0() + Duration::minutes(n),
        preview: format!("cuerpo {n}"),
        ..Default::default()
    }
}

#[test]
fn list_count_upsert_flags_move_delete() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path()).unwrap();
    let inbox = folder(&s, 1, "INBOX", FolderRole::Inbox);
    let arch = folder(&s, 1, "Archive", FolderRole::Archive);
    let other = folder(&s, 2, "INBOX", FolderRole::Inbox);

    let mut ids = Vec::new();
    for n in 0..30 {
        ids.push(s.put_message(env(inbox, n, &format!("asunto {n}")), b"From: a\r\n\r\nhola").unwrap());
    }
    assert_eq!(s.count(inbox).unwrap().total, 30);
    assert_eq!(s.count(inbox).unwrap().unread, 30);
    let page = s.list_envelopes(inbox, 0, 10).unwrap();
    assert_eq!(page.len(), 10);
    assert_eq!(page[0].subject, "asunto 29"); // fecha descendente
    assert_eq!(s.list_envelopes(inbox, 25, 10).unwrap().len(), 5);
    assert!(s.list_envelopes(inbox, 1_000, 10).unwrap().is_empty());
    assert_eq!(s.get_raw(ids[0]).unwrap(), b"From: a\r\n\r\nhola");

    // Mismo id remoto sin id local → reemplaza, no duplica.
    let mut again = env(inbox, 5, "asunto 5 editado");
    again.date = t0() + Duration::days(10);
    let id = s.put_message(again, b"x").unwrap();
    assert_eq!(id, ids[5]);
    assert_eq!(s.count(inbox).unwrap().total, 30);
    assert_eq!(s.list_envelopes(inbox, 0, 1).unwrap()[0].subject, "asunto 5 editado");

    // Banderas.
    s.set_flags(ids[0], Flags { seen: true, ..Default::default() }).unwrap();
    assert_eq!(s.count(inbox).unwrap().unread, 29);
    s.set_flags(ids[0], Flags { seen: true, flagged: true, ..Default::default() }).unwrap();
    assert_eq!(s.count(inbox).unwrap().unread, 29);
    assert!(s.get_envelope(ids[0]).unwrap().unwrap().flags.flagged);

    // Mover.
    s.move_message(ids[1], arch).unwrap();
    assert_eq!(s.count(inbox).unwrap().total, 29);
    assert_eq!(s.count(arch).unwrap(), iuredesk_store::FolderCount { total: 1, unread: 1 });
    assert_eq!(s.list_envelopes(arch, 0, 10).unwrap()[0].id, ids[1]);
    assert!(matches!(s.move_message(ids[1], other), Err(StoreError::Invalid(_))));

    // Borrar.
    s.delete_message(ids[2]).unwrap();
    assert!(s.get_envelope(ids[2]).unwrap().is_none());
    assert_eq!(s.count(inbox).unwrap().total, 28);
    assert!(!s.list_ids(inbox).unwrap().contains(&ids[2]));
    s.delete_message(ids[2]).unwrap(); // idempotente
    assert!(s.set_flags(ids[2], Flags::default()).is_err());

    // Carpeta inexistente.
    assert!(s.put_message(env(FolderId(999), 1, "x"), b"").is_err());

    // Persistencia: tras reabrir, la caché se reconstruye igual.
    let before: Vec<MessageId> = s.list_ids(inbox).unwrap();
    drop(s);
    let s = Store::open(dir.path()).unwrap();
    assert_eq!(s.list_ids(inbox).unwrap(), before);
    assert_eq!(s.count(inbox).unwrap().total, 28);
    let new_id = s.put_message(env(inbox, 100, "nuevo"), b"").unwrap();
    assert!(new_id.get() > ids.iter().map(|i| i.get()).max().unwrap());

    // Borrar la carpeta borra sus mensajes.
    s.delete_folder(inbox).unwrap();
    assert!(s.get_folder(inbox).unwrap().is_none());
    assert!(s.list_envelopes(inbox, 0, 10).unwrap().is_empty());
    assert_eq!(s.message_count().unwrap(), 1); // el de Archive
}

#[test]
fn incremental_threading_and_rethread() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path()).unwrap();
    let inbox = folder(&s, 1, "INBOX", FolderRole::Inbox);

    // La respuesta llega antes que el original.
    let mut reply = env(inbox, 2, "Re: Plan");
    reply.in_reply_to = Some("m1@test".into());
    reply.references = vec!["m1@test".into()];
    let r = s.put_message(reply, b"").unwrap();
    let orig = s.put_message(env(inbox, 1, "Plan"), b"").unwrap();
    let mut reply2 = env(inbox, 3, "Re: Plan");
    reply2.references = vec!["m1@test".into(), "m2@test".into()];
    let r2 = s.put_message(reply2, b"").unwrap();

    let t = s.get_envelope(orig).unwrap().unwrap().thread.unwrap();
    assert_eq!(s.get_envelope(r).unwrap().unwrap().thread, Some(t));
    assert_eq!(s.get_envelope(r2).unwrap().unwrap().thread, Some(t));
    let th = s.get_thread(t).unwrap().unwrap();
    assert_eq!(th.message_count, 3);
    assert_eq!(th.unread_count, 3);
    assert_eq!(th.subject, "Plan");
    assert_eq!(s.thread_envelopes(t).unwrap().iter().map(|e| e.id).collect::<Vec<_>>(), vec![orig, r, r2]);
    s.set_flags(r, Flags { seen: true, ..Default::default() }).unwrap();
    assert_eq!(s.get_thread(t).unwrap().unwrap().unread_count, 2);

    // Agrupación por asunto solo con el recálculo completo.
    let mut loose = env(inbox, 4, "RE: Plan");
    loose.message_id = Some("suelto@test".into());
    let l = s.put_message(loose, b"").unwrap();
    assert_ne!(s.get_envelope(l).unwrap().unwrap().thread, Some(t));
    assert_eq!(s.rethread_account(AccountId(1)).unwrap(), 1);
    let t2 = s.get_envelope(orig).unwrap().unwrap().thread.unwrap();
    assert_eq!(s.thread_envelopes(t2).unwrap().len(), 4);
    assert_eq!(s.get_thread(t2).unwrap().unwrap().unread_count, 3);
    assert_eq!(s.stats().unwrap().threads, 1);

    s.delete_message(l).unwrap();
    assert_eq!(s.get_thread(t2).unwrap().unwrap().message_count, 3);
}

#[test]
fn search_with_filters() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path()).unwrap();
    let inbox = folder(&s, 1, "INBOX", FolderRole::Inbox);
    let sent = folder(&s, 1, "Enviados", FolderRole::Sent);
    let mut batch = Vec::new();
    for n in 0..200i64 {
        let mut e =
            env(if n % 2 == 0 { inbox } else { sent }, n, if n % 10 == 0 { "Presupuesto anual" } else { "Otro tema" });
        e.flags.seen = n % 4 == 0;
        e.has_attachments = n % 20 == 0;
        if e.has_attachments {
            e.attachments =
                vec![iuredesk_model::AttachmentMeta { filename: "cifras.xlsx".into(), ..Default::default() }];
        }
        batch.push(NewMessage {
            envelope: e,
            raw: None,
            body_text: Some(format!("texto número {n} con la palabra clave{}", n % 7)),
        });
    }
    s.put_messages(batch).unwrap();

    assert_eq!(s.search("presupuesto", 100).unwrap().len(), 20);
    let r = s.search("presupuesto in:inbox", 100).unwrap();
    assert_eq!(r.len(), 20); // n % 10 == 0 ⇒ par ⇒ inbox
    assert!(r.windows(2).all(|w| w[0].date >= w[1].date));
    assert_eq!(s.search("presupuesto in:enviados", 100).unwrap().len(), 0);
    assert_eq!(s.search("has:attachment", 100).unwrap().len(), 10);
    assert_eq!(s.search("adjunto:cifras", 100).unwrap().len(), 10);
    assert_eq!(s.search("clave3", 100).unwrap().len(), 29);
    let unread_inbox = s.search("in:inbox is:unread", 1000).unwrap();
    assert_eq!(unread_inbox.len(), 50);
    assert!(unread_inbox.iter().all(|e| !e.flags.seen && e.folder == inbox));
    assert_eq!(s.search("presupuesto is:unread", 3).unwrap().len(), 3);
    let after = (t0() + Duration::days(2)).format("%Y-%m-%d").to_string();
    assert_eq!(s.search(&format!("after:{after}"), 1000).unwrap().len(), 0);
    assert_eq!(s.search("before:2026-05-02 from:ana@example.com", 1000).unwrap().len(), 200);
    assert!(s.search("", 10).unwrap().is_empty());
    assert!(s.search("in:noexiste", 10).unwrap().is_empty());

    // Reindexar sin extractor: se pierde el cuerpo pero se conserva lo demás.
    assert_eq!(s.reindex_all(None).unwrap(), 200);
    assert_eq!(s.search("presupuesto", 100).unwrap().len(), 20);
    assert_eq!(s.search("clave3", 100).unwrap().len(), 0);
    let extract = |raw: &[u8]| Some(String::from_utf8_lossy(raw).into_owned());
    assert_eq!(s.reindex_all(Some(&extract)).unwrap(), 200);
    assert!(!s.search_needs_body_reindex().unwrap());

    // Borrar el índice en disco: al abrir se reconstruye desde redb.
    drop(s);
    std::fs::remove_dir_all(dir.path().join("search")).unwrap();
    let s = Store::open(dir.path()).unwrap();
    assert!(s.search_needs_body_reindex().unwrap());
    assert_eq!(s.search("presupuesto", 100).unwrap().len(), 20);
}

#[test]
fn concurrent_readers_and_writer() {
    let dir = tempfile::tempdir().unwrap();
    let s = std::sync::Arc::new(Store::open(dir.path()).unwrap());
    let inbox = folder(&s, 1, "INBOX", FolderRole::Inbox);
    let w = {
        let s = s.clone();
        std::thread::spawn(move || {
            for chunk in 0..10i64 {
                let batch = (0..50)
                    .map(|i| NewMessage { envelope: env(inbox, chunk * 50 + i, "x"), ..Default::default() })
                    .collect();
                s.put_messages(batch).unwrap();
            }
        })
    };
    let readers: Vec<_> = (0..3)
        .map(|_| {
            let s = s.clone();
            std::thread::spawn(move || {
                for _ in 0..50 {
                    let page = s.list_envelopes(inbox, 0, 100).unwrap();
                    assert!(page.windows(2).all(|w| w[0].date >= w[1].date));
                }
            })
        })
        .collect();
    w.join().unwrap();
    for r in readers {
        r.join().unwrap();
    }
    assert_eq!(s.list_ids(inbox).unwrap().len(), 500);
    assert_eq!(s.count(inbox).unwrap().total, 500);
}

#[test]
fn remote_id_lookups() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path()).unwrap();
    let a = folder(&s, 1, "INBOX", FolderRole::Inbox);
    let b = folder(&s, 1, "Archive", FolderRole::Archive);
    let x = s.put_message(env(a, 1, "uno"), b"x").unwrap();
    let y = s.put_message(env(b, 2, "dos"), b"y").unwrap();
    assert_eq!(s.remote_ids(a).unwrap(), vec![("uid-1".to_string(), x)]);
    assert_eq!(s.remote_ids(b).unwrap(), vec![("uid-2".to_string(), y)]);
    assert_eq!(s.message_by_remote(a, "uid-1").unwrap(), Some(x));
    assert_eq!(s.message_by_remote(a, "uid-2").unwrap(), None);
    s.move_message(x, b).unwrap();
    assert!(s.remote_ids(a).unwrap().is_empty());
    assert!(s.remote_ids(FolderId(u64::MAX)).unwrap().is_empty());
}
