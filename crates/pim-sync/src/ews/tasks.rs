//! Tareas por EWS (F8.4, hito 3): la carpeta de tareas del buzón como una lista, con los mismos
//! pasos que los contactos (cambios incrementales con `SyncFolderItems`, subir solo lo cambiado
//! con `ChangeKey`, fusión a tres bandas si cambió en el servidor).

use iuredesk_model::{Task, TaskId, TaskList, TaskListId};
use iuredesk_store::{CollectionRef, PimRef};
use serde_json::Value;

use super::{Ews, raw_json};
use crate::error::{PimError, Result};
use crate::map::ews::task as tmap;
use crate::map::graph as gmap;
use crate::merge;
use crate::ops::{self, PimOp};
use crate::provider::Ctx;
use crate::{ConflictResolution, PimEvent};

/// Ruta local (y carpeta distinguida de EWS) de la lista.
pub(crate) const TASKS: &str = "tasks";

impl Ews {
    /// La lista de tareas de la cuenta (se crea la primera vez).
    pub(crate) fn task_list(&self, ctx: &Ctx) -> Result<TaskListId> {
        let store = &ctx.store;
        if let Some(l) = store.task_lists()?.into_iter().find(|l| l.account == Some(ctx.acct) && l.remote_path == TASKS)
        {
            return Ok(l.id);
        }
        let id = store.put_task_list(&TaskList {
            id: TaskListId::UNASSIGNED,
            account: Some(ctx.acct),
            name: if ctx.opts.spanish { "Tareas".into() } else { "Tasks".into() },
            remote_path: TASKS.into(),
            sync_token: None,
        })?;
        ctx.emit(PimEvent::CollectionsChanged { account: ctx.acct });
        Ok(id)
    }

    /// Cambios de la carpeta de tareas desde la pasada anterior.
    pub(crate) async fn pull_tasks(&self, ctx: &Ctx, list: TaskListId) -> Result<()> {
        let store = &ctx.store;
        let cref = CollectionRef::TaskList(list);
        let mut st = store.collection_sync(cref)?;
        let changes = self.folder_changes(TASKS, st.delta_link.as_deref()).await?;
        let pending = ops::pending(store, ctx.acct)?;
        let mut changed = false;
        let wanted: Vec<String> = changes.upserts.iter().filter(|id| !pending.deleted.contains(*id)).cloned().collect();
        for e in self.client.get_tasks(&wanted).await? {
            let Some(v) = tmap::task_json(&e, ctx.opts.user_tz) else { continue };
            let Some(id) = v.get("id").and_then(Value::as_str).map(str::to_string) else { continue };
            changed |= apply_task(ctx, &mut st, &pending, list, &id, &v)?;
        }
        changed |= changes.finish(ctx, &mut st, &pending)?;
        store.put_collection_sync(cref, &st)?;
        if changed {
            ctx.changed(cref);
        }
        Ok(())
    }

    async fn fetch_task(&self, ctx: &Ctx, id: &str) -> Result<Option<Value>> {
        Ok(self.client.get_task(id).await?.and_then(|e| tmap::task_json(&e, ctx.opts.user_tz)))
    }

    pub(crate) async fn push_task(&self, ctx: &Ctx, id: TaskId) -> Result<()> {
        let store = &ctx.store;
        let Some(local) = store.get_task(id)? else { return Ok(()) };
        let r = PimRef::Task(id);
        let tz = ctx.opts.user_tz;
        let saved = raw_json(store, r);
        let result = match &saved {
            Some((raw, base_json)) => {
                let base = gmap::task_from_graph(base_json);
                let updates = tmap::update_xml(&gmap::task_patch(Some(&base), &local), tz);
                if updates.is_empty() {
                    return Ok(());
                }
                match self.client.update_item(&raw.href, raw.etag.as_deref(), &updates).await {
                    Err(e) if iuredesk_ews::calendar::is_conflict(&e) => {
                        return self.resolve_task(ctx, local, base).await;
                    }
                    Err(e) if e.is_not_found() => {
                        store.delete_task(id)?;
                        ctx.conflict(r, &local.summary, ConflictResolution::ServerWon);
                        ctx.changed(CollectionRef::TaskList(local.list));
                        return Ok(());
                    }
                    other => other?,
                }
            }
            None => {
                let xml = tmap::create_xml(&gmap::task_patch(None, &local), tz);
                self.client.create_task(TASKS, &xml).await?
            }
        };
        let Some(server) = self.fetch_task(ctx, &result.0).await? else {
            return Err(PimError::Invalid("Exchange no devolvió la tarea guardada".into()));
        };
        commit_task(ctx, &local, &server)
    }

    async fn resolve_task(&self, ctx: &Ctx, local: Task, base: Task) -> Result<()> {
        let r = PimRef::Task(local.id);
        let Some(remote_id) = local.remote_id.clone() else { return Err(PimError::Conflict) };
        let Some(sj) = self.fetch_task(ctx, &remote_id).await? else {
            ctx.store.delete_task(local.id)?;
            ctx.conflict(r, &local.summary, ConflictResolution::ServerWon);
            ctx.changed(CollectionRef::TaskList(local.list));
            return Ok(());
        };
        let mut server = gmap::task_from_graph(&sj);
        server.id = local.id;
        server.list = local.list;
        match merge::merge_task(&base, &local, &server) {
            Some(m) => {
                let updates = tmap::update_xml(&gmap::task_patch(Some(&server), &m), ctx.opts.user_tz);
                let ck = gmap::etag(&sj).filter(|e| !e.is_empty());
                let id = if updates.is_empty() {
                    remote_id.clone()
                } else {
                    self.client
                        .update_item(&remote_id, ck.as_deref(), &updates)
                        .await
                        .map_err(
                            |e| if iuredesk_ews::calendar::is_conflict(&e) { PimError::Conflict } else { e.into() },
                        )?
                        .0
                };
                ctx.store.put_task(&m)?;
                let Some(after) = self.fetch_task(ctx, &id).await? else { return Err(PimError::Conflict) };
                commit_task(ctx, &m, &after)?;
                ctx.conflict(r, &m.summary, ConflictResolution::Merged);
            }
            None => {
                commit_task(ctx, &local, &sj)?;
                ctx.store.put_task(&server)?;
                ctx.conflict(r, &server.summary, ConflictResolution::ServerWon);
            }
        }
        Ok(())
    }

    pub(crate) async fn push_delete_task(&self, ctx: &Ctx, list: TaskListId, href: &str) -> Result<()> {
        self.client.delete_item(href, false).await?;
        let cref = CollectionRef::TaskList(list);
        let mut st = ctx.store.collection_sync(cref)?;
        if st.items.remove(href).is_some() {
            ctx.store.put_collection_sync(cref, &st)?;
        }
        Ok(())
    }
}

fn apply_task(
    ctx: &Ctx,
    st: &mut iuredesk_store::CollectionSync,
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
    let mut t = gmap::task_from_graph(v);
    t.id = old_refs
        .iter()
        .find_map(|r| match r {
            PimRef::Task(i) => Some(*i),
            _ => None,
        })
        .unwrap_or(TaskId::UNASSIGNED);
    t.list = list;
    let r = PimRef::Task(store.put_task(&t)?);
    let bytes = serde_json::to_vec(v).map_err(|e| PimError::Invalid(e.to_string()))?;
    let etag = gmap::etag(v).filter(|e| !e.is_empty());
    store.put_raw_object(r, &bytes, id, etag.as_deref())?;
    let item = st.items.entry(id.to_string()).or_default();
    item.etag = etag;
    item.refs = vec![r];
    Ok(true)
}

fn commit_task(ctx: &Ctx, pushed: &Task, server: &Value) -> Result<()> {
    let store = &ctx.store;
    let r = PimRef::Task(pushed.id);
    let cref = CollectionRef::TaskList(pushed.list);
    let Some(remote_id) = server.get("id").and_then(Value::as_str) else {
        return Err(PimError::Invalid("Exchange no devolvió el id".into()));
    };
    let etag = gmap::etag(server).filter(|e| !e.is_empty());
    let current = store.get_task(pushed.id)?;
    let strip = |t: &Task| Task { remote_id: None, etag: None, updated: None, uid: String::new(), ..t.clone() };
    let edited = current.as_ref().is_some_and(|c| strip(c) != strip(pushed));
    let t = match (edited, current) {
        (true, Some(mut c)) => {
            c.remote_id = Some(remote_id.to_string());
            c.etag = etag.clone();
            c
        }
        _ => {
            let mut t = gmap::task_from_graph(server);
            t.id = pushed.id;
            t.list = pushed.list;
            t
        }
    };
    store.put_task(&t)?;
    let bytes = serde_json::to_vec(server).map_err(|e| PimError::Invalid(e.to_string()))?;
    store.put_raw_object(r, &bytes, remote_id, etag.as_deref())?;
    let mut st = store.collection_sync(cref)?;
    let item = st.items.entry(remote_id.to_string()).or_default();
    item.etag = etag;
    if !item.refs.contains(&r) {
        item.refs.push(r);
    }
    store.put_collection_sync(cref, &st)?;
    if edited {
        let op = PimOp::PutTask { task: pushed.id };
        store.enqueue_op(ctx.acct, op.kind(), &op.encode())?;
    }
    ctx.changed(cref);
    Ok(())
}
