//! Contactos en la sesión: nombre para mostrar (nunca una fila vacía), contactos vacíos, la
//! selección de la lista y «Eliminar» (`contacts.delete`) por el camino local-primero de
//! `iuredesk-pim-sync` (se borra en local, se encola el borrado en el servidor —Graph o
//! CardDAV— y se puede deshacer). Las libretas de solo lectura (Iurefficient) lo dicen.
//! UX-10: crear y editar (`save_contact_edit`), también local primero y con deshacer; al subirlo,
//! `pim-sync` mezcla con la vCard del servidor, así que lo que el editor no muestra se conserva.

use iuredesk_model::{AddressBook, AddressBookId, Contact, ContactId, Labeled, PostalAddress};
use iuredesk_pim_sync::PimError;
use iuredesk_pim_sync::ops::PimOp;
use iuredesk_store::PimRef;

use super::{Inverse, Notice, NoticeKind, Outcome, Session, store_err};
use crate::lang::Text;
use crate::{EngineError, Result};

/// Contactos que se borran de una vez, como mucho.
const MAX_DELETE: usize = 5_000;
/// Largo máximo de un campo de una línea, de las notas y número de correos/teléfonos/direcciones.
const MAX_FIELD: usize = 512;
const MAX_NOTES: usize = 20_000;
const MAX_ENTRIES: usize = 50;

/// Recorta espacios y largo (por caracteres, nunca a media letra).
fn clip(s: &str, max: usize) -> String {
    s.trim().chars().take(max).collect()
}

/// Un contacto editado, limpio: campos recortados, entradas vacías fuera, `FN` nunca vacío.
/// `Err` con el texto para el usuario si no tiene nada o un correo no lo parece.
pub fn clean_contact(c: &Contact) -> std::result::Result<Contact, Text> {
    let mut c = c.clone();
    for f in [&mut c.given_name, &mut c.family_name, &mut c.nickname, &mut c.org, &mut c.title] {
        *f = clip(f, MAX_FIELD);
    }
    c.full_name = clip(&c.full_name, MAX_FIELD);
    c.notes = c.notes.trim().chars().take(MAX_NOTES).collect();
    let labeled = |v: &[Labeled]| -> Vec<Labeled> {
        v.iter()
            .map(|l| Labeled::new(clip(&l.label, 64), clip(&l.value, MAX_FIELD)))
            .filter(|l| !l.value.is_empty())
            .take(MAX_ENTRIES)
            .collect()
    };
    c.emails = labeled(&c.emails);
    c.phones = labeled(&c.phones);
    if let Some(bad) = c.emails.iter().find(|e| {
        let (user, host) = e.value.split_once('@').unwrap_or(("", ""));
        user.is_empty() || host.is_empty() || e.value.chars().any(char::is_whitespace)
    }) {
        return Err(Text::new(
            format!("«{}» doesn't look like an email address.", bad.value),
            format!("«{}» no parece una dirección de correo.", bad.value),
        ));
    }
    c.addresses = c
        .addresses
        .iter()
        .map(|a| PostalAddress {
            label: clip(&a.label, 64),
            street: clip(&a.street, MAX_FIELD),
            city: clip(&a.city, MAX_FIELD),
            region: clip(&a.region, MAX_FIELD),
            postal_code: clip(&a.postal_code, 32),
            country: clip(&a.country, MAX_FIELD),
        })
        .filter(|a| [&a.street, &a.city, &a.region, &a.postal_code, &a.country].iter().any(|s| !s.is_empty()))
        .take(MAX_ENTRIES)
        .collect();
    if contact_is_empty(&c) {
        return Err(Text::new(
            "Write at least a name, an email or a phone.",
            "Escribe al menos un nombre, un correo o un teléfono.",
        ));
    }
    if c.full_name.is_empty() {
        c.full_name = contact_display_name(&c).unwrap_or_default();
    }
    Ok(c)
}

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

    /// Un contacto por id (del almacén, al día).
    pub fn contact(&self, id: ContactId) -> Option<Contact> {
        self.store.get_contact(id).ok().flatten()
    }

    /// Libretas en las que se puede escribir (para «Nuevo contacto»).
    pub fn writable_address_books(&self) -> Vec<AddressBook> {
        self.store.address_books().unwrap_or_default().into_iter().filter(|b| !b.read_only).collect()
    }

    /// Nombre de la libreta de un contacto y si es de solo lectura.
    pub fn address_book_of(&self, id: AddressBookId) -> Option<AddressBook> {
        self.store.get_address_book(id).ok().flatten()
    }

    /// Libreta para un contacto nuevo: la primera en la que se puede escribir, o una local
    /// «Contactos» si no hay ninguna.
    fn writable_book(&self) -> Result<AddressBookId> {
        match self.writable_address_books().first() {
            Some(b) => Ok(b.id),
            None => self
                .store
                .put_address_book(&AddressBook {
                    name: crate::lang::pick("Contacts", "Contactos").to_string(),
                    ..Default::default()
                })
                .map_err(store_err),
        }
    }

    /// UX-10: guarda un contacto nuevo (`id` sin asignar) o editado. Local primero: el servidor se
    /// entera por la cola. Con deshacer. Un error de validación vuelve como aviso de error (el
    /// editor sigue abierto).
    pub fn save_contact_edit(&mut self, edited: Contact) -> Result<Outcome> {
        let mut c = match clean_contact(&edited) {
            Ok(c) => c,
            Err(t) => return Ok(Outcome::Notice(Notice::error(t))),
        };
        let old = if c.id == ContactId::UNASSIGNED {
            None
        } else {
            Some(self.store.get_contact(c.id).map_err(store_err)?.ok_or(EngineError::NoSelection)?)
        };
        if old.is_none() && self.address_book_of(c.address_book).is_none_or(|b| b.read_only) {
            c.address_book = self.writable_book()?;
        }
        if let Some(o) = &old {
            // La libreta no cambia al editar (moverlo sería borrar y crear en otra cuenta).
            c.address_book = o.address_book;
            c.uid = o.uid.clone();
            c.remote_id = o.remote_id.clone();
            c.etag = o.etag.clone();
            if *o == c {
                return Ok(Outcome::Notice(Notice::info("No changes.", "Sin cambios.")));
            }
        }
        if self.address_book_of(c.address_book).is_some_and(|b| b.read_only) {
            let name = self.address_book_of(c.address_book).map(|b| b.name).unwrap_or_default();
            return Ok(Outcome::Notice(Notice::error(Text::new(
                format!("«{name}» is read-only: change this contact there."),
                format!("«{name}» es de solo lectura: cambia este contacto allá."),
            ))));
        }
        let id = match &self.pim.handle {
            Some(h) => h.save_contact(&c, None),
            None => iuredesk_pim_sync::ops::save_contact(&self.store, &c, None),
        }
        .map_err(pim_err)?;
        self.contacts_changed();
        self.contact_selection = vec![id];
        let name = contact_display_name(&c).unwrap_or_default();
        let (text, inverse) = match old {
            None => (
                Text::new(format!("Added «{name}» to your contacts"), format!("Se agregó «{name}» a tus contactos")),
                Inverse::ContactsAdded(vec![id]),
            ),
            Some(o) => {
                (Text::new(format!("Saved «{name}»"), format!("Se guardó «{name}»")), Inverse::ContactEdit(Box::new(o)))
            }
        };
        self.push_undo(text.clone(), vec![(inverse, None)]);
        Ok(Outcome::Notice(Notice { kind: NoticeKind::Success, text, undo: true, secs: None }))
    }

    /// Deshace una edición: el contacto vuelve a como estaba (y se sube así).
    pub(crate) fn undo_contact_edit(&mut self, old: Contact) -> Result<()> {
        if self.store.get_contact(old.id).map_err(store_err)?.is_none() {
            // Lo borraron después: no se resucita.
            return Ok(());
        }
        match &self.pim.handle {
            Some(h) => h.save_contact(&old, None),
            None => iuredesk_pim_sync::ops::save_contact(&self.store, &old, None),
        }
        .map_err(pim_err)?;
        self.contacts_changed();
        Ok(())
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

    /// R1.2: agrega los contactos de un `.vcf` abierto a la primera libreta en la que se puede
    /// escribir (o a una local «Contactos» si no hay). Con deshacer.
    pub fn add_file_contacts(&mut self, contacts: &[Contact]) -> Result<Outcome> {
        let book = self.writable_book()?;
        let mut ids = Vec::new();
        for c in contacts.iter().take(crate::open::MAX_ITEMS) {
            let mut c = c.clone();
            c.id = ContactId::UNASSIGNED;
            c.address_book = book;
            c.remote_id = None;
            c.etag = None;
            let id = match &self.pim.handle {
                Some(h) => h.save_contact(&c, None),
                None => iuredesk_pim_sync::ops::save_contact(&self.store, &c, None),
            }
            .map_err(pim_err)?;
            ids.push(id);
        }
        self.contacts_changed();
        let text = match (ids.len(), contacts.first()) {
            (0, _) => return Ok(Outcome::Notice(Notice::info("Nothing to add.", "No hay nada que agregar."))),
            (1, Some(c)) => {
                let name = contact_display_name(c).unwrap_or_else(|| "(Sin nombre)".into());
                Text::new(format!("Added «{name}» to your contacts"), format!("Se agregó «{name}» a tus contactos"))
            }
            (n, _) => Text::new(format!("Added {n} contacts"), format!("Se agregaron {n} contactos")),
        };
        self.push_undo(text.clone(), vec![(Inverse::ContactsAdded(ids), None)]);
        Ok(Outcome::Notice(Notice { kind: NoticeKind::Success, text, undo: true, secs: None }))
    }

    pub(crate) fn undo_contacts_added(&mut self, ids: &[ContactId]) -> Result<()> {
        for &id in ids {
            let r = match &self.pim.handle {
                Some(h) => h.delete_contact(id),
                None => iuredesk_pim_sync::ops::delete_contact(&self.store, id),
            };
            if let Err(e) = r
                && !matches!(e, PimError::ReadOnly)
            {
                self.contacts_changed();
                return Err(pim_err(e));
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
    use super::*;

    #[test]
    fn cleaning_trims_drops_empty_and_fills_the_name() {
        let c = Contact {
            given_name: "  Ana ".into(),
            family_name: "Ruiz".into(),
            emails: vec![Labeled::new("work", " ana@x.com "), Labeled::new("home", "  ")],
            phones: vec![Labeled::new("", "")],
            addresses: vec![PostalAddress { label: "home".into(), ..Default::default() }],
            ..Default::default()
        };
        let out = clean_contact(&c).unwrap();
        assert_eq!(out.full_name, "Ana Ruiz");
        assert_eq!(out.given_name, "Ana");
        assert_eq!(out.emails, vec![Labeled::new("work", "ana@x.com")]);
        assert!(out.phones.is_empty() && out.addresses.is_empty());
        // Un nombre para mostrar puesto a mano se respeta.
        let fixed = Contact { full_name: "Dra. Ana".into(), ..c };
        assert_eq!(clean_contact(&fixed).unwrap().full_name, "Dra. Ana");
    }

    #[test]
    fn cleaning_rejects_empty_contacts_and_bad_emails() {
        assert!(clean_contact(&Contact::default()).is_err());
        let blank = Contact { emails: vec![Labeled::new("", "   ")], ..Default::default() };
        assert!(clean_contact(&blank).is_err());
        for bad in ["ana", "@x.com", "ana@", "ana @x.com"] {
            let c = Contact { emails: vec![Labeled::new("", bad)], ..Default::default() };
            let err = clean_contact(&c).unwrap_err();
            assert!(err.es.contains("no parece"), "{bad}: {}", err.es);
        }
    }

    #[test]
    fn cleaning_bounds_hostile_sizes() {
        let c = Contact {
            full_name: "é".repeat(10_000),
            phones: (0..500).map(|i| Labeled::new("", i.to_string())).collect(),
            notes: "x".repeat(100_000),
            ..Default::default()
        };
        let out = clean_contact(&c).unwrap();
        assert_eq!(out.full_name.chars().count(), MAX_FIELD);
        assert_eq!(out.phones.len(), MAX_ENTRIES);
        assert_eq!(out.notes.len(), MAX_NOTES);
    }

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
