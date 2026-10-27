//! Lo que la sincronización PIM (`iuredesk-pim-sync`) necesita guardar además del modelo:
//!
//! - **Objeto crudo** ([`RawObject`]) de cada evento, contacto o tarea sincronizado: el `.ics`,
//!   `.vcf` o JSON de Graph tal como llegó del servidor (en el almacén de blobs) con su `href` y
//!   su `ETag`. Es la **base** de las ediciones: al subir un cambio se *parchea* este objeto en vez
//!   de regenerarlo, así no se pierde lo que puso otro cliente (`X-…`, alarmas raras…), y es la
//!   versión común para fusionar cuando el servidor responde 412.
//! - **Estado por colección** ([`CollectionSync`]): `sync-token`, `ctag`, `deltaLink` de Graph y
//!   el mapa `href` → (`ETag`, elementos locales) que permite la diferencia de ETags y encontrar
//!   el elemento local de un `href`.
//!
//! Tablas aditivas (se crean al abrir; no cambia la versión de esquema).

use std::collections::BTreeMap;

use iuredesk_model::{AddressBookId, BlobHash, CalendarId, Contact, ContactId, Event, EventId, TaskId, TaskListId};
use redb::ReadableDatabase;
use serde::{Deserialize, Serialize};

use crate::error::{Result, StoreError};
use crate::pim::{children, remove_child, unindex_event};
use crate::tables::*;
use crate::{Store, codec};

/// Elemento PIM local.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum PimRef {
    /// Evento.
    Event(EventId),
    /// Contacto.
    Contact(ContactId),
    /// Tarea.
    Task(TaskId),
}

impl PimRef {
    /// Clave de tabla (tipo, id).
    pub(crate) fn key(self) -> (u8, u64) {
        match self {
            Self::Event(i) => (1, i.get()),
            Self::Contact(i) => (2, i.get()),
            Self::Task(i) => (3, i.get()),
        }
    }
}

/// Colección local sincronizada.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum CollectionRef {
    /// Calendario.
    Calendar(CalendarId),
    /// Libreta de direcciones.
    AddressBook(AddressBookId),
    /// Lista de tareas.
    TaskList(TaskListId),
}

impl CollectionRef {
    fn key(self) -> (u8, u64) {
        match self {
            Self::Calendar(i) => (1, i.get()),
            Self::AddressBook(i) => (2, i.get()),
            Self::TaskList(i) => (3, i.get()),
        }
    }
}

/// Objeto crudo del servidor de un elemento PIM.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawObject {
    /// Contenido en el almacén de blobs.
    pub blob: BlobHash,
    /// `href` (URL absoluta en DAV, id en Graph).
    pub href: String,
    /// `ETag` de esta versión.
    pub etag: Option<String>,
}

/// Un objeto remoto conocido de una colección.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncedItem {
    /// `ETag` de la última versión descargada o subida.
    pub etag: Option<String>,
    /// Elementos locales que salen de él (un `.ics` con `VEVENT` y `VTODO` da dos).
    pub refs: Vec<PimRef>,
}

/// Estado de sincronización de una colección.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionSync {
    /// `sync-token` de RFC 6578.
    pub sync_token: Option<String>,
    /// `getctag` de CalendarServer.
    pub ctag: Option<String>,
    /// El servidor no admite `sync-collection` en esta colección.
    pub sync_unsupported: bool,
    /// `@odata.deltaLink` de Microsoft Graph.
    pub delta_link: Option<String>,
    /// `href` → objeto conocido.
    pub items: BTreeMap<String, SyncedItem>,
}

impl CollectionSync {
    /// `href` del que sale un elemento local.
    pub fn href_of(&self, r: PimRef) -> Option<&str> {
        self.items.iter().find(|(_, v)| v.refs.contains(&r)).map(|(k, _)| k.as_str())
    }
}

/// Elementos máximos que se aceptan en el estado de una colección.
const MAX_COLLECTION_ITEMS: usize = 200_000;

impl Store {
    // ------------------------------------------------------------- objetos crudos

    /// Guarda el objeto crudo de un elemento (contenido en blobs) y devuelve su registro.
    pub fn put_raw_object(&self, r: PimRef, data: &[u8], href: &str, etag: Option<&str>) -> Result<RawObject> {
        let blob = self.blobs().put(data)?;
        let raw = RawObject { blob, href: href.to_string(), etag: etag.map(str::to_string) };
        let bytes = codec::encode(&raw)?;
        self.write(|tx, _| {
            tx.open_table(PIM_RAW)?.insert(r.key(), bytes.as_slice())?;
            Ok(())
        })?;
        Ok(raw)
    }

    /// Registro del objeto crudo de un elemento.
    pub fn raw_object(&self, r: PimRef) -> Result<Option<RawObject>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(PIM_RAW)?;
        let v = t.get(r.key())?;
        match v {
            Some(g) => Ok(Some(codec::decode(g.value())?)),
            None => Ok(None),
        }
    }

    /// Registro y contenido del objeto crudo. Si el blob falta o está dañado, `None`.
    pub fn raw_object_data(&self, r: PimRef) -> Result<Option<(RawObject, Vec<u8>)>> {
        let Some(raw) = self.raw_object(r)? else { return Ok(None) };
        match self.blobs().get(&raw.blob) {
            Ok(data) => Ok(Some((raw, data))),
            Err(StoreError::Io(_) | StoreError::Corrupt(_) | StoreError::NotFound { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Olvida el objeto crudo (el blob queda para la limpieza general).
    pub fn delete_raw_object(&self, r: PimRef) -> Result<()> {
        self.write(|tx, _| {
            tx.open_table(PIM_RAW)?.remove(r.key())?;
            Ok(())
        })
    }

    // ------------------------------------------------------------- estado por colección

    /// Estado de sincronización de una colección (vacío si nunca se sincronizó).
    pub fn collection_sync(&self, c: CollectionRef) -> Result<CollectionSync> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(PIM_COLLECTIONS)?;
        let v = t.get(c.key())?;
        match v {
            Some(g) => Ok(codec::decode(g.value())?),
            None => Ok(CollectionSync::default()),
        }
    }

    /// Guarda el estado de sincronización de una colección.
    pub fn put_collection_sync(&self, c: CollectionRef, state: &CollectionSync) -> Result<()> {
        if state.items.len() > MAX_COLLECTION_ITEMS {
            return Err(StoreError::TooLarge {
                what: "estado de colección",
                size: state.items.len() as u64,
                max: MAX_COLLECTION_ITEMS as u64,
            });
        }
        let bytes = codec::encode(state)?;
        self.write(|tx, _| {
            tx.open_table(PIM_COLLECTIONS)?.insert(c.key(), bytes.as_slice())?;
            Ok(())
        })
    }

    // ------------------------------------------------------------- consultas y bajas

    /// Todos los eventos de un calendario (sin orden garantizado).
    pub fn events_in_calendar(&self, cal: CalendarId) -> Result<Vec<Event>> {
        children(self, EVENTS, CALENDAR_EVENTS, cal.get())
    }

    /// Todos los contactos de una libreta, sin ordenar (más barato que [`Store::contacts`]).
    pub fn contacts_in_book(&self, book: AddressBookId) -> Result<Vec<Contact>> {
        children(self, CONTACTS, BOOK_CONTACTS, book.get())
    }

    /// Borra un calendario con sus eventos, objetos crudos y estado de sincronización.
    pub fn delete_calendar(&self, id: CalendarId) -> Result<()> {
        let events = self.events_in_calendar(id)?;
        self.write(|tx, _| {
            for e in &events {
                if let Some(old) = remove_child::<Event>(tx, EVENTS, CALENDAR_EVENTS, e.id.get(), |o| o.calendar.get())?
                {
                    let mut days = tx.open_table(EVENT_DAYS)?;
                    let mut long = tx.open_table(EVENT_LONG)?;
                    unindex_event(&mut days, &mut long, &old)?;
                }
                tx.open_table(PIM_RAW)?.remove(PimRef::Event(e.id).key())?;
            }
            tx.open_table(CALENDARS)?.remove(id.get())?;
            tx.open_table(PIM_COLLECTIONS)?.remove(CollectionRef::Calendar(id).key())?;
            Ok(())
        })
    }

    /// Borra una libreta con sus contactos, objetos crudos y estado de sincronización.
    pub fn delete_address_book(&self, id: AddressBookId) -> Result<()> {
        let contacts = self.contacts_in_book(id)?;
        self.write(|tx, _| {
            for c in &contacts {
                remove_child::<Contact>(tx, CONTACTS, BOOK_CONTACTS, c.id.get(), |o| o.address_book.get())?;
                tx.open_table(PIM_RAW)?.remove(PimRef::Contact(c.id).key())?;
            }
            tx.open_table(ADDRESS_BOOKS)?.remove(id.get())?;
            tx.open_table(PIM_COLLECTIONS)?.remove(CollectionRef::AddressBook(id).key())?;
            Ok(())
        })
    }

    /// Borra una lista de tareas con sus tareas, objetos crudos y estado de sincronización.
    pub fn delete_task_list(&self, id: TaskListId) -> Result<()> {
        let tasks = self.tasks(id)?;
        self.write(|tx, _| {
            for t in &tasks {
                remove_child::<iuredesk_model::Task>(tx, TASKS, LIST_TASKS, t.id.get(), |o| o.list.get())?;
                tx.open_table(PIM_RAW)?.remove(PimRef::Task(t.id).key())?;
            }
            tx.open_table(TASK_LISTS)?.remove(id.get())?;
            tx.open_table(PIM_COLLECTIONS)?.remove(CollectionRef::TaskList(id).key())?;
            Ok(())
        })
    }

    /// Un calendario.
    pub fn get_calendar(&self, id: CalendarId) -> Result<Option<iuredesk_model::Calendar>> {
        self.read_rec(CALENDARS, id.get())
    }

    /// Una libreta.
    pub fn get_address_book(&self, id: AddressBookId) -> Result<Option<iuredesk_model::AddressBook>> {
        self.read_rec(ADDRESS_BOOKS, id.get())
    }

    /// Una lista de tareas.
    pub fn get_task_list(&self, id: TaskListId) -> Result<Option<iuredesk_model::TaskList>> {
        self.read_rec(TASK_LISTS, id.get())
    }

    /// Una tarea.
    pub fn get_task(&self, id: TaskId) -> Result<Option<iuredesk_model::Task>> {
        self.read_rec(TASKS, id.get())
    }
}

#[cfg(test)]
mod tests {
    use iuredesk_model::{AccountId, AddressBook, Calendar, Task, TaskList};

    use super::*;

    #[test]
    fn raw_objects_and_collection_state() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let cal = s.put_calendar(&Calendar { account: AccountId(1), name: "c".into(), ..Default::default() }).unwrap();
        let ev = s.put_event(&Event { calendar: cal, uid: "u1".into(), ..Default::default() }).unwrap();
        let r = PimRef::Event(ev);
        assert!(s.raw_object(r).unwrap().is_none());
        let raw = s.put_raw_object(r, b"BEGIN:VCALENDAR", "https://x/a.ics", Some("\"1\"")).unwrap();
        assert_eq!(s.raw_object(r).unwrap().unwrap(), raw);
        assert_eq!(s.raw_object_data(r).unwrap().unwrap().1, b"BEGIN:VCALENDAR");

        let c = CollectionRef::Calendar(cal);
        assert_eq!(s.collection_sync(c).unwrap(), CollectionSync::default());
        let mut st = CollectionSync { sync_token: Some("t1".into()), ..Default::default() };
        st.items.insert("https://x/a.ics".into(), SyncedItem { etag: Some("\"1\"".into()), refs: vec![r] });
        s.put_collection_sync(c, &st).unwrap();
        assert_eq!(s.collection_sync(c).unwrap(), st);
        assert_eq!(st.href_of(r), Some("https://x/a.ics"));
        assert_eq!(s.events_in_calendar(cal).unwrap().len(), 1);

        // Borrar el evento borra el crudo; borrar el calendario borra todo.
        s.delete_event(ev).unwrap();
        assert!(s.raw_object(r).unwrap().is_none());
        let ev2 = s.put_event(&Event { calendar: cal, uid: "u2".into(), ..Default::default() }).unwrap();
        s.put_raw_object(PimRef::Event(ev2), b"x", "h", None).unwrap();
        s.delete_calendar(cal).unwrap();
        assert!(s.get_event(ev2).unwrap().is_none());
        assert!(s.get_calendar(cal).unwrap().is_none());
        assert!(s.raw_object(PimRef::Event(ev2)).unwrap().is_none());
        assert_eq!(s.collection_sync(c).unwrap(), CollectionSync::default());
    }

    #[test]
    fn delete_books_and_lists() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let b = s.put_address_book(&AddressBook::default()).unwrap();
        let c = s.put_contact(&Contact { address_book: b, ..Default::default() }).unwrap();
        assert_eq!(s.contacts_in_book(b).unwrap().len(), 1);
        s.delete_address_book(b).unwrap();
        assert!(s.get_contact(c).unwrap().is_none());
        assert!(s.get_address_book(b).unwrap().is_none());
        let l = s.put_task_list(&TaskList::default()).unwrap();
        let t = s.put_task(&Task { list: l, ..Default::default() }).unwrap();
        assert!(s.get_task(t).unwrap().is_some());
        s.delete_task_list(l).unwrap();
        assert!(s.get_task(t).unwrap().is_none());
        assert!(s.get_task_list(l).unwrap().is_none());
    }
}
