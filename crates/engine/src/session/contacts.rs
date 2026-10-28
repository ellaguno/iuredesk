//! Contactos en la sesión: nombre para mostrar (nunca una fila vacía), contactos vacíos, la
//! selección de la lista y «Eliminar» (`contacts.delete`) por el camino local-primero de
//! `iuredesk-pim-sync` (se borra en local, se encola el borrado en el servidor —Graph o
//! CardDAV— y se puede deshacer). Las libretas de solo lectura (Iurefficient) lo dicen.

use iuredesk_model::{Contact, ContactId};
use iuredesk_pim_sync::PimError;
use iuredesk_pim_sync::ops::PimOp;
use iuredesk_store::PimRef;

use super::{Inverse, Notice, NoticeKind, Outcome, Session, store_err};
use crate::lang::Text;
use crate::{EngineError, Result};

/// Contactos que se borran de una vez, como mucho.
const MAX_DELETE: usize = 5_000;

/// Un contacto borrado, para deshacer: el contacto y su objeto crudo del servidor.
#[derive(Clone, Debug)]
pub(crate) struct ContactSnap {
    contact: Contact,
    raw: Option<(Vec<u8>, String, Option<String>)>,
}

/// Nombre para mostrar: el nombre, o nombre y apellidos, apodo, empresa, correo o teléfono.
/// `None` si el contacto no tiene nada de eso (la UI muestra «(Sin nombre)»).
pub fn contact_display_name(c: &Contact) -> Option<String> {
    let full = c.full_name.trim();
    if !full.is_empty() {
        return Some(full.to_string());
    }
    let parts: Vec<&str> = [c.given_name.trim(), c.family_name.trim()].into_iter().filter(|s| !s.is_empty()).collect();
    if !parts.is_empty() {
        return Some(parts.join(" "));
    }
    [c.nickname.trim(), c.org.trim()]
        .into_iter()
        .find(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| c.emails.iter().map(|e| e.value.trim()).find(|s| !s.is_empty()).map(str::to_string))
        .or_else(|| c.phones.iter().map(|p| p.value.trim()).find(|s| !s.is_empty()).map(str::to_string))
}

/// ¿El contacto está vacío (sin nombre, empresa, correo, teléfono, dirección ni notas)? Se ocultan
/// por omisión de la lista.
pub fn contact_is_empty(c: &Contact) -> bool {
    contact_display_name(c).is_none()
        && c.addresses.iter().all(|a| [&a.street, &a.city, &a.region, &a.country].iter().all(|s| s.trim().is_empty()))
        && c.notes.trim().is_empty()
        && c.title.trim().is_empty()
        && c.birthday.is_none()
}

fn pim_err(e: PimError) -> EngineError {
    match e {
        PimError::ReadOnly => EngineError::Invalid("libreta de solo lectura".into()),
        other => EngineError::Store(other.to_string()),
    }
}

impl Session {
    /// Contactos seleccionados en la lista (los fija la UI; `contacts.delete` actúa sobre ellos).
    pub fn set_contact_selection(&mut self, ids: Vec<ContactId>) {
        self.contact_selection = ids;
    }

    /// Selección de contactos.
    pub fn contact_selection(&self) -> &[ContactId] {
        &self.contact_selection
    }

    /// ¿Se puede borrar el contacto (su libreta no es de solo lectura)?
    pub fn contact_deletable(&self, id: ContactId) -> bool {
        self.store
            .get_contact(id)
            .ok()
            .flatten()
            .and_then(|c| self.store.get_address_book(c.address_book).ok().flatten())
            .is_some_and(|b| !b.read_only)
    }

    /// `contacts.delete`: con más de uno pide confirmación (`Outcome::Confirm`), salvo que el
    /// argumento sea `yes` (`contacts.delete:yes`, también desde la CLI o MCP).
    pub(crate) fn run_delete_contacts(&mut self, arg: Option<&str>) -> Result<Outcome> {
        let ids = self.contact_selection.clone();
        if ids.len() > 1 && arg != Some("yes") {
            let n = ids.len();
            return Ok(Outcome::Confirm {
                text: Text::new(
                    format!("Delete {n} contacts? They are also deleted from the server; you can undo it right after."),
                    format!(
                        "¿Eliminar {n} contactos? También se eliminan del servidor; puedes deshacerlo justo después."
                    ),
                ),
                yes: Text::new(format!("Delete {n}"), format!("Eliminar {n}")),
                command: "contacts.delete:yes".into(),
            });
        }
        self.delete_contacts(&ids)
    }

    /// Borra contactos (local primero; el servidor se entera por la cola) con deshacer.
    pub fn delete_contacts(&mut self, ids: &[ContactId]) -> Result<Outcome> {
        let mut snaps = Vec::new();
        let mut read_only: Vec<String> = Vec::new();
        for &id in ids.iter().take(MAX_DELETE) {
            let Some(contact) = self.store.get_contact(id).map_err(store_err)? else { continue };
            let book = self.store.get_address_book(contact.address_book).map_err(store_err)?;
            if book.as_ref().is_some_and(|b| b.read_only) {
                let name = book.map(|b| b.name).unwrap_or_default();
                if !read_only.contains(&name) {
                    read_only.push(name);
                }
                continue;
            }
            let raw = self.store.raw_object_data(PimRef::Contact(id)).ok().flatten().map(|(r, d)| (d, r.href, r.etag));
            let r = match &self.pim.handle {
                Some(h) => h.delete_contact(id),
                None => iuredesk_pim_sync::ops::delete_contact(&self.store, id),
            };
            match r {
                Ok(()) => snaps.push(ContactSnap { contact, raw }),
                Err(PimError::ReadOnly) => {}
                Err(e) => {
                    self.contacts_changed();
                    return Err(pim_err(e));
                }
            }
        }
        self.contacts_changed();
        self.contact_selection.retain(|id| !snaps.iter().any(|s| s.contact.id == *id));
        let books = read_only.join(", ");
        if snaps.is_empty() {
            return Ok(Outcome::Notice(if read_only.is_empty() {
                Notice::info("Select a contact first.", "Primero selecciona un contacto.")
            } else {
                Notice::info(
                    format!("«{books}» is read-only: its contacts can't be deleted from IureDesk."),
                    format!("«{books}» es de solo lectura: sus contactos no se pueden eliminar desde IureDesk."),
                )
            }));
        }
        let n = snaps.len();
        let what = match snaps.first().filter(|_| n == 1) {
            Some(s) => {
                let name = contact_display_name(&s.contact).unwrap_or_else(|| "(Sin nombre)".into());
                Text::new(format!("Deleted «{name}»"), format!("Se eliminó «{name}»"))
            }
            None => Text::new(format!("Deleted {n} contacts"), format!("Se eliminaron {n} contactos")),
        };
        let text = if read_only.is_empty() {
            what.clone()
        } else {
            Text::new(
                format!("{}. «{books}» is read-only and was left as is.", what.en),
                format!("{}. «{books}» es de solo lectura y quedó igual.", what.es),
            )
        };
        self.push_undo(what, vec![(Inverse::Contacts(snaps), None)]);
        Ok(Outcome::Notice(Notice { kind: NoticeKind::Success, text, undo: true, secs: None }))
    }

    /// Deshace un borrado de contactos (desde `undo_last`): si el borrado aún no salió de la
    /// máquina se cancela y vuelve tal cual; si ya se borró en el servidor, se vuelve a crear.
    pub(crate) fn undo_contacts(&mut self, snaps: Vec<ContactSnap>) -> Result<()> {
        let pending = self.store.peek_ops(10_000).unwrap_or_default();
        for snap in snaps {
            let href = snap.raw.as_ref().map(|r| r.1.clone()).or(snap.contact.remote_id.clone());
            let mut cancelled = false;
            if let Some(href) = &href {
                for op in &pending {
                    if let Some(PimOp::DeleteContact { href: h, .. }) = PimOp::decode(&op.kind, &op.payload)
                        && h == *href
                        && self.store.ack_op(op.seq).is_ok()
                    {
                        cancelled = true;
                    }
                }
            }
            if cancelled || href.is_none() {
                self.store.put_contact(&snap.contact).map_err(store_err)?;
                if let Some((data, href, etag)) = &snap.raw {
                    let _ = self.store.put_raw_object(PimRef::Contact(snap.contact.id), data, href, etag.as_deref());
                }
                // Sin `href` nunca llegó al servidor: que se suba.
                if href.is_none() {
                    let c = snap.contact.clone();
                    let _ = match &self.pim.handle {
                        Some(h) => h.save_contact(&c, None),
                        None => iuredesk_pim_sync::ops::save_contact(&self.store, &c, None),
                    };
                }
            } else {
                let mut c = snap.contact.clone();
                c.id = ContactId::UNASSIGNED;
                c.remote_id = None;
                c.etag = None;
                match &self.pim.handle {
                    Some(h) => h.save_contact(&c, None),
                    None => iuredesk_pim_sync::ops::save_contact(&self.store, &c, None),
                }
                .map_err(pim_err)?;
            }
        }
        self.contacts_changed();
        Ok(())
    }

    fn contacts_changed(&mut self) {
        self.contacts_cache = None;
        self.data_gen = self.data_gen.wrapping_add(1);
    }
}

#[cfg(test)]
mod tests {
    use iuredesk_model::Labeled;

    use super::*;

    #[test]
    fn display_name_never_empty_when_there_is_data() {
        let mut c = Contact::default();
        assert_eq!(contact_display_name(&c), None);
        assert!(contact_is_empty(&c));
        c.phones.push(Labeled::new("cell", "+52 55 1234"));
        assert_eq!(contact_display_name(&c).as_deref(), Some("+52 55 1234"));
        c.emails.push(Labeled::new("", "ana@x.com"));
        assert_eq!(contact_display_name(&c).as_deref(), Some("ana@x.com"));
        c.org = "Contoso".into();
        assert_eq!(contact_display_name(&c).as_deref(), Some("Contoso"));
        c.given_name = "Ana".into();
        c.family_name = "Ruiz".into();
        assert_eq!(contact_display_name(&c).as_deref(), Some("Ana Ruiz"));
        c.full_name = "  Ana R. ".into();
        assert_eq!(contact_display_name(&c).as_deref(), Some("Ana R."));
        assert!(!contact_is_empty(&c));
        let notes = Contact { notes: "algo".into(), ..Default::default() };
        assert!(!contact_is_empty(&notes));
    }
}
