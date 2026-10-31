//! Calendario, contactos, notas y tareas.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use iuredesk_model::{
    AddressBook, AddressBookId, Calendar, CalendarId, Contact, ContactId, Event, EventId, Note, NoteId,
    SharedCalendars, Task, TaskId, TaskList, TaskListId,
};
use redb::{ReadableDatabase, ReadableTable, Table, TableDefinition, WriteTransaction};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::codec::day_of;
use crate::error::{Result, StoreError};
use crate::tables::*;
use crate::{Store, bump_seq, get_rec, next_id, put_rec};

/// Un evento que ocupa más días que esto (o que se repite) va a la tabla de eventos largos, que se
/// revisa en cada consulta, en lugar de tener una entrada por día.
pub const MAX_INDEXED_DAYS: i64 = 62;
/// Rango máximo de días de una consulta de calendario.
const MAX_QUERY_DAYS: i64 = 3_660;
/// Resultados máximos de una consulta de calendario.
const MAX_EVENTS: usize = 20_000;

/// Días [primero, último] que ocupa un evento, o `None` si va a la tabla de largos.
fn event_days(e: &Event) -> Option<(i64, i64)> {
    if e.is_recurring() {
        return None;
    }
    let s = e.start.to_utc();
    let end = e.end.to_utc().max(s);
    let first = day_of(s);
    // Fin exclusivo: un evento que acaba a medianoche no ocupa el día siguiente.
    let last = if end > s { day_of(end - chrono::Duration::milliseconds(1)) } else { first };
    if last.saturating_sub(first) > MAX_INDEXED_DAYS { None } else { Some((first, last.max(first))) }
}

/// Guarda un registro con id propio: lo asigna si falta y mantiene el índice (padre, hijo).
fn upsert_child<T: Serialize + DeserializeOwned>(
    tx: &WriteTransaction,
    kind: &str,
    table: TableDefinition<u64, &[u8]>,
    index: TableDefinition<(u64, u64), ()>,
    id: u64,
    parent: u64,
    parent_of: impl Fn(&T) -> u64,
    value: impl FnOnce(u64) -> T,
) -> Result<u64> {
    let mut meta = tx.open_table(META)?;
    let id = if id == 0 {
        next_id(&mut meta, kind)?
    } else {
        bump_seq(&mut meta, kind, id)?;
        id
    };
    let mut t = tx.open_table(table)?;
    let mut idx = tx.open_table(index)?;
    if let Some(old) = get_rec::<T>(&t, id)? {
        idx.remove((parent_of(&old), id))?;
    }
    put_rec(&mut t, id, &value(id))?;
    idx.insert((parent, id), ())?;
    Ok(id)
}

pub(crate) fn children<T: DeserializeOwned>(
    store: &Store,
    table: TableDefinition<u64, &[u8]>,
    index: TableDefinition<(u64, u64), ()>,
    parent: u64,
) -> Result<Vec<T>> {
    let tx = store.db.begin_read()?;
    let idx = tx.open_table(index)?;
    let t = tx.open_table(table)?;
    let mut out = Vec::new();
    for item in idx.range((parent, 0)..=(parent, u64::MAX))? {
        let (k, _) = item?;
        if let Some(v) = get_rec::<T>(&t, k.value().1)? {
            out.push(v);
        }
    }
    Ok(out)
}

pub(crate) fn remove_child<T: DeserializeOwned>(
    tx: &WriteTransaction,
    table: TableDefinition<u64, &[u8]>,
    index: TableDefinition<(u64, u64), ()>,
    id: u64,
    parent_of: impl Fn(&T) -> u64,
) -> Result<Option<T>> {
    let mut t = tx.open_table(table)?;
    let old = get_rec::<T>(&t, id)?;
    if let Some(o) = &old {
        tx.open_table(index)?.remove((parent_of(o), id))?;
        t.remove(id)?;
    }
    Ok(old)
}

fn put_simple<T: Serialize>(
    tx: &WriteTransaction,
    kind: &str,
    table: TableDefinition<u64, &[u8]>,
    id: u64,
    v: impl FnOnce(u64) -> T,
) -> Result<u64> {
    let mut meta = tx.open_table(META)?;
    let id = if id == 0 {
        next_id(&mut meta, kind)?
    } else {
        bump_seq(&mut meta, kind, id)?;
        id
    };
    let mut t = tx.open_table(table)?;
    put_rec(&mut t, id, &v(id))?;
    Ok(id)
}

pub(crate) fn unindex_event(
    days: &mut Table<'_, (i64, u64), ()>,
    long: &mut Table<'_, u64, ()>,
    e: &Event,
) -> Result<()> {
    match event_days(e) {
        Some((a, b)) => {
            for d in a..=b {
                days.remove((d, e.id.get()))?;
            }
        }
        None => {
            long.remove(e.id.get())?;
        }
    }
    Ok(())
}

impl Store {
    // ---------------------------------------------------------------- calendarios

    /// Guarda un calendario y devuelve su id.
    pub fn put_calendar(&self, cal: &Calendar) -> Result<CalendarId> {
        let mut c = cal.clone();
        self.write(|tx, _| {
            let id = put_simple(tx, "calendar", CALENDARS, c.id.get(), |id| {
                c.id = CalendarId(id);
                c.clone()
            })?;
            Ok(CalendarId(id))
        })
    }

    /// Todos los calendarios.
    pub fn calendars(&self) -> Result<Vec<Calendar>> {
        self.read_all(CALENDARS)
    }

    /// Calendarios que son la misma colección remota vista desde varias cuentas (BUG-DUP): cuál
    /// es el primario de cada duplicado. Ver [`iuredesk_model::SharedCalendars`].
    pub fn shared_calendars(&self) -> Result<SharedCalendars> {
        let cals = self.calendars()?;
        let labels = self.account_labels()?;
        Ok(SharedCalendars::detect(&cals, |a| labels.iter().find(|l| l.id == a).map(|l| l.email.clone())))
    }

    /// Guarda un evento y devuelve su id. El calendario debe existir.
    pub fn put_event(&self, event: &Event) -> Result<EventId> {
        let mut e = event.clone();
        e.attendees.truncate(1_000);
        e.exdates.truncate(1_000);
        e.reminders.truncate(50);
        self.write(|tx, _| {
            if tx.open_table(CALENDARS)?.get(e.calendar.get())?.is_none() {
                return Err(StoreError::not_found("calendario", e.calendar));
            }
            let old: Option<Event> =
                if e.id.is_unassigned() { None } else { get_rec(&tx.open_table(EVENTS)?, e.id.get())? };
            if let Some(old) = &old {
                let mut days = tx.open_table(EVENT_DAYS)?;
                let mut long = tx.open_table(EVENT_LONG)?;
                unindex_event(&mut days, &mut long, old)?;
            }
            let cal = e.calendar.get();
            let id = upsert_child::<Event>(
                tx,
                "event",
                EVENTS,
                CALENDAR_EVENTS,
                e.id.get(),
                cal,
                |o| o.calendar.get(),
                |id| {
                    e.id = EventId(id);
                    e.clone()
                },
            )?;
            match event_days(&e) {
                Some((a, b)) => {
                    let mut days = tx.open_table(EVENT_DAYS)?;
                    for d in a..=b {
                        days.insert((d, id), ())?;
                    }
                }
                None => {
                    tx.open_table(EVENT_LONG)?.insert(id, ())?;
                }
            }
            Ok(EventId(id))
        })
    }

    /// Un evento.
    pub fn get_event(&self, id: EventId) -> Result<Option<Event>> {
        self.read_rec(EVENTS, id.get())
    }

    /// Borra un evento.
    pub fn delete_event(&self, id: EventId) -> Result<()> {
        self.write(|tx, _| {
            if let Some(old) = remove_child::<Event>(tx, EVENTS, CALENDAR_EVENTS, id.get(), |o| o.calendar.get())? {
                let mut days = tx.open_table(EVENT_DAYS)?;
                let mut long = tx.open_table(EVENT_LONG)?;
                unindex_event(&mut days, &mut long, &old)?;
            }
            tx.open_table(PIM_RAW)?.remove(crate::PimRef::Event(id).key())?;
            Ok(())
        })
    }

    /// Eventos de los calendarios dados (todos si `cals` está vacío) que se solapan con
    /// `[from, to)`, ordenados por inicio.
    ///
    /// Los eventos recurrentes se devuelven como su evento maestro si empiezan antes de `to`; la
    /// expansión de `RRULE`/`EXDATE` corresponde a la capa superior: las vistas deben usar
    /// `iuredesk_pim_sync::occurrences_between` (L3), que expande con el motor acotado de
    /// `iuredesk-dav` y aplica las excepciones guardadas en el objeto crudo.
    pub fn events_between(&self, cals: &[CalendarId], from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Vec<Event>> {
        if to <= from {
            return Ok(Vec::new());
        }
        let d0 = day_of(from);
        let d1 = day_of(to - chrono::Duration::milliseconds(1));
        if d1.saturating_sub(d0) > MAX_QUERY_DAYS {
            return Err(StoreError::Invalid(format!("rango de más de {MAX_QUERY_DAYS} días")));
        }
        let tx = self.db.begin_read()?;
        let days = tx.open_table(EVENT_DAYS)?;
        let long = tx.open_table(EVENT_LONG)?;
        let events = tx.open_table(EVENTS)?;
        let mut ids = BTreeSet::new();
        for item in days.range((d0, 0)..=(d1, u64::MAX))? {
            let (k, _) = item?;
            ids.insert(k.value().1);
            if ids.len() > MAX_EVENTS {
                break;
            }
        }
        for item in long.iter()? {
            let (k, _) = item?;
            ids.insert(k.value());
        }
        let mut out = Vec::new();
        for id in ids {
            let Some(e) = get_rec::<Event>(&events, id)? else { continue };
            if !cals.is_empty() && !cals.contains(&e.calendar) {
                continue;
            }
            let s = e.start.to_utc();
            let end = e.end.to_utc().max(s);
            let overlaps = if e.is_recurring() { s < to } else { s < to && (end > from || (end == s && s >= from)) };
            if overlaps {
                out.push(e);
                if out.len() >= MAX_EVENTS {
                    break;
                }
            }
        }
        out.sort_by_key(|e| (e.start.to_utc(), e.id));
        Ok(out)
    }

    // ---------------------------------------------------------------- contactos

    /// Guarda una libreta y devuelve su id.
    pub fn put_address_book(&self, book: &AddressBook) -> Result<AddressBookId> {
        let mut b = book.clone();
        self.write(|tx, _| {
            let id = put_simple(tx, "address_book", ADDRESS_BOOKS, b.id.get(), |id| {
                b.id = AddressBookId(id);
                b.clone()
            })?;
            Ok(AddressBookId(id))
        })
    }

    /// Todas las libretas.
    pub fn address_books(&self) -> Result<Vec<AddressBook>> {
        self.read_all(ADDRESS_BOOKS)
    }

    /// Guarda un contacto y devuelve su id. La libreta debe existir.
    pub fn put_contact(&self, contact: &Contact) -> Result<ContactId> {
        let mut c = contact.clone();
        c.emails.truncate(100);
        c.phones.truncate(100);
        c.addresses.truncate(100);
        self.write(|tx, _| {
            if tx.open_table(ADDRESS_BOOKS)?.get(c.address_book.get())?.is_none() {
                return Err(StoreError::not_found("libreta", c.address_book));
            }
            let book = c.address_book.get();
            let id = upsert_child::<Contact>(
                tx,
                "contact",
                CONTACTS,
                BOOK_CONTACTS,
                c.id.get(),
                book,
                |o| o.address_book.get(),
                |id| {
                    c.id = ContactId(id);
                    c.clone()
                },
            )?;
            Ok(ContactId(id))
        })
    }

    /// Un contacto.
    pub fn get_contact(&self, id: ContactId) -> Result<Option<Contact>> {
        self.read_rec(CONTACTS, id.get())
    }

    /// Contactos de una libreta, por nombre.
    pub fn contacts(&self, book: AddressBookId) -> Result<Vec<Contact>> {
        let mut v: Vec<Contact> = children(self, CONTACTS, BOOK_CONTACTS, book.get())?;
        v.sort_by_cached_key(|c| c.full_name.to_lowercase());
        Ok(v)
    }

    /// Contactos cuyo nombre, correo u organización contienen `text` (sin distinguir
    /// mayúsculas), para autocompletar. Recorrido lineal: las libretas son de miles, no millones.
    pub fn find_contacts(&self, text: &str, limit: usize) -> Result<Vec<Contact>> {
        let needle = text.trim().to_lowercase();
        let limit = limit.min(1_000);
        let tx = self.db.begin_read()?;
        let t = tx.open_table(CONTACTS)?;
        let mut out = Vec::new();
        for item in t.iter()? {
            let (_, v) = item?;
            let c: Contact = crate::codec::decode(v.value())?;
            let hit = needle.is_empty()
                || c.full_name.to_lowercase().contains(&needle)
                || c.org.to_lowercase().contains(&needle)
                || c.emails.iter().any(|e| e.value.to_lowercase().contains(&needle));
            if hit {
                out.push(c);
                if out.len() >= limit {
                    break;
                }
            }
        }
        out.sort_by_cached_key(|c| c.full_name.to_lowercase());
        Ok(out)
    }

    /// Borra un contacto.
    pub fn delete_contact(&self, id: ContactId) -> Result<()> {
        self.write(|tx, _| {
            remove_child::<Contact>(tx, CONTACTS, BOOK_CONTACTS, id.get(), |o| o.address_book.get())?;
            tx.open_table(PIM_RAW)?.remove(crate::PimRef::Contact(id).key())?;
            Ok(())
        })
    }

    // ---------------------------------------------------------------- notas

    /// Guarda una nota y devuelve su id.
    pub fn put_note(&self, note: &Note) -> Result<NoteId> {
        let mut n = note.clone();
        n.tags.truncate(100);
        self.write(|tx, _| {
            let id = put_simple(tx, "note", NOTES, n.id.get(), |id| {
                n.id = NoteId(id);
                n.clone()
            })?;
            Ok(NoteId(id))
        })
    }

    /// Una nota.
    pub fn get_note(&self, id: NoteId) -> Result<Option<Note>> {
        self.read_rec(NOTES, id.get())
    }

    /// Todas las notas: fijadas primero, luego por última modificación descendente.
    pub fn notes(&self) -> Result<Vec<Note>> {
        let mut v: Vec<Note> = self.read_all(NOTES)?;
        v.sort_by(|a, b| b.pinned.cmp(&a.pinned).then(b.updated.cmp(&a.updated)));
        Ok(v)
    }

    /// Borra una nota.
    pub fn delete_note(&self, id: NoteId) -> Result<()> {
        self.write(|tx, _| {
            tx.open_table(NOTES)?.remove(id.get())?;
            Ok(())
        })
    }

    // ---------------------------------------------------------------- tareas

    /// Guarda una lista de tareas y devuelve su id.
    pub fn put_task_list(&self, list: &TaskList) -> Result<TaskListId> {
        let mut l = list.clone();
        self.write(|tx, _| {
            let id = put_simple(tx, "task_list", TASK_LISTS, l.id.get(), |id| {
                l.id = TaskListId(id);
                l.clone()
            })?;
            Ok(TaskListId(id))
        })
    }

    /// Todas las listas de tareas.
    pub fn task_lists(&self) -> Result<Vec<TaskList>> {
        self.read_all(TASK_LISTS)
    }

    /// Guarda una tarea y devuelve su id. La lista debe existir.
    pub fn put_task(&self, task: &Task) -> Result<TaskId> {
        let mut t = task.clone();
        self.write(|tx, _| {
            if tx.open_table(TASK_LISTS)?.get(t.list.get())?.is_none() {
                return Err(StoreError::not_found("lista de tareas", t.list));
            }
            let list = t.list.get();
            let id = upsert_child::<Task>(
                tx,
                "task",
                TASKS,
                LIST_TASKS,
                t.id.get(),
                list,
                |o| o.list.get(),
                |id| {
                    t.id = TaskId(id);
                    t.clone()
                },
            )?;
            Ok(TaskId(id))
        })
    }

    /// Tareas de una lista: pendientes primero (por vencimiento y prioridad), luego terminadas.
    pub fn tasks(&self, list: TaskListId) -> Result<Vec<Task>> {
        let mut v: Vec<Task> = children(self, TASKS, LIST_TASKS, list.get())?;
        v.sort_by_key(|t| {
            (
                t.is_done(),
                t.due.as_ref().map(|d| d.to_utc()).unwrap_or(DateTime::<Utc>::MAX_UTC),
                if t.priority == 0 { 10 } else { t.priority },
                t.id,
            )
        });
        Ok(v)
    }

    /// Borra una tarea.
    pub fn delete_task(&self, id: TaskId) -> Result<()> {
        self.write(|tx, _| {
            remove_child::<Task>(tx, TASKS, LIST_TASKS, id.get(), |o| o.list.get())?;
            tx.open_table(PIM_RAW)?.remove(crate::PimRef::Task(id).key())?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use chrono::{NaiveDate, TimeZone};
    use iuredesk_model::{AccountId, EventTime};

    use super::*;

    fn at(d: u32, h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 3, d, h, 0, 0).unwrap()
    }

    fn ev(cal: CalendarId, s: EventTime, e: EventTime) -> Event {
        Event { calendar: cal, summary: "x".into(), start: s, end: e, ..Default::default() }
    }

    fn dt(d: DateTime<Utc>) -> EventTime {
        EventTime::DateTime { utc: d, tzid: Some("Europe/Madrid".into()) }
    }

    /// BUG-DUP: la colección principal de Gmail vista también desde la cuenta de Workspace.
    #[test]
    fn shared_collection_has_one_primary() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        for (id, email) in [(1, "eduardo@llaguno.com"), (2, "eduardo.llaguno@gmail.com")] {
            let label = crate::AccountLabel { id: AccountId(id), name: String::new(), email: email.into() };
            s.put_account_label(&label).unwrap();
        }
        let put = |acct, path: &str| {
            s.put_calendar(&Calendar { account: AccountId(acct), remote_path: path.into(), ..Default::default() })
                .unwrap()
        };
        let own = put(1, "https://apidata.googleusercontent.com/caldav/v2/eduardo%40llaguno.com/events/");
        let dup = put(1, "https://apidata.googleusercontent.com/caldav/v2/eduardo.llaguno%40gmail.com/events/");
        let main = put(2, "https://apidata.googleusercontent.com/caldav/v2/eduardo.llaguno@gmail.com/events/");
        let shared = s.shared_calendars().unwrap();
        assert_eq!(shared.primary_of(dup), Some(main));
        assert!(!shared.is_duplicate(main) && !shared.is_duplicate(own));
    }

    #[test]
    fn calendar_range_queries() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let c1 =
            s.put_calendar(&Calendar { account: AccountId(1), name: "Trabajo".into(), ..Default::default() }).unwrap();
        let c2 =
            s.put_calendar(&Calendar { account: AccountId(1), name: "Casa".into(), ..Default::default() }).unwrap();
        let a = s.put_event(&ev(c1, dt(at(2, 9)), dt(at(2, 10)))).unwrap();
        let b = s.put_event(&ev(c2, dt(at(3, 23)), dt(at(4, 1)))).unwrap(); // cruza medianoche
        let allday = s
            .put_event(&ev(
                c1,
                EventTime::Date(NaiveDate::from_ymd_opt(2026, 3, 5).unwrap()),
                EventTime::Date(NaiveDate::from_ymd_opt(2026, 3, 6).unwrap()),
            ))
            .unwrap();
        let long = s.put_event(&ev(c1, dt(at(1, 0)), dt(Utc.with_ymd_and_hms(2026, 12, 1, 0, 0, 0).unwrap()))).unwrap();
        let mut rec = ev(
            c2,
            dt(Utc.with_ymd_and_hms(2025, 1, 1, 8, 0, 0).unwrap()),
            dt(Utc.with_ymd_and_hms(2025, 1, 1, 9, 0, 0).unwrap()),
        );
        rec.rrule = Some("FREQ=WEEKLY".into());
        let rec = s.put_event(&rec).unwrap();

        let ids = |v: Vec<Event>| v.into_iter().map(|e| e.id).collect::<Vec<_>>();
        assert_eq!(ids(s.events_between(&[], at(2, 0), at(3, 0)).unwrap()), vec![rec, long, a]);
        assert_eq!(ids(s.events_between(&[c2], at(4, 0), at(5, 0)).unwrap()), vec![rec, b]);
        assert_eq!(ids(s.events_between(&[c1], at(5, 0), at(6, 0)).unwrap()), vec![long, allday]);
        assert!(s.events_between(&[], at(6, 0), at(5, 0)).unwrap().is_empty());

        // Mover el evento actualiza el índice por días.
        let mut moved = s.get_event(a).unwrap().unwrap();
        moved.start = dt(at(20, 9));
        moved.end = dt(at(20, 10));
        s.put_event(&moved).unwrap();
        assert!(!ids(s.events_between(&[c1], at(2, 0), at(3, 0)).unwrap()).contains(&a));
        assert!(ids(s.events_between(&[c1], at(20, 0), at(21, 0)).unwrap()).contains(&a));
        s.delete_event(a).unwrap();
        assert!(s.get_event(a).unwrap().is_none());
        assert!(!ids(s.events_between(&[], at(20, 0), at(21, 0)).unwrap()).contains(&a));
        // Calendario inexistente.
        assert!(s.put_event(&ev(CalendarId(999), dt(at(1, 0)), dt(at(1, 1)))).is_err());
    }

    #[test]
    fn contacts_notes_tasks() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let book = s.put_address_book(&AddressBook { name: "Personal".into(), ..Default::default() }).unwrap();
        let c = s
            .put_contact(&Contact {
                address_book: book,
                full_name: "Zoe Álvarez".into(),
                emails: vec![iuredesk_model::Labeled::new("work", "zoe@example.com")],
                ..Default::default()
            })
            .unwrap();
        s.put_contact(&Contact { address_book: book, full_name: "Ana".into(), ..Default::default() }).unwrap();
        assert_eq!(s.contacts(book).unwrap()[0].full_name, "Ana");
        assert_eq!(s.find_contacts("ZOE@", 10).unwrap()[0].id, c);
        s.delete_contact(c).unwrap();
        assert_eq!(s.contacts(book).unwrap().len(), 1);

        let n = s.put_note(&Note { title: "Ideas".into(), body: "# Hola".into(), ..Default::default() }).unwrap();
        assert_eq!(s.get_note(n).unwrap().unwrap().title, "Ideas");
        assert_eq!(s.notes().unwrap().len(), 1);
        s.delete_note(n).unwrap();
        assert!(s.notes().unwrap().is_empty());

        let l = s.put_task_list(&TaskList { name: "Hoy".into(), ..Default::default() }).unwrap();
        let done = s
            .put_task(&Task {
                list: l,
                summary: "hecha".into(),
                status: iuredesk_model::TaskStatus::Completed,
                ..Default::default()
            })
            .unwrap();
        let todo =
            s.put_task(&Task { list: l, summary: "pendiente".into(), priority: 1, ..Default::default() }).unwrap();
        assert_eq!(s.tasks(l).unwrap().iter().map(|t| t.id).collect::<Vec<_>>(), vec![todo, done]);
        s.delete_task(done).unwrap();
        assert_eq!(s.tasks(l).unwrap().len(), 1);
    }
}
