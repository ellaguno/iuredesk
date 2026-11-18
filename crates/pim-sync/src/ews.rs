//! Proveedor Exchange Web Services (Exchange Server local, F8.4): calendarios, contactos
//! ([`contacts`]) y la libreta global de la organización ([`directory`]).
//!
//! Mismo planteamiento que [`crate::graph`]: cada elemento de EWS se guarda como JSON con la forma
//! de Graph ([`crate::map::ews`]) y se reutiliza su traducción al modelo, el parche con solo lo
//! cambiado y las series (el maestro va en `seriesMasterId`, como en Graph, así que «toda la
//! serie» y «este y los siguientes» funcionan igual).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::{Duration, Utc};
use iuredesk_ews::calendar::{Notify, Response};
use iuredesk_ews::{BasicAuth, EwsClient};
use iuredesk_model::{Calendar, CalendarId, Event, EventId, EventTime};
use iuredesk_store::{CollectionRef, CollectionSync, PimRef, Store};
use serde_json::{Map, Value, json};

use crate::error::{PimError, Result};
use crate::map::ews as emap;
use crate::map::graph as gmap;
use crate::merge;
use crate::ops::{self, PimOp};
use crate::provider::{BoxFuture, Ctx, Provider};
use crate::{ConflictResolution, PimEvent};

mod contacts;

/// Días de cada consulta de `CalendarView` (el servidor limita las apariciones por consulta).
const VIEW_DAYS: i64 = 31;

pub(crate) struct Ews {
    client: EwsClient,
    me: Vec<String>,
}

impl Ews {
    pub async fn new(ctx: &Ctx, url: &str) -> Result<Self> {
        let a = &ctx.account.account;
        let password = ctx.creds.password(a).await?;
        let user = if a.incoming.username.trim().is_empty() { a.email.clone() } else { a.incoming.username.clone() };
        let client = EwsClient::new(url, Arc::new(BasicAuth::new(&user, &password)), ctx.opts.timeout)?;
        Ok(Self { client, me: ctx.account.emails() })
    }

    /// JSON (forma de Graph) de un elemento, con el maestro de su serie si se conoce.
    fn to_json(&self, ctx: &Ctx, e: &iuredesk_ews::Element, master: Option<&str>) -> Option<Value> {
        let mut v = emap::item_json(e, &self.me, ctx.opts.user_tz)?;
        if let (Some(m), Some(o)) = (master, v.as_object_mut()) {
            o.insert("seriesMasterId".into(), json!(m));
        }
        Some(v)
    }

    /// El elemento `id` como JSON, conservando el maestro de la serie que ya se sabía.
    async fn fetch(&self, ctx: &Ctx, id: &str, master: Option<&str>) -> Result<Option<Value>> {
        let Some(e) = self.client.get_calendar_item(id).await? else { return Ok(None) };
        Ok(self.to_json(ctx, &e, master))
    }

    // -------------------------------------------------------------- colecciones

    async fn refresh(&self, ctx: &Ctx) -> Result<Vec<(CalendarId, String)>> {
        let store = &ctx.store;
        let acct = ctx.acct;
        let mut changed = false;
        let remote = self.client.calendar_folders().await?;
        let local: Vec<Calendar> = store.calendars()?.into_iter().filter(|c| c.account == acct).collect();
        let mut cals = Vec::new();
        for f in &remote {
            let old = local.iter().find(|x| x.remote_path == f.id);
            let new = Calendar {
                id: old.map_or(CalendarId::UNASSIGNED, |o| o.id),
                account: acct,
                name: f.name.clone(),
                color: old.and_then(|o| o.color),
                remote_path: f.id.clone(),
                read_only: false,
                sync_token: None,
            };
            let lid = if old != Some(&new) {
                changed = true;
                store.put_calendar(&new)?
            } else {
                new.id
            };
            cals.push((lid, f.id.clone()));
        }
        for c in local.iter().filter(|c| !c.remote_path.is_empty() && !cals.iter().any(|(_, r)| *r == c.remote_path)) {
            store.delete_calendar(c.id)?;
            changed = true;
        }
        if changed {
            ctx.emit(PimEvent::CollectionsChanged { account: acct });
        }
        Ok(cals)
    }

    // -------------------------------------------------------------- bajar

    /// Compara las apariciones de la ventana con lo guardado (por `ChangeKey`), pide las nuevas o
    /// cambiadas y quita las que ya no están.
    async fn pull(&self, ctx: &Ctx, cal: CalendarId, folder: &str) -> Result<()> {
        let store = &ctx.store;
        let cref = CollectionRef::Calendar(cal);
        let now = Utc::now();
        let from = now - Duration::days(ctx.opts.graph_past_days.clamp(0, 3_650));
        let to = now + Duration::days(ctx.opts.graph_future_days.clamp(1, 3_650));
        let mut view: Vec<(String, Option<String>)> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut complete = true;
        let mut at = from;
        while at < to {
            let end = (at + Duration::days(VIEW_DAYS)).min(to);
            let (items, all) = self.client.calendar_view(folder, at, end).await?;
            complete &= all;
            for i in items {
                if seen.insert(i.id.clone()) {
                    view.push((i.id, i.change_key));
                }
            }
            at = end;
        }
        let mut st = store.collection_sync(cref)?;
        let pending = ops::pending(store, ctx.acct)?;
        let mut changed = false;
        let wanted: Vec<String> = view
            .iter()
            .filter(|(id, ck)| !pending.deleted.contains(id) && st.items.get(id).is_none_or(|i| i.etag != *ck))
            .map(|(id, _)| id.clone())
            .collect();
        // Maestro de cada serie (uno por UID y pasada).
        let mut masters: HashMap<String, Option<String>> = HashMap::new();
        for e in self.client.get_calendar_items(&wanted).await? {
            let Some(base) = self.to_json(ctx, &e, None) else { continue };
            let Some(id) = base.get("id").and_then(Value::as_str).map(str::to_string) else { continue };
            let master = if matches!(base.get("type").and_then(Value::as_str), Some("occurrence" | "exception")) {
                let uid = base.get("iCalUId").and_then(Value::as_str).unwrap_or(&id).to_string();
                match masters.get(&uid) {
                    Some(m) => m.clone(),
                    None => {
                        let m = self.client.recurring_master(&id).await?;
                        masters.insert(uid, m.clone());
                        m
                    }
                }
            } else {
                None
            };
            let Some(v) = self.to_json(ctx, &e, master.as_deref()) else { continue };
            changed |= apply_item(ctx, &mut st, &pending, cal, &id, &v)?;
        }
        // Lo que ya no aparece en la ventana: borrado en el servidor (o fuera de la ventana). Si
        // la vista vino incompleta (demasiadas apariciones), no se borra nada.
        if complete {
            let gone: Vec<String> = st.items.keys().filter(|k| !seen.contains(*k)).cloned().collect();
            for id in gone {
                changed |= remove_item(ctx, &mut st, &pending, &id)?;
            }
        }
        store.put_collection_sync(cref, &st)?;
        if changed {
            ctx.changed(cref);
        }
        Ok(())
    }

    // -------------------------------------------------------------- subir

    /// ¿Se avisa a los asistentes de un cambio? Solo si es una reunión que organizamos.
    fn notify_for(base: &Value, attendees: bool) -> Notify {
        let organizer = base.get("isOrganizer").and_then(Value::as_bool).unwrap_or(true);
        if organizer && attendees { Notify::Changed } else { Notify::None }
    }

    async fn push_event(&self, ctx: &Ctx, id: EventId) -> Result<()> {
        let store = &ctx.store;
        let Some(local) = store.get_event(id)? else { return Ok(()) };
        let Some(cal) = store.get_calendar(local.calendar)? else { return Ok(()) };
        if cal.read_only {
            return Err(PimError::ReadOnly);
        }
        let r = PimRef::Event(id);
        let tz = ctx.opts.user_tz;
        let saved = raw_json(store, r);
        let result = match &saved {
            Some((raw, base_json)) => {
                let base = gmap::event_from_graph(base_json);
                let patch = gmap::event_patch(Some(&base), &local);
                let updates = emap::update_xml(&patch, tz);
                if updates.is_empty() {
                    return Ok(());
                }
                let notify = Self::notify_for(base_json, !local.attendees.is_empty() || !base.attendees.is_empty());
                match self.client.update_calendar_item(&raw.href, raw.etag.as_deref(), &updates, notify).await {
                    Err(e) if iuredesk_ews::calendar::is_conflict(&e) => {
                        return self.resolve_event(ctx, local, base, base_json).await;
                    }
                    Err(e) if e.is_not_found() => {
                        store.delete_event(id)?;
                        ctx.conflict(r, &local.summary, ConflictResolution::ServerWon);
                        ctx.changed(CollectionRef::Calendar(local.calendar));
                        return Ok(());
                    }
                    other => other?,
                }
            }
            None => {
                let xml = emap::create_xml(&gmap::event_patch(None, &local), tz);
                let notify = if local.attendees.is_empty() { Notify::None } else { Notify::All };
                self.client.create_calendar_item(&cal.remote_path, &xml, notify).await?
            }
        };
        let master = saved.as_ref().and_then(|(_, v)| v.get("seriesMasterId").and_then(Value::as_str));
        let Some(server) = self.fetch(ctx, &result.0, master).await? else {
            return Err(PimError::Invalid("Exchange no devolvió el evento guardado".into()));
        };
        self.commit(ctx, &local, &server)
    }

    /// El servidor cambió el evento mientras lo editábamos: fusión a tres bandas; si los cambios
    /// chocan, gana el servidor.
    async fn resolve_event(&self, ctx: &Ctx, local: Event, base: Event, base_json: &Value) -> Result<()> {
        let r = PimRef::Event(local.id);
        let Some(remote_id) = local.remote_id.clone() else { return Err(PimError::Conflict) };
        let master = base_json.get("seriesMasterId").and_then(Value::as_str);
        let Some(server_json) = self.fetch(ctx, &remote_id, master).await? else {
            ctx.store.delete_event(local.id)?;
            ctx.conflict(r, &local.summary, ConflictResolution::ServerWon);
            ctx.changed(CollectionRef::Calendar(local.calendar));
            return Ok(());
        };
        let mut server = gmap::event_from_graph(&server_json);
        server.id = local.id;
        server.calendar = local.calendar;
        match merge::merge_event(&base, &local, &server) {
            Some(m) => {
                let updates = emap::update_xml(&gmap::event_patch(Some(&server), &m), ctx.opts.user_tz);
                let ck = server_json.get("@odata.etag").and_then(Value::as_str);
                let notify = Self::notify_for(&server_json, !m.attendees.is_empty());
                let (id, _) = if updates.is_empty() {
                    (remote_id.clone(), None)
                } else {
                    self.client.update_calendar_item(&remote_id, ck, &updates, notify).await.map_err(|e| {
                        if iuredesk_ews::calendar::is_conflict(&e) { PimError::Conflict } else { e.into() }
                    })?
                };
                ctx.store.put_event(&m)?;
                let Some(after) = self.fetch(ctx, &id, master).await? else { return Err(PimError::Conflict) };
                self.commit(ctx, &m, &after)?;
                ctx.conflict(r, &m.summary, ConflictResolution::Merged);
            }
            None => {
                self.commit(ctx, &local, &server_json)?;
                // Gana el servidor: lo local pasa a ser lo del servidor.
                let mut e = gmap::event_from_graph(&server_json);
                e.id = local.id;
                e.calendar = local.calendar;
                ctx.store.put_event(&e)?;
                ctx.conflict(r, &server.summary, ConflictResolution::ServerWon);
            }
        }
        Ok(())
    }

    /// Guarda la versión del servidor como base nueva. Si el evento cambió en local durante la
    /// subida, se conserva lo local y se encola otra subida.
    fn commit(&self, ctx: &Ctx, pushed: &Event, server: &Value) -> Result<()> {
        let store = &ctx.store;
        let r = PimRef::Event(pushed.id);
        let cref = CollectionRef::Calendar(pushed.calendar);
        let Some(remote_id) = server.get("id").and_then(Value::as_str) else {
            return Err(PimError::Invalid("Exchange no devolvió el id".into()));
        };
        let etag = gmap::etag(server).filter(|e| !e.is_empty());
        let current = store.get_event(pushed.id)?;
        let strip = |e: &Event| Event { remote_id: None, etag: None, updated: None, uid: String::new(), ..e.clone() };
        let edited = current.as_ref().is_some_and(|c| strip(c) != strip(pushed));
        let ev = match (edited, current) {
            (true, Some(mut c)) => {
                c.remote_id = Some(remote_id.to_string());
                c.etag = etag.clone();
                c
            }
            _ => {
                let mut e = gmap::event_from_graph(server);
                e.id = pushed.id;
                e.calendar = pushed.calendar;
                e
            }
        };
        store.put_event(&ev)?;
        let bytes = serde_json::to_vec(server).map_err(|e| PimError::Invalid(e.to_string()))?;
        store.put_raw_object(r, &bytes, remote_id, etag.as_deref())?;
        let mut st = store.collection_sync(cref)?;
        // Una edición de una aparición puede cambiar su id: se olvida el anterior.
        st.items.retain(|k, i| k == remote_id || !i.refs.contains(&r));
        let item = st.items.entry(remote_id.to_string()).or_default();
        item.etag = etag;
        if !item.refs.contains(&r) {
            item.refs.push(r);
        }
        store.put_collection_sync(cref, &st)?;
        if edited {
            let op = PimOp::PutEvent { event: pushed.id };
            store.enqueue_op(ctx.acct, op.kind(), &op.encode())?;
        }
        ctx.changed(cref);
        Ok(())
    }

    /// Cambia el maestro de una serie con un parche de Graph (lo hace el servidor en todas las
    /// apariciones). Si el maestro ya no existe, no hay nada que hacer.
    async fn patch_master(&self, ctx: &Ctx, master: &str, patch: &Map<String, Value>) -> Result<bool> {
        let updates = emap::update_xml(patch, ctx.opts.user_tz);
        if updates.is_empty() {
            return Ok(true);
        }
        let Some(m) = self.client.get_calendar_item(master).await? else { return Ok(false) };
        let notify = if iuredesk_ews::calendar::organized_by_me(&m) { Notify::Changed } else { Notify::None };
        match self.client.update_calendar_item(master, None, &updates, notify).await {
            Ok(_) => Ok(true),
            Err(e) if e.is_not_found() => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// R4.5: «toda la serie» → el maestro.
    async fn push_series(&self, ctx: &Ctx, master: &str, patch: &crate::series::SeriesPatch) -> Result<()> {
        let Some(current) = self.fetch(ctx, master, None).await? else { return Ok(()) };
        let body = gmap::series_patch_json(&current, patch);
        if body.is_empty() {
            return Ok(());
        }
        self.patch_master(ctx, master, &body).await.map(|_| ())
    }

    /// R4.5: «este y los siguientes» (mismo reparto que en Graph): la serie termina el día antes
    /// de la aparición y se crea otra desde ella con lo editado.
    async fn push_split(
        &self,
        ctx: &Ctx,
        calendar: CalendarId,
        master: &str,
        at: &EventTime,
        patch: &crate::series::SeriesPatch,
        token: &str,
    ) -> Result<()> {
        let Some(mjson) = self.fetch(ctx, master, None).await? else { return Ok(()) };
        let original = mjson.get("recurrence").cloned().unwrap_or(Value::Null);
        let m_ev = gmap::event_from_graph(&mjson);
        if original.is_null() || !m_ev.is_recurring() {
            return Ok(());
        }
        let at_t = match (&m_ev.start, at) {
            (EventTime::DateTime { tzid, .. }, EventTime::DateTime { utc, .. }) => {
                EventTime::DateTime { utc: *utc, tzid: tzid.clone() }
            }
            (_, t) => t.clone(),
        };
        let len = m_ev.end.to_utc() - m_ev.start.to_utc();
        let at_end = match &at_t {
            EventTime::DateTime { utc, tzid } => EventTime::DateTime { utc: *utc + len, tzid: tzid.clone() },
            EventTime::Date(d) => EventTime::Date(*d + Duration::days(len.num_days().max(1))),
        };
        let mut edited = m_ev.clone();
        if let Some(x) = &patch.summary {
            edited.summary = x.clone();
        }
        if let Some(x) = &patch.location {
            edited.location = x.clone();
        }
        if let Some(x) = &patch.description {
            edited.description = x.clone();
        }
        if let Some(x) = &patch.attendees {
            edited.attendees = x.clone();
        }
        if let Some(x) = &patch.reminders {
            edited.reminders = x.clone();
        }
        let (s, e) = patch.times(&at_t, &at_end);
        edited.start = s;
        edited.end = e;
        let (head, tail) = crate::series::split_series(&m_ev, &at_t, &edited, ctx.opts.user_tz)?;
        if head.is_none() {
            return self.push_series(ctx, master, patch).await;
        }
        let local_day = match &at_t {
            EventTime::Date(d) => *d,
            EventTime::DateTime { utc, tzid } => match tzid.as_deref().and_then(|z| z.parse::<chrono_tz::Tz>().ok()) {
                Some(z) => utc.with_timezone(&z).date_naive(),
                None => utc.date_naive(),
            },
        };
        let Some(last_day) = local_day.pred_opt() else { return Ok(()) };
        let mut cut = original.clone();
        if let Some(Value::Object(r)) = cut.get_mut("range") {
            r.insert("type".into(), json!("endDate"));
            r.insert("endDate".into(), json!(last_day.format("%Y-%m-%d").to_string()));
            r.remove("numberOfOccurrences");
        }
        let one = |k: &str, v: Value| -> Map<String, Value> { [(k.to_string(), v)].into_iter().collect() };
        if !self.patch_master(ctx, master, &one("recurrence", cut)).await? {
            return Ok(());
        }
        let Some(cal) = ctx.store.get_calendar(calendar)? else { return Ok(()) };
        let xml = emap::create_xml(&gmap::event_patch(None, &tail), ctx.opts.user_tz);
        let notify = if tail.attendees.is_empty() { Notify::None } else { Notify::All };
        let created = match self.client.create_calendar_item(&cal.remote_path, &xml, notify).await {
            Ok((id, _)) => id,
            Err(e) => {
                // Sin la serie nueva: se devuelve la regla para no perder las apariciones.
                let _ = self.patch_master(ctx, master, &one("recurrence", original)).await;
                return Err(e.into());
            }
        };
        let state = json!({ "created": created, "recurrence": original });
        ctx.store.put_sync_state(&ops::split_state_key(token), state.to_string().as_bytes())?;
        Ok(())
    }

    /// Deshace [`Ews::push_split`]: la regla original vuelve al maestro y se borra la serie nueva.
    async fn push_unsplit(&self, ctx: &Ctx, master: &str, token: &str) -> Result<()> {
        let key = ops::split_state_key(token);
        let Some(raw) = ctx.store.sync_state(&key)? else { return Ok(()) };
        let state: Value = serde_json::from_slice(&raw).unwrap_or(Value::Null);
        if let Some(rec) = state.get("recurrence").filter(|r| !r.is_null()) {
            let patch: Map<String, Value> = [("recurrence".to_string(), rec.clone())].into_iter().collect();
            self.patch_master(ctx, master, &patch).await?;
        }
        if let Some(created) = state.get("created").and_then(Value::as_str).filter(|c| !c.is_empty()) {
            self.client.delete_calendar_item(created, Notify::All).await?;
        }
        ctx.store.delete_sync_state(&key)?;
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
        let response = match gmap::rsvp_action(partstat) {
            Some("accept") => Response::Accept,
            Some("decline") => Response::Decline,
            Some("tentativelyAccept") => Response::Tentative,
            _ => return Ok(()),
        };
        let Some(remote) = ev.remote_id.clone() else { return Ok(()) };
        self.client.respond(&remote, response, comment).await?;
        Ok(())
    }

    async fn push_delete(&self, ctx: &Ctx, calendar: CalendarId, href: &str) -> Result<()> {
        // Si organizamos la reunión, el servidor manda la cancelación (como Outlook al borrarla);
        // en una cita sin asistentes o de la que somos invitados no se manda nada.
        self.client.delete_calendar_item(href, Notify::All).await?;
        let cref = CollectionRef::Calendar(calendar);
        let mut st = ctx.store.collection_sync(cref)?;
        if st.items.remove(href).is_some() {
            ctx.store.put_collection_sync(cref, &st)?;
        }
        Ok(())
    }
}

impl Provider for Ews {
    fn sync<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            for (id, folder) in self.refresh(ctx).await? {
                self.pull(ctx, id, &folder).await?;
            }
            let book = self.contact_book(ctx)?;
            self.pull_contacts(ctx, book).await
        })
    }

    fn push<'a>(&'a self, ctx: &'a Ctx, op: &'a PimOp) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            match op {
                PimOp::PutEvent { event } => self.push_event(ctx, *event).await,
                PimOp::PatchSeries { master, patch, .. } => self.push_series(ctx, master, patch).await,
                PimOp::SplitSeries { calendar, master, at, patch, token } => {
                    self.push_split(ctx, *calendar, master, at, patch, token).await
                }
                PimOp::UndoSplitSeries { master, token, .. } => self.push_unsplit(ctx, master, token).await,
                PimOp::Rsvp { event, partstat, comment } => {
                    self.push_rsvp(ctx, *event, partstat, comment.as_deref()).await
                }
                PimOp::DeleteEvent { calendar, href, .. } => self.push_delete(ctx, *calendar, href).await,
                PimOp::PutContact { contact } => self.push_contact(ctx, *contact).await,
                PimOp::DeleteContact { book, href, .. } => self.push_delete_contact(ctx, *book, href).await,
                // Tareas por EWS: hito 3 de F8.4 (la cuenta aún no tiene listas, así que no se
                // encolan).
                PimOp::PutTask { .. } | PimOp::DeleteTask { .. } => Ok(()),
            }
        })
    }
}

fn raw_json(store: &Store, r: PimRef) -> Option<(iuredesk_store::RawObject, Value)> {
    let (raw, data) = store.raw_object_data(r).ok()??;
    let v: Value = serde_json::from_slice(&data).ok()?;
    Some((raw, v))
}

/// Guarda un elemento bajado (salvo que tenga cambios locales sin subir).
fn apply_item(
    ctx: &Ctx,
    st: &mut CollectionSync,
    pending: &ops::Pending,
    cal: CalendarId,
    id: &str,
    v: &Value,
) -> Result<bool> {
    let store = &ctx.store;
    let old_refs = st.items.get(id).map(|i| i.refs.clone()).unwrap_or_default();
    if old_refs.iter().any(|r| pending.dirty.contains(r)) {
        return Ok(false);
    }
    // R4.5: la vista trae las apariciones de una serie, no su maestro. Si guardamos el maestro
    // (una serie creada aquí), al llegar sus apariciones se descarta: si no, se vería dos veces.
    if let Some(master) = v.get("seriesMasterId").and_then(Value::as_str).filter(|m| !m.is_empty() && *m != id)
        && let Some(item) = st.items.remove(master)
    {
        for r in item.refs {
            if let PimRef::Event(e) = r {
                store.delete_event(e)?;
            }
        }
    }
    let mut e = gmap::event_from_graph(v);
    e.id = old_refs
        .iter()
        .find_map(|r| match r {
            PimRef::Event(i) => Some(*i),
            _ => None,
        })
        .unwrap_or(EventId::UNASSIGNED);
    e.calendar = cal;
    let r = PimRef::Event(store.put_event(&e)?);
    let bytes = serde_json::to_vec(v).map_err(|e| PimError::Invalid(e.to_string()))?;
    let etag = gmap::etag(v).filter(|e| !e.is_empty());
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
        match r {
            PimRef::Event(i) => ctx.store.delete_event(*i)?,
            PimRef::Contact(i) => ctx.store.delete_contact(*i)?,
            PimRef::Task(i) => ctx.store.delete_task(*i)?,
        }
    }
    Ok(!item.refs.is_empty())
}

/// Busca `query` en la libreta global de la organización de una cuenta de Exchange.
pub(crate) async fn directory(ctx: &Ctx, url: &str, query: &str) -> Result<Vec<iuredesk_model::Mailbox>> {
    let ews = Ews::new(ctx, url).await?;
    Ok(ews
        .client
        .resolve_names(query)
        .await?
        .into_iter()
        .map(|a| {
            if a.name.trim().is_empty() || a.name.eq_ignore_ascii_case(&a.email) {
                iuredesk_model::Mailbox::addr(a.email)
            } else {
                iuredesk_model::Mailbox::new(a.name, a.email)
            }
        })
        .collect())
}
