//! EWS contra un servidor falso: Basic, credencial rechazada, `ErrorServerBusy` con espera,
//! carpetas distinguidas (una que no existe), árbol de carpetas (filtra calendario), sincronizar
//! elementos por páginas y después cambios (borrado, leído), datos por lote (uno ya no existe),
//! MIME, banderas y categorías, mover (id nuevo), borrar lo que ya no está, crear desde MIME y
//! enviar, historial por fecha.

#[path = "../../dav/tests/common/mod.rs"]
mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use chrono::{TimeZone, Utc};
use common::{Req, Resp, serve};
use iuredesk_ews::{BasicAuth, EwsClient, EwsError, MessageDisposition};

const T: &str = "http://schemas.microsoft.com/exchange/services/2006/types";
const M: &str = "http://schemas.microsoft.com/exchange/services/2006/messages";

#[derive(Default)]
struct Fake {
    /// (id, carpeta, asunto, leído, día de octubre)
    items: Vec<(String, String, String, bool, u32)>,
    busy_once: bool,
    seq: u32,
    bodies: Vec<String>,
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
        r#"<m:{op}ResponseMessage ResponseClass="Error"><m:MessageText>x</m:MessageText><m:ResponseCode>{code}</m:ResponseCode></m:{op}ResponseMessage>"#
    )
}

/// Todos los valores del atributo `Id` dentro de etiquetas `tag` del cuerpo.
fn ids_in(body: &str, tag: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(i) = rest.find(&format!("<t:{tag} Id=\"")) {
        rest = &rest[i + tag.len() + 8..];
        let end = rest.find('"').unwrap();
        out.push(rest[..end].to_string());
        rest = &rest[end..];
    }
    out
}

fn message_xml(it: &(String, String, String, bool, u32)) -> String {
    format!(
        r#"<t:Message><t:ItemId Id="{}" ChangeKey="ck"/><t:ParentFolderId Id="{}"/><t:Subject>{}</t:Subject><t:DateTimeReceived>2026-10-{:02}T10:00:00Z</t:DateTimeReceived><t:IsRead>{}</t:IsRead><t:From><t:Mailbox><t:Name>Bea</t:Name><t:EmailAddress>bea@contoso.com</t:EmailAddress></t:Mailbox></t:From><t:ConversationId Id="c-{}"/></t:Message>"#,
        it.0, it.1, it.2, it.4, it.3, it.0
    )
}

fn handle(st: &Mutex<Fake>, req: Req) -> Resp {
    let mut f = st.lock().unwrap();
    let auth = req.header("Authorization").unwrap_or_default().to_string();
    let good = format!("Basic {}", base64::engine::general_purpose::STANDARD.encode("CONTOSO\\ana:secreto"));
    if auth != good {
        return Resp::new(401, "").with("WWW-Authenticate", "Basic realm=\"x\"");
    }
    let b = req.body.clone();
    f.bodies.push(b.clone());
    assert!(b.contains(r#"<t:RequestServerVersion Version="Exchange2013_SP1"/>"#));
    if f.busy_once {
        f.busy_once = false;
        return Resp::xml(
            500,
            envelope(&format!(
                r#"<s:Fault><faultcode>a:ErrorServerBusy</faultcode><faultstring>ocupado</faultstring><detail><e:ResponseCode xmlns:e="x">ErrorServerBusy</e:ResponseCode><e:MessageXml xmlns:e="x"><t:Value xmlns:t="{T}" Name="BackOffMilliseconds">10</t:Value></e:MessageXml></detail></s:Fault>"#
            )),
        );
    }
    if b.contains("<m:GetFolder>") {
        let names: Vec<String> = ids_in(&b, "DistinguishedFolderId");
        let msgs: String = names
            .iter()
            .map(|n| {
                if n == "archive" {
                    err("GetFolder", "ErrorFolderNotFound")
                } else {
                    ok("GetFolder", &format!(r#"<m:Folders><t:Folder><t:FolderId Id="F-{n}"/></t:Folder></m:Folders>"#))
                }
            })
            .collect();
        return response("GetFolder", &msgs);
    }
    if b.contains("<m:SyncFolderHierarchy>") {
        let folders = [
            ("F-inbox", "Bandeja de entrada", "IPF.Note"),
            ("F-sentitems", "Elementos enviados", "IPF.Note"),
            ("F-proj", "Proyectos", "IPF.Note"),
            ("F-cal", "Calendario", "IPF.Appointment"),
        ];
        let changes: String = folders
            .iter()
            .map(|(id, name, class)| {
                format!(
                    r#"<t:Create><t:Folder><t:FolderId Id="{id}"/><t:ParentFolderId Id="root"/><t:FolderClass>{class}</t:FolderClass><t:DisplayName>{name}</t:DisplayName><t:ChildFolderCount>0</t:ChildFolderCount></t:Folder></t:Create>"#
                )
            })
            .collect();
        return response(
            "SyncFolderHierarchy",
            &ok(
                "SyncFolderHierarchy",
                &format!(
                    "<m:SyncState>H1</m:SyncState><m:IncludesLastFolderInRange>true</m:IncludesLastFolderInRange><m:Changes>{changes}<t:Delete><t:FolderId Id=\"F-old\"/></t:Delete></m:Changes>"
                ),
            ),
        );
    }
    if b.contains("<m:SyncFolderItems>") {
        // Estado «S0» → primera página (2), «S1» → resto, «S2» → cambios.
        let state = b.split("<m:SyncState>").nth(1).and_then(|s| s.split('<').next()).unwrap_or("").to_string();
        let (changes, next, last) = match state.as_str() {
            "" => (
                f.items.iter().take(2).map(|i| format!(r#"<t:Create><t:Message><t:ItemId Id="{}" ChangeKey="ck"/></t:Message></t:Create>"#, i.0)).collect::<String>(),
                "S1",
                false,
            ),
            "S1" => (
                f.items.iter().skip(2).map(|i| format!(r#"<t:Create><t:Message><t:ItemId Id="{}" ChangeKey="ck"/></t:Message></t:Create>"#, i.0)).collect(),
                "S2",
                true,
            ),
            _ => (
                r#"<t:Delete><t:ItemId Id="m2"/></t:Delete><t:ReadFlagChange><t:ItemId Id="m1"/><t:IsRead>true</t:IsRead></t:ReadFlagChange><t:Update><t:Message><t:ItemId Id="m3" ChangeKey="ck2"/></t:Message></t:Update>"#.to_string(),
                "S3",
                true,
            ),
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
    if b.contains("<m:GetItem>") {
        let ids = ids_in(&b, "ItemId");
        if b.contains("<t:IncludeMimeContent>true") {
            let id = &ids[0];
            let Some(it) = f.items.iter().find(|i| &i.0 == id) else {
                return response("GetItem", &err("GetItem", "ErrorItemNotFound"));
            };
            let mime = format!("From: bea@contoso.com\r\nSubject: {}\r\n\r\nCuerpo de {}\r\n", it.2, it.2);
            let b64 = base64::engine::general_purpose::STANDARD.encode(mime);
            return response(
                "GetItem",
                &ok(
                    "GetItem",
                    &format!(
                        r#"<m:Items><t:Message><t:MimeContent CharacterSet="UTF-8">{}
{}</t:MimeContent><t:ItemId Id="{id}"/></t:Message></m:Items>"#,
                        &b64[..b64.len() / 2],
                        &b64[b64.len() / 2..]
                    ),
                ),
            );
        }
        let msgs: String = ids
            .iter()
            .map(|id| match f.items.iter().find(|i| &i.0 == id) {
                Some(it) => ok("GetItem", &format!("<m:Items>{}</m:Items>", message_xml(it))),
                None => err("GetItem", "ErrorItemNotFound"),
            })
            .collect();
        return response("GetItem", &msgs);
    }
    if b.contains("<m:FindItem ") {
        let mut found: Vec<_> = f.items.iter().filter(|i| i.4 < 5).cloned().collect();
        found.sort_by_key(|i| std::cmp::Reverse(i.4));
        let items: String = found
            .iter()
            .map(|i| format!(r#"<t:Message><t:ItemId Id="{}"/><t:DateTimeReceived>2026-10-{:02}T10:00:00Z</t:DateTimeReceived></t:Message>"#, i.0, i.4))
            .collect();
        return response(
            "FindItem",
            &ok(
                "FindItem",
                &format!(
                    r#"<m:RootFolder TotalItemsInView="{}" IncludesLastItemInRange="true"><t:Items>{items}</t:Items></m:RootFolder>"#,
                    found.len()
                ),
            ),
        );
    }
    if b.contains("<m:UpdateItem ") {
        return response(
            "UpdateItem",
            &ok("UpdateItem", "<m:Items><t:Message><t:ItemId Id=\"m1\"/></t:Message></m:Items>"),
        );
    }
    if b.contains("<m:MoveItem>") {
        let id = ids_in(&b, "ItemId").remove(0);
        let to = ids_in(&b, "FolderId").remove(0);
        f.seq += 1;
        let new = format!("{id}-moved{}", f.seq);
        return match f.items.iter_mut().find(|i| i.0 == id) {
            Some(it) => {
                it.0 = new.clone();
                it.1 = to;
                response(
                    "MoveItem",
                    &ok("MoveItem", &format!(r#"<m:Items><t:Message><t:ItemId Id="{new}"/></t:Message></m:Items>"#)),
                )
            }
            None => response("MoveItem", &err("MoveItem", "ErrorItemNotFound")),
        };
    }
    if b.contains("<m:DeleteItem ") {
        let id = ids_in(&b, "ItemId").remove(0);
        let before = f.items.len();
        f.items.retain(|i| i.0 != id);
        return if f.items.len() < before {
            response("DeleteItem", &ok("DeleteItem", ""))
        } else {
            response("DeleteItem", &err("DeleteItem", "ErrorItemNotFound"))
        };
    }
    if b.contains("<m:CreateItem ") {
        let sending = b.contains(r#"MessageDisposition="SendAndSaveCopy""#);
        let inner = if sending {
            String::new()
        } else {
            r#"<m:Items><t:Message><t:ItemId Id="new-1"/></t:Message></m:Items>"#.to_string()
        };
        return response("CreateItem", &ok("CreateItem", &inner));
    }
    Resp::new(400, "")
}

async fn setup(busy_once: bool) -> (Arc<Mutex<Fake>>, String) {
    let st = Arc::new(Mutex::new(Fake {
        items: vec![
            ("m1".into(), "F-inbox".into(), "Uno".into(), false, 10),
            ("m2".into(), "F-inbox".into(), "Dos".into(), false, 3),
            ("m3".into(), "F-inbox".into(), "Tres".into(), true, 1),
        ],
        busy_once,
        ..Default::default()
    }));
    let h = Arc::clone(&st);
    let base = serve(move |req| handle(&h, req)).await;
    (st, format!("{base}/EWS/Exchange.asmx"))
}

fn client(url: &str, password: &str) -> EwsClient {
    EwsClient::new(url, Arc::new(BasicAuth::new("CONTOSO\\ana", password)), Duration::from_secs(10))
        .unwrap()
        .with_retry(3, Duration::from_millis(200))
}

#[tokio::test]
async fn ews_mail_operations_against_a_fake_server() {
    let (st, url) = setup(true).await;

    // Contraseña equivocada: se reintenta una vez y se rinde con «credencial rechazada».
    let err = client(&url, "otra").sync_folder_hierarchy(None).await.unwrap_err();
    assert_eq!(err, EwsError::Unauthorized);
    assert!(err.needs_reauth());

    let c = client(&url, "secreto");
    // Ocupado una vez (BackOffMilliseconds=10): espera y sigue.
    let wk = c.distinguished_folders(&["inbox", "sentitems", "archive"]).await.unwrap();
    assert_eq!(wk[0], ("inbox".to_string(), Some("F-inbox".to_string())));
    assert_eq!(wk[2], ("archive".to_string(), None), "carpeta que no existe en el servidor");

    let h = c.sync_folder_hierarchy(None).await.unwrap();
    assert_eq!(h.state, "H1");
    assert!(h.last);
    let mail: Vec<_> = h.changed.iter().filter(|f| f.is_mail()).map(|f| f.name.as_str()).collect();
    assert_eq!(mail, ["Bandeja de entrada", "Elementos enviados", "Proyectos"]);
    assert_eq!(h.deleted, ["F-old"]);

    // Elementos: dos páginas y después los cambios.
    let p1 = c.sync_folder_items("F-inbox", None, 2).await.unwrap();
    assert_eq!((p1.created.len(), p1.last, p1.state.as_str()), (2, false, "S1"));
    let p2 = c.sync_folder_items("F-inbox", Some(&p1.state), 2).await.unwrap();
    assert_eq!((p2.created.len(), p2.last), (1, true));
    let ch = c.sync_folder_items("F-inbox", Some(&p2.state), 512).await.unwrap();
    assert_eq!(ch.deleted, ["m2"]);
    assert_eq!(ch.read_changes, [("m1".to_string(), true)]);
    assert_eq!(ch.updated, [("m3".to_string(), Some("ck2".to_string()))]);

    // Datos por lote: el que ya no existe se omite.
    let s = c.get_summaries(&["m1".into(), "nope".into(), "m3".into()]).await.unwrap();
    assert_eq!(s.iter().map(|i| i.subject.as_str()).collect::<Vec<_>>(), ["Uno", "Tres"]);
    assert_eq!(s[0].from.as_ref().unwrap().name, "Bea");
    assert_eq!(s[0].conversation_id.as_deref(), Some("c-m1"));
    assert_eq!(s[1].is_read, Some(true));

    // MIME (base64 partido en líneas).
    let mime = c.get_mime("m1").await.unwrap();
    assert!(String::from_utf8_lossy(&mime).contains("Cuerpo de Uno"));
    assert!(c.get_mime("nope").await.unwrap_err().is_not_found());

    // Banderas, categorías y quitar categorías.
    c.update_flags("m1", Some(true), Some(true), Some(&["Cliente & Co".to_string()])).await.unwrap();
    c.update_flags("m1", None, None, Some(&[])).await.unwrap();
    c.update_flags("m1", None, None, None).await.unwrap();
    {
        let f = st.lock().unwrap();
        let ups: Vec<&String> = f.bodies.iter().filter(|b| b.contains("<m:UpdateItem ")).collect();
        assert_eq!(ups.len(), 2, "sin cambios no se llama");
        assert!(
            ups[0].contains("<t:IsRead>true</t:IsRead>") && ups[0].contains("<t:FlagStatus>Flagged</t:FlagStatus>")
        );
        assert!(ups[0].contains("<t:String>Cliente &amp; Co</t:String>"));
        assert!(ups[1].contains(r#"<t:DeleteItemField><t:FieldURI FieldURI="item:Categories"/>"#));
    }

    // Mover cambia el id; mover o borrar lo que ya no está da igual.
    let new = c.move_item("m1", "F-proj").await.unwrap();
    assert_eq!(new.as_deref(), Some("m1-moved1"));
    assert_eq!(c.move_item("m1", "F-proj").await.unwrap(), None);
    c.delete_item("m3", true).await.unwrap();
    c.delete_item("m3", true).await.unwrap();
    assert!(st.lock().unwrap().bodies.iter().any(|b| b.contains(r#"DeleteType="HardDelete""#)));

    // Crear desde MIME: borrador con id; copia normal (leída); enviar a «Enviados».
    let raw = b"From: ana@contoso.com\r\nTo: bob@x.com\r\nBcc: oculto@x.com\r\nSubject: Hola\r\n\r\nQue tal\r\n";
    assert_eq!(
        c.create_from_mime(raw, "drafts", MessageDisposition::Save { draft: true }).await.unwrap().as_deref(),
        Some("new-1")
    );
    c.create_from_mime(raw, "F-sentitems", MessageDisposition::Save { draft: false }).await.unwrap();
    assert_eq!(c.create_from_mime(raw, "sentitems", MessageDisposition::SendAndSaveCopy).await.unwrap(), None);
    {
        let f = st.lock().unwrap();
        let creates: Vec<&String> = f.bodies.iter().filter(|b| b.contains("<m:CreateItem ")).collect();
        let b64 = base64::engine::general_purpose::STANDARD.encode(raw);
        assert!(creates.iter().all(|b| b.contains(&b64)));
        assert!(creates[0].contains(r#"<t:DistinguishedFolderId Id="drafts"/>"#) && !creates[0].contains("3591"));
        assert!(creates[1].contains(r#"PropertyTag="3591""#), "copia normal: no borrador");
        assert!(
            creates[2].contains(r#"MessageDisposition="SendAndSaveCopy""#) && creates[2].contains(r#"Id="sentitems""#)
        );
    }

    // Historial: antes de una fecha, del más reciente al más antiguo.
    let before = Utc.with_ymd_and_hms(2026, 10, 5, 0, 0, 0).unwrap();
    let old = c.find_before("F-inbox", before, 100).await.unwrap();
    assert_eq!(old.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(), ["m2"]);
    assert!(old[0].1.is_some());
}

#[tokio::test]
async fn busy_forever_gives_up_with_server_busy() {
    let st = Arc::new(Mutex::new(0u32));
    let h = Arc::clone(&st);
    let base = serve(move |_req| {
        *h.lock().unwrap() += 1;
        Resp::new(503, "")
    })
    .await;
    let c = EwsClient::new(
        &format!("{base}/EWS/Exchange.asmx"),
        Arc::new(BasicAuth::new("u", "p")),
        Duration::from_secs(5),
    )
    .unwrap()
    .with_retry(2, Duration::from_millis(5));
    let err = c.sync_folder_hierarchy(None).await.unwrap_err();
    assert!(matches!(err, EwsError::ServerBusy { .. }) && err.is_transient(), "{err:?}");
    assert_eq!(*st.lock().unwrap(), 3, "un intento y dos reintentos");
}
