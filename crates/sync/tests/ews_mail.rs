//! Correo de Exchange Server local por EWS contra un servidor falso con estado (F8.2): primera vez
//! (lo reciente con `FindItem`, recorrido de ids por páginas hasta un `SyncState`, lo que llegó
//! mientras tanto), historial antiguo, cambios del servidor (nuevo con aviso, leído, borrado),
//! operaciones locales (bandera, mover con id nuevo, borrar), MIME bajo demanda, la bandeja de
//! salida del servidor oculta y el envío con `SendAndSaveCopy` y `Bcc:`.

#[path = "../../dav/tests/common/mod.rs"]
mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use chrono::{DateTime, Utc};
use common::{Req, Resp, serve};
use iuredesk_accounts::{MemoryStore, SecretPurpose, SecretStore};
use iuredesk_compose::{OutgoingMessage, Transport};
use iuredesk_ews::{BasicAuth, EwsClient};
use iuredesk_model::account::{Account, AccountKind, AuthMethod, Protocol, Security, ServerConfig};
use iuredesk_model::{Flags, FolderRole, MessageId};
use iuredesk_store::Store;
use iuredesk_sync::{EwsSendTransport, SyncEngine, SyncEvent, SyncOptions};

const T: &str = "http://schemas.microsoft.com/exchange/services/2006/types";
const M: &str = "http://schemas.microsoft.com/exchange/services/2006/messages";

#[derive(Clone)]
struct Item {
    id: String,
    folder: String,
    subject: String,
    read: bool,
    flagged: bool,
    received: DateTime<Utc>,
}

#[derive(Default)]
struct Ews {
    items: Vec<Item>,
    /// (secuencia, carpeta, «c»rear/«d»borrar/«r»leído/«u»nleído, id)
    events: Vec<(u64, String, char, String)>,
    seq: u64,
    ids: u64,
    sent: Vec<String>,
    bodies: Vec<String>,
}

impl Ews {
    fn event(&mut self, folder: &str, kind: char, id: &str) {
        self.seq += 1;
        let s = self.seq;
        self.events.push((s, folder.to_string(), kind, id.to_string()));
    }

    fn add(&mut self, folder: &str, subject: &str, days_ago: i64) -> String {
        self.ids += 1;
        let id = format!("I{}", self.ids);
        self.items.push(Item {
            id: id.clone(),
            folder: folder.into(),
            subject: subject.into(),
            read: false,
            flagged: false,
            received: Utc::now() - chrono::Duration::days(days_ago),
        });
        self.event(folder, 'c', &id);
        id
    }
}

fn envelope(inner: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body>{inner}</s:Body></s:Envelope>"#
    )
}

fn response(op: &str, messages: &str) -> Resp {
    Resp::xml(
        200,
        envelope(&format!(
            r#"<m:{op}Response xmlns:m="{M}" xmlns:t="{T}"><m:ResponseMessages>{messages}</m:ResponseMessages></m:{op}Response>"#
        )),
    )
}

fn ok(op: &str, inner: &str) -> String {
    format!(
        r#"<m:{op}ResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode>{inner}</m:{op}ResponseMessage>"#
    )
}

fn err(op: &str, code: &str) -> String {
    format!(
        r#"<m:{op}ResponseMessage ResponseClass="Error"><m:ResponseCode>{code}</m:ResponseCode></m:{op}ResponseMessage>"#
    )
}

/// Valores del atributo `attr` en etiquetas `<t:tag …>`.
fn attrs(body: &str, tag: &str, attr: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in body.split(&format!("<t:{tag} ")).skip(1) {
        let head = part.split('>').next().unwrap_or_default();
        if let Some(v) = head.split(&format!("{attr}=\"")).nth(1).and_then(|v| v.split('"').next()) {
            out.push(v.to_string());
        }
    }
    out
}

fn between<'a>(body: &'a str, open: &str, close: &str) -> Option<&'a str> {
    body.split(open).nth(1).and_then(|r| r.split(close).next())
}

fn message_xml(i: &Item) -> String {
    format!(
        r#"<t:Message><t:ItemId Id="{}"/><t:ParentFolderId Id="{}"/><t:Subject>{}</t:Subject><t:DateTimeReceived>{}</t:DateTimeReceived><t:IsRead>{}</t:IsRead><t:Flag><t:FlagStatus>{}</t:FlagStatus></t:Flag><t:From><t:Mailbox><t:Name>Bea</t:Name><t:EmailAddress>bea@contoso.com</t:EmailAddress></t:Mailbox></t:From><t:InternetMessageId>&lt;{}@contoso.com&gt;</t:InternetMessageId><t:ConversationId Id="conv-{}"/></t:Message>"#,
        i.id,
        i.folder,
        i.subject,
        i.received.format("%Y-%m-%dT%H:%M:%SZ"),
        i.read,
        if i.flagged { "Flagged" } else { "NotFlagged" },
        i.id,
        i.id
    )
}

const FOLDERS: &[(&str, &str, &str)] = &[
    ("F-inbox", "Bandeja de entrada", "IPF.Note"),
    ("F-sent", "Elementos enviados", "IPF.Note"),
    ("F-drafts", "Borradores", "IPF.Note"),
    ("F-del", "Elementos eliminados", "IPF.Note"),
    ("F-junk", "Correo no deseado", "IPF.Note"),
    ("F-out", "Bandeja de salida", "IPF.Note"),
    ("F-proj", "Proyectos", "IPF.Note"),
    ("F-cal", "Calendario", "IPF.Appointment"),
];

fn handle(st: &Mutex<Ews>, req: Req) -> Resp {
    let mut g = st.lock().unwrap();
    let good = format!("Basic {}", base64::engine::general_purpose::STANDARD.encode("CONTOSO\\ana:secreto"));
    if req.header("Authorization") != Some(good.as_str()) {
        return Resp::new(401, "");
    }
    let b = req.body.clone();
    g.bodies.push(b.clone());
    if b.contains("<m:GetFolder>") {
        let msgs: String = attrs(&b, "DistinguishedFolderId", "Id")
            .iter()
            .map(|n| {
                let id = match n.as_str() {
                    "inbox" => "F-inbox",
                    "sentitems" => "F-sent",
                    "drafts" => "F-drafts",
                    "deleteditems" => "F-del",
                    "junkemail" => "F-junk",
                    "outbox" => "F-out",
                    "msgfolderroot" => "F-root",
                    _ => return err("GetFolder", "ErrorFolderNotFound"),
                };
                ok("GetFolder", &format!(r#"<m:Folders><t:Folder><t:FolderId Id="{id}"/></t:Folder></m:Folders>"#))
            })
            .collect();
        return response("GetFolder", &msgs);
    }
    if b.contains("<m:SyncFolderHierarchy>") {
        let changes: String = FOLDERS
            .iter()
            .map(|(id, name, class)| {
                format!(
                    r#"<t:Create><t:Folder><t:FolderId Id="{id}"/><t:ParentFolderId Id="F-root"/><t:FolderClass>{class}</t:FolderClass><t:DisplayName>{name}</t:DisplayName></t:Folder></t:Create>"#
                )
            })
            .collect();
        return response(
            "SyncFolderHierarchy",
            &ok(
                "SyncFolderHierarchy",
                &format!(
                    "<m:SyncState>H</m:SyncState><m:IncludesLastFolderInRange>true</m:IncludesLastFolderInRange><m:Changes>{changes}</m:Changes>"
                ),
            ),
        );
    }
    if b.contains("<m:SyncFolderItems>") {
        let folder = attrs(&b, "FolderId", "Id").remove(0);
        let max: usize = between(&b, "<m:MaxChangesReturned>", "<").and_then(|m| m.parse().ok()).unwrap_or(512);
        let state = between(&b, "<m:SyncState>", "<").unwrap_or("").to_string();
        let create = |id: &str| format!(r#"<t:Create><t:Message><t:ItemId Id="{id}"/></t:Message></t:Create>"#);
        // Recorrido inicial «P<desde>:<seq>» por páginas de 2 (para probar varias llamadas).
        let page = max.min(2);
        let (changes, next, last) = if state.is_empty() || state.starts_with('P') {
            let (from, seq) = match state.strip_prefix('P').and_then(|s| s.split_once(':')) {
                Some((f, s)) => (f.parse::<usize>().unwrap(), s.parse::<u64>().unwrap()),
                None => (0, g.seq),
            };
            let ids: Vec<String> = g.items.iter().filter(|i| i.folder == folder).map(|i| i.id.clone()).collect();
            let chunk: String = ids.iter().skip(from).take(page).map(|i| create(i)).collect();
            if from + page < ids.len() {
                (chunk, format!("P{}:{seq}", from + page), false)
            } else {
                (chunk, format!("S{seq}"), true)
            }
        } else {
            let since: u64 = state.trim_start_matches('S').parse().unwrap();
            let mut out = String::new();
            for (_, _, kind, id) in g.events.iter().filter(|e| e.0 > since && e.1 == folder) {
                out.push_str(&match kind {
                    'c' => create(id),
                    'd' => format!(r#"<t:Delete><t:ItemId Id="{id}"/></t:Delete>"#),
                    r => format!(
                        r#"<t:ReadFlagChange><t:ItemId Id="{id}"/><t:IsRead>{}</t:IsRead></t:ReadFlagChange>"#,
                        *r == 'r'
                    ),
                });
            }
            (out, format!("S{}", g.seq), true)
        };
        return response(
            "SyncFolderItems",
            &ok(
                "SyncFolderItems",
                &format!(
                    "<m:SyncState>{next}</m:SyncState><m:IncludesLastItemInRange>{last}</m:IncludesLastItemInRange><m:Changes>{changes}</m:Changes>"
                ),
            ),
        );
    }
    if b.contains("<m:FindItem ") {
        let folder = attrs(&b, "FolderId", "Id").remove(0);
        let at = DateTime::parse_from_rfc3339(&attrs(&b, "Constant", "Value").remove(0)).unwrap().with_timezone(&Utc);
        let before = b.contains("<t:IsLessThan>");
        let mut found: Vec<Item> = g
            .items
            .iter()
            .filter(|i| i.folder == folder && if before { i.received < at } else { i.received >= at })
            .cloned()
            .collect();
        found.sort_by_key(|i| std::cmp::Reverse(i.received));
        let items: String = found
            .iter()
            .map(|i| {
                format!(
                    r#"<t:Message><t:ItemId Id="{}"/><t:DateTimeReceived>{}</t:DateTimeReceived></t:Message>"#,
                    i.id,
                    i.received.format("%Y-%m-%dT%H:%M:%SZ")
                )
            })
            .collect();
        return response(
            "FindItem",
            &ok("FindItem", &format!("<m:RootFolder><t:Items>{items}</t:Items></m:RootFolder>")),
        );
    }
    if b.contains("<m:GetItem>") {
        let ids = attrs(&b, "ItemId", "Id");
        if b.contains("<t:IncludeMimeContent>true") {
            let Some(i) = g.items.iter().find(|i| i.id == ids[0]) else {
                return response("GetItem", &err("GetItem", "ErrorItemNotFound"));
            };
            let mime = format!(
                "From: Bea <bea@contoso.com>\r\nTo: ana@contoso.com\r\nSubject: {}\r\nMessage-ID: <{}@contoso.com>\r\n\r\nCuerpo de {}\r\n",
                i.subject, i.id, i.subject
            );
            let b64 = base64::engine::general_purpose::STANDARD.encode(mime);
            return response(
                "GetItem",
                &ok(
                    "GetItem",
                    &format!("<m:Items><t:Message><t:MimeContent>{b64}</t:MimeContent></t:Message></m:Items>"),
                ),
            );
        }
        let msgs: String = ids
            .iter()
            .map(|id| match g.items.iter().find(|i| &i.id == id) {
                Some(i) => ok("GetItem", &format!("<m:Items>{}</m:Items>", message_xml(i))),
                None => err("GetItem", "ErrorItemNotFound"),
            })
            .collect();
        return response("GetItem", &msgs);
    }
    if b.contains("<m:UpdateItem ") {
        let id = attrs(&b, "ItemId", "Id").remove(0);
        if let Some(i) = g.items.iter_mut().find(|i| i.id == id) {
            if let Some(r) = between(&b, "<t:IsRead>", "<") {
                i.read = r == "true";
            }
            if let Some(f) = between(&b, "<t:FlagStatus>", "<") {
                i.flagged = f == "Flagged";
            }
        }
        return response("UpdateItem", &ok("UpdateItem", ""));
    }
    if b.contains("<m:MoveItem>") {
        let id = attrs(&b, "ItemId", "Id").remove(0);
        let to = attrs(&b, "FolderId", "Id").remove(0);
        let Some(pos) = g.items.iter().position(|i| i.id == id) else {
            return response("MoveItem", &err("MoveItem", "ErrorItemNotFound"));
        };
        g.ids += 1;
        let new = format!("I{}", g.ids);
        let from = g.items[pos].folder.clone();
        g.items[pos].id = new.clone();
        g.items[pos].folder = to.clone();
        g.event(&from, 'd', &id);
        g.event(&to, 'c', &new);
        return response(
            "MoveItem",
            &ok("MoveItem", &format!(r#"<m:Items><t:Message><t:ItemId Id="{new}"/></t:Message></m:Items>"#)),
        );
    }
    if b.contains("<m:DeleteItem ") {
        let id = attrs(&b, "ItemId", "Id").remove(0);
        if let Some(pos) = g.items.iter().position(|i| i.id == id) {
            let f = g.items.remove(pos).folder;
            g.event(&f, 'd', &id);
            return response("DeleteItem", &ok("DeleteItem", ""));
        }
        return response("DeleteItem", &err("DeleteItem", "ErrorItemNotFound"));
    }
    if b.contains("<m:CreateItem ") {
        let mime = between(&b, "<t:MimeContent CharacterSet=\"UTF-8\">", "<").unwrap_or_default();
        let raw = String::from_utf8(base64::engine::general_purpose::STANDARD.decode(mime).unwrap()).unwrap();
        g.sent.push(raw);
        return response("CreateItem", &ok("CreateItem", ""));
    }
    Resp::new(400, "")
}

fn account() -> Account {
    let srv = |p| ServerConfig {
        protocol: p,
        host: String::new(),
        port: 0,
        security: Security::Plain,
        auth: AuthMethod::Password,
        username: "CONTOSO\\ana".into(),
    };
    Account {
        id: "exch".into(),
        display_name: "Ana".into(),
        email: "ana@contoso.com".into(),
        kind: AccountKind::Exchange,
        incoming: srv(Protocol::Ews),
        outgoing: srv(Protocol::Ews),
        caldav_url: None,
        carddav_url: None,
        identities: vec![],
        shared_of: None,
    }
}

fn local(store: &Store, remote: &str) -> Option<MessageId> {
    let acct = store.account_labels().unwrap()[0].id;
    store.folders(acct).unwrap().into_iter().find_map(|f| store.message_by_remote(f.id, remote).unwrap())
}

async fn until(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..500 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("tiempo agotado: {what}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn exchange_mail_through_ews() {
    let state = Arc::new(Mutex::new(Ews::default()));
    let (recent1, recent2, recent3, old) = {
        let mut g = state.lock().unwrap();
        let r1 = g.add("F-inbox", "Reciente uno", 1);
        let r2 = g.add("F-inbox", "Reciente dos", 2);
        let r3 = g.add("F-inbox", "Reciente tres", 3);
        let old = g.add("F-inbox", "Antiguo", 60);
        g.add("F-out", "En la salida del servidor", 0);
        (r1, r2, r3, old)
    };
    let h = Arc::clone(&state);
    let base = serve(move |req| handle(&h, req)).await;
    let mut acc = account();
    acc.incoming.host = base.clone();
    acc.outgoing.host = base.clone();

    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    let secrets = Arc::new(MemoryStore::default());
    secrets.set("exch", SecretPurpose::Incoming, "secreto").unwrap();
    let opts = SyncOptions {
        poll_interval: Duration::from_secs(3600),
        graph_inbox_poll: Duration::from_secs(3600),
        prefetch_recent: 1,
        ..Default::default()
    };
    let sh = SyncEngine::start_with(Arc::clone(&store), vec![acc.clone()], None, secrets.clone(), opts);
    let mut events = sh.events();
    sh.sync_now_wait("exch").await.unwrap();

    let acct = store.account_labels().unwrap()[0].id;
    let folders = store.folders(acct).unwrap();
    let role = |r: FolderRole| folders.iter().find(|f| f.role == r).unwrap().id;
    let (inbox, trash) = (role(FolderRole::Inbox), role(FolderRole::Trash));
    let proj = folders.iter().find(|f| f.name == "Proyectos").unwrap().id;
    assert!(
        folders.iter().all(|f| f.name != "Calendario" && f.name != "Bandeja de salida"),
        "sin calendario ni salida"
    );
    assert!(folders.iter().all(|f| f.parent.is_none()), "la raíz del buzón no es una carpeta");
    // Primera vez: lo reciente; el recorrido de ids terminó (3 llamadas de 2) sin duplicar.
    for r in [&recent1, &recent2, &recent3] {
        assert!(local(&store, r).is_some(), "{r}");
    }
    let calls = state
        .lock()
        .unwrap()
        .bodies
        .iter()
        .filter(|b| b.contains("<m:SyncFolderItems>") && b.contains("F-inbox"))
        .count();
    assert!(calls >= 2, "recorrido por páginas");
    // El más reciente ya trae cuerpo (precarga).
    assert!(store.get_envelope(local(&store, &recent1).unwrap()).unwrap().unwrap().blob.is_some());
    // Lo antiguo llega por el historial.
    until("historial", || local(&store, &old).is_some()).await;
    assert_eq!(store.count(inbox).unwrap().total, 4);

    // Cambios del servidor: uno nuevo (con aviso), uno leído en otro cliente, uno borrado.
    let fresh = {
        let mut g = state.lock().unwrap();
        let id = g.add("F-inbox", "Nuevo", 0);
        let pos = g.items.iter().position(|i| i.id == recent2).unwrap();
        g.items[pos].read = true;
        g.event("F-inbox", 'r', &recent2.clone());
        let pos = g.items.iter().position(|i| i.id == recent3).unwrap();
        g.items.remove(pos);
        g.event("F-inbox", 'd', &recent3.clone());
        id
    };
    while events.try_recv().is_ok() {}
    sh.sync_now_wait("exch").await.unwrap();
    let new_local = local(&store, &fresh).expect("nuevo");
    assert!(store.get_envelope(local(&store, &recent2).unwrap()).unwrap().unwrap().flags.seen);
    assert!(local(&store, &recent3).is_none(), "borrado en el servidor");
    let mut notified = false;
    while let Ok(e) = events.try_recv() {
        notified |= matches!(e, SyncEvent::NewMail { count: 1, .. });
    }
    assert!(notified, "aviso de correo nuevo");

    // Operaciones locales: bandera, mover (el id cambia y se adopta), borrar.
    let m1 = local(&store, &recent1).unwrap();
    sh.set_flags(m1, Flags { seen: true, flagged: true, ..Default::default() }).unwrap();
    until("bandera", || state.lock().unwrap().items.iter().any(|i| i.id == recent1 && i.flagged && i.read)).await;
    sh.move_message(m1, proj).unwrap();
    until("mover", || {
        store
            .get_envelope(m1)
            .unwrap()
            .is_some_and(|e| e.folder == proj && e.remote_id.as_deref() != Some(recent1.as_str()))
    })
    .await;
    let moved_id = store.get_envelope(m1).unwrap().unwrap().remote_id.unwrap();
    assert!(state.lock().unwrap().items.iter().any(|i| i.id == moved_id && i.folder == "F-proj"));
    sh.delete_message(new_local).unwrap();
    until("borrar", || !state.lock().unwrap().items.iter().any(|i| i.id == fresh)).await;
    // La siguiente pasada no duplica lo movido ni revive lo borrado.
    sh.sync_now_wait("exch").await.unwrap();
    assert_eq!(store.count(proj).unwrap().total, 1);
    assert_eq!(local(&store, &moved_id), Some(m1));
    let _ = trash;

    // MIME bajo demanda.
    let m_old = local(&store, &old).unwrap();
    sh.fetch_body(m_old).await.unwrap();
    assert!(String::from_utf8_lossy(&store.get_raw(m_old).unwrap()).contains("Cuerpo de Antiguo"));

    // Envío: SendAndSaveCopy con los ocultos en Bcc.
    let t = EwsSendTransport::for_account(&acc, secrets, Duration::from_secs(10)).unwrap();
    assert!(t.saves_to_sent());
    let out = OutgoingMessage {
        envelope_from: "ana@contoso.com".into(),
        recipients: vec!["bob@x.com".into(), "oculto@x.com".into()],
        raw: b"From: ana@contoso.com\r\nTo: bob@x.com\r\nSubject: Hola\r\n\r\nQue tal\r\n".to_vec(),
    };
    t.send(&out).await.unwrap();
    {
        let g = state.lock().unwrap();
        assert_eq!(g.sent.len(), 1);
        assert!(g.sent[0].starts_with("Bcc: oculto@x.com\r\n"));
        assert!(g.bodies.iter().any(|b| b.contains(r#"MessageDisposition="SendAndSaveCopy""#)));
    }
    // Un cliente con contraseña equivocada: «credencial rechazada».
    let bad = EwsClient::new(
        &iuredesk_ews::server_url(&base, 0, false).unwrap(),
        Arc::new(BasicAuth::new("CONTOSO\\ana", "otra")),
        Duration::from_secs(5),
    )
    .unwrap();
    assert!(EwsSendTransport::new(bad).send(&out).await.is_err());
    sh.shutdown().await;
}
