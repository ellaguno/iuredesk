//! Tablas de redb.
//!
//! Convención: los registros completos van como `&[u8]` (postcard, ver [`crate::codec`]) y los
//! índices son claves compuestas con valor `()`, de modo que las consultas son recorridos de rango.

use redb::TableDefinition;

/// Metadatos: versión de esquema y contadores de ids (`seq:<tipo>`).
pub(crate) const META: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");

/// Etiqueta de cuenta (nombre y correo) para la barra lateral.
pub(crate) const ACCOUNTS: TableDefinition<u64, &[u8]> = TableDefinition::new("accounts");

/// Carpetas por id.
pub(crate) const FOLDERS: TableDefinition<u64, &[u8]> = TableDefinition::new("folders");
/// Índice (cuenta, carpeta).
pub(crate) const ACCOUNT_FOLDERS: TableDefinition<(u64, u64), ()> = TableDefinition::new("account_folders");
/// Recuento local (total, no leídos) por carpeta.
pub(crate) const FOLDER_COUNTS: TableDefinition<u64, (u64, u64)> = TableDefinition::new("folder_counts");

/// Sobres por id de mensaje.
pub(crate) const ENVELOPES: TableDefinition<u64, &[u8]> = TableDefinition::new("envelopes");
/// Índice (carpeta, fecha invertida, mensaje): listar una carpeta por fecha descendente es un
/// recorrido de rango ascendente.
pub(crate) const FOLDER_INDEX: TableDefinition<(u64, u64, u64), ()> = TableDefinition::new("folder_index");
/// Banderas por mensaje (bits de `Flags`), para cambios y filtros rápidos.
pub(crate) const FLAGS: TableDefinition<u64, u8> = TableDefinition::new("flags");
/// (carpeta, id remoto) → mensaje, para que la sincronización encuentre lo que ya tiene.
pub(crate) const REMOTE_INDEX: TableDefinition<(u64, &str), u64> = TableDefinition::new("remote_index");
/// (cuenta, Message-ID) → mensaje.
pub(crate) const MSGID_INDEX: TableDefinition<(u64, &str), u64> = TableDefinition::new("msgid_index");
/// (cuenta, Message-ID citado que aún no llegó) → hilo que lo espera.
pub(crate) const ORPHAN_REFS: TableDefinition<(u64, &str), u64> = TableDefinition::new("orphan_refs");

/// Hilos por id.
pub(crate) const THREADS: TableDefinition<u64, &[u8]> = TableDefinition::new("threads");
/// Índice (hilo, mensaje).
pub(crate) const THREAD_MEMBERS: TableDefinition<(u64, u64), ()> = TableDefinition::new("thread_members");

/// Calendarios por id.
pub(crate) const CALENDARS: TableDefinition<u64, &[u8]> = TableDefinition::new("calendars");
/// Eventos por id.
pub(crate) const EVENTS: TableDefinition<u64, &[u8]> = TableDefinition::new("events");
/// Índice (día, evento): una entrada por cada día que ocupa un evento corto.
pub(crate) const EVENT_DAYS: TableDefinition<(i64, u64), ()> = TableDefinition::new("event_days");
/// Eventos recurrentes o de más de [`crate::pim::MAX_INDEXED_DAYS`] días: se revisan siempre.
pub(crate) const EVENT_LONG: TableDefinition<u64, ()> = TableDefinition::new("event_long");
/// Índice (calendario, evento).
pub(crate) const CALENDAR_EVENTS: TableDefinition<(u64, u64), ()> = TableDefinition::new("calendar_events");

/// Libretas por id.
pub(crate) const ADDRESS_BOOKS: TableDefinition<u64, &[u8]> = TableDefinition::new("address_books");
/// Contactos por id.
pub(crate) const CONTACTS: TableDefinition<u64, &[u8]> = TableDefinition::new("contacts");
/// Índice (libreta, contacto).
pub(crate) const BOOK_CONTACTS: TableDefinition<(u64, u64), ()> = TableDefinition::new("book_contacts");

/// Notas por id.
pub(crate) const NOTES: TableDefinition<u64, &[u8]> = TableDefinition::new("notes");

/// Listas de tareas por id.
pub(crate) const TASK_LISTS: TableDefinition<u64, &[u8]> = TableDefinition::new("task_lists");
/// Tareas por id.
pub(crate) const TASKS: TableDefinition<u64, &[u8]> = TableDefinition::new("tasks");
/// Índice (lista, tarea).
pub(crate) const LIST_TASKS: TableDefinition<(u64, u64), ()> = TableDefinition::new("list_tasks");

/// Estado de sincronización genérico (clave libre → bytes opacos).
pub(crate) const SYNC_STATE: TableDefinition<&str, &[u8]> = TableDefinition::new("sync_state");
/// Cola de operaciones pendientes (offline), por número de secuencia.
pub(crate) const OPS_QUEUE: TableDefinition<u64, &[u8]> = TableDefinition::new("ops_queue");

/// Objeto crudo de cada elemento PIM sincronizado (`.ics`, `.vcf` o JSON de Graph): clave
/// (tipo, id local) → [`crate::RawObject`]. Ver [`crate::pim_remote`].
pub(crate) const PIM_RAW: TableDefinition<(u8, u64), &[u8]> = TableDefinition::new("pim_raw");
/// Estado de sincronización por colección (calendario, libreta, lista de tareas): clave
/// (tipo, id local) → [`crate::CollectionSync`].
pub(crate) const PIM_COLLECTIONS: TableDefinition<(u8, u64), &[u8]> = TableDefinition::new("pim_collections");

/// Crea todas las tablas (las transacciones de lectura fallan si una tabla no existe).
pub(crate) fn create_all(tx: &redb::WriteTransaction) -> Result<(), redb::TableError> {
    tx.open_table(META)?;
    tx.open_table(ACCOUNTS)?;
    tx.open_table(FOLDERS)?;
    tx.open_table(ACCOUNT_FOLDERS)?;
    tx.open_table(FOLDER_COUNTS)?;
    tx.open_table(ENVELOPES)?;
    tx.open_table(FOLDER_INDEX)?;
    tx.open_table(FLAGS)?;
    tx.open_table(REMOTE_INDEX)?;
    tx.open_table(MSGID_INDEX)?;
    tx.open_table(ORPHAN_REFS)?;
    tx.open_table(THREADS)?;
    tx.open_table(THREAD_MEMBERS)?;
    tx.open_table(CALENDARS)?;
    tx.open_table(EVENTS)?;
    tx.open_table(EVENT_DAYS)?;
    tx.open_table(EVENT_LONG)?;
    tx.open_table(CALENDAR_EVENTS)?;
    tx.open_table(ADDRESS_BOOKS)?;
    tx.open_table(CONTACTS)?;
    tx.open_table(BOOK_CONTACTS)?;
    tx.open_table(NOTES)?;
    tx.open_table(TASK_LISTS)?;
    tx.open_table(TASKS)?;
    tx.open_table(LIST_TASKS)?;
    tx.open_table(SYNC_STATE)?;
    tx.open_table(OPS_QUEUE)?;
    tx.open_table(PIM_RAW)?;
    tx.open_table(PIM_COLLECTIONS)?;
    Ok(())
}
