//! Pruebas de extremo a extremo con datos sintéticos (nada personal): perfiles de Thunderbird,
//! Apple Mail, Gmail Takeout, Maildir, `.eml`, `.ics`, `.vcf` y PST generado al vuelo.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use iuredesk_import::{ImportEvent, ImportOptions, ImportReport, Importer, Notice, SourceKind, detect};
use iuredesk_model::{FolderRole, MessageEnvelope};
use iuredesk_store::Store;

fn msg(id: &str, subject: &str, body: &str) -> String {
    format!(
        "From: Ana Ejemplo <ana@example.com>\nTo: Luis <luis@example.org>\nSubject: {subject}\nDate: Mon, 6 Jan 2025 10:00:00 +0000\nMessage-ID: <{id}@example.com>\n\n{body}\n"
    )
}

fn run(store: &Store, paths: &[PathBuf]) -> ImportReport {
    let opts = ImportOptions { batch_messages: 3, ..Default::default() };
    let mut events = 0usize;
    let r = Importer::new(store).options(opts).run(paths, |_| events += 1).unwrap();
    assert!(events > 0);
    r
}

/// Carpetas de una cuenta por nombre de cuenta: (ruta, función, mensajes).
fn folders(store: &Store, account: &str) -> Vec<(String, FolderRole, Vec<MessageEnvelope>)> {
    let acct = store
        .account_labels()
        .unwrap()
        .into_iter()
        .find(|a| a.name == account)
        .unwrap_or_else(|| panic!("no está la cuenta {account}: {:?}", store.account_labels().unwrap()));
    store
        .folders(acct.id)
        .unwrap()
        .into_iter()
        .map(|f| {
            let msgs = store.list_envelopes(f.id, 0, 1000).unwrap();
            (f.remote_path, f.role, msgs)
        })
        .collect()
}

fn folder<'a>(
    fs: &'a [(String, FolderRole, Vec<MessageEnvelope>)],
    path: &str,
) -> &'a (String, FolderRole, Vec<MessageEnvelope>) {
    fs.iter()
        .find(|f| f.0 == path)
        .unwrap_or_else(|| panic!("no está {path}: {:?}", fs.iter().map(|f| &f.0).collect::<Vec<_>>()))
}

fn write(p: &Path, data: impl AsRef<[u8]>) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, data).unwrap();
}

#[test]
fn thunderbird_profile_mbox_tree_flags_books_and_resume() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("thunderbird");
    let prof = root.join("x1.default-release");
    write(
        &root.join("profiles.ini"),
        "[Profile0]\nName=default-release\nIsRelative=1\nPath=x1.default-release\nDefault=1\n",
    );
    write(
        &prof.join("prefs.js"),
        "user_pref(\"mail.server.server1.directory-rel\", \"[ProfD]Mail/Local Folders\");\nuser_pref(\"mail.server.server1.name\", \"Carpetas locales\");\nuser_pref(\"mail.server.server1.type\", \"none\");\nuser_pref(\"ldap_2.servers.pab.filename\", \"abook.sqlite\");\nuser_pref(\"ldap_2.servers.pab.description\", \"Personal\");\n",
    );
    let lf = prof.join("Mail/Local Folders");
    // Bandeja (CRLF), con: leído+destacado, expurgado, respondido y un From sin escapar.
    let inbox = format!(
        "From - Mon Jan 06 10:00:00 2025\r\nX-Mozilla-Status: 0005\r\nX-Mozilla-Keys: $label1\r\n{}\r\nFrom - Mon Jan 06 11:00:00 2025\r\nX-Mozilla-Status: 0009\r\n{}\r\nFrom - Mon Jan 06 12:00:00 2025\r\nX-Mozilla-Status: 0002\r\n{}",
        msg("tb1", "Uno", "Hola\r\n\r\nFrom here on, ok").replace('\n', "\r\n"),
        msg("tb2", "Borrado", "x").replace('\n', "\r\n"),
        msg("tb3", "Tres", "y").replace('\n', "\r\n"),
    );
    write(&lf.join("Inbox"), inbox);
    write(&lf.join("Inbox.msf"), "// índice mork");
    write(&lf.join("Enviados"), format!("From - Tue Jan 07 09:00:00 2025\n{}", msg("tb4", "Enviado", "z")));
    write(&lf.join("Proyectos"), "");
    write(&lf.join("Proyectos.msf"), "");
    write(
        &lf.join("Proyectos.sbd/ACME"),
        format!("From - Tue Jan 07 09:00:00 2025\n{}", msg("tb5", "ACME", ">From escapado")),
    );
    write(&lf.join("msgFilterRules.dat"), "version=\"9\"");
    std::fs::copy(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/abook.sqlite"), prof.join("abook.sqlite"))
        .unwrap();
    write(&prof.join("calendar-data/local.sqlite"), "x");

    assert_eq!(detect(&root), Some(SourceKind::Thunderbird));
    let store_dir = dir.path().join("store");
    let store = Store::open(&store_dir).unwrap();
    let r = run(&store, std::slice::from_ref(&root));
    assert_eq!(r.counts.messages, 4, "{r:?}");
    assert_eq!(r.skipped_by_reason.get("deleted_in_source"), Some(&1));
    assert_eq!(r.counts.contacts, 121);
    assert!(r.notices.iter().any(|n| matches!(n, Notice::ThunderbirdCalendarNeedsExport { .. })));

    let fs = folders(&store, "Thunderbird · Carpetas locales");
    let inbox = folder(&fs, "Inbox");
    assert_eq!(inbox.1, FolderRole::Inbox);
    assert_eq!(inbox.2.len(), 2);
    let uno = inbox.2.iter().find(|m| m.subject == "Uno").unwrap();
    assert!(uno.flags.seen && uno.flags.flagged);
    assert_eq!(uno.keywords, vec!["$label1".to_string()]);
    let tres = inbox.2.iter().find(|m| m.subject == "Tres").unwrap();
    assert!(tres.flags.answered && !tres.flags.seen);
    let raw = store.get_raw(uno.id).unwrap();
    assert!(String::from_utf8_lossy(&raw).contains("From here on, ok"));
    assert_eq!(folder(&fs, "Enviados").1, FolderRole::Sent);
    let acme = folder(&fs, "Proyectos/ACME");
    assert_eq!(acme.1, FolderRole::Custom);
    assert!(String::from_utf8_lossy(&store.get_raw(acme.2[0].id).unwrap()).contains("\nFrom escapado"));
    assert_eq!(folder(&fs, "Proyectos").2.len(), 0);

    let books = store.address_books().unwrap();
    let pab = books.iter().find(|b| b.name == "Personal").unwrap();
    let contacts = store.contacts(pab.id).unwrap();
    let ana = contacts.iter().find(|c| c.uid == "card-vcard").unwrap();
    assert_eq!(ana.full_name, "Ana Sintética");
    assert_eq!(ana.org, "Ejemplo S.A.");
    let p7 = contacts.iter().find(|c| c.uid == "card-007").unwrap();
    assert_eq!(p7.primary_email(), Some("persona7@example.com"));

    // Segunda pasada: reanuda (todo hecho) y no duplica.
    let r2 = run(&store, std::slice::from_ref(&root));
    assert!(r2.resumed);
    assert_eq!(r2.counts.messages, 0);
    // Sin reanudación: relee todo pero la deduplicación lo detecta.
    let opts = ImportOptions { resume: false, ..Default::default() };
    let r3 = Importer::new(&store).options(opts).run(&[root], |_| {}).unwrap();
    assert_eq!(r3.counts.messages, 0);
    assert_eq!(r3.counts.duplicates, 4);
    assert_eq!(folder(&folders(&store, "Thunderbird · Carpetas locales"), "Inbox").2.len(), 2);
    assert_eq!(store.contacts(pab.id).unwrap().len(), 121, "contactos actualizados por UID, no duplicados");
}

#[test]
fn gmail_takeout_labels() {
    let dir = tempfile::tempdir().unwrap();
    let mail = dir.path().join("Takeout/Mail");
    let mut mbox = String::new();
    for (i, labels) in
        ["Inbox,Unread,Important", "Sent", "\"Clientes/ACME\",Opened,Starred", "Archived,Category Updates", "Spam"]
            .iter()
            .enumerate()
    {
        mbox.push_str(&format!(
            "From 1{i}@xxx Mon Jan 06 10:00:00 +0000 2025\nX-GM-THRID: 1\nX-Gmail-Labels: {labels}\n"
        ));
        mbox.push_str(&msg(&format!("g{i}"), &format!("G{i}"), "cuerpo"));
        mbox.push('\n');
    }
    write(&mail.join("Todo el correo, incluido Spam y Papelera.mbox"), mbox);
    write(
        &dir.path().join("Takeout/Calendar/Trabajo.ics"),
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:t1\r\nDTSTART:20250110T100000Z\r\nDTEND:20250110T110000Z\r\nSUMMARY:Junta\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
    );
    let store = Store::open(dir.path().join("store")).unwrap();
    let r = run(&store, &[dir.path().join("Takeout")]);
    assert_eq!(r.counts.messages, 5);
    assert_eq!(r.counts.events, 1);
    let fs = folders(&store, "Gmail (Takeout)");
    let inbox = folder(&fs, "Bandeja de entrada");
    assert_eq!(inbox.1, FolderRole::Inbox);
    assert!(!inbox.2[0].flags.seen);
    assert_eq!(folder(&fs, "Enviados").1, FolderRole::Sent);
    let acme = folder(&fs, "Clientes/ACME");
    assert!(acme.2[0].flags.flagged && acme.2[0].flags.seen);
    assert_eq!(folder(&fs, "Archivo").1, FolderRole::Archive);
    assert_eq!(folder(&fs, "Correo no deseado").1, FolderRole::Junk);
    // El archivo en sí no crea carpeta vacía en la cuenta genérica.
    assert!(
        store
            .account_labels()
            .unwrap()
            .iter()
            .all(|a| a.name != "Importado"
                || folders(&store, "Importado").iter().all(|f| !f.0.contains("Todo el correo")))
    );
    assert!(store.calendars().unwrap().iter().any(|c| c.name == "Trabajo"));
}

#[test]
fn maildir_plus_plus_and_eml_folder() {
    let dir = tempfile::tempdir().unwrap();
    let md = dir.path().join("Maildir");
    for sub in ["cur", "new", "tmp", ".Enviados/cur", ".Enviados/new", ".Clientes.ACME/cur", ".Clientes.ACME/new"] {
        std::fs::create_dir_all(md.join(sub)).unwrap();
    }
    // En Windows «:» no se permite en nombres de archivo (NTFS lo toma como flujo alterno): los
    // Maildir de Windows usan «!2,», que el importador también entiende.
    let sep = if cfg!(windows) { "!2," } else { ":2," };
    write(&md.join(format!("cur/1700000000.M1.host{sep}FS")), msg("m1", "Leído", "a"));
    write(&md.join("new/1700000001.M2.host"), msg("m2", "Nuevo", "b"));
    write(&md.join(format!(".Enviados/cur/1700000002.M3.host{sep}S")), msg("m3", "Env", "c"));
    write(&md.join(format!(".Clientes.ACME/cur/1700000003.M4.host{sep}RS")), msg("m4", "Cli", "d"));
    let emls = dir.path().join("Exportados");
    write(&emls.join("a.eml"), msg("e1", "Eml 1", "x"));
    write(&emls.join("b.eml"), msg("e1", "Eml 1", "x").replace('\n', "\r\n"));
    write(&emls.join("vacio.eml"), "");
    write(&emls.join("basura.eml"), "esto no es un correo");
    let store = Store::open(dir.path().join("store")).unwrap();
    let r = run(&store, &[md, emls]);
    assert_eq!(r.counts.messages, 5, "{r:?}");
    assert_eq!(r.counts.duplicates, 1, "mismo correo con CRLF");
    assert_eq!(r.skipped_by_reason.get("empty"), Some(&1));
    assert_eq!(r.skipped_by_reason.get("malformed"), Some(&1));
    let fs = folders(&store, "Importado");
    let inbox = folder(&fs, "Bandeja de entrada");
    assert_eq!(inbox.1, FolderRole::Inbox);
    let leido = inbox.2.iter().find(|m| m.subject == "Leído").unwrap();
    assert!(leido.flags.seen && leido.flags.flagged);
    assert!(!inbox.2.iter().find(|m| m.subject == "Nuevo").unwrap().flags.seen);
    assert_eq!(folder(&fs, "Enviados").1, FolderRole::Sent);
    assert!(folder(&fs, "Clientes/ACME").2[0].flags.answered);
    assert_eq!(folder(&fs, "Exportados").2.len(), 1);
}

fn emlx(msg: &str, flags: u64) -> Vec<u8> {
    let mut v = format!("{}\n", msg.len()).into_bytes();
    v.extend_from_slice(msg.as_bytes());
    v.extend_from_slice(format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\">\n<dict>\n\t<key>date-received</key>\n\t<integer>1736157600</integer>\n\t<key>flags</key>\n\t<integer>{flags}</integer>\n</dict>\n</plist>\n").as_bytes());
    v
}

#[test]
fn apple_mail_emlx_partial_and_nested() {
    let dir = tempfile::tempdir().unwrap();
    let lib = dir.path().join("Mail");
    std::fs::create_dir_all(lib.join("V10/MailData")).unwrap();
    std::fs::create_dir_all(lib.join("V9/MailData")).unwrap();
    let acct = lib.join("V10/2F3A-UUID");
    let inbox_msgs = acct.join("INBOX.mbox/1111-2222/Data/1/Messages");
    write(
        &inbox_msgs.join("1.emlx"),
        emlx(
            &msg("a1", "Leído y destacado", "x").replace("To: Luis <luis@example.org>", "To: ana@example.com"),
            1 | (1 << 4),
        ),
    );
    let partial = "From: a@example.com\r\nSubject: Con adjunto\r\nMessage-ID: <a2@example.com>\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"b\"\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nver adjunto\r\n--b\r\nContent-Type: application/pdf; name=f.pdf\r\nContent-Disposition: attachment; filename=f.pdf\r\nContent-Transfer-Encoding: base64\r\nX-Apple-Content-Length: 9\r\n\r\n\r\n--b--\r\n";
    write(&inbox_msgs.join("2.partial.emlx"), emlx(partial, 0));
    write(&acct.join("INBOX.mbox/1111-2222/Data/1/Attachments/2/2/f.pdf"), b"%PDF-demo");
    write(&acct.join("Sent Messages.mbox/Messages/3.emlx"), emlx(&msg("a3", "Enviado", "y"), 1));
    write(&acct.join("Proyectos.mbox/ACME.mbox/Messages/4.emlx"), emlx(&msg("a4", "Hijo", "z"), 0));
    // Buzón exportado («Exportar buzón»): mbox dentro del paquete.
    write(&acct.join("Exportado.mbox/mbox"), format!("From a@x Mon Jan 06 10:00:00 2025\n{}", msg("a5", "Exp", "w")));
    // Copia vieja en V9: no se importa.
    write(&lib.join("V9/OLD/INBOX.mbox/Messages/9.emlx"), emlx(&msg("old", "Viejo", "v"), 0));

    assert_eq!(detect(&lib), Some(SourceKind::AppleMail));
    let store = Store::open(dir.path().join("store")).unwrap();
    let r = run(&store, &[lib]);
    assert_eq!(r.counts.messages, 5, "{r:?}");
    let fs = folders(&store, "Apple Mail · ana@example.com");
    let inbox = folder(&fs, "INBOX");
    assert_eq!(inbox.1, FolderRole::Inbox);
    let first = inbox.2.iter().find(|m| m.message_id.as_deref() == Some("a1@example.com")).unwrap();
    assert!(first.flags.seen && first.flags.flagged);
    let with = inbox.2.iter().find(|m| m.subject == "Con adjunto").unwrap();
    assert!(with.has_attachments);
    let raw = store.get_raw(with.id).unwrap();
    let parsed = mail_parser::MessageParser::default().parse(&raw).unwrap();
    assert_eq!(parsed.attachments().next().unwrap().contents(), b"%PDF-demo");
    assert_eq!(folder(&fs, "Sent Messages").1, FolderRole::Sent);
    assert_eq!(folder(&fs, "Proyectos/ACME").2.len(), 1);
    assert_eq!(folder(&fs, "Exportado").2.len(), 1);
}

#[test]
fn calendars_contacts_tasks_notes() {
    let dir = tempfile::tempdir().unwrap();
    let ics = dir.path().join("agenda.ics");
    write(
        &ics,
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Prueba//ES\r\nX-WR-CALNAME:Personal\r\nBEGIN:VTIMEZONE\r\nTZID:Europe/Madrid\r\nBEGIN:STANDARD\r\nDTSTART:19701025T030000\r\nTZOFFSETFROM:+0200\r\nTZOFFSETTO:+0100\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\nBEGIN:VEVENT\r\nUID:serie-1\r\nDTSTART;TZID=Europe/Madrid:20250106T090000\r\nDTEND;TZID=Europe/Madrid:20250106T100000\r\nRRULE:FREQ=WEEKLY;COUNT=5\r\nSUMMARY:Semanal\r\nBEGIN:VALARM\r\nTRIGGER:-PT15M\r\nACTION:DISPLAY\r\nEND:VALARM\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:serie-1\r\nRECURRENCE-ID;TZID=Europe/Madrid:20250113T090000\r\nDTSTART;TZID=Europe/Madrid:20250113T120000\r\nDTEND;TZID=Europe/Madrid:20250113T130000\r\nSUMMARY:Semanal (movida)\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:dia-1\r\nDTSTART;VALUE=DATE:20250201\r\nDTEND;VALUE=DATE:20250202\r\nSUMMARY:Feriado\r\nEND:VEVENT\r\nBEGIN:VTODO\r\nUID:todo-1\r\nSUMMARY:Pagar\r\nDUE;VALUE=DATE:20250115\r\nSTATUS:NEEDS-ACTION\r\nEND:VTODO\r\nBEGIN:VJOURNAL\r\nUID:j-1\r\nDTSTART:20250105T100000Z\r\nSUMMARY:Diario\r\nDESCRIPTION:Notas del día\r\nEND:VJOURNAL\r\nBEGIN:VEVENT\r\nUID:roto\r\nEND:VCALENDAR\r\n",
    );
    let vcf = dir.path().join("Contactos de prueba.vcf");
    write(
        &vcf,
        "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:c1\r\nFN:Ana Ejemplo\r\nN:Ejemplo;Ana;;;\r\nEMAIL;TYPE=work:ana@example.com\r\nEND:VCARD\r\nBEGIN:VCARD\r\nVERSION:2.1\r\nN;ENCODING=QUOTED-PRINTABLE;CHARSET=UTF-8:Pe=C3=B1a;Jos=C3=A9\r\nFN;ENCODING=QUOTED-PRINTABLE;CHARSET=UTF-8:Jos=C3=A9 Pe=C3=B1a\r\nTEL;CELL:+34600000000\r\nEND:VCARD\r\nBEGIN:VCARD\r\nVERSION:4.0\r\nFN:Sin UID\r\nEMAIL:sinuid@example.com\r\nEND:VCARD\r\n",
    );
    let store = Store::open(dir.path().join("store")).unwrap();
    let r = run(&store, &[ics.clone(), vcf.clone()]);
    assert_eq!((r.counts.events, r.counts.tasks, r.counts.notes, r.counts.contacts), (2, 1, 1, 3), "{r:?}");
    let cal = store.calendars().unwrap().into_iter().find(|c| c.name == "Personal").unwrap();
    let evs = store.events_in_calendar(cal.id).unwrap();
    let serie = evs.iter().find(|e| e.uid == "serie-1").unwrap();
    assert_eq!(serie.summary, "Semanal");
    assert_eq!(serie.rrule.as_deref(), Some("FREQ=WEEKLY;COUNT=5"));
    assert_eq!(serie.reminders.len(), 1);
    // La excepción vive en el objeto crudo: la expansión la respeta.
    let occ = iuredesk_pim_sync::occurrences_between(
        &store,
        &[cal.id],
        "2025-01-01T00:00:00Z".parse().unwrap(),
        "2025-03-01T00:00:00Z".parse().unwrap(),
        chrono_tz::UTC,
    )
    .unwrap();
    assert!(occ.iter().any(|o| o.summary == "Semanal (movida)"), "{occ:?}");
    assert_eq!(occ.iter().filter(|o| o.uid == "serie-1").count(), 5);
    let book = store.address_books().unwrap().into_iter().find(|b| b.name == "Contactos de prueba").unwrap();
    let cs = store.contacts(book.id).unwrap();
    assert!(cs.iter().any(|c| c.full_name == "José Peña" && c.given_name == "José"));
    assert!(cs.iter().any(|c| c.full_name == "Sin UID" && c.uid.starts_with("import-")));
    let notes = store.notes().unwrap();
    assert_eq!(notes[0].title, "Diario");
    // Reimportar: actualiza por UID, no duplica.
    let opts = ImportOptions { resume: false, ..Default::default() };
    Importer::new(&store).options(opts).run(&[ics, vcf], |_| {}).unwrap();
    assert_eq!(store.events_in_calendar(cal.id).unwrap().len(), 2);
    assert_eq!(store.contacts(book.id).unwrap().len(), 3);
    assert_eq!(store.notes().unwrap().len(), 1);
}

#[test]
fn outlook_pst_generated() {
    use pst_writer::{
        UnicodePstAttachment, UnicodePstFile, UnicodePstMessage, UnicodePstRecipient, UnicodePstRecipientType,
    };
    let dir = tempfile::tempdir().unwrap();
    let pst = dir.path().join("archivo.pst");
    let recipients = [
        UnicodePstRecipient { name: "Luis", email: "luis@example.org", recipient_type: UnicodePstRecipientType::To },
        UnicodePstRecipient { name: "Copia", email: "cc@example.org", recipient_type: UnicodePstRecipientType::Cc },
    ];
    let m = UnicodePstMessage {
        subject: "Presupuesto 2025",
        sender_name: "Ana Ejemplo",
        sender_email: "ana@example.com",
        recipients: &recipients,
        body: "Va el presupuesto, señor.",
        html_body: Some("<p>Va el <b>presupuesto</b>, señor.</p>"),
        message_id: "<pst-1@example.com>",
        delivery_time: 133_800_000_000_000_000,
    };
    let att =
        UnicodePstAttachment { filename: "datos.csv", mime_type: "text/csv", content_id: None, data: b"a,b\n1,2\n" };
    drop(UnicodePstFile::create_with_attachments(&pst, &m, &[att]).unwrap());
    drop(
        UnicodePstFile::append_in_folder(
            &pst,
            &["Clientes", "ACME"],
            &UnicodePstMessage { subject: "Pedido ACME", message_id: "<pst-2@example.com>", ..m },
        )
        .unwrap(),
    );
    assert_eq!(detect(&pst), Some(SourceKind::OutlookPst));
    let store = Store::open(dir.path().join("store")).unwrap();
    let r = run(&store, std::slice::from_ref(&pst));
    assert_eq!(r.counts.messages, 2, "{r:?}");
    let acct = store.account_labels().unwrap().into_iter().find(|a| a.name.starts_with("Outlook")).unwrap();
    let all: Vec<_> = store
        .folders(acct.id)
        .unwrap()
        .into_iter()
        .flat_map(|f| {
            store.list_envelopes(f.id, 0, 100).unwrap().into_iter().map(move |e| (f.remote_path.clone(), f.role, e))
        })
        .collect();
    let (path, role, first) = all.iter().find(|(_, _, e)| e.subject == "Presupuesto 2025").unwrap();
    assert_eq!(*role, FolderRole::Inbox, "{path}");
    assert_eq!(first.from[0].addr, "ana@example.com");
    assert_eq!(first.to[0].addr, "luis@example.org");
    assert_eq!(first.cc[0].addr, "cc@example.org");
    assert_eq!(first.message_id.as_deref(), Some("pst-1@example.com"));
    assert!(first.attachments.iter().any(|a| a.filename == "datos.csv"));
    let raw = store.get_raw(first.id).unwrap();
    let parsed = mail_parser::MessageParser::default().parse(&raw).unwrap();
    assert!(parsed.body_html(0).unwrap().contains("<b>presupuesto</b>"));
    assert!(parsed.body_text(0).unwrap().contains("señor"));
    let (path, _, _) = all.iter().find(|(_, _, e)| e.subject == "Pedido ACME").unwrap();
    assert!(path.ends_with("Clientes/ACME"), "{path}");
    // Reimportar sin reanudación: nada nuevo.
    let opts = ImportOptions { resume: false, ..Default::default() };
    let r2 = Importer::new(&store).options(opts).run(&[pst], |_| {}).unwrap();
    assert_eq!((r2.counts.messages, r2.counts.duplicates), (0, 2));
}

#[test]
fn damaged_pst_and_hostile_mbox() {
    let dir = tempfile::tempdir().unwrap();
    let pst = dir.path().join("roto.pst");
    write(&pst, b"!BDN\0\0basura que no es un PST".repeat(50));
    let ost = dir.path().join("cache.ost");
    write(&ost, b"no es ost");
    let mbox = dir.path().join("hostil.mbox");
    let mut data = b"From x Mon Jan 06 10:00:00 2025\nSubject: grande\n\n".to_vec();
    data.extend(std::iter::repeat_n(b'A', 3 * 1024 * 1024));
    data.extend_from_slice(format!("\n\nFrom y Mon Jan 06 10:00:00 2025\n{}", msg("ok", "Chico", "ok")).as_bytes());
    write(&mbox, &data);
    let store = Store::open(dir.path().join("store")).unwrap();
    let opts = ImportOptions { max_message_bytes: 1024 * 1024, ..Default::default() };
    let r = Importer::new(&store).options(opts).run(&[pst, ost, mbox], |_| {}).unwrap();
    assert_eq!(r.counts.messages, 1);
    assert_eq!(r.skipped_by_reason.get("too_large"), Some(&1));
    assert!(r.skipped_by_reason.contains_key("malformed") || r.skipped_by_reason.contains_key("damaged"));
    assert!(r.notices.iter().any(|n| matches!(n, Notice::OstUnsupported { .. })));
    // Un origen inexistente sí es error.
    assert!(Importer::new(&store).run(&[dir.path().join("no-existe")], |_| {}).is_err());
}

#[test]
fn cancel_then_resume_mid_mbox() {
    let dir = tempfile::tempdir().unwrap();
    let mbox = dir.path().join("grande.mbox");
    let mut data = String::new();
    for i in 0..50 {
        data.push_str(&format!("From x Mon Jan 06 10:00:00 2025\n{}\n", msg(&format!("c{i}"), &format!("M{i}"), "b")));
    }
    write(&mbox, data);
    let store = Store::open(dir.path().join("store")).unwrap();
    let flag = Arc::new(AtomicBool::new(false));
    let f2 = flag.clone();
    let mut seen = 0u64;
    let opts = ImportOptions { batch_messages: 10, ..Default::default() };
    let r = Importer::new(&store)
        .options(opts.clone())
        .cancel_flag(flag.clone())
        .run(std::slice::from_ref(&mbox), |ev| {
            if let ImportEvent::Progress(p) = ev {
                seen = p.counts.messages;
                if seen >= 20 {
                    f2.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }
        })
        .unwrap();
    assert!(r.cancelled);
    let first = r.counts.messages;
    assert!((20..50).contains(&first), "{first}");
    flag.store(false, std::sync::atomic::Ordering::Relaxed);
    let r2 = Importer::new(&store).options(opts).cancel_flag(flag).run(&[mbox], |_| {}).unwrap();
    assert!(r2.resumed && !r2.cancelled);
    assert_eq!(r2.counts.messages + first, 50, "{r2:?}");
    assert_eq!(r2.counts.duplicates, 0, "reanuda tras el último lote confirmado");
    assert_eq!(store.stats().unwrap().messages, 50);
}
