//! Correo de Microsoft 365 por Graph contra un servidor falso: árbol de carpetas, delta paginado
//! con `@removed` (borrado y movido), 429 con `Retry-After`, MIME bajo demanda, banderas, mover y
//! borrar desde aquí, y `sendMail` con `Bcc:`.

#[path = "../../dav/tests/common/mod.rs"]
mod common;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use common::{Req, Resp, serve};
use iuredesk_accounts::MemoryStore;
use iuredesk_compose::{OutgoingMessage, Transport};
use iuredesk_graph::{GraphClient, GraphError, GraphSendTransport};
use iuredesk_model::account::{Account, AccountKind, AuthMethod, OAuthProvider, Protocol, Security, ServerConfig};
use iuredesk_model::{Flags, FolderRole, MessageId};
use iuredesk_oauth::{BoxFuture, TokenAudience, TokenProvider};
use iuredesk_store::Store;
use iuredesk_sync::{SyncEngine, SyncOptions};
use serde_json::{Value, json};

struct FakeTokens;

impl TokenProvider for FakeTokens {
    fn access_token<'a>(
        &'a self,
        _a: &'a Account,
        _aud: TokenAudience,
    ) -> BoxFuture<'a, Result<String, iuredesk_oauth::Error>> {
        Box::pin(async { Ok("tok-graph".to_string()) })
    }
    fn invalidate(&self, _account_id: &str) {}
}

const FOLDERS: &[(&str, &str, &str)] = &[
    ("F-inbox", "inbox", "Bandeja de entrada"),
    ("F-arch", "archive", "Archivo"),
    ("F-sent", "sentitems", "Elementos enviados"),
    ("F-drafts", "drafts", "Borradores"),
    ("F-del", "deleteditems", "Elementos eliminados"),
    ("F-junk", "junkemail", "Correo no deseado"),
    ("F-proj", "", "Proyectos"),
];

#[derive(Default)]
struct G {
    base: String,
    /// id → (carpeta, json)
    msgs: BTreeMap<String, (String, Value)>,
    /// (secuencia, carpeta, id, borrado)
    log: Vec<(u64, String, String, bool)>,
    seq: u64,
    throttle_once: bool,
    requests: Vec<String>,
    patches: Vec<(String, Value)>,
    sent: Vec<String>,
}

impl G {
    fn put(&mut self, folder: &str, id: &str, v: Value) {
        self.seq += 1;
        if let Some((old, _)) = self.msgs.get(id)
            && old != folder
        {
            self.log.push((self.seq, old.clone(), id.to_string(), true));
        }
        self.msgs.insert(id.to_string(), (folder.to_string(), v));
        self.log.push((self.seq, folder.to_string(), id.to_string(), false));
    }

    fn remove(&mut self, id: &str) {
        self.seq += 1;
        if let Some((f, _)) = self.msgs.remove(id) {
            self.log.push((self.seq, f, id.to_string(), true));
        }
    }

    fn json(&self, id: &str) -> Option<Value> {
        self.msgs.get(id).map(|(f, v)| {
            let mut v = v.clone();
            v["id"] = json!(id);
            v["parentFolderId"] = json!(f);
            v
        })
    }
}

fn msg(subject: &str, conv: &str, minutes_ago: i64) -> Value {
    let d = (chrono::Utc::now() - chrono::Duration::minutes(minutes_ago)).to_rfc3339();
    json!({
        "subject": subject,
        "from": {"emailAddress": {"name": "Bea", "address": "bea@contoso.com"}},
        "toRecipients": [{"emailAddress": {"address": "ana@contoso.com"}}],
        "receivedDateTime": d,
        "isRead": false,
        "flag": {"flagStatus": "notFlagged"},
        "categories": [],
        "conversationId": conv,
        "internetMessageId": format!("<{subject}@contoso.com>"),
        "bodyPreview": format!("Texto de {subject}"),
        "hasAttachments": false
    })
}

fn path_only(p: &str) -> &str {
    p.split('?').next().unwrap_or_default()
}

fn handle(st: &Mutex<G>, req: Req) -> Resp {
    let mut g = st.lock().unwrap();
    g.requests.push(format!("{} {}", req.method, req.path));
    if req.header("Authorization") != Some("Bearer tok-graph") {
        return Resp::new(401, "");
    }
    let path = path_only(&req.path).trim_start_matches("/v1.0").to_string();
    let base = g.base.clone();
    let seg: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    match (req.method.as_str(), seg.as_slice()) {
        ("GET", ["me", "always429"]) => Resp::new(429, "").with("Retry-After", "0"),
        ("GET", ["me", "mailFolders"]) => {
            let value: Vec<Value> = FOLDERS
                .iter()
                .map(|(id, _, name)| json!({"id": id, "displayName": name, "parentFolderId": "root", "childFolderCount": 0}))
                .collect();
            Resp::new(200, json!({ "value": value }).to_string())
        }
        ("GET", ["me", "mailFolders", wk]) => match FOLDERS.iter().find(|(_, w, _)| !w.is_empty() && w == wk) {
            Some((id, _, _)) => Resp::new(200, json!({ "id": id }).to_string()),
            None => Resp::new(404, json!({"error": {"code": "ErrorFolderNotFound"}}).to_string()),
        },
        ("GET", ["me", "mailFolders", fid, "messages", "delta"]) => {
            let fid = fid.to_string();
            if fid == "F-inbox" && g.throttle_once {
                g.throttle_once = false;
                return Resp::new(429, json!({"error": {"code": "TooManyRequests"}}).to_string())
                    .with("Retry-After", "0");
            }
            let q = req.path.split_once('?').map(|(_, q)| q).unwrap_or_default();
            let token: Option<u64> =
                q.split('&').find_map(|kv| kv.strip_prefix("$deltatoken=")).and_then(|t| t.parse().ok());
            let link = format!("{base}/v1.0/me/mailFolders/{fid}/messages/delta?$deltatoken={}", g.seq);
            match token {
                None => {
                    let ids: Vec<String> =
                        g.msgs.iter().filter(|(_, (f, _))| *f == fid).map(|(k, _)| k.clone()).collect();
                    let page2 = q.contains("skiptoken=p2");
                    let (first, second) = ids.split_at(ids.len() / 2);
                    let (items, next) = if page2 || ids.len() < 2 {
                        (if page2 { second } else { &ids[..] }, None)
                    } else {
                        (first, Some(format!("{base}/v1.0/me/mailFolders/{fid}/messages/delta?$skiptoken=p2")))
                    };
                    let value: Vec<Value> = items.iter().filter_map(|i| g.json(i)).collect();
                    let mut body = json!({ "value": value });
                    match next {
                        Some(n) => body["@odata.nextLink"] = json!(n),
                        None => body["@odata.deltaLink"] = json!(link),
                    }
                    Resp::new(200, body.to_string())
                }
                Some(t) => {
                    let mut value = Vec::new();
                    for (seq, f, id, removed) in g.log.iter().filter(|e| e.0 > t && e.1 == fid) {
                        let _ = seq;
                        if *removed {
                            value.push(json!({"id": id, "@removed": {"reason": "deleted"}}));
                        } else if let Some(v) = g.json(id).filter(|v| v["parentFolderId"] == json!(f)) {
                            value.push(v);
                        }
                    }
                    Resp::new(200, json!({ "value": value, "@odata.deltaLink": link }).to_string())
                }
            }
        }
        ("GET", ["me", "messages", id, "$value"]) => {
            let id = id.to_string();
            match g.json(&id) {
                Some(v) => {
                    let subject = v["subject"].as_str().unwrap_or_default().to_string();
                    let mime = format!(
                        "From: Bea <bea@contoso.com>\r\nTo: ana@contoso.com\r\nSubject: {subject}\r\nMessage-ID: <{subject}@contoso.com>\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nCuerpo completo de {subject}\r\n"
                    );
                    Resp::new(200, mime)
                }
                None => Resp::new(404, ""),
            }
        }
        ("GET", ["me", "messages", id]) => match g.json(id) {
            Some(v) => Resp::new(200, v.to_string()),
            None => Resp::new(404, json!({"error": {"code": "ErrorItemNotFound"}}).to_string()),
        },
        ("PATCH", ["me", "messages", id]) => {
            let id = id.to_string();
            let patch: Value = serde_json::from_str(&req.body).unwrap_or(Value::Null);
            g.patches.push((id.clone(), patch.clone()));
            if let Some((f, mut v)) = g.msgs.get(&id).cloned() {
                if let Some(Value::Object(m)) = Some(patch) {
                    for (k, x) in m {
                        v[k] = x;
                    }
                }
                g.put(&f, &id, v);
            }
            Resp::new(200, g.json(&id).unwrap_or(Value::Null).to_string())
        }
        ("POST", ["me", "messages", id, "move"]) => {
            let id = id.to_string();
            let dest: Value = serde_json::from_str(&req.body).unwrap_or(Value::Null);
            let dest = dest["destinationId"].as_str().unwrap_or_default().to_string();
            match g.msgs.get(&id).cloned() {
                Some((_, v)) => {
                    g.put(&dest, &id, v);
                    Resp::new(201, g.json(&id).unwrap_or(Value::Null).to_string())
                }
                None => Resp::new(404, ""),
            }
        }
        ("POST", ["me", "messages", id, "permanentDelete"]) => {
            let id = id.to_string();
            g.remove(&id);
            Resp::new(204, "")
        }
        ("POST", ["me", "sendMail"]) => {
            let mime = base64::engine::general_purpose::STANDARD.decode(req.body.trim()).unwrap_or_default();
            g.sent.push(String::from_utf8_lossy(&mime).into_owned());
            Resp::new(202, "")
        }
        _ => Resp::new(404, json!({"error": {"code": "NotFound"}}).to_string()),
    }
}

fn account() -> Account {
    let srv = |p| ServerConfig {
        protocol: p,
        host: "outlook.office365.com".into(),
        port: 993,
        security: Security::Tls,
        auth: AuthMethod::OAuth2 { provider: OAuthProvider::Microsoft },
        username: "ana@contoso.com".into(),
    };
    Account {
        id: "m365".into(),
        display_name: "Ana".into(),
        email: "ana@contoso.com".into(),
        kind: AccountKind::Microsoft365,
        incoming: srv(Protocol::Imap),
        outgoing: srv(Protocol::Smtp),
        caldav_url: None,
        carddav_url: None,
        identities: vec![],
    }
}

fn by_remote(store: &Store, id: &str) -> Option<MessageId> {
    let acct = store.account_labels().unwrap()[0].id;
    store.folders(acct).unwrap().into_iter().find_map(|f| store.message_by_remote(f.id, id).unwrap())
}

async fn until(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while !f() {
        assert!(tokio::time::Instant::now() < deadline, "tiempo agotado esperando: {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn graph_mail_delta_removed_throttling_ops_and_send() {
    let state = Arc::new(Mutex::new(G { throttle_once: true, ..Default::default() }));
    {
        let mut g = state.lock().unwrap();
        g.put("F-inbox", "m1", msg("uno", "c1", 5));
        g.put("F-inbox", "m2", msg("dos", "c2", 4));
        g.put("F-inbox", "m3", msg("tres", "c2", 3));
        g.put("F-arch", "m4", msg("cuatro", "c4", 60));
    }
    let h = Arc::clone(&state);
    let base = serve(move |req| handle(&h, req)).await;
    state.lock().unwrap().base = base.clone();

    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    let opts = SyncOptions {
        poll_interval: Duration::from_secs(3600),
        graph_inbox_poll: Duration::from_secs(3600),
        graph_base_url: format!("{base}/v1.0"),
        prefetch_recent: 1,
        ..Default::default()
    };
    let tokens: Arc<dyn TokenProvider> = Arc::new(FakeTokens);
    let sh = SyncEngine::start_with(
        Arc::clone(&store),
        vec![account()],
        Some(Arc::clone(&tokens)),
        Arc::new(MemoryStore::default()),
        opts,
    );
    sh.sync_now_wait("m365").await.unwrap();

    let acct = store.account_labels().unwrap()[0].id;
    let folders = store.folders(acct).unwrap();
    let role = |r: FolderRole| folders.iter().find(|f| f.role == r).unwrap().id;
    let (inbox, arch, trash) = (role(FolderRole::Inbox), role(FolderRole::Archive), role(FolderRole::Trash));
    assert_eq!(folders.len(), 7);
    assert!(folders.iter().any(|f| f.name == "Proyectos" && f.role == FolderRole::Custom));
    assert_eq!(store.count(inbox).unwrap().total, 3, "delta paginado completo");
    assert_eq!(store.count(arch).unwrap().total, 1);
    {
        let g = state.lock().unwrap();
        let tries =
            g.requests.iter().filter(|r| r.contains("F-inbox/messages/delta") && !r.contains("skiptoken")).count();
        assert!(tries >= 2, "429 con Retry-After y reintento: {:?}", g.requests);
    }
    let (m1, m2, m3) =
        (by_remote(&store, "m1").unwrap(), by_remote(&store, "m2").unwrap(), by_remote(&store, "m3").unwrap());
    let e2 = store.get_envelope(m2).unwrap().unwrap();
    assert_eq!(e2.thread, store.get_envelope(m3).unwrap().unwrap().thread, "conversationId une el hilo");
    assert_eq!(e2.preview, "Texto de dos");

    // MIME bajo demanda (m3, el más reciente, ya se precargó).
    assert!(store.get_envelope(m3).unwrap().unwrap().blob.is_some());
    sh.fetch_body(m1).await.unwrap();
    let raw = store.get_raw(m1).unwrap();
    assert!(String::from_utf8_lossy(&raw).contains("Cuerpo completo de uno"));

    // Cambios en el servidor: m1 leído, m2 borrado, m3 movido al archivo.
    {
        let mut g = state.lock().unwrap();
        let mut v = g.msgs["m1"].1.clone();
        v["isRead"] = json!(true);
        v["categories"] = json!(["Cliente"]);
        g.put("F-inbox", "m1", v);
        g.remove("m2");
        let v3 = g.msgs["m3"].1.clone();
        g.put("F-arch", "m3", v3);
    }
    sh.sync_now_wait("m365").await.unwrap();
    let e1 = store.get_envelope(m1).unwrap().unwrap();
    assert!(e1.flags.seen);
    assert_eq!(e1.keywords, vec!["Cliente".to_string()], "categorías → etiquetas");
    assert!(store.get_envelope(m2).unwrap().is_none(), "@removed y 404 → borrado");
    let e3 = store.get_envelope(m3).unwrap().unwrap();
    assert_eq!(e3.folder, arch, "@removed + sigue existiendo → movido, mismo mensaje local");
    assert!(e3.blob.is_some(), "conserva el cuerpo");

    // Operaciones locales → Graph.
    sh.set_flags(m1, Flags { seen: true, flagged: true, ..Default::default() }).unwrap();
    until("PATCH flag", || {
        state.lock().unwrap().patches.iter().any(|(id, p)| id == "m1" && p["flag"]["flagStatus"] == "flagged")
    })
    .await;
    sh.move_message(m1, arch).unwrap();
    until("move", || state.lock().unwrap().msgs.get("m1").is_some_and(|(f, _)| f == "F-arch")).await;
    let m4 = by_remote(&store, "m4").unwrap();
    sh.delete_message(m4).unwrap();
    until("permanentDelete", || !state.lock().unwrap().msgs.contains_key("m4")).await;
    sh.sync_now_wait("m365").await.unwrap();
    assert_eq!(store.get_envelope(m1).unwrap().unwrap().folder, arch);
    assert_eq!(store.count(inbox).unwrap().total, 0);
    let _ = trash;

    // Envío por Graph sendMail con Bcc para los ocultos.
    let client =
        GraphClient::new(account(), Arc::clone(&tokens), &format!("{base}/v1.0"), Duration::from_secs(10)).unwrap();
    let t = GraphSendTransport::new(client);
    assert!(t.saves_to_sent());
    let out = OutgoingMessage {
        envelope_from: "ana@contoso.com".into(),
        recipients: vec!["bob@x.com".into(), "oculto@x.com".into()],
        raw: b"From: ana@contoso.com\r\nTo: bob@x.com\r\nSubject: Hola\r\n\r\nQue tal\r\n".to_vec(),
    };
    t.send(&out).await.unwrap();
    let sent = state.lock().unwrap().sent.clone();
    assert_eq!(sent.len(), 1);
    assert!(sent[0].starts_with("Bcc: oculto@x.com\r\n"));
    assert!(sent[0].contains("Subject: Hola"));

    // Limitación persistente: se rinde con Throttled (la capa de arriba reintenta luego).
    let c = GraphClient::new(account(), tokens, &format!("{base}/v1.0"), Duration::from_secs(10))
        .unwrap()
        .with_retry(1, Duration::from_millis(10));
    let err = c.get_json(&c.url("/me/always429")).await.unwrap_err();
    assert!(matches!(err, GraphError::Throttled { retry_after: Some(0) }), "{err:?}");
    sh.shutdown().await;
}

/// Prueba real (opcional): `IUREDESK_GRAPH_TOKEN` con un token de acceso de Graph (Mail.ReadWrite).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "necesita un token real de Microsoft Graph (IUREDESK_GRAPH_TOKEN)"]
async fn live_graph_mail() {
    let Ok(token) = std::env::var("IUREDESK_GRAPH_TOKEN") else {
        eprintln!("sin IUREDESK_GRAPH_TOKEN: se omite");
        return;
    };
    struct Live(String);
    impl TokenProvider for Live {
        fn access_token<'a>(
            &'a self,
            _a: &'a Account,
            _aud: TokenAudience,
        ) -> BoxFuture<'a, Result<String, iuredesk_oauth::Error>> {
            let t = self.0.clone();
            Box::pin(async move { Ok(t) })
        }
        fn invalidate(&self, _account_id: &str) {}
    }
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    let o = SyncOptions { backfill: false, ..Default::default() };
    let sh = SyncEngine::start_with(
        Arc::clone(&store),
        vec![account()],
        Some(Arc::new(Live(token))),
        Arc::new(MemoryStore::default()),
        o,
    );
    sh.sync_now_wait("m365").await.unwrap();
    let acct = store.account_labels().unwrap()[0].id;
    for f in store.folders(acct).unwrap() {
        eprintln!("{:>8}  {:?} {}", store.count(f.id).unwrap().total, f.role, f.name);
    }
    sh.shutdown().await;
}
