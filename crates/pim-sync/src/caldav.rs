//! Proveedor CalDAV/CardDAV (iCloud, Google, Fastmail, Nextcloud, genérico).

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use iuredesk_dav::calendar::ICalendar;
use iuredesk_dav::dav::{Collection, CollectionKind, Precondition, Service, SyncState};
use iuredesk_dav::vcard::VCard;
use iuredesk_dav::{DavClient, DavOptions};
use iuredesk_model::{
    AddressBook, AddressBookId, Calendar, CalendarId, Contact, ContactId, Event, EventId, Task, TaskId, TaskList,
    TaskListId,
};
use iuredesk_store::{CollectionRef, CollectionSync, PimRef, Store};
use reqwest::Url;
use serde::{Deserialize, Serialize};

use crate::ConflictResolution;
use crate::account::{DavAuth, TokenAuth};
use crate::error::{PimError, Result};
use crate::map::{ical, vcard};
use crate::merge;
use crate::ops::{self, Pending, PimOp};
use crate::provider::{BoxFuture, Ctx, Provider, state_ref};

/// Resultado del descubrimiento, guardado para no repetirlo en cada pasada.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct DiscoveryState {
    cal_homes: Vec<String>,
    card_homes: Vec<String>,
    outbox: Option<String>,
}

fn discovery_key(ctx: &Ctx) -> String {
    format!("pim/{}/dav-discovery", ctx.acct.get())
}

/// ¿El servidor programa las invitaciones (RFC 6638)? Según el descubrimiento guardado.
pub(crate) fn server_schedules(store: &Store, acct: iuredesk_model::AccountId) -> bool {
    let key = format!("pim/{}/dav-discovery", acct.get());
    store
        .sync_state(&key)
        .ok()
        .flatten()
        .and_then(|b| serde_json::from_slice::<DiscoveryState>(&b).ok())
        .is_some_and(|d| d.outbox.is_some())
}

pub(crate) struct CalDav {
    client: DavClient,
    caldav: Option<String>,
    carddav: Option<String>,
}

impl CalDav {
    /// Cliente con las credenciales de la cuenta.
    pub async fn connect(ctx: &Ctx, caldav: Option<String>, carddav: Option<String>, auth: &DavAuth) -> Result<Self> {
        let opts = DavOptions { timeout: ctx.opts.timeout, ..DavOptions::default() };
        let client = match auth {
            DavAuth::Password { username } => {
                let pw = ctx.creds.password(&ctx.account.account).await?;
                DavClient::new(iuredesk_dav::BasicAuth::new(username, &pw), opts)?
            }
            DavAuth::OAuth => {
                DavClient::new(TokenAuth { tokens: ctx.creds.tokens()?, account: ctx.account.account.clone() }, opts)?
            }
        };
        Ok(Self { client, caldav, carddav })
    }

    async fn discovery(&self, ctx: &Ctx) -> Result<DiscoveryState> {
        let key = discovery_key(ctx);
        if let Some(st) = ctx.store.sync_state(&key)?.and_then(|b| serde_json::from_slice::<DiscoveryState>(&b).ok())
            && (!st.cal_homes.is_empty() || !st.card_homes.is_empty())
        {
            return Ok(st);
        }
        let mut st = DiscoveryState::default();
        if let Some(start) = &self.caldav {
            let d = self.client.discover(start, Service::CalDav).await?;
            st.cal_homes = d.calendar_homes.iter().map(Url::to_string).collect();
            st.outbox = d.schedule_outbox.map(|u| u.to_string());
        }
        if let Some(start) = &self.carddav {
            let d = self.client.discover(start, Service::CardDav).await?;
            st.card_homes = d.addressbook_homes.iter().map(Url::to_string).collect();
        }
        if st.cal_homes.is_empty() && st.card_homes.is_empty() {
            return Err(PimError::Invalid("el servidor no tiene calendarios ni libretas".into()));
        }
        let bytes = serde_json::to_vec(&st).map_err(|e| PimError::Invalid(e.to_string()))?;
        ctx.store.put_sync_state(&key, &bytes)?;
        Ok(st)
    }

    /// Alta, cambios y baja de colecciones locales. Devuelve las colecciones a sincronizar.
    async fn refresh_collections(&self, ctx: &Ctx, disc: &DiscoveryState) -> Result<Vec<Coll>> {
        let mut remote: Vec<Collection> = Vec::new();
        for home in disc.cal_homes.iter().chain(disc.card_homes.iter()) {
            let url = Url::parse(home).map_err(|e| PimError::Invalid(e.to_string()))?;
            remote.extend(self.client.list_collections(&url).await?);
        }
        let store = &ctx.store;
        let acct = ctx.acct;
        let mut changed = false;
        let mut out = Vec::new();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let cals: Vec<Calendar> = store.calendars()?.into_iter().filter(|c| c.account == acct).collect();
        let lists: Vec<TaskList> = store.task_lists()?.into_iter().filter(|l| l.account == Some(acct)).collect();
        let books: Vec<AddressBook> = store.address_books()?.into_iter().filter(|b| b.account == acct).collect();
        for c in remote {
            let path = c.url.to_string();
            if !seen.insert(path.clone()) {
                continue;
            }
            let name = c.display_name.clone().filter(|n| !n.trim().is_empty()).unwrap_or_else(|| last_segment(&c.url));
            match c.kind {
                CollectionKind::Calendar => {
                    let has =
                        |x: &str| c.components.is_empty() || c.components.iter().any(|k| k.eq_ignore_ascii_case(x));
                    let mut calendar = None;
                    let mut list = None;
                    if has("VEVENT") {
                        let old = cals.iter().find(|x| x.remote_path == path);
                        let new = Calendar {
                            id: old.map_or(CalendarId::UNASSIGNED, |o| o.id),
                            account: acct,
                            name: name.clone(),
                            color: c.color.as_deref().and_then(parse_color),
                            remote_path: path.clone(),
                            read_only: c.read_only,
                            sync_token: old.and_then(|o| o.sync_token.clone()),
                        };
                        if old != Some(&new) {
                            calendar = Some(store.put_calendar(&new)?);
                            changed = true;
                        } else {
                            calendar = old.map(|o| o.id);
                        }
                    }
                    // Solo listas con VTODO declarado (si el servidor no dice nada, son eventos).
                    if c.components.iter().any(|k| k.eq_ignore_ascii_case("VTODO")) {
                        let old = lists.iter().find(|x| x.remote_path == path);
                        let new = TaskList {
                            id: old.map_or(TaskListId::UNASSIGNED, |o| o.id),
                            account: Some(acct),
                            name,
                            remote_path: path.clone(),
                            sync_token: old.and_then(|o| o.sync_token.clone()),
                        };
                        if old != Some(&new) {
                            list = Some(store.put_task_list(&new)?);
                            changed = true;
                        } else {
                            list = old.map(|o| o.id);
                        }
                    }
                    let state = match (calendar, list) {
                        (Some(c), _) => CollectionRef::Calendar(c),
                        (None, Some(l)) => CollectionRef::TaskList(l),
                        (None, None) => continue,
                    };
                    out.push(Coll { url: c.url.clone(), state, calendar, list, book: None });
                }
                CollectionKind::AddressBook => {
                    let old = books.iter().find(|x| x.remote_path == path);
                    let new = AddressBook {
                        id: old.map_or(AddressBookId::UNASSIGNED, |o| o.id),
                        account: acct,
                        name,
                        remote_path: path.clone(),
                        read_only: c.read_only,
                        sync_token: old.and_then(|o| o.sync_token.clone()),
                    };
                    let id = if old != Some(&new) {
                        changed = true;
                        store.put_address_book(&new)?
                    } else {
                        new.id
                    };
                    out.push(Coll {
                        url: c.url.clone(),
                        state: CollectionRef::AddressBook(id),
                        calendar: None,
                        list: None,
                        book: Some(id),
                    });
                }
            }
        }
        // Bajas: colecciones remotas que ya no están (las locales sin ruta se respetan).
        for c in cals.iter().filter(|c| !c.remote_path.is_empty() && !seen.contains(&c.remote_path)) {
            store.delete_calendar(c.id)?;
            changed = true;
        }
        for l in lists.iter().filter(|l| !l.remote_path.is_empty() && !seen.contains(&l.remote_path)) {
            store.delete_task_list(l.id)?;
            changed = true;
        }
        for b in books.iter().filter(|b| !b.remote_path.is_empty() && !seen.contains(&b.remote_path)) {
            store.delete_address_book(b.id)?;
            changed = true;
        }
        if changed {
            ctx.emit(crate::PimEvent::CollectionsChanged { account: acct });
        }
        Ok(out)
    }

    /// Cambios de una colección.
    async fn pull(&self, ctx: &Ctx, coll: &Coll) -> Result<()> {
        let store = &ctx.store;
        let mut st = store.collection_sync(coll.state)?;
        let known: BTreeMap<Url, String> =
            st.items.iter().filter_map(|(h, i)| Some((Url::parse(h).ok()?, i.etag.clone()?))).collect();
        let dav_state = SyncState {
            sync_token: st.sync_token.clone(),
            ctag: st.ctag.clone(),
            sync_unsupported: st.sync_unsupported,
        };
        let outcome = self.client.sync(&coll.url, &dav_state, &known).await?;
        let pending = ops::pending(store, ctx.acct)?;
        let wanted: Vec<Url> = outcome
            .changed
            .iter()
            .filter(|i| !pending.deleted.contains(i.url.as_str()))
            .map(|i| i.url.clone())
            .collect();
        let kind = if coll.book.is_some() { CollectionKind::AddressBook } else { CollectionKind::Calendar };
        let resources =
            if wanted.is_empty() { Vec::new() } else { self.client.multiget(&coll.url, kind, &wanted).await? };
        let mut changed = false;
        for r in resources {
            let Some(data) = r.data else { continue };
            changed |= apply_resource(ctx, coll, &mut st, &pending, r.url.as_str(), r.etag.as_deref(), &data)?;
        }
        for d in &outcome.deleted {
            if pending.deleted.contains(d.as_str()) {
                st.items.remove(d.as_str());
                continue;
            }
            changed |= remove_href(ctx, &mut st, &pending, d.as_str())?;
        }
        st.sync_token = outcome.state.sync_token;
        st.ctag = outcome.state.ctag;
        st.sync_unsupported = outcome.state.sync_unsupported;
        store.put_collection_sync(coll.state, &st)?;
        if changed {
            for c in coll.local_refs() {
                ctx.changed(c);
            }
        }
        Ok(())
    }

    async fn push_put<D: DavDoc>(&self, ctx: &Ctx, r: PimRef) -> Result<()> {
        let store = &ctx.store;
        let tz = ctx.opts.user_tz;
        let now = Utc::now();
        let Some(local) = D::load(store, r)? else { return Ok(()) };
        let Some((path, cref, read_only)) = local.collection(store, ctx.acct)? else { return Ok(()) };
        if read_only {
            return Err(PimError::ReadOnly);
        }
        let coll_url = Url::parse(&path).map_err(|e| PimError::Invalid(e.to_string()))?;
        let base = store.raw_object_data(r)?.and_then(|(raw, data)| {
            let text = String::from_utf8_lossy(&data).into_owned();
            D::parse(store, &text, local.uid(), tz).map(|b| (raw, text, b))
        });
        let (href, body, cond, base) = match base {
            Some((raw, text, base)) => {
                let Some(body) = D::patch(store, &text, &base, &local, tz, now) else { return Ok(()) };
                let href = Url::parse(&raw.href).map_err(|e| PimError::Invalid(e.to_string()))?;
                (href, body, raw.etag.clone().map_or(Precondition::None, Precondition::IfMatch), Some(base))
            }
            None => {
                let href =
                    coll_url.join(&file_name(local.uid(), D::EXT)).map_err(|e| PimError::Invalid(e.to_string()))?;
                (href, local.fresh(store, now), Precondition::Create, None)
            }
        };
        match self.client.put(&href, &body, D::KIND, cond).await {
            Ok(res) => self.commit(ctx, cref, &local, &href, res.etag, body).await,
            Err(iuredesk_dav::Error::PreconditionFailed) => self.resolve(ctx, cref, &href, base, local).await,
            Err(iuredesk_dav::Error::NotFound) if base.is_some() => {
                // Lo borraron en el servidor mientras lo editábamos: gana el servidor.
                local.delete(store)?;
                let mut st = store.collection_sync(cref)?;
                st.items.remove(href.as_str());
                store.put_collection_sync(cref, &st)?;
                ctx.conflict(r, &local.summary(), ConflictResolution::ServerWon);
                ctx.changed(local.local_collection());
                Ok(())
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Tras subir: guarda el objeto como nueva base y apunta su ETag.
    async fn commit<D: DavDoc>(
        &self,
        ctx: &Ctx,
        cref: CollectionRef,
        pushed: &D,
        href: &Url,
        etag: Option<String>,
        body: String,
    ) -> Result<()> {
        let store = &ctx.store;
        let (etag, body) = match etag {
            Some(e) => (Some(e), body),
            // Sin ETag en la respuesta: se relee (el servidor pudo normalizar el objeto).
            None => match self.client.get(href).await {
                Ok(res) => (res.etag, res.data.unwrap_or(body)),
                Err(_) => (None, body),
            },
        };
        let r = pushed.pim_ref();
        // Se relee el registro: si lo editaron durante la subida, se encola otra.
        let Some(mut current) = D::load(store, r)? else { return Ok(()) };
        let edited_meanwhile = !current.same_content(pushed);
        current.set_remote(href.as_str(), etag.as_deref());
        current.save(store)?;
        store.put_raw_object(r, body.as_bytes(), href.as_str(), etag.as_deref())?;
        let mut st = store.collection_sync(cref)?;
        let item = st.items.entry(href.to_string()).or_default();
        item.etag = etag;
        if !item.refs.contains(&r) {
            item.refs.push(r);
        }
        store.put_collection_sync(cref, &st)?;
        if edited_meanwhile && let Some(op) = D::put_op(r) {
            store.enqueue_op(ctx.acct, op.kind(), &op.encode())?;
        }
        ctx.changed(current.local_collection());
        Ok(())
    }

    /// 412: relee, fusiona y vuelve a subir; si chocan, gana el servidor.
    async fn resolve<D: DavDoc>(
        &self,
        ctx: &Ctx,
        cref: CollectionRef,
        href: &Url,
        base: Option<D>,
        local: D,
    ) -> Result<()> {
        let store = &ctx.store;
        let tz = ctx.opts.user_tz;
        let res = self.client.get(href).await?;
        let text = res.data.unwrap_or_default();
        let mut server = D::parse(store, &text, local.uid(), tz).ok_or(PimError::Conflict)?;
        server.adopt(&local);
        let merged = base.and_then(|b| D::merge(&b, &local, &server));
        let r = local.pim_ref();
        match merged {
            Some(m) => {
                let body = D::patch(store, &text, &server, &m, tz, Utc::now()).unwrap_or_else(|| text.clone());
                let cond = res.etag.clone().map_or(Precondition::None, Precondition::IfMatch);
                match self.client.put(href, &body, D::KIND, cond).await {
                    Ok(put) => {
                        m.save(store)?;
                        self.commit(ctx, cref, &m, href, put.etag, body).await?;
                        ctx.conflict(r, &m.summary(), ConflictResolution::Merged);
                        Ok(())
                    }
                    Err(iuredesk_dav::Error::PreconditionFailed) => Err(PimError::Conflict),
                    Err(e) => Err(e.into()),
                }
            }
            None => {
                server.set_remote(href.as_str(), res.etag.as_deref());
                server.save(store)?;
                store.put_raw_object(r, text.as_bytes(), href.as_str(), res.etag.as_deref())?;
                let mut st = store.collection_sync(cref)?;
                let item = st.items.entry(href.to_string()).or_default();
                item.etag = res.etag;
                if !item.refs.contains(&r) {
                    item.refs.push(r);
                }
                store.put_collection_sync(cref, &st)?;
                ctx.conflict(r, &server.summary(), ConflictResolution::ServerWon);
                ctx.changed(server.local_collection());
                Ok(())
            }
        }
    }

    async fn push_delete(&self, ctx: &Ctx, cref: CollectionRef, href: &str, etag: Option<&str>) -> Result<()> {
        let url = Url::parse(href).map_err(|e| PimError::Invalid(e.to_string()))?;
        let cond = etag.map_or(Precondition::None, |e| Precondition::IfMatch(e.to_string()));
        let cref = state_ref(&ctx.store, ctx.acct, cref)?;
        match self.client.delete(&url, cond).await {
            Ok(()) => {
                let mut st = ctx.store.collection_sync(cref)?;
                if st.items.remove(href).is_some() {
                    ctx.store.put_collection_sync(cref, &st)?;
                }
                Ok(())
            }
            // Cambió en el servidor después de nuestra última lectura: gana el servidor; la
            // próxima pasada lo vuelve a bajar.
            Err(iuredesk_dav::Error::PreconditionFailed) => {
                let mut st = ctx.store.collection_sync(cref)?;
                if let Some(item) = st.items.get_mut(href) {
                    item.etag = None;
                    item.refs.clear();
                    ctx.store.put_collection_sync(cref, &st)?;
                }
                ctx.emit(crate::PimEvent::Conflict {
                    account: ctx.acct,
                    item: PimRef::Event(EventId::UNASSIGNED),
                    summary: last_segment(&url),
                    resolution: ConflictResolution::ServerWon,
                });
                Ok(())
            }
            Err(e) => Err(e.into()),
        }
    }
}

impl Provider for CalDav {
    fn sync<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let disc = self.discovery(ctx).await?;
            let colls = self.refresh_collections(ctx, &disc).await?;
            let mut first_err = None;
            for c in &colls {
                if let Err(e) = self.pull(ctx, c).await {
                    // Una colección rota no detiene las demás.
                    if e.needs_reauth() {
                        return Err(e);
                    }
                    ctx.emit_error(&e);
                    first_err.get_or_insert(e);
                }
            }
            match first_err {
                Some(e) if e.is_transient() => Err(e),
                _ => Ok(()),
            }
        })
    }

    fn push<'a>(&'a self, ctx: &'a Ctx, op: &'a PimOp) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            match op {
                PimOp::PutEvent { event } | PimOp::Rsvp { event, .. } => {
                    self.push_put::<Event>(ctx, PimRef::Event(*event)).await
                }
                PimOp::PutTask { task } => self.push_put::<Task>(ctx, PimRef::Task(*task)).await,
                PimOp::PutContact { contact } => self.push_put::<Contact>(ctx, PimRef::Contact(*contact)).await,
                PimOp::DeleteEvent { calendar, href, etag } => {
                    self.push_delete(ctx, CollectionRef::Calendar(*calendar), href, etag.as_deref()).await
                }
                PimOp::DeleteTask { list, href, etag } => {
                    self.push_delete(ctx, CollectionRef::TaskList(*list), href, etag.as_deref()).await
                }
                PimOp::DeleteContact { book, href, etag } => {
                    self.push_delete(ctx, CollectionRef::AddressBook(*book), href, etag.as_deref()).await
                }
            }
        })
    }
}

/// Colección remota y sus contrapartes locales.
struct Coll {
    url: Url,
    state: CollectionRef,
    calendar: Option<CalendarId>,
    list: Option<TaskListId>,
    book: Option<AddressBookId>,
}

impl Coll {
    fn local_refs(&self) -> Vec<CollectionRef> {
        let mut v = Vec::new();
        if let Some(c) = self.calendar {
            v.push(CollectionRef::Calendar(c));
        }
        if let Some(l) = self.list {
            v.push(CollectionRef::TaskList(l));
        }
        if let Some(b) = self.book {
            v.push(CollectionRef::AddressBook(b));
        }
        v
    }
}

fn last_segment(u: &Url) -> String {
    u.path().trim_end_matches('/').rsplit('/').next().unwrap_or_default().to_string()
}

/// `#RRGGBB` o `#RRGGBBAA` → `0xRRGGBB`.
fn parse_color(s: &str) -> Option<u32> {
    let hex = s.trim().trim_start_matches('#');
    u32::from_str_radix(hex.get(..6)?, 16).ok()
}

/// Nombre de archivo seguro a partir del `UID`.
fn file_name(uid: &str, ext: &str) -> String {
    let clean: String = uid
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '-' })
        .take(120)
        .collect();
    let clean = if clean.trim_matches('-').is_empty() { ops::new_uid("x").replace('@', "-") } else { clean };
    format!("{clean}.{ext}")
}

/// Aplica un objeto descargado. Devuelve `true` si cambió algo local.
fn apply_resource(
    ctx: &Ctx,
    coll: &Coll,
    st: &mut CollectionSync,
    pending: &Pending,
    href: &str,
    etag: Option<&str>,
    data: &str,
) -> Result<bool> {
    let store = &ctx.store;
    let tz = ctx.opts.user_tz;
    let old_refs = st.items.get(href).map(|i| i.refs.clone()).unwrap_or_default();
    // Con cambios locales pendientes no se pisa nada (ni el ETag: sigue siendo la base del 412).
    if old_refs.iter().any(|r| pending.dirty.contains(r)) {
        return Ok(false);
    }
    let mut refs: Vec<PimRef> = Vec::new();
    if let Some(book) = coll.book {
        let Ok(cards) = VCard::parse_all(data) else { return Ok(false) };
        for card in cards.iter().take(1) {
            let (mut c, photo) = vcard::contact_from_vcard(card);
            let existing = old_refs.iter().find_map(|r| match r {
                PimRef::Contact(id) => Some(*id),
                _ => None,
            });
            c.id = existing.unwrap_or(ContactId::UNASSIGNED);
            c.address_book = book;
            c.photo = match photo {
                Some(bytes) => Some(store.blobs().put(&bytes)?),
                None => None,
            };
            c.remote_id = Some(href.to_string());
            c.etag = etag.map(str::to_string);
            let id = store.put_contact(&c)?;
            store.put_raw_object(PimRef::Contact(id), data.as_bytes(), href, etag)?;
            refs.push(PimRef::Contact(id));
        }
    } else {
        let Ok(cal) = ICalendar::parse(data) else { return Ok(false) };
        if let Some(cal_id) = coll.calendar {
            for mut e in ical::events_from_ics(&cal, tz) {
                let existing = old_refs.iter().find_map(|r| match r {
                    PimRef::Event(id) => store.get_event(*id).ok().flatten().filter(|x| x.uid == e.uid).map(|x| x.id),
                    _ => None,
                });
                e.id = existing.unwrap_or(EventId::UNASSIGNED);
                e.calendar = cal_id;
                e.remote_id = Some(href.to_string());
                e.etag = etag.map(str::to_string);
                let id = store.put_event(&e)?;
                store.put_raw_object(PimRef::Event(id), data.as_bytes(), href, etag)?;
                refs.push(PimRef::Event(id));
            }
        }
        if let Some(list) = coll.list {
            for mut t in ical::tasks_from_ics(&cal, tz) {
                let existing = old_refs.iter().find_map(|r| match r {
                    PimRef::Task(id) => store.get_task(*id).ok().flatten().filter(|x| x.uid == t.uid).map(|x| x.id),
                    _ => None,
                });
                t.id = existing.unwrap_or(TaskId::UNASSIGNED);
                t.list = list;
                t.remote_id = Some(href.to_string());
                t.etag = etag.map(str::to_string);
                let id = store.put_task(&t)?;
                store.put_raw_object(PimRef::Task(id), data.as_bytes(), href, etag)?;
                refs.push(PimRef::Task(id));
            }
        }
    }
    for gone in old_refs.iter().filter(|r| !refs.contains(r)) {
        delete_ref(store, *gone)?;
    }
    let item = st.items.entry(href.to_string()).or_default();
    item.etag = etag.map(str::to_string);
    item.refs = refs;
    Ok(true)
}

fn delete_ref(store: &Store, r: PimRef) -> Result<()> {
    match r {
        PimRef::Event(id) => store.delete_event(id)?,
        PimRef::Task(id) => store.delete_task(id)?,
        PimRef::Contact(id) => store.delete_contact(id)?,
    }
    Ok(())
}

fn remove_href(ctx: &Ctx, st: &mut CollectionSync, pending: &Pending, href: &str) -> Result<bool> {
    let Some(item) = st.items.remove(href) else { return Ok(false) };
    for r in &item.refs {
        if pending.dirty.contains(r) {
            // Borrado en el servidor y editado aquí: gana el servidor.
            ctx.conflict(*r, href, ConflictResolution::ServerWon);
        }
        delete_ref(&ctx.store, *r)?;
    }
    Ok(!item.refs.is_empty())
}

// ------------------------------------------------------------------ elementos subibles

/// Lo común a eventos, tareas y contactos para subirlos.
pub(crate) trait DavDoc: Sized + Clone + Send + Sync {
    const KIND: CollectionKind;
    const EXT: &'static str;
    fn load(store: &Store, r: PimRef) -> Result<Option<Self>>;
    fn pim_ref(&self) -> PimRef;
    fn uid(&self) -> &str;
    fn summary(&self) -> String;
    /// (ruta de la colección, clave de estado, solo lectura).
    fn collection(
        &self,
        store: &Store,
        acct: iuredesk_model::AccountId,
    ) -> Result<Option<(String, CollectionRef, bool)>>;
    fn local_collection(&self) -> CollectionRef;
    /// Lee la versión del objeto con ese `UID` (ids locales vacíos).
    fn parse(store: &Store, data: &str, uid: &str, tz: Tz) -> Option<Self>;
    /// `data` con los cambios `base` → `new`; `None` si no cambió nada.
    fn patch(store: &Store, data: &str, base: &Self, new: &Self, tz: Tz, now: DateTime<Utc>) -> Option<String>;
    fn fresh(&self, store: &Store, now: DateTime<Utc>) -> String;
    fn merge(base: &Self, local: &Self, server: &Self) -> Option<Self>;
    /// Copia ids locales (id, colección) de `local`.
    fn adopt(&mut self, local: &Self);
    fn set_remote(&mut self, href: &str, etag: Option<&str>);
    /// Mismo contenido (sin contar metadatos de sincronización).
    fn same_content(&self, other: &Self) -> bool;
    fn save(&self, store: &Store) -> Result<()>;
    fn delete(&self, store: &Store) -> Result<()>;
    fn put_op(r: PimRef) -> Option<PimOp>;
}

impl DavDoc for Event {
    const KIND: CollectionKind = CollectionKind::Calendar;
    const EXT: &'static str = "ics";
    fn load(store: &Store, r: PimRef) -> Result<Option<Self>> {
        let PimRef::Event(id) = r else { return Ok(None) };
        Ok(store.get_event(id)?)
    }
    fn pim_ref(&self) -> PimRef {
        PimRef::Event(self.id)
    }
    fn uid(&self) -> &str {
        &self.uid
    }
    fn summary(&self) -> String {
        self.summary.clone()
    }
    fn collection(
        &self,
        store: &Store,
        _acct: iuredesk_model::AccountId,
    ) -> Result<Option<(String, CollectionRef, bool)>> {
        Ok(store
            .get_calendar(self.calendar)?
            .filter(|c| !c.remote_path.is_empty())
            .map(|c| (c.remote_path, CollectionRef::Calendar(self.calendar), c.read_only)))
    }
    fn local_collection(&self) -> CollectionRef {
        CollectionRef::Calendar(self.calendar)
    }
    fn parse(_store: &Store, data: &str, uid: &str, tz: Tz) -> Option<Self> {
        let cal = ICalendar::parse(data).ok()?;
        ical::events_from_ics(&cal, tz).into_iter().find(|e| e.uid == uid)
    }
    fn patch(_store: &Store, data: &str, base: &Self, new: &Self, tz: Tz, now: DateTime<Utc>) -> Option<String> {
        let mut cal = ICalendar::parse(data).ok()?;
        ical::patch_event(&mut cal, base, new, tz, now).then(|| cal.to_ics())
    }
    fn fresh(&self, _store: &Store, now: DateTime<Utc>) -> String {
        ical::new_event_ics(self, now).to_ics()
    }
    fn merge(base: &Self, local: &Self, server: &Self) -> Option<Self> {
        merge::merge_event(base, local, server)
    }
    fn adopt(&mut self, local: &Self) {
        self.id = local.id;
        self.calendar = local.calendar;
    }
    fn set_remote(&mut self, href: &str, etag: Option<&str>) {
        self.remote_id = Some(href.to_string());
        self.etag = etag.map(str::to_string);
    }
    fn same_content(&self, o: &Self) -> bool {
        let strip = |e: &Event| Event { remote_id: None, etag: None, updated: None, ..e.clone() };
        strip(self) == strip(o)
    }
    fn save(&self, store: &Store) -> Result<()> {
        store.put_event(self)?;
        Ok(())
    }
    fn delete(&self, store: &Store) -> Result<()> {
        Ok(store.delete_event(self.id)?)
    }
    fn put_op(r: PimRef) -> Option<PimOp> {
        match r {
            PimRef::Event(event) => Some(PimOp::PutEvent { event }),
            _ => None,
        }
    }
}

impl DavDoc for Task {
    const KIND: CollectionKind = CollectionKind::Calendar;
    const EXT: &'static str = "ics";
    fn load(store: &Store, r: PimRef) -> Result<Option<Self>> {
        let PimRef::Task(id) = r else { return Ok(None) };
        Ok(store.get_task(id)?)
    }
    fn pim_ref(&self) -> PimRef {
        PimRef::Task(self.id)
    }
    fn uid(&self) -> &str {
        &self.uid
    }
    fn summary(&self) -> String {
        self.summary.clone()
    }
    fn collection(
        &self,
        store: &Store,
        acct: iuredesk_model::AccountId,
    ) -> Result<Option<(String, CollectionRef, bool)>> {
        let Some(list) = store.get_task_list(self.list)?.filter(|l| !l.remote_path.is_empty()) else { return Ok(None) };
        let cref = state_ref(store, acct, CollectionRef::TaskList(self.list))?;
        Ok(Some((list.remote_path, cref, false)))
    }
    fn local_collection(&self) -> CollectionRef {
        CollectionRef::TaskList(self.list)
    }
    fn parse(_store: &Store, data: &str, uid: &str, tz: Tz) -> Option<Self> {
        let cal = ICalendar::parse(data).ok()?;
        ical::tasks_from_ics(&cal, tz).into_iter().find(|t| t.uid == uid)
    }
    fn patch(_store: &Store, data: &str, base: &Self, new: &Self, _tz: Tz, now: DateTime<Utc>) -> Option<String> {
        let mut cal = ICalendar::parse(data).ok()?;
        ical::patch_task(&mut cal, base, new, now).then(|| cal.to_ics())
    }
    fn fresh(&self, _store: &Store, now: DateTime<Utc>) -> String {
        ical::new_task_ics(self, now).to_ics()
    }
    fn merge(base: &Self, local: &Self, server: &Self) -> Option<Self> {
        merge::merge_task(base, local, server)
    }
    fn adopt(&mut self, local: &Self) {
        self.id = local.id;
        self.list = local.list;
    }
    fn set_remote(&mut self, href: &str, etag: Option<&str>) {
        self.remote_id = Some(href.to_string());
        self.etag = etag.map(str::to_string);
    }
    fn same_content(&self, o: &Self) -> bool {
        let strip = |t: &Task| Task { remote_id: None, etag: None, updated: None, ..t.clone() };
        strip(self) == strip(o)
    }
    fn save(&self, store: &Store) -> Result<()> {
        store.put_task(self)?;
        Ok(())
    }
    fn delete(&self, store: &Store) -> Result<()> {
        Ok(store.delete_task(self.id)?)
    }
    fn put_op(r: PimRef) -> Option<PimOp> {
        match r {
            PimRef::Task(task) => Some(PimOp::PutTask { task }),
            _ => None,
        }
    }
}

impl DavDoc for Contact {
    const KIND: CollectionKind = CollectionKind::AddressBook;
    const EXT: &'static str = "vcf";
    fn load(store: &Store, r: PimRef) -> Result<Option<Self>> {
        let PimRef::Contact(id) = r else { return Ok(None) };
        Ok(store.get_contact(id)?)
    }
    fn pim_ref(&self) -> PimRef {
        PimRef::Contact(self.id)
    }
    fn uid(&self) -> &str {
        &self.uid
    }
    fn summary(&self) -> String {
        self.full_name.clone()
    }
    fn collection(
        &self,
        store: &Store,
        _acct: iuredesk_model::AccountId,
    ) -> Result<Option<(String, CollectionRef, bool)>> {
        Ok(store
            .get_address_book(self.address_book)?
            .filter(|b| !b.remote_path.is_empty())
            .map(|b| (b.remote_path, CollectionRef::AddressBook(self.address_book), b.read_only)))
    }
    fn local_collection(&self) -> CollectionRef {
        CollectionRef::AddressBook(self.address_book)
    }
    fn parse(store: &Store, data: &str, uid: &str, _tz: Tz) -> Option<Self> {
        let cards = VCard::parse_all(data).ok()?;
        let card = cards.iter().find(|c| c.uid().as_deref() == Some(uid)).or_else(|| cards.first())?;
        let (mut c, photo) = vcard::contact_from_vcard(card);
        c.photo = photo.and_then(|b| store.blobs().put(&b).ok());
        Some(c)
    }
    fn patch(store: &Store, data: &str, base: &Self, new: &Self, _tz: Tz, now: DateTime<Utc>) -> Option<String> {
        let cards = VCard::parse_all(data).ok()?;
        let mut card = cards.into_iter().next()?;
        let photo = new.photo.and_then(|h| store.blobs().get(&h).ok());
        let mut old = base.clone();
        // La base de un contacto subido aquí trae la uid del archivo; el resto se compara igual.
        old.uid.clone_from(&new.uid);
        vcard::patch_contact(&mut card, &old, new, photo.as_deref(), now).then(|| card.to_vcf())
    }
    fn fresh(&self, store: &Store, now: DateTime<Utc>) -> String {
        let photo = self.photo.and_then(|h| store.blobs().get(&h).ok());
        vcard::new_vcard(self, photo.as_deref(), now).to_vcf()
    }
    fn merge(base: &Self, local: &Self, server: &Self) -> Option<Self> {
        merge::merge_contact(base, local, server)
    }
    fn adopt(&mut self, local: &Self) {
        self.id = local.id;
        self.address_book = local.address_book;
    }
    fn set_remote(&mut self, href: &str, etag: Option<&str>) {
        self.remote_id = Some(href.to_string());
        self.etag = etag.map(str::to_string);
    }
    fn same_content(&self, o: &Self) -> bool {
        let strip = |c: &Contact| Contact { remote_id: None, etag: None, updated: None, ..c.clone() };
        strip(self) == strip(o)
    }
    fn save(&self, store: &Store) -> Result<()> {
        store.put_contact(self)?;
        Ok(())
    }
    fn delete(&self, store: &Store) -> Result<()> {
        Ok(store.delete_contact(self.id)?)
    }
    fn put_op(r: PimRef) -> Option<PimOp> {
        match r {
            PimRef::Contact(contact) => Some(PimOp::PutContact { contact }),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(parse_color("#0B5FFFFF"), Some(0x0B5FFF));
        assert_eq!(parse_color("x"), None);
        assert_eq!(file_name("a/b c@x", "ics"), "a-b-c-x.ics");
        assert!(file_name("///", "vcf").ends_with(".vcf"));
        assert_eq!(last_segment(&Url::parse("https://x/dav/cal/work/").unwrap()), "work");
    }
}
