//! Proveedor Google Tasks (`tasks/v1`): Google CalDAV no sirve `VTODO`, así que las tareas de
//! una cuenta de Google van por esta API (alcance `https://www.googleapis.com/auth/tasks`, token
//! de [`iuredesk_oauth::TokenAudience::Api`]).
//!
//! - Listas: `GET /users/@me/lists` → listas locales con ruta `gtasks:<id>` (el prefijo las separa
//!   de las colecciones CalDAV de la misma cuenta).
//! - Tareas: `GET /lists/{id}/tasks`; la primera vez completo (lo que no vuelve se borra) y luego
//!   incremental con `updatedMin` (guardado en `CollectionSync::sync_token`) y `showDeleted`.
//! - Subir: `POST` (nueva) o `PATCH` solo con lo cambiado; `DELETE`. Google no admite `If-Match`
//!   en Tasks: gana la última escritura.

use chrono::{Duration, SecondsFormat, Utc};
use iuredesk_graph::{Body, GraphClient, enc};
use iuredesk_model::{Task, TaskId, TaskList, TaskListId};
use iuredesk_store::{CollectionRef, CollectionSync, PimRef};
use reqwest::Method;
use serde_json::Value;

use crate::error::{PimError, Result};
use crate::map::gtasks as gmap;
use crate::ops::{self, PimOp};
use crate::provider::{BoxFuture, Ctx, Provider};
use crate::{ConflictResolution, PimEvent};

/// Raíz de la API de Google Tasks.
pub const GOOGLE_TASKS_BASE: &str = "https://tasks.googleapis.com/tasks/v1";
/// Prefijo de la ruta local (`TaskList::remote_path`) de las listas de Google Tasks.
pub const GOOGLE_TASKS_LIST_PREFIX: &str = "gtasks:";
pub(crate) const PREFIX: &str = GOOGLE_TASKS_LIST_PREFIX;
/// Tareas por página (máximo de la API).
const PAGE: usize = 100;
/// Margen de `updatedMin` (relojes distintos, escrituras durante la pasada).
const SKEW_SECS: i64 = 120;

pub(crate) struct GoogleTasks {
    client: GraphClient,
}

fn s<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str)
}

/// Id remoto de una lista local (`None` si no es de Google Tasks).
pub(crate) fn list_remote(l: &TaskList) -> Option<&str> {
    l.remote_path.strip_prefix(PREFIX).filter(|r| !r.is_empty())
}

impl GoogleTasks {
    pub fn new(ctx: &Ctx, base: &str) -> Result<Self> {
        let client = GraphClient::new(ctx.account.account.clone(), ctx.creds.tokens()?, base, ctx.opts.timeout)?
            .with_prefer("return=representation")
            .with_max_pages(ctx.opts.graph_max_pages);
        Ok(Self { client })
    }

    async fn get(&self, url: &str) -> Result<Value> {
        let r = self.client.request(Method::GET, url, None, &[]).await?;
        if !r.is_success() {
            return Err(r.error().into());
        }
        Ok(r.body)
    }

    /// Recorre `items` + `nextPageToken`.
    async fn pages(&self, ctx: &Ctx, path: &str) -> Result<Vec<Value>> {
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        for _ in 0..ctx.opts.graph_max_pages.max(1) {
            let sep = if path.contains('?') { '&' } else { '?' };
            let url = match &token {
                Some(t) => format!("{}{path}{sep}pageToken={}", self.client.base(), enc(t)),
                None => self.client.url(path),
            };
            let page = self.get(&url).await?;
            out.extend(page.get("items").and_then(Value::as_array).cloned().unwrap_or_default());
            match s(&page, "nextPageToken").filter(|t| !t.is_empty()) {
                Some(t) => token = Some(t.to_string()),
                None => return Ok(out),
            }
        }
        Ok(out)
    }

    /// Listas remotas → locales (alta, nombre, baja).
    async fn refresh(&self, ctx: &Ctx) -> Result<Vec<(TaskListId, String)>> {
        let store = &ctx.store;
        let acct = ctx.acct;
        let remote = self.pages(ctx, &format!("/users/@me/lists?maxResults={PAGE}")).await?;
        let local: Vec<TaskList> =
            store.task_lists()?.into_iter().filter(|l| l.account == Some(acct) && list_remote(l).is_some()).collect();
        let mut out = Vec::new();
        let mut changed = false;
        for l in &remote {
            let Some(id) = s(l, "id").filter(|i| !i.is_empty()) else { continue };
            let path = format!("{PREFIX}{id}");
            let old = local.iter().find(|x| x.remote_path == path);
            let new = TaskList {
                id: old.map_or(TaskListId::UNASSIGNED, |o| o.id),
                account: Some(acct),
                name: s(l, "title").filter(|t| !t.trim().is_empty()).unwrap_or("Tasks").chars().take(255).collect(),
                remote_path: path,
                sync_token: None,
            };
            let lid = if old != Some(&new) {
                changed = true;
                store.put_task_list(&new)?
            } else {
                new.id
            };
            out.push((lid, id.to_string()));
        }
        for l in local.iter().filter(|l| !out.iter().any(|(id, _)| *id == l.id)) {
            store.delete_task_list(l.id)?;
            changed = true;
        }
        if changed {
            ctx.emit(PimEvent::CollectionsChanged { account: acct });
        }
        Ok(out)
    }

    /// Cambios de una lista.
    async fn pull(&self, ctx: &Ctx, list: TaskListId, remote: &str) -> Result<()> {
        let store = &ctx.store;
        let cref = CollectionRef::TaskList(list);
        let mut st = store.collection_sync(cref)?;
        let started = Utc::now();
        let since = st.sync_token.clone();
        let mut path = format!("/lists/{}/tasks?maxResults={PAGE}&showCompleted=true&showHidden=true", enc(remote));
        if let Some(t) = &since {
            path.push_str(&format!("&showDeleted=true&updatedMin={}", enc(t)));
        }
        let items = self.pages(ctx, &path).await?;
        let pending = ops::pending(store, ctx.acct)?;
        let mut changed = false;
        let mut seen: Vec<String> = Vec::new();
        for v in &items {
            let Some(id) = s(v, "id").filter(|i| !i.is_empty()) else { continue };
            seen.push(id.to_string());
            if gmap::is_deleted(v) {
                changed |= remove_item(ctx, &mut st, &pending, id)?;
                continue;
            }
            if pending.deleted.contains(id) {
                continue;
            }
            changed |= apply_item(ctx, &mut st, &pending, list, id, v)?;
        }
        if since.is_none() {
            let gone: Vec<String> = st.items.keys().filter(|k| !seen.contains(k)).cloned().collect();
            for id in gone {
                changed |= remove_item(ctx, &mut st, &pending, &id)?;
            }
        }
        let mark = started - Duration::seconds(SKEW_SECS);
        st.sync_token = Some(mark.to_rfc3339_opts(SecondsFormat::Millis, true));
        store.put_collection_sync(cref, &st)?;
        if changed {
            ctx.changed(cref);
        }
        Ok(())
    }

    async fn push_task(&self, ctx: &Ctx, id: TaskId) -> Result<()> {
        let store = &ctx.store;
        let Some(local) = store.get_task(id)? else { return Ok(()) };
        let Some(list) = store.get_task_list(local.list)? else { return Ok(()) };
        let Some(remote_list) = list_remote(&list).map(str::to_string) else { return Ok(()) };
        let r = PimRef::Task(id);
        let base = store
            .raw_object_data(r)?
            .and_then(|(raw, data)| serde_json::from_slice::<Value>(&data).ok().map(|v| (raw, v)));
        let lpath = format!("/lists/{}/tasks", enc(&remote_list));
        let (method, url, body) = match &base {
            Some((raw, v)) => {
                let patch = gmap::task_patch(Some(&gmap::task_from_google(v)), &local);
                if patch.is_empty() {
                    return Ok(());
                }
                (Method::PATCH, self.client.url(&format!("{lpath}/{}", enc(&raw.href))), patch)
            }
            None => (Method::POST, self.client.url(&lpath), gmap::task_patch(None, &local)),
        };
        let body = Value::Object(body);
        let resp = self.client.request(method.clone(), &url, Some(Body::Json(&body)), &[]).await?;
        if resp.status == 404 && method == Method::PATCH {
            // Se borró en el servidor: gana el servidor.
            let href = base.map(|(raw, _)| raw.href).unwrap_or_default();
            let mut st = store.collection_sync(CollectionRef::TaskList(local.list))?;
            st.items.remove(&href);
            store.put_collection_sync(CollectionRef::TaskList(local.list), &st)?;
            store.delete_task(id)?;
            ctx.conflict(r, &local.summary, ConflictResolution::ServerWon);
            ctx.changed(CollectionRef::TaskList(local.list));
            return Ok(());
        }
        if !resp.is_success() {
            return Err(resp.error().into());
        }
        self.commit(ctx, &local, &resp.body)
    }

    /// Guarda la respuesta como base nueva. Si la tarea cambió durante la subida, conserva lo
    /// local y encola otra subida.
    fn commit(&self, ctx: &Ctx, pushed: &Task, server: &Value) -> Result<()> {
        let store = &ctx.store;
        let Some(remote_id) = s(server, "id").filter(|i| !i.is_empty()) else {
            return Err(PimError::Invalid("Google Tasks no devolvió el id".into()));
        };
        let etag = gmap::etag(server);
        let r = PimRef::Task(pushed.id);
        let current = store.get_task(pushed.id)?;
        let strip = |t: &Task| Task { remote_id: None, etag: None, updated: None, ..t.clone() };
        let edited = current.as_ref().is_some_and(|c| strip(c) != strip(pushed));
        let mut t = match current {
            Some(c) if edited => c,
            Some(c) => {
                let mut t = gmap::task_from_google(server);
                t.id = c.id;
                t.list = c.list;
                t.priority = c.priority;
                t.uid = c.uid;
                t.created = c.created;
                t
            }
            // Se borró en local durante la subida: el borrado ya está en la cola.
            None => return Ok(()),
        };
        t.remote_id = Some(remote_id.to_string());
        t.etag = etag.clone();
        store.put_task(&t)?;
        let bytes = serde_json::to_vec(server).map_err(|e| PimError::Invalid(e.to_string()))?;
        store.put_raw_object(r, &bytes, remote_id, etag.as_deref())?;
        let cref = CollectionRef::TaskList(t.list);
        let mut st = store.collection_sync(cref)?;
        let item = st.items.entry(remote_id.to_string()).or_default();
        item.etag = etag;
        if !item.refs.contains(&r) {
            item.refs.push(r);
        }
        store.put_collection_sync(cref, &st)?;
        if edited {
            let op = PimOp::PutTask { task: t.id };
            store.enqueue_op(ctx.acct, op.kind(), &op.encode())?;
        }
        ctx.changed(cref);
        Ok(())
    }

    async fn push_delete(&self, ctx: &Ctx, list: TaskListId, href: &str) -> Result<()> {
        let Some(l) = ctx.store.get_task_list(list)? else { return Ok(()) };
        let Some(remote_list) = list_remote(&l) else { return Ok(()) };
        let url = self.client.url(&format!("/lists/{}/tasks/{}", enc(remote_list), enc(href)));
        let resp = self.client.request(Method::DELETE, &url, None, &[]).await?;
        if resp.is_success() || matches!(resp.status, 404 | 410) {
            let cref = CollectionRef::TaskList(list);
            let mut st = ctx.store.collection_sync(cref)?;
            if st.items.remove(href).is_some() {
                ctx.store.put_collection_sync(cref, &st)?;
            }
            return Ok(());
        }
        Err(resp.error().into())
    }
}

fn apply_item(
    ctx: &Ctx,
    st: &mut CollectionSync,
    pending: &ops::Pending,
    list: TaskListId,
    id: &str,
    v: &Value,
) -> Result<bool> {
    let store = &ctx.store;
    let old_refs = st.items.get(id).map(|i| i.refs.clone()).unwrap_or_default();
    if old_refs.iter().any(|r| pending.dirty.contains(r)) {
        return Ok(false);
    }
    let old = old_refs.iter().find_map(|r| match r {
        PimRef::Task(i) => store.get_task(*i).ok().flatten(),
        _ => None,
    });
    let mut t = gmap::task_from_google(v);
    t.list = list;
    if let Some(o) = &old {
        // Google no guarda prioridad ni el `UID` iCalendar: se conservan los locales.
        t.id = o.id;
        t.priority = o.priority;
        t.created = o.created;
        if !o.uid.is_empty() {
            t.uid = o.uid.clone();
        }
        if Some(o) == Some(&t) {
            return Ok(false);
        }
    }
    let tid = store.put_task(&t)?;
    let r = PimRef::Task(tid);
    let bytes = serde_json::to_vec(v).map_err(|e| PimError::Invalid(e.to_string()))?;
    let etag = gmap::etag(v);
    store.put_raw_object(r, &bytes, id, etag.as_deref())?;
    let item = st.items.entry(id.to_string()).or_default();
    item.etag = etag;
    item.refs = vec![r];
    Ok(true)
}

fn remove_item(ctx: &Ctx, st: &mut CollectionSync, pending: &ops::Pending, id: &str) -> Result<bool> {
    let Some(item) = st.items.remove(id) else { return Ok(false) };
    for r in &item.refs {
        if pending.dirty.contains(r) {
            ctx.conflict(*r, id, ConflictResolution::ServerWon);
        }
        if let PimRef::Task(i) = r {
            ctx.store.delete_task(*i)?;
        }
    }
    Ok(!item.refs.is_empty())
}

impl Provider for GoogleTasks {
    fn sync<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            for (id, remote) in self.refresh(ctx).await? {
                self.pull(ctx, id, &remote).await?;
            }
            Ok(())
        })
    }

    fn push<'a>(&'a self, ctx: &'a Ctx, op: &'a PimOp) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            match op {
                PimOp::PutTask { task } => self.push_task(ctx, *task).await,
                PimOp::DeleteTask { list, href, .. } => self.push_delete(ctx, *list, href).await,
                // Calendario y contactos van por CalDAV/CardDAV.
                _ => Ok(()),
            }
        })
    }
}

/// CalDAV/CardDAV (calendario, contactos) + Google Tasks (tareas) de una cuenta de Google.
pub(crate) struct WithTasks {
    pub dav: Option<Box<dyn Provider>>,
    pub tasks: GoogleTasks,
}

fn is_task_op(op: &PimOp) -> bool {
    matches!(op, PimOp::PutTask { .. } | PimOp::DeleteTask { .. })
}

impl Provider for WithTasks {
    fn sync<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            // Un fallo de uno no deja sin sincronizar al otro.
            let dav = match &self.dav {
                Some(d) => d.sync(ctx).await,
                None => Ok(()),
            };
            if let Err(e) = &dav
                && e.needs_reauth()
                && !matches!(e, PimError::MissingAccess { .. })
            {
                return dav;
            }
            let tasks = self.tasks.sync(ctx).await;
            dav.and(tasks)
        })
    }

    fn push<'a>(&'a self, ctx: &'a Ctx, op: &'a PimOp) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if is_task_op(op) {
                // Una tarea de una lista CalDAV (no de Google) se queda en CalDAV.
                let google = match op {
                    PimOp::PutTask { task } => ctx
                        .store
                        .get_task(*task)?
                        .and_then(|t| ctx.store.get_task_list(t.list).ok().flatten())
                        .is_none_or(|l| list_remote(&l).is_some()),
                    PimOp::DeleteTask { list, .. } => {
                        ctx.store.get_task_list(*list)?.is_none_or(|l| list_remote(&l).is_some())
                    }
                    _ => true,
                };
                if google {
                    return self.tasks.push(ctx, op).await;
                }
            }
            match &self.dav {
                Some(d) => d.push(ctx, op).await,
                None => Ok(()),
            }
        })
    }
}
