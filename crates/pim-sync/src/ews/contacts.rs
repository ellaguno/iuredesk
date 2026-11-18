//! Contactos por EWS (F8.4, hito 2): la carpeta de contactos del buzón como una libreta.
//!
//! Bajar es incremental con `SyncFolderItems` (el `SyncState` va en `delta_link`, como el
//! `deltaLink` de Graph): solo los ids de lo creado, cambiado o borrado, y los datos de lo
//! creado o cambiado con `GetItem`. Un estado que el servidor ya no reconoce vuelve a empezar
//! desde cero. Subir, como el calendario: solo los campos cambiados con el `ChangeKey`, y
//! fusión a tres bandas si el contacto cambió en el servidor.

use iuredesk_model::{AddressBook, AddressBookId, Contact, ContactId};
use iuredesk_store::{CollectionRef, PimRef};
use serde_json::Value;

use super::{Ews, raw_json};
use crate::error::{PimError, Result};
use crate::map::ews::contact as cmap;
use crate::map::graph as gmap;
use crate::merge;
use crate::ops::{self, PimOp};
use crate::provider::Ctx;
use crate::{ConflictResolution, PimEvent};

/// Cambios de una carpeta según `SyncFolderItems`.
#[derive(Debug, Default)]
pub(crate) struct FolderChanges {
    /// Creados o cambiados (hay que pedir sus datos).
    pub upserts: Vec<String>,
    /// Borrados.
    pub deleted: Vec<String>,
    /// Estado nuevo (con su prefijo), para `delta_link`.
    pub state: Option<String>,
    /// Desde cero: lo local que no esté en `upserts` ya no existe.
    pub full: bool,
}

impl FolderChanges {
    /// Aplica los borrados (y, desde cero, quita lo que no volvió) y guarda el estado nuevo.
    pub fn finish(self, ctx: &Ctx, st: &mut iuredesk_store::CollectionSync, pending: &ops::Pending) -> Result<bool> {
        let mut changed = false;
        for id in &self.deleted {
            changed |= super::remove_item(ctx, st, pending, id)?;
        }
        if self.full {
            let gone: Vec<String> = st.items.keys().filter(|k| !self.upserts.contains(k)).cloned().collect();
            for id in gone {
                changed |= super::remove_item(ctx, st, pending, &id)?;
            }
        }
        st.delta_link = self.state;
        Ok(changed)
    }
}

/// Ruta local (y carpeta distinguida de EWS) de la libreta.
pub(crate) const CONTACTS: &str = "contacts";
/// Prefijo del estado guardado (distingue el `SyncState` de EWS de otros).
const STATE_PREFIX: &str = "ews-sync:";
/// Llamadas a `SyncFolderItems` por pasada (512 cambios cada una).
const MAX_PAGES: usize = 40;

impl Ews {
    /// La libreta de contactos de la cuenta (se crea la primera vez).
    pub(crate) fn contact_book(&self, ctx: &Ctx) -> Result<AddressBookId> {
        let store = &ctx.store;
        if let Some(b) = store.address_books()?.into_iter().find(|b| b.account == ctx.acct && b.remote_path == CONTACTS)
        {
            return Ok(b.id);
        }
        let id = store.put_address_book(&AddressBook {
            account: ctx.acct,
            name: if ctx.opts.spanish { "Contactos".into() } else { "Contacts".into() },
            remote_path: CONTACTS.into(),
            ..Default::default()
        })?;
        ctx.emit(PimEvent::CollectionsChanged { account: ctx.acct });
        Ok(id)
    }

    /// Cambios de una carpeta (`contacts`, `tasks`) desde el estado guardado en `saved`: los ids
    /// creados o cambiados, los borrados y el estado nuevo. Si el servidor olvidó el estado,
    /// vuelve a empezar (`full`: lo local que no aparezca ya no existe).
    pub(crate) async fn folder_changes(&self, folder: &str, saved: Option<&str>) -> Result<FolderChanges> {
        let mut state = saved.and_then(|s| s.strip_prefix(STATE_PREFIX)).map(str::to_string);
        let mut out = FolderChanges { full: state.is_none(), ..Default::default() };
        for _ in 0..MAX_PAGES {
            let page = match self.client.sync_folder_items(folder, state.as_deref(), 512).await {
                Ok(p) => p,
                Err(e)
                    if !out.full
                        && matches!(
                            e.code(),
                            Some("ErrorInvalidSyncStateData" | "ErrorInvalidSyncStateDataVersion")
                        ) =>
                {
                    // El servidor olvidó el estado: todo de nuevo.
                    state = None;
                    out = FolderChanges { full: true, ..Default::default() };
                    continue;
                }
                Err(e) => return Err(e.into()),
            };
            for (id, _) in page.created.iter().chain(&page.updated) {
                out.deleted.retain(|d| d != id);
                if !out.upserts.contains(id) {
                    out.upserts.push(id.clone());
                }
            }
            for id in &page.deleted {
                out.upserts.retain(|u| u != id);
                out.deleted.push(id.clone());
            }
            state = Some(page.state.clone());
            if page.last {
                break;
            }
        }
        out.state = state.map(|s| format!("{STATE_PREFIX}{s}"));
        Ok(out)
    }

    /// Cambios de la carpeta de contactos desde la pasada anterior.
    pub(crate) async fn pull_contacts(&self, ctx: &Ctx, book: AddressBookId) -> Result<()> {
        let store = &ctx.store;
        let cref = CollectionRef::AddressBook(book);
        let mut st = store.collection_sync(cref)?;
        let changes = self.folder_changes(CONTACTS, st.delta_link.as_deref()).await?;
        let pending = ops::pending(store, ctx.acct)?;
        let mut changed = false;
        let wanted: Vec<String> = changes.upserts.iter().filter(|id| !pending.deleted.contains(*id)).cloned().collect();
        for e in self.client.get_contacts(&wanted).await? {
            let Some(v) = cmap::contact_json(&e) else { continue };
            let Some(id) = v.get("id").and_then(Value::as_str).map(str::to_string) else { continue };
            changed |= apply_contact(ctx, &mut st, &pending, book, &id, &v)?;
        }
        changed |= changes.finish(ctx, &mut st, &pending)?;
        store.put_collection_sync(cref, &st)?;
        if changed {
            ctx.changed(cref);
        }
        Ok(())
    }

    pub(crate) async fn push_contact(&self, ctx: &Ctx, id: ContactId) -> Result<()> {
        let store = &ctx.store;
        let Some(local) = store.get_contact(id)? else { return Ok(()) };
        let r = PimRef::Contact(id);
        let saved = raw_json(store, r);
        let result = match &saved {
            Some((raw, base_json)) => {
                let base = gmap::contact_from_graph(base_json);
                let updates = cmap::update_xml(&gmap::contact_patch(Some(&base), &local));
                if updates.is_empty() {
                    return Ok(());
                }
                match self.client.update_item(&raw.href, raw.etag.as_deref(), &updates).await {
                    Err(e) if iuredesk_ews::calendar::is_conflict(&e) => {
                        return self.resolve_contact(ctx, local, base).await;
                    }
                    Err(e) if e.is_not_found() => {
                        store.delete_contact(id)?;
                        ctx.conflict(r, &local.full_name, ConflictResolution::ServerWon);
                        ctx.changed(CollectionRef::AddressBook(local.address_book));
                        return Ok(());
                    }
                    other => other?,
                }
            }
            None => {
                let xml = cmap::create_xml(&gmap::contact_patch(None, &local));
                self.client.create_contact(CONTACTS, &xml).await?
            }
        };
        let Some(server) = self.client.get_contact(&result.0).await?.and_then(|e| cmap::contact_json(&e)) else {
            return Err(PimError::Invalid("Exchange no devolvió el contacto guardado".into()));
        };
        commit_contact(ctx, &local, &server)
    }

    async fn resolve_contact(&self, ctx: &Ctx, local: Contact, base: Contact) -> Result<()> {
        let r = PimRef::Contact(local.id);
        let Some(remote_id) = local.remote_id.clone() else { return Err(PimError::Conflict) };
        let Some(sj) = self.client.get_contact(&remote_id).await?.and_then(|e| cmap::contact_json(&e)) else {
            ctx.store.delete_contact(local.id)?;
            ctx.conflict(r, &local.full_name, ConflictResolution::ServerWon);
            ctx.changed(CollectionRef::AddressBook(local.address_book));
            return Ok(());
        };
        let adopt = |v: &Value, l: &Contact| {
            let mut c = gmap::contact_from_graph(v);
            c.id = l.id;
            c.address_book = l.address_book;
            c.photo = l.photo;
            c
        };
        let server = adopt(&sj, &local);
        match merge::merge_contact(&base, &local, &server) {
            Some(m) => {
                let updates = cmap::update_xml(&gmap::contact_patch(Some(&server), &m));
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
                ctx.store.put_contact(&m)?;
                let Some(after) = self.client.get_contact(&id).await?.and_then(|e| cmap::contact_json(&e)) else {
                    return Err(PimError::Conflict);
                };
                commit_contact(ctx, &m, &after)?;
                ctx.conflict(r, &m.full_name, ConflictResolution::Merged);
            }
            None => {
                commit_contact(ctx, &local, &sj)?;
                ctx.store.put_contact(&server)?;
                ctx.conflict(r, &server.full_name, ConflictResolution::ServerWon);
            }
        }
        Ok(())
    }

    pub(crate) async fn push_delete_contact(&self, ctx: &Ctx, book: AddressBookId, href: &str) -> Result<()> {
        self.client.delete_item(href, false).await?;
        let cref = CollectionRef::AddressBook(book);
        let mut st = ctx.store.collection_sync(cref)?;
        if st.items.remove(href).is_some() {
            ctx.store.put_collection_sync(cref, &st)?;
        }
        Ok(())
    }
}

/// Guarda un contacto bajado (salvo que tenga cambios locales sin subir). La foto local se
/// conserva (EWS no la trae en `AllProperties`).
fn apply_contact(
    ctx: &Ctx,
    st: &mut iuredesk_store::CollectionSync,
    pending: &ops::Pending,
    book: AddressBookId,
    id: &str,
    v: &Value,
) -> Result<bool> {
    let store = &ctx.store;
    let old_refs = st.items.get(id).map(|i| i.refs.clone()).unwrap_or_default();
    if old_refs.iter().any(|r| pending.dirty.contains(r)) {
        return Ok(false);
    }
    let old = old_refs.iter().find_map(|r| match r {
        PimRef::Contact(i) => store.get_contact(*i).ok().flatten(),
        _ => None,
    });
    let mut c = gmap::contact_from_graph(v);
    c.id = old.as_ref().map_or(ContactId::UNASSIGNED, |o| o.id);
    c.photo = old.and_then(|o| o.photo);
    c.address_book = book;
    let r = PimRef::Contact(store.put_contact(&c)?);
    let bytes = serde_json::to_vec(v).map_err(|e| PimError::Invalid(e.to_string()))?;
    let etag = gmap::etag(v).filter(|e| !e.is_empty());
    store.put_raw_object(r, &bytes, id, etag.as_deref())?;
    let item = st.items.entry(id.to_string()).or_default();
    item.etag = etag;
    item.refs = vec![r];
    Ok(true)
}

/// Base nueva tras subir; si el contacto cambió en local durante la subida, se conserva y se
/// encola otra subida.
fn commit_contact(ctx: &Ctx, pushed: &Contact, server: &Value) -> Result<()> {
    let store = &ctx.store;
    let r = PimRef::Contact(pushed.id);
    let cref = CollectionRef::AddressBook(pushed.address_book);
    let Some(remote_id) = server.get("id").and_then(Value::as_str) else {
        return Err(PimError::Invalid("Exchange no devolvió el id".into()));
    };
    let etag = gmap::etag(server).filter(|e| !e.is_empty());
    let current = store.get_contact(pushed.id)?;
    let strip = |c: &Contact| Contact { remote_id: None, etag: None, updated: None, uid: String::new(), ..c.clone() };
    let edited = current.as_ref().is_some_and(|c| strip(c) != strip(pushed));
    let c = match (edited, current) {
        (true, Some(mut c)) => {
            c.remote_id = Some(remote_id.to_string());
            c.etag = etag.clone();
            c
        }
        _ => {
            let mut c = gmap::contact_from_graph(server);
            c.id = pushed.id;
            c.address_book = pushed.address_book;
            c.photo = pushed.photo;
            c
        }
    };
    store.put_contact(&c)?;
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
        let op = PimOp::PutContact { contact: pushed.id };
        store.enqueue_op(ctx.acct, op.kind(), &op.encode())?;
    }
    ctx.changed(cref);
    Ok(())
}
