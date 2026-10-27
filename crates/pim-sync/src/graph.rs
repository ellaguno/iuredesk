//! Proveedor Microsoft Graph: calendarios, contactos y To Do (Microsoft 365 / Outlook.com).

use chrono::{Duration, Utc};
use iuredesk_graph::{Body, GraphClient};
use iuredesk_model::{
    AddressBook, AddressBookId, Calendar, CalendarId, Contact, ContactId, Event, EventId, Task, TaskId, TaskList,
    TaskListId,
};
use iuredesk_store::{CollectionRef, CollectionSync, PimRef, Store};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

use crate::error::{PimError, Result};
use crate::map::graph as gmap;
use crate::merge;
use crate::ops::{self, PimOp};
use crate::provider::{BoxFuture, Ctx, Provider};
use crate::{ConflictResolution, PimEvent};

/// Ruta local de la libreta única de contactos de Graph.
const CONTACTS_PATH: &str = "contacts";

/// Proveedor Graph sobre el cliente compartido de `iuredesk-graph` (tokens, 401, 429 con
/// `Retry-After`, tamaños acotados).
pub(crate) struct Graph {
    client: GraphClient,
}

/// Respuesta de Graph.
struct Resp {
    status: StatusCode,
    body: Value,
}

impl Graph {
    pub fn new(ctx: &Ctx, base: &str) -> Result<Self> {
        let client = GraphClient::new(ctx.account.account.clone(), ctx.creds.tokens()?, base, ctx.opts.timeout)?
            .with_prefer("outlook.timezone=\"UTC\", odata.maxpagesize=100")
            .with_max_pages(ctx.opts.graph_max_pages);
        Ok(Self { client })
    }

    fn url(&self, path: &str) -> String {
        self.client.url(path)
    }

    /// Los enlaces `@odata.nextLink`/`deltaLink` solo se siguen dentro de la misma raíz (el token
    /// no debe viajar a otro sitio).
    fn same_origin(&self, link: &str) -> bool {
        self.client.same_origin(link)
    }

    async fn request(
        &self,
        _ctx: &Ctx,
        method: Method,
        url: &str,
        body: Option<&Value>,
        if_match: Option<&str>,
    ) -> Result<Resp> {
        let headers: Vec<(&str, &str)> = if_match.map(|e| vec![("If-Match", e)]).unwrap_or_default();
        let r = self.client.request(method, url, body.map(Body::Json), &headers).await?;
        let status = StatusCode::from_u16(r.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        Ok(Resp { status, body: r.body })
    }

    fn error(resp: &Resp, what: &str) -> PimError {
        let code = resp.body.get("error").and_then(|e| e.get("code")).and_then(Value::as_str).unwrap_or_default();
        PimError::Graph { status: resp.status.as_u16(), detail: format!("{what}: {code}") }
    }

    async fn get_json(&self, ctx: &Ctx, url: &str) -> Result<Value> {
        let r = self.request(ctx, Method::GET, url, None, None).await?;
        if !r.status.is_success() {
            return Err(Self::error(&r, "GET"));
        }
        Ok(r.body)
    }

    /// Lista paginada (`value` + `@odata.nextLink`).
    async fn list(&self, ctx: &Ctx, path: &str) -> Result<Vec<Value>> {
        let mut out = Vec::new();
        let mut next = Some(self.url(path));
        let mut pages = 0usize;
        while let Some(url) = next.take() {
            let page = self.get_json(ctx, &url).await?;
            out.extend(page.get("value").and_then(Value::as_array).cloned().unwrap_or_default());
            pages = pages.saturating_add(1);
            next = page
                .get("@odata.nextLink")
                .and_then(Value::as_str)
                .filter(|l| self.same_origin(l) && pages < ctx.opts.graph_max_pages)
                .map(str::to_string);
        }
        Ok(out)
    }

    /// Recorre un delta desde `start` (o el `deltaLink` guardado). Devuelve los objetos y el
    /// `deltaLink` nuevo. Un `deltaLink` caducado (410) vuelve a empezar desde `start`.
    async fn delta(&self, ctx: &Ctx, saved: Option<&str>, start: &str) -> Result<(Vec<Value>, Option<String>, bool)> {
        let mut full = saved.is_none();
        let mut url = saved.filter(|l| self.same_origin(l)).map_or_else(|| self.url(start), str::to_string);
        let mut items = Vec::new();
        for _ in 0..ctx.opts.graph_max_pages.max(1) {
            let r = self.request(ctx, Method::GET, &url, None, None).await?;
            if r.status == StatusCode::GONE && !full {
                full = true;
                items.clear();
                url = self.url(start);
                continue;
            }
            if !r.status.is_success() {
                return Err(Self::error(&r, "delta"));
            }
            items.extend(r.body.get("value").and_then(Value::as_array).cloned().unwrap_or_default());
            if let Some(next) = r.body.get("@odata.nextLink").and_then(Value::as_str).filter(|l| self.same_origin(l)) {
                url = next.to_string();
                continue;
            }
            let delta = r.body.get("@odata.deltaLink").and_then(Value::as_str).map(str::to_string);
            return Ok((items, delta, full));
        }
        // Demasiadas páginas: se sigue en la próxima pasada desde la última página pedida.
        Ok((items, Some(url), full))
    }

    // -------------------------------------------------------------- colecciones

    async fn refresh(
        &self,
        ctx: &Ctx,
    ) -> Result<(Vec<(CalendarId, String)>, AddressBookId, Vec<(TaskListId, String)>)> {
        let store = &ctx.store;
        let acct = ctx.acct;
        let mut changed = false;

        let remote_cals = self.list(ctx, "/me/calendars").await?;
        let local_cals: Vec<Calendar> = store.calendars()?.into_iter().filter(|c| c.account == acct).collect();
        let mut cals = Vec::new();
        for c in &remote_cals {
            let Some(id) = c.get("id").and_then(Value::as_str) else { continue };
            let old = local_cals.iter().find(|x| x.remote_path == id);
            let color = c
                .get("hexColor")
                .and_then(Value::as_str)
                .and_then(|h| u32::from_str_radix(h.trim_start_matches('#').get(..6)?, 16).ok());
            let new = Calendar {
                id: old.map_or(CalendarId::UNASSIGNED, |o| o.id),
                account: acct,
                name: c.get("name").and_then(Value::as_str).unwrap_or("Calendar").to_string(),
                color: color.or(old.and_then(|o| o.color)),
                remote_path: id.to_string(),
                read_only: !c.get("canEdit").and_then(Value::as_bool).unwrap_or(true),
                sync_token: None,
            };
            let lid = if old != Some(&new) {
                changed = true;
                store.put_calendar(&new)?
            } else {
                new.id
            };
            cals.push((lid, id.to_string()));
        }
        for c in
            local_cals.iter().filter(|c| !c.remote_path.is_empty() && !cals.iter().any(|(_, r)| *r == c.remote_path))
        {
            store.delete_calendar(c.id)?;
            changed = true;
        }

        let book =
            match store.address_books()?.into_iter().find(|b| b.account == acct && b.remote_path == CONTACTS_PATH) {
                Some(b) => b.id,
                None => {
                    changed = true;
                    store.put_address_book(&AddressBook {
                        account: acct,
                        name: "Contacts".into(),
                        remote_path: CONTACTS_PATH.into(),
                        ..Default::default()
                    })?
                }
            };

        let remote_lists = self.list(ctx, "/me/todo/lists").await?;
        let local_lists: Vec<TaskList> = store.task_lists()?.into_iter().filter(|l| l.account == Some(acct)).collect();
        let mut lists = Vec::new();
        for l in &remote_lists {
            let Some(id) = l.get("id").and_then(Value::as_str) else { continue };
            let old = local_lists.iter().find(|x| x.remote_path == id);
            let new = TaskList {
                id: old.map_or(TaskListId::UNASSIGNED, |o| o.id),
                account: Some(acct),
                name: l.get("displayName").and_then(Value::as_str).unwrap_or("Tasks").to_string(),
                remote_path: id.to_string(),
                sync_token: None,
            };
            let lid = if old != Some(&new) {
                changed = true;
                store.put_task_list(&new)?
            } else {
                new.id
            };
            lists.push((lid, id.to_string()));
        }
        for l in
            local_lists.iter().filter(|l| !l.remote_path.is_empty() && !lists.iter().any(|(_, r)| *r == l.remote_path))
        {
            store.delete_task_list(l.id)?;
            changed = true;
        }
        if changed {
            ctx.emit(PimEvent::CollectionsChanged { account: acct });
        }
        Ok((cals, book, lists))
    }

    // -------------------------------------------------------------- bajar

    async fn pull(&self, ctx: &Ctx, cref: CollectionRef, start: &str, apply: Apply) -> Result<()> {
        let store = &ctx.store;
        let mut st = store.collection_sync(cref)?;
        let (items, delta, full) = self.delta(ctx, st.delta_link.as_deref(), start).await?;
        let pending = ops::pending(store, ctx.acct)?;
        let mut changed = false;
        let mut seen: Vec<String> = Vec::new();
        for v in &items {
            let Some(id) = v.get("id").and_then(Value::as_str) else { continue };
            seen.push(id.to_string());
            if gmap::is_removed(v) {
                changed |= remove_item(ctx, &mut st, &pending, id)?;
                continue;
            }
            if pending.deleted.contains(id) {
                continue;
            }
            changed |= apply_item(ctx, &mut st, &pending, cref, id, v, apply)?;
        }
        if full {
            // Desde cero: lo local que no volvió ya no existe (o salió de la ventana).
            let gone: Vec<String> = st.items.keys().filter(|k| !seen.contains(k)).cloned().collect();
            for id in gone {
                changed |= remove_item(ctx, &mut st, &pending, &id)?;
            }
        }
        st.delta_link = delta;
        store.put_collection_sync(cref, &st)?;
        if changed {
            ctx.changed(cref);
        }
        Ok(())
    }

    // -------------------------------------------------------------- subir

    async fn push_event(&self, ctx: &Ctx, id: EventId) -> Result<()> {
        let store = &ctx.store;
        let Some(local) = store.get_event(id)? else { return Ok(()) };
        let Some(cal) = store.get_calendar(local.calendar)? else { return Ok(()) };
        if cal.read_only {
            return Err(PimError::ReadOnly);
        }
        let r = PimRef::Event(id);
        let base = raw_json(store, r).map(|(raw, v)| (raw, gmap::event_from_graph(&v)));
        let (url, method, body, etag, base) = match base {
            Some((raw, b)) => {
                let patch = gmap::event_patch(Some(&b), &local);
                if patch.is_empty() {
                    return Ok(());
                }
                (self.url(&format!("/me/events/{}", enc(&raw.href))), Method::PATCH, patch, raw.etag, Some(b))
            }
            None => (
                self.url(&format!("/me/calendars/{}/events", enc(&cal.remote_path))),
                Method::POST,
                gmap::event_patch(None, &local),
                None,
                None,
            ),
        };
        let resp = self.request(ctx, method, &url, Some(&Value::Object(body)), etag.as_deref()).await?;
        if resp.status == StatusCode::PRECONDITION_FAILED {
            return self.resolve_event(ctx, local, base).await;
        }
        if resp.status == StatusCode::NOT_FOUND && base.is_some() {
            store.delete_event(id)?;
            ctx.conflict(r, &local.summary, ConflictResolution::ServerWon);
            ctx.changed(CollectionRef::Calendar(local.calendar));
            return Ok(());
        }
        if !resp.status.is_success() {
            return Err(Self::error(&resp, "evento"));
        }
        self.commit(
            ctx,
            CollectionRef::Calendar(local.calendar),
            r,
            &resp.body,
            |v| {
                let mut e = gmap::event_from_graph(v);
                e.id = local.id;
                e.calendar = local.calendar;
                Doc::Event(e)
            },
            Doc::Event(local.clone()),
        )
    }

    async fn resolve_event(&self, ctx: &Ctx, local: Event, base: Option<Event>) -> Result<()> {
        let store = &ctx.store;
        let r = PimRef::Event(local.id);
        let Some(remote_id) = local.remote_id.clone() else { return Err(PimError::Conflict) };
        let server_json = self.get_json(ctx, &self.url(&format!("/me/events/{}", enc(&remote_id)))).await?;
        let mut server = gmap::event_from_graph(&server_json);
        server.id = local.id;
        server.calendar = local.calendar;
        let cref = CollectionRef::Calendar(local.calendar);
        match base.and_then(|b| merge::merge_event(&b, &local, &server)) {
            Some(m) => {
                let patch = gmap::event_patch(Some(&server), &m);
                let url = self.url(&format!("/me/events/{}", enc(&remote_id)));
                let resp = self
                    .request(ctx, Method::PATCH, &url, Some(&Value::Object(patch)), gmap::etag(&server_json).as_deref())
                    .await?;
                if !resp.status.is_success() {
                    return Err(PimError::Conflict);
                }
                // La fusión es el estado local nuevo (si no, `commit` la tomaría por una edición
                // hecha durante la subida).
                store.put_event(&m)?;
                self.commit(
                    ctx,
                    cref,
                    r,
                    &resp.body,
                    |v| {
                        let mut e = gmap::event_from_graph(v);
                        e.id = m.id;
                        e.calendar = m.calendar;
                        Doc::Event(e)
                    },
                    Doc::Event(m.clone()),
                )?;
                ctx.conflict(r, &m.summary, ConflictResolution::Merged);
            }
            None => {
                self.commit(ctx, cref, r, &server_json, |_| Doc::Event(server.clone()), Doc::Event(local.clone()))?;
                ctx.conflict(r, &server.summary, ConflictResolution::ServerWon);
            }
        }
        Ok(())
    }

    async fn push_contact(&self, ctx: &Ctx, id: ContactId) -> Result<()> {
        let store = &ctx.store;
        let Some(local) = store.get_contact(id)? else { return Ok(()) };
        let r = PimRef::Contact(id);
        let base = raw_json(store, r).map(|(raw, v)| (raw, gmap::contact_from_graph(&v)));
        let (url, method, body, etag, base) = match base {
            Some((raw, b)) => {
                let patch = gmap::contact_patch(Some(&b), &local);
                if patch.is_empty() {
                    return Ok(());
                }
                (self.url(&format!("/me/contacts/{}", enc(&raw.href))), Method::PATCH, patch, raw.etag, Some(b))
            }
            None => (self.url("/me/contacts"), Method::POST, gmap::contact_patch(None, &local), None, None),
        };
        let resp = self.request(ctx, method, &url, Some(&Value::Object(body)), etag.as_deref()).await?;
        let cref = CollectionRef::AddressBook(local.address_book);
        let adopt = |v: &Value, l: &Contact| {
            let mut c = gmap::contact_from_graph(v);
            c.id = l.id;
            c.address_book = l.address_book;
            c.photo = l.photo;
            Doc::Contact(c)
        };
        if resp.status == StatusCode::PRECONDITION_FAILED {
            let Some(remote_id) = local.remote_id.clone() else { return Err(PimError::Conflict) };
            let sj = self.get_json(ctx, &self.url(&format!("/me/contacts/{}", enc(&remote_id)))).await?;
            let Doc::Contact(server) = adopt(&sj, &local) else { return Err(PimError::Conflict) };
            match base.and_then(|b| merge::merge_contact(&b, &local, &server)) {
                Some(m) => {
                    let patch = gmap::contact_patch(Some(&server), &m);
                    let resp = self
                        .request(ctx, Method::PATCH, &url, Some(&Value::Object(patch)), gmap::etag(&sj).as_deref())
                        .await?;
                    if !resp.status.is_success() {
                        return Err(PimError::Conflict);
                    }
                    store.put_contact(&m)?;
                    self.commit(ctx, cref, r, &resp.body, |v| adopt(v, &m), Doc::Contact(m.clone()))?;
                    ctx.conflict(r, &m.full_name, ConflictResolution::Merged);
                }
                None => {
                    self.commit(ctx, cref, r, &sj, |v| adopt(v, &local), Doc::Contact(local.clone()))?;
                    ctx.conflict(r, &server.full_name, ConflictResolution::ServerWon);
                }
            }
            return Ok(());
        }
        if !resp.status.is_success() {
            return Err(Self::error(&resp, "contacto"));
        }
        self.commit(ctx, cref, r, &resp.body, |v| adopt(v, &local), Doc::Contact(local.clone()))
    }

    async fn push_task(&self, ctx: &Ctx, id: TaskId) -> Result<()> {
        let store = &ctx.store;
        let Some(local) = store.get_task(id)? else { return Ok(()) };
        let Some(list) = store.get_task_list(local.list)? else { return Ok(()) };
        let r = PimRef::Task(id);
        let base = raw_json(store, r).map(|(raw, v)| (raw, gmap::task_from_graph(&v)));
        let lpath = format!("/me/todo/lists/{}/tasks", enc(&list.remote_path));
        let (url, method, body, etag, base) = match base {
            Some((raw, b)) => {
                let patch = gmap::task_patch(Some(&b), &local);
                if patch.is_empty() {
                    return Ok(());
                }
                (self.url(&format!("{lpath}/{}", enc(&raw.href))), Method::PATCH, patch, raw.etag, Some(b))
            }
            None => (self.url(&lpath), Method::POST, gmap::task_patch(None, &local), None, None),
        };
        let resp = self.request(ctx, method, &url, Some(&Value::Object(body)), etag.as_deref()).await?;
        let cref = CollectionRef::TaskList(local.list);
        let adopt = |v: &Value, l: &Task| {
            let mut t = gmap::task_from_graph(v);
            t.id = l.id;
            t.list = l.list;
            Doc::Task(t)
        };
        if resp.status == StatusCode::PRECONDITION_FAILED {
            let sj = self.get_json(ctx, &url).await?;
            let Doc::Task(server) = adopt(&sj, &local) else { return Err(PimError::Conflict) };
            match base.and_then(|b| merge::merge_task(&b, &local, &server)) {
                Some(m) => {
                    let patch = gmap::task_patch(Some(&server), &m);
                    let resp = self
                        .request(ctx, Method::PATCH, &url, Some(&Value::Object(patch)), gmap::etag(&sj).as_deref())
                        .await?;
                    if !resp.status.is_success() {
                        return Err(PimError::Conflict);
                    }
                    store.put_task(&m)?;
                    self.commit(ctx, cref, r, &resp.body, |v| adopt(v, &m), Doc::Task(m.clone()))?;
                    ctx.conflict(r, &m.summary, ConflictResolution::Merged);
                }
                None => {
                    self.commit(ctx, cref, r, &sj, |v| adopt(v, &local), Doc::Task(local.clone()))?;
                    ctx.conflict(r, &server.summary, ConflictResolution::ServerWon);
                }
            }
            return Ok(());
        }
        if !resp.status.is_success() {
            return Err(Self::error(&resp, "tarea"));
        }
        self.commit(ctx, cref, r, &resp.body, |v| adopt(v, &local), Doc::Task(local.clone()))
    }

    /// Guarda la respuesta del servidor como base nueva y actualiza el registro local. Si el
    /// registro cambió durante la subida (`pushed` distinto del actual), conserva lo local y
    /// encola otra subida.
    fn commit(
        &self,
        ctx: &Ctx,
        cref: CollectionRef,
        r: PimRef,
        server: &Value,
        to_doc: impl Fn(&Value) -> Doc,
        pushed: Doc,
    ) -> Result<()> {
        let store = &ctx.store;
        let Some(remote_id) = server.get("id").and_then(Value::as_str) else {
            return Err(PimError::Invalid("Graph no devolvió el id".into()));
        };
        let etag = gmap::etag(server);
        let current = Doc::load(store, r)?;
        let edited = current.as_ref().is_some_and(|c| !c.same_content(&pushed));
        let doc = match (edited, current) {
            (true, Some(mut c)) => {
                c.set_remote(remote_id, etag.as_deref());
                c
            }
            _ => to_doc(server),
        };
        doc.save(store)?;
        let bytes = serde_json::to_vec(server).map_err(|e| PimError::Invalid(e.to_string()))?;
        store.put_raw_object(r, &bytes, remote_id, etag.as_deref())?;
        let mut st = store.collection_sync(cref)?;
        let item = st.items.entry(remote_id.to_string()).or_default();
        item.etag = etag;
        if !item.refs.contains(&r) {
            item.refs.push(r);
        }
        store.put_collection_sync(cref, &st)?;
        if edited && let Some(op) = doc.put_op() {
            store.enqueue_op(ctx.acct, op.kind(), &op.encode())?;
        }
        ctx.changed(cref);
        Ok(())
    }

    async fn push_rsvp(
        &self,
        ctx: &Ctx,
        id: EventId,
        partstat: &iuredesk_model::PartStat,
        comment: Option<&str>,
    ) -> Result<()> {
        let Some(ev) = ctx.store.get_event(id)? else { return Ok(()) };
        let (Some(remote), Some(action)) = (ev.remote_id.clone(), gmap::rsvp_action(partstat)) else { return Ok(()) };
        let url = self.url(&format!("/me/events/{}/{action}", enc(&remote)));
        let body = json!({ "sendResponse": true, "comment": comment.unwrap_or_default() });
        let resp = self.request(ctx, Method::POST, &url, Some(&body), None).await?;
        if resp.status.is_success() || resp.status == StatusCode::NOT_FOUND {
            Ok(())
        } else {
            Err(Self::error(&resp, "respuesta"))
        }
    }

    async fn push_delete(
        &self,
        ctx: &Ctx,
        path: String,
        cref: CollectionRef,
        href: &str,
        etag: Option<&str>,
    ) -> Result<()> {
        let resp = self.request(ctx, Method::DELETE, &self.url(&path), None, etag).await?;
        let mut st = ctx.store.collection_sync(cref)?;
        if resp.status.is_success() || resp.status == StatusCode::NOT_FOUND || resp.status == StatusCode::GONE {
            if st.items.remove(href).is_some() {
                ctx.store.put_collection_sync(cref, &st)?;
            }
            return Ok(());
        }
        if resp.status == StatusCode::PRECONDITION_FAILED {
            // Cambió en el servidor: gana el servidor; el próximo delta lo trae de vuelta. Se
            // olvida el `deltaLink` para pedirlo todo y recuperarlo seguro.
            st.items.remove(href);
            st.delta_link = None;
            ctx.store.put_collection_sync(cref, &st)?;
            ctx.emit(PimEvent::Conflict {
                account: ctx.acct,
                item: PimRef::Event(EventId::UNASSIGNED),
                summary: href.to_string(),
                resolution: ConflictResolution::ServerWon,
            });
            return Ok(());
        }
        Err(Self::error(&resp, "borrar"))
    }
}

impl Provider for Graph {
    fn sync<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let (cals, book, lists) = self.refresh(ctx).await?;
            let now = Utc::now();
            let from = (now - Duration::days(ctx.opts.graph_past_days.clamp(0, 3_650))).format("%Y-%m-%dT00:00:00Z");
            let to = (now + Duration::days(ctx.opts.graph_future_days.clamp(1, 3_650))).format("%Y-%m-%dT00:00:00Z");
            for (id, remote) in cals {
                let start =
                    format!("/me/calendars/{}/calendarView/delta?startDateTime={from}&endDateTime={to}", enc(&remote));
                self.pull(ctx, CollectionRef::Calendar(id), &start, Apply::Event(id)).await?;
            }
            self.pull(ctx, CollectionRef::AddressBook(book), "/me/contacts/delta", Apply::Contact(book)).await?;
            for (id, remote) in lists {
                let start = format!("/me/todo/lists/{}/tasks/delta", enc(&remote));
                self.pull(ctx, CollectionRef::TaskList(id), &start, Apply::Task(id)).await?;
            }
            Ok(())
        })
    }

    fn push<'a>(&'a self, ctx: &'a Ctx, op: &'a PimOp) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            match op {
                PimOp::PutEvent { event } => self.push_event(ctx, *event).await,
                PimOp::Rsvp { event, partstat, comment } => {
                    self.push_rsvp(ctx, *event, partstat, comment.as_deref()).await
                }
                PimOp::PutContact { contact } => self.push_contact(ctx, *contact).await,
                PimOp::PutTask { task } => self.push_task(ctx, *task).await,
                PimOp::DeleteEvent { calendar, href, etag } => {
                    self.push_delete(
                        ctx,
                        format!("/me/events/{}", enc(href)),
                        CollectionRef::Calendar(*calendar),
                        href,
                        etag.as_deref(),
                    )
                    .await
                }
                PimOp::DeleteContact { book, href, etag } => {
                    self.push_delete(
                        ctx,
                        format!("/me/contacts/{}", enc(href)),
                        CollectionRef::AddressBook(*book),
                        href,
                        etag.as_deref(),
                    )
                    .await
                }
                PimOp::DeleteTask { list, href, etag } => {
                    let Some(l) = ctx.store.get_task_list(*list)? else { return Ok(()) };
                    let path = format!("/me/todo/lists/{}/tasks/{}", enc(&l.remote_path), enc(href));
                    self.push_delete(ctx, path, CollectionRef::TaskList(*list), href, etag.as_deref()).await
                }
            }
        })
    }
}

/// Codifica un id de Graph para la ruta (los ids traen `=`, `+`, `/`, `-`, `_`).
fn enc(id: &str) -> String {
    let mut out = String::with_capacity(id.len());
    for b in id.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'=') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn raw_json(store: &Store, r: PimRef) -> Option<(iuredesk_store::RawObject, Value)> {
    let (raw, data) = store.raw_object_data(r).ok()??;
    let v: Value = serde_json::from_slice(&data).ok()?;
    Some((raw, v))
}

/// Dónde va cada objeto de un delta.
#[derive(Clone, Copy)]
enum Apply {
    Event(CalendarId),
    Contact(AddressBookId),
    Task(TaskListId),
}

/// Elemento local de cualquier tipo.
enum Doc {
    Event(Event),
    Contact(Contact),
    Task(Task),
}

impl Doc {
    fn load(store: &Store, r: PimRef) -> Result<Option<Self>> {
        Ok(match r {
            PimRef::Event(id) => store.get_event(id)?.map(Doc::Event),
            PimRef::Contact(id) => store.get_contact(id)?.map(Doc::Contact),
            PimRef::Task(id) => store.get_task(id)?.map(Doc::Task),
        })
    }

    fn save(&self, store: &Store) -> Result<()> {
        match self {
            Doc::Event(e) => {
                store.put_event(e)?;
            }
            Doc::Contact(c) => {
                store.put_contact(c)?;
            }
            Doc::Task(t) => {
                store.put_task(t)?;
            }
        }
        Ok(())
    }

    fn set_remote(&mut self, id: &str, etag: Option<&str>) {
        let (r, e) = match self {
            Doc::Event(x) => (&mut x.remote_id, &mut x.etag),
            Doc::Contact(x) => (&mut x.remote_id, &mut x.etag),
            Doc::Task(x) => (&mut x.remote_id, &mut x.etag),
        };
        *r = Some(id.to_string());
        *e = etag.map(str::to_string);
    }

    fn same_content(&self, o: &Doc) -> bool {
        match (self, o) {
            (Doc::Event(a), Doc::Event(b)) => {
                let s =
                    |e: &Event| Event { remote_id: None, etag: None, updated: None, uid: String::new(), ..e.clone() };
                s(a) == s(b)
            }
            (Doc::Contact(a), Doc::Contact(b)) => {
                let s = |c: &Contact| Contact {
                    remote_id: None,
                    etag: None,
                    updated: None,
                    uid: String::new(),
                    ..c.clone()
                };
                s(a) == s(b)
            }
            (Doc::Task(a), Doc::Task(b)) => {
                let s = |t: &Task| Task { remote_id: None, etag: None, updated: None, uid: String::new(), ..t.clone() };
                s(a) == s(b)
            }
            _ => false,
        }
    }

    fn put_op(&self) -> Option<PimOp> {
        Some(match self {
            Doc::Event(e) => PimOp::PutEvent { event: e.id },
            Doc::Contact(c) => PimOp::PutContact { contact: c.id },
            Doc::Task(t) => PimOp::PutTask { task: t.id },
        })
    }
}

fn apply_item(
    ctx: &Ctx,
    st: &mut CollectionSync,
    pending: &ops::Pending,
    _cref: CollectionRef,
    id: &str,
    v: &Value,
    apply: Apply,
) -> Result<bool> {
    let store = &ctx.store;
    let old_refs = st.items.get(id).map(|i| i.refs.clone()).unwrap_or_default();
    if old_refs.iter().any(|r| pending.dirty.contains(r)) {
        return Ok(false);
    }
    // Delta de Graph: cada objeto viene completo salvo en cambios mínimos; se fusiona sobre la base
    // guardada para no perder campos que el delta no repitió.
    let existing_raw = old_refs.first().and_then(|r| raw_json(store, *r)).map(|(_, j)| j);
    let full = match existing_raw {
        Some(Value::Object(mut base)) => {
            if let Value::Object(m) = v {
                for (k, val) in m {
                    base.insert(k.clone(), val.clone());
                }
            }
            Value::Object(base)
        }
        _ => v.clone(),
    };
    let r = match apply {
        Apply::Event(cal) => {
            let mut e = gmap::event_from_graph(&full);
            e.id = first_event(&old_refs).unwrap_or(EventId::UNASSIGNED);
            e.calendar = cal;
            PimRef::Event(store.put_event(&e)?)
        }
        Apply::Contact(book) => {
            let mut c = gmap::contact_from_graph(&full);
            let old = old_refs.iter().find_map(|r| match r {
                PimRef::Contact(i) => store.get_contact(*i).ok().flatten(),
                _ => None,
            });
            c.id = old.as_ref().map_or(ContactId::UNASSIGNED, |o| o.id);
            c.photo = old.and_then(|o| o.photo);
            c.address_book = book;
            PimRef::Contact(store.put_contact(&c)?)
        }
        Apply::Task(list) => {
            let mut t = gmap::task_from_graph(&full);
            t.id = old_refs
                .iter()
                .find_map(|r| match r {
                    PimRef::Task(i) => Some(*i),
                    _ => None,
                })
                .unwrap_or(TaskId::UNASSIGNED);
            t.list = list;
            PimRef::Task(store.put_task(&t)?)
        }
    };
    let bytes = serde_json::to_vec(&full).map_err(|e| PimError::Invalid(e.to_string()))?;
    let etag = gmap::etag(&full);
    store.put_raw_object(r, &bytes, id, etag.as_deref())?;
    let item = st.items.entry(id.to_string()).or_default();
    item.etag = etag;
    item.refs = vec![r];
    Ok(true)
}

fn first_event(refs: &[PimRef]) -> Option<EventId> {
    refs.iter().find_map(|r| match r {
        PimRef::Event(i) => Some(*i),
        _ => None,
    })
}

fn remove_item(ctx: &Ctx, st: &mut CollectionSync, pending: &ops::Pending, id: &str) -> Result<bool> {
    let Some(item) = st.items.remove(id) else { return Ok(false) };
    for r in &item.refs {
        if pending.dirty.contains(r) {
            ctx.conflict(*r, id, ConflictResolution::ServerWon);
        }
        match r {
            PimRef::Event(i) => ctx.store.delete_event(*i)?,
            PimRef::Contact(i) => ctx.store.delete_contact(*i)?,
            PimRef::Task(i) => ctx.store.delete_task(*i)?,
        }
    }
    Ok(!item.refs.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_encoding() {
        assert_eq!(enc("AAMk/a+b=="), "AAMk%2Fa%2Bb==");
    }
}
