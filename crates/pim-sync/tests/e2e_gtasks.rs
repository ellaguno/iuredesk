//! Extremo a extremo contra una API de Google Tasks falsa: listas, alta completa paginada,
//! incremental con `updatedMin` + `showDeleted`, y subidas locales primero (completar = `PATCH`
//! solo con el estado, nueva = `POST`, borrar = `DELETE`).

#[path = "../../dav/tests/common/mod.rs"]
mod common;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use common::{Req, Resp, serve};
use iuredesk_accounts::MemoryStore;
use iuredesk_model::account::{Account, AccountKind, AuthMethod, OAuthProvider, Protocol, Security, ServerConfig};
use iuredesk_model::{EventTime, Task, TaskStatus};
use iuredesk_oauth::{BoxFuture, TokenAudience, TokenProvider};
use iuredesk_pim_sync::{DavAuth, PimAccount, PimSource, PimSyncEngine, PimSyncOptions};
use iuredesk_store::Store;
use serde_json::{Value, json};

struct FakeTokens;

impl TokenProvider for FakeTokens {
    fn access_token<'a>(
        &'a self,
        _a: &'a Account,
        aud: TokenAudience,
    ) -> BoxFuture<'a, Result<String, iuredesk_oauth::Error>> {
        Box::pin(async move {
            assert_eq!(aud, TokenAudience::Api);
            Ok("tok-google".to_string())
        })
    }
    fn invalidate(&self, _account_id: &str) {}
}

#[derive(Default)]
struct G {
    tasks: BTreeMap<String, Value>,
    next: u32,
    requests: Vec<String>,
    patches: Vec<(String, Value)>,
}

fn now() -> String {
    Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

fn task(id: &str, title: &str, done: bool) -> Value {
    json!({
        "kind": "tasks#task", "id": id, "etag": format!("\"{id}-1\""), "title": title,
        "notes": "", "status": if done { "completed" } else { "needsAction" },
        "due": "2026-11-03T00:00:00.000Z", "updated": now(),
    })
}

fn query(path: &str, key: &str) -> Option<String> {
    let q = path.split_once('?')?.1;
    q.split('&').find_map(|kv| kv.strip_prefix(&format!("{key}=")).map(|v| v.replace("%3A", ":")))
}

fn route(st: &Mutex<G>, req: Req) -> Resp {
    let mut g = st.lock().unwrap();
    assert_eq!(req.header("authorization"), Some("Bearer tok-google"));
    g.requests.push(format!("{} {}", req.method, req.path));
    let path = req.path.split('?').next().unwrap_or_default().to_string();
    let json_resp = |v: Value| Resp::new(200, v.to_string()).with("Content-Type", "application/json");
    match (req.method.as_str(), path.as_str()) {
        ("GET", "/tasks/v1/users/@me/lists") | ("GET", "/tasks/v1/users/%40me/lists") => {
            json_resp(json!({ "items": [{ "id": "L1", "title": "Mis tareas" }] }))
        }
        ("GET", "/tasks/v1/lists/L1/tasks") => {
            let since = query(&req.path, "updatedMin").and_then(|t| DateTime::parse_from_rfc3339(&t).ok());
            if since.is_some() {
                assert_eq!(query(&req.path, "showDeleted").as_deref(), Some("true"));
            }
            let all: Vec<Value> = g
                .tasks
                .values()
                .filter(|t| match since {
                    Some(s) => DateTime::parse_from_rfc3339(t["updated"].as_str().unwrap()).unwrap() >= s,
                    None => !t.get("deleted").and_then(Value::as_bool).unwrap_or(false),
                })
                .cloned()
                .collect();
            // Páginas de dos.
            let start: usize = query(&req.path, "pageToken").and_then(|p| p.parse().ok()).unwrap_or(0);
            let page: Vec<Value> = all.iter().skip(start).take(2).cloned().collect();
            let mut body = json!({ "items": page });
            if start + 2 < all.len() {
                body["nextPageToken"] = json!((start + 2).to_string());
            }
            json_resp(body)
        }
        ("POST", "/tasks/v1/lists/L1/tasks") => {
            g.next += 1;
            let id = format!("n{}", g.next);
            let mut v: Value = serde_json::from_str(&req.body).unwrap();
            v["id"] = json!(id);
            v["etag"] = json!(format!("\"{id}-1\""));
            v["updated"] = json!(now());
            g.tasks.insert(id, v.clone());
            json_resp(v)
        }
        (m, p) if p.starts_with("/tasks/v1/lists/L1/tasks/") => {
            let id = p.trim_start_matches("/tasks/v1/lists/L1/tasks/").to_string();
            let Some(cur) = g.tasks.get(&id).cloned() else { return Resp::new(404, "{}") };
            match m {
                "PATCH" => {
                    let patch: Value = serde_json::from_str(&req.body).unwrap();
                    g.patches.push((id.clone(), patch.clone()));
                    let mut v = cur;
                    for (k, val) in patch.as_object().unwrap() {
                        v[k] = val.clone();
                    }
                    v["updated"] = json!(now());
                    g.tasks.insert(id, v.clone());
                    json_resp(v)
                }
                "DELETE" => {
                    let mut v = cur;
                    v["deleted"] = json!(true);
                    v["updated"] = json!(now());
                    g.tasks.insert(id, v);
                    Resp::new(204, "")
                }
                _ => Resp::new(405, "{}"),
            }
        }
        _ => Resp::new(404, "{}"),
    }
}

fn account() -> Account {
    let srv = |p| ServerConfig {
        protocol: p,
        host: "imap.gmail.com".into(),
        port: 993,
        security: Security::Tls,
        auth: AuthMethod::OAuth2 { provider: OAuthProvider::Google },
        username: "ana@gmail.com".into(),
    };
    Account {
        id: "ana-gmail".into(),
        display_name: "Ana".into(),
        email: "ana@gmail.com".into(),
        kind: AccountKind::Gmail,
        incoming: srv(Protocol::Imap),
        outgoing: srv(Protocol::Smtp),
        caldav_url: None,
        carddav_url: None,
        identities: vec![],
    }
}

fn tasks(store: &Store) -> Vec<Task> {
    let list = store.task_lists().unwrap().into_iter().find(|l| l.remote_path == "gtasks:L1").unwrap();
    let mut v = store.tasks(list.id).unwrap();
    v.sort_by(|a, b| a.summary.cmp(&b.summary));
    v
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn google_tasks_full_incremental_and_local_first() {
    let state = Arc::new(Mutex::new(G::default()));
    {
        let mut g = state.lock().unwrap();
        for (id, title, done) in
            [("t1", "Pagar renta", false), ("t2", "Llamar al notario", true), ("t3", "Café", false)]
        {
            g.tasks.insert(id.into(), task(id, title, done));
        }
    }
    let h = Arc::clone(&state);
    let base = serve(move |req| route(&h, req)).await;

    // Por omisión una cuenta de Google lleva la API de Tasks.
    assert_eq!(
        PimAccount::from_account(&account()).unwrap().tasks_api.as_deref(),
        Some(iuredesk_pim_sync::GOOGLE_TASKS_BASE)
    );
    let pim = PimAccount {
        account: account(),
        source: PimSource::Dav { caldav: None, carddav: None, auth: DavAuth::OAuth },
        tasks_api: Some(format!("{base}/tasks/v1")),
    };
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    let opts = PimSyncOptions { poll_interval: Duration::from_secs(3600), sync_on_start: false, ..Default::default() };
    let tokens: Arc<dyn TokenProvider> = Arc::new(FakeTokens);
    let handle =
        PimSyncEngine::start(Arc::clone(&store), vec![pim], Some(tokens), Arc::new(MemoryStore::default()), opts);
    handle.sync_now_wait("ana-gmail").await.unwrap();

    // --- completo (dos páginas)
    let list = store.task_lists().unwrap().into_iter().find(|l| l.remote_path == "gtasks:L1").unwrap();
    assert_eq!(list.name, "Mis tareas");
    let acct = handle.account_id("ana-gmail").unwrap();
    assert_eq!(list.account, Some(acct));
    let ts = tasks(&store);
    assert_eq!(
        ts.iter().map(|t| t.summary.as_str()).collect::<Vec<_>>(),
        vec!["Café", "Llamar al notario", "Pagar renta"]
    );
    assert!(ts[1].is_done());
    assert_eq!(ts[2].due, Some(EventTime::Date(chrono::NaiveDate::from_ymd_opt(2026, 11, 3).unwrap())));
    assert!(state.lock().unwrap().requests.iter().any(|r| r.contains("pageToken=2")));

    // --- incremental: t3 borrado en el servidor, t1 cambiado
    {
        let mut g = state.lock().unwrap();
        g.requests.clear();
        let t3 = g.tasks.get_mut("t3").unwrap();
        t3["deleted"] = json!(true);
        t3["updated"] = json!(now());
        let t1 = g.tasks.get_mut("t1").unwrap();
        t1["title"] = json!("Pagar la renta");
        t1["updated"] = json!(now());
    }
    handle.sync_now_wait("ana-gmail").await.unwrap();
    assert!(state.lock().unwrap().requests.iter().any(|r| r.contains("updatedMin=") && r.contains("showDeleted=true")));
    let ts = tasks(&store);
    assert_eq!(ts.iter().map(|t| t.summary.as_str()).collect::<Vec<_>>(), vec!["Llamar al notario", "Pagar la renta"]);

    // --- local primero: completar t1 (PATCH solo del estado), nueva (POST), borrar t2 (DELETE)
    let mut t1 = ts[1].clone();
    t1.status = TaskStatus::Completed;
    t1.completed = Some(Utc::now());
    t1.percent_complete = 100;
    handle.save_task(&t1).unwrap();
    let new = Task { list: list.id, summary: "Preparar escrito".into(), ..Default::default() };
    let new_id = handle.save_task(&new).unwrap();
    handle.delete_task(ts[0].id).unwrap();
    handle.sync_now_wait("ana-gmail").await.unwrap();

    let id = {
        let g = state.lock().unwrap();
        let (pid, patch) = g.patches.iter().find(|(id, _)| id == "t1").unwrap();
        assert_eq!(pid, "t1");
        assert_eq!(patch["status"], json!("completed"));
        assert!(patch.get("title").is_none(), "solo lo cambiado: {patch}");
        assert_eq!(g.tasks["t2"]["deleted"], json!(true));
        let created = g.tasks.values().find(|t| t["title"] == json!("Preparar escrito")).unwrap();
        assert_eq!(created["status"], json!("needsAction"));
        created["id"].as_str().unwrap().to_string()
    };
    let local = store.get_task(new_id).unwrap().unwrap();
    assert_eq!(local.remote_id.as_deref(), Some(id.as_str()));
    let ts = tasks(&store);
    assert_eq!(ts.iter().map(|t| t.summary.as_str()).collect::<Vec<_>>(), vec!["Pagar la renta", "Preparar escrito"]);
    assert!(ts[0].is_done());
    // La cola quedó vacía (nada se reintenta).
    assert!(store.peek_ops(100).unwrap().is_empty());
    handle.shutdown().await;
}
