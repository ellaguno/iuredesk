//! UX-1: borrar contactos por el camino local-primero: el borrado se encola para el servidor
//! (Graph/CardDAV), «Deshacer» cancela la operación si aún no salió y devuelve el contacto con su
//! objeto crudo; las libretas de solo lectura (Iurefficient) lo dicen y no se tocan.

use std::sync::Arc;

use iuredesk_accounts::MemoryStore;
use iuredesk_engine::{Module, OfflineTransport, Outcome, Session, SessionConfig, Zone};
use iuredesk_model::{AccountId, AddressBook, Contact, Labeled};
use iuredesk_pim_sync::ops::PimOp;
use iuredesk_store::{PimRef, Store};

fn session(dir: &std::path::Path) -> Session {
    iuredesk_engine::lang::set_lang(iuredesk_engine::lang::Lang::Es);
    let store = Store::open(dir).unwrap();
    let mut s = Session::new(SessionConfig {
        store: Arc::new(store),
        accounts: None,
        secrets: Arc::new(MemoryStore::default()),
        prefs_path: None,
        now: None,
        zone: Zone::utc(),
        sample: false,
        sync: None,
        transport: Some(Arc::new(OfflineTransport)),
    })
    .unwrap();
    s.module = Module::Contacts;
    s
}

fn contact(book: iuredesk_model::AddressBookId, name: &str, href: &str) -> Contact {
    Contact {
        address_book: book,
        uid: format!("uid-{name}"),
        full_name: name.into(),
        emails: vec![Labeled::new("", format!("{}@contoso.com", name.to_lowercase()))],
        remote_id: Some(href.into()),
        etag: Some("W/\"1\"".into()),
        ..Default::default()
    }
}

fn delete_ops(s: &Session) -> Vec<String> {
    s.store()
        .peek_ops(1_000)
        .unwrap()
        .into_iter()
        .filter_map(|o| match PimOp::decode(&o.kind, &o.payload) {
            Some(PimOp::DeleteContact { href, .. }) => Some(href),
            _ => None,
        })
        .collect()
}

#[test]
fn delete_enqueues_server_delete_and_undo_cancels_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(dir.path());
    let st = s.store().clone();
    let book = st
        .put_address_book(&AddressBook {
            account: AccountId(7),
            name: "Contactos".into(),
            remote_path: "contacts".into(),
            ..Default::default()
        })
        .unwrap();
    let a = st.put_contact(&contact(book, "Ana", "AAMk-ana")).unwrap();
    let b = st.put_contact(&contact(book, "Beto", "AAMk-beto")).unwrap();
    st.put_raw_object(PimRef::Contact(a), br#"{"id":"AAMk-ana"}"#, "AAMk-ana", Some("W/\"1\"")).unwrap();
    st.put_raw_object(PimRef::Contact(b), br#"{"id":"AAMk-beto"}"#, "AAMk-beto", Some("W/\"1\"")).unwrap();
    assert_eq!(s.contacts().len(), 2);

    // Dos seleccionados: Supr («mail.delete» en Contactos) pide confirmación.
    s.set_contact_selection(vec![a, b]);
    assert!(s.is_enabled("mail.delete"));
    let out = s.run("mail.delete").unwrap();
    let Outcome::Confirm { command, yes, .. } = out else { panic!("se esperaba confirmación: {out:?}") };
    assert_eq!(yes.es, "Eliminar 2");
    assert_eq!(s.contacts().len(), 2, "sin confirmar no se borra");

    // Confirmado: se borran en local y el borrado queda en la cola para el servidor.
    let out = s.run(&command).unwrap();
    let Outcome::Notice(n) = out else { panic!("{out:?}") };
    assert!(n.undo && n.text.es.contains("Se eliminaron 2"), "{:?}", n.text);
    assert!(s.contacts().is_empty());
    let mut ops = delete_ops(&s);
    ops.sort();
    assert_eq!(ops, vec!["AAMk-ana".to_string(), "AAMk-beto".to_string()]);

    // Deshacer: se cancela la cola y vuelven con su objeto crudo (sin subirlos de nuevo).
    s.run("undo").unwrap();
    assert_eq!(s.contacts().len(), 2);
    assert!(delete_ops(&s).is_empty(), "el borrado pendiente se canceló");
    assert!(st.raw_object(PimRef::Contact(a)).unwrap().is_some());
}

#[test]
fn read_only_books_say_so() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(dir.path());
    let st = s.store().clone();
    let book = st
        .put_address_book(&AddressBook {
            account: AccountId(9),
            name: "Iurefficient".into(),
            read_only: true,
            ..Default::default()
        })
        .unwrap();
    let id = st.put_contact(&contact(book, "Cliente", "iure-1")).unwrap();
    assert!(!s.contact_deletable(id));
    s.set_contact_selection(vec![id]);
    let Outcome::Notice(n) = s.run("contacts.delete").unwrap() else { panic!() };
    assert!(!n.undo && n.text.es.contains("solo lectura"), "{:?}", n.text);
    assert_eq!(s.contacts().len(), 1);
    // Sin selección no hace nada (y lo dice).
    s.set_contact_selection(Vec::new());
    let Outcome::Notice(n) = s.run("contacts.delete").unwrap() else { panic!() };
    assert!(n.text.es.contains("selecciona un contacto"));
}

fn put_ops(s: &Session) -> Vec<iuredesk_model::ContactId> {
    s.store()
        .peek_ops(1_000)
        .unwrap()
        .into_iter()
        .filter_map(|o| match PimOp::decode(&o.kind, &o.payload) {
            Some(PimOp::PutContact { contact }) => Some(contact),
            _ => None,
        })
        .collect()
}

#[test]
fn new_contact_is_saved_queued_and_undone() {
    // UX-10 (dueño 2026-11-03: «no pude editar contactos»).
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(dir.path());
    let st = s.store().clone();
    let book = st
        .put_address_book(&AddressBook { account: AccountId(7), name: "Contactos".into(), ..Default::default() })
        .unwrap();
    // «Nuevo» en Contactos abre el editor (lo atiende la UI), no el redactor.
    assert!(matches!(s.run("mail.new").unwrap(), Outcome::Assist(a) if a == "contacts.new"));

    let draft = Contact {
        address_book: book,
        given_name: " Ana ".into(),
        family_name: "Ruiz".into(),
        emails: vec![Labeled::new("work", "ana@contoso.com"), Labeled::new("home", "")],
        ..Default::default()
    };
    let Outcome::Notice(n) = s.save_contact_edit(draft).unwrap() else { panic!() };
    assert!(n.undo && n.text.es.contains("Se agregó «Ana Ruiz»"), "{:?}", n.text);
    let c = s.contacts().first().cloned().unwrap();
    assert_eq!((c.full_name.as_str(), c.emails.len()), ("Ana Ruiz", 1));
    assert!(!c.uid.is_empty());
    assert_eq!(s.contact_selection(), &[c.id], "el nuevo queda elegido");
    assert_eq!(put_ops(&s), vec![c.id], "la subida quedó en la cola");

    s.run("undo").unwrap();
    assert!(s.contacts().is_empty());
}

#[test]
fn editing_keeps_identity_and_undo_restores() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(dir.path());
    let st = s.store().clone();
    let book = st
        .put_address_book(&AddressBook { account: AccountId(7), name: "Contactos".into(), ..Default::default() })
        .unwrap();
    let id = st.put_contact(&contact(book, "Ana", "card-ana.vcf")).unwrap();
    let mut edited = s.contact(id).unwrap();
    edited.phones.push(Labeled::new("mobile", "+52 55 1234 5678"));
    edited.org = "Contoso".into();
    // La UI no puede cambiar la identidad remota ni la libreta al editar.
    edited.uid = "otro".into();
    edited.remote_id = None;
    let Outcome::Notice(n) = s.save_contact_edit(edited).unwrap() else { panic!() };
    assert!(n.undo && n.text.es.contains("Se guardó «Ana»"), "{:?}", n.text);
    let now = s.contact(id).unwrap();
    assert_eq!(now.org, "Contoso");
    assert_eq!(now.uid, "uid-Ana");
    assert_eq!(now.remote_id.as_deref(), Some("card-ana.vcf"));
    assert_eq!(put_ops(&s), vec![id]);

    // Guardar igual no crea nada que deshacer.
    let Outcome::Notice(n) = s.save_contact_edit(now.clone()).unwrap() else { panic!() };
    assert!(!n.undo && n.text.es.contains("Sin cambios"));

    s.run("undo").unwrap();
    let back = s.contact(id).unwrap();
    assert_eq!(back.org, "");
    assert!(back.phones.is_empty());

    // Validación: un correo que no lo es deja el contacto como estaba.
    let mut bad = back.clone();
    bad.emails = vec![Labeled::new("", "ana arroba contoso")];
    let Outcome::Notice(n) = s.save_contact_edit(bad).unwrap() else { panic!() };
    assert_eq!(n.kind, iuredesk_engine::NoticeKind::Error);
    assert_eq!(s.contact(id).unwrap().emails, back.emails);
}

#[test]
fn read_only_contacts_cannot_be_edited() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(dir.path());
    let st = s.store().clone();
    let ro = st
        .put_address_book(&AddressBook {
            account: AccountId(9),
            name: "Iurefficient".into(),
            read_only: true,
            ..Default::default()
        })
        .unwrap();
    let id = st.put_contact(&contact(ro, "Cliente", "iure-1")).unwrap();
    let mut edited = s.contact(id).unwrap();
    edited.org = "X".into();
    let Outcome::Notice(n) = s.save_contact_edit(edited).unwrap() else { panic!() };
    assert!(n.text.es.contains("solo lectura"), "{:?}", n.text);
    assert_eq!(s.contact(id).unwrap().org, "");
    // Uno nuevo sin libreta en la que se pueda escribir va a una local «Contactos».
    let draft = Contact { address_book: ro, full_name: "Nuevo".into(), ..Default::default() };
    let Outcome::Notice(n) = s.save_contact_edit(draft).unwrap() else { panic!() };
    assert!(n.undo, "{:?}", n.text);
    let books = s.writable_address_books();
    assert_eq!(books.len(), 1);
    assert!(s.contacts().iter().any(|c| c.full_name == "Nuevo" && c.address_book == books[0].id));
}
