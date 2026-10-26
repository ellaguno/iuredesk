//! Almacén local: redb + blobs comprimidos + índice tantivy (capa L2).
//!
//! Offline-first (PLAN.md D4/D5/D8): la interfaz lee siempre de aquí; la red solo sincroniza.
//!
//! Estructura en disco (`root/`):
//! - `meta.redb`: metadatos (carpetas, sobres, hilos, eventos, contactos, notas, tareas, estado de
//!   sincronización y cola de operaciones). Ver [`tables`].
//! - `blobs/`: contenido crudo direccionado por SHA-256 y comprimido (ver [`blob`]).
//! - `search/`: índice tantivy de mensajes (ver [`search`]).
//!
//! La API es síncrona y [`Store`] es `Send + Sync`: se comparte en un `Arc` entre el runtime de
//! sincronización y la interfaz. Las escrituras se serializan; las lecturas son instantáneas
//! (*snapshots*) de redb y no bloquean a las escrituras.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

pub mod blob;
mod codec;
pub mod error;
mod mail;
mod pim;
mod queue;
#[cfg(feature = "sample")]
pub mod sample;
pub mod search;
mod tables;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};

use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition, WriteTransaction};
use serde::Serialize;
use serde::de::DeserializeOwned;

pub use blob::{BlobStore, MAX_BLOB_SIZE};
pub use error::{Result, StoreError};
pub use mail::{AccountLabel, FolderCount, NewMessage};
pub use pim::MAX_INDEXED_DAYS;
pub use queue::{MAX_OP_PAYLOAD, QueuedOp};
pub use search::{ParsedQuery, QueryTerm, TermField, parse_query};

use crate::search::{IndexDoc, SearchIndex};

/// Versión actual del esquema de redb.
pub const SCHEMA_VERSION: u32 = 1;

/// Migración de un esquema `n` a `n + 1` (la posición `i` migra de `i + 1` a `i + 2`).
type Migration = fn(&WriteTransaction) -> Result<()>;
/// Migraciones registradas. Al cambiar el formato de un registro o una tabla se añade aquí una
/// función y se incrementa [`SCHEMA_VERSION`].
const MIGRATIONS: &[Migration] = &[];

/// Clave de la versión de esquema en `meta`.
const META_SCHEMA: &str = "schema_version";
/// Marca: el índice se reconstruyó sin el texto completo de los cuerpos.
const META_NEEDS_BODY_REINDEX: &str = "search_needs_body_reindex";

/// Cambio pendiente en la caché de carpetas, aplicado tras confirmar la transacción.
#[derive(Debug)]
enum CacheOp {
    Insert(u64, (u64, u64)),
    Remove(u64, (u64, u64)),
    Invalidate(u64),
}

/// Efectos secundarios de una transacción de escritura.
#[derive(Default)]
struct Pending {
    cache: Vec<CacheOp>,
    index: Vec<IndexDoc>,
    unindex: Vec<u64>,
}

/// Lista ordenada (fecha invertida, mensaje) de una carpeta, compartida con los lectores.
type FolderKeys = Arc<Vec<(u64, u64)>>;

/// Almacén local de un perfil.
pub struct Store {
    root: PathBuf,
    db: Database,
    blobs: BlobStore,
    search: SearchIndex,
    write_lock: Mutex<()>,
    /// Caché por carpeta del orden de la lista: da `list_envelopes` con desplazamiento en O(1)
    /// (un recorrido de rango de redb con `offset` sería O(offset)).
    folder_cache: RwLock<HashMap<u64, FolderKeys>>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store").field("root", &self.root).finish_non_exhaustive()
    }
}

/// Estadísticas del almacén.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StoreStats {
    /// Mensajes.
    pub messages: u64,
    /// Hilos.
    pub threads: u64,
    /// Carpetas.
    pub folders: u64,
    /// Eventos.
    pub events: u64,
    /// Contactos.
    pub contacts: u64,
    /// Documentos en el índice de búsqueda.
    pub indexed: u64,
    /// Operaciones pendientes en la cola.
    pub queued_ops: u64,
    /// Bytes de `meta.redb`.
    pub db_bytes: u64,
    /// Bytes de blobs.
    pub blob_bytes: u64,
    /// Bytes del índice de búsqueda.
    pub search_bytes: u64,
}

impl Store {
    /// Abre (o crea) el almacén en el directorio `root`.
    ///
    /// Aplica las migraciones pendientes. Si el índice de búsqueda falta o es de otra versión, lo
    /// reconstruye con lo que hay en redb (asunto, direcciones, extracto); para recuperar el texto
    /// completo de los cuerpos, la capa de sincronización llama a [`Store::reindex_all`] con un
    /// extractor de texto (ver [`Store::search_needs_body_reindex`]).
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        std::fs::create_dir_all(&root)?;
        let db = Database::create(root.join("meta.redb"))?;
        let blobs = BlobStore::open(root.join("blobs"))?;
        let (search, rebuilt) = SearchIndex::open(&root.join("search"))?;
        let store =
            Self { root, db, blobs, search, write_lock: Mutex::new(()), folder_cache: RwLock::new(HashMap::new()) };
        store.migrate()?;
        if rebuilt && store.message_count()? > 0 {
            store.reindex_all(None)?;
            store.set_meta(META_NEEDS_BODY_REINDEX, &[1])?;
        }
        Ok(store)
    }

    /// Directorio raíz del almacén.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Almacén de blobs.
    pub fn blobs(&self) -> &BlobStore {
        &self.blobs
    }

    fn migrate(&self) -> Result<()> {
        let _g = self.lock_writes();
        let tx = self.db.begin_write()?;
        tables::create_all(&tx)?;
        let found = {
            let meta = tx.open_table(tables::META)?;
            let v = meta.get(META_SCHEMA)?.map(|g| g.value().to_vec());
            v.and_then(|b| b.try_into().ok().map(u32::from_le_bytes))
        };
        match found {
            None => {}
            Some(v) if v > SCHEMA_VERSION => {
                return Err(StoreError::SchemaTooNew { found: v, supported: SCHEMA_VERSION });
            }
            Some(v) => {
                for step in v..SCHEMA_VERSION {
                    let idx = usize::try_from(step.saturating_sub(1)).unwrap_or(usize::MAX);
                    let m = MIGRATIONS
                        .get(idx)
                        .ok_or_else(|| StoreError::Invalid(format!("falta la migración {step} → {}", step + 1)))?;
                    m(&tx)?;
                }
            }
        }
        {
            let mut meta = tx.open_table(tables::META)?;
            meta.insert(META_SCHEMA, SCHEMA_VERSION.to_le_bytes().as_slice())?;
        }
        tx.commit()?;
        Ok(())
    }

    fn lock_writes(&self) -> MutexGuard<'_, ()> {
        self.write_lock.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Ejecuta una transacción de escritura. Orden: redb (sin confirmar) → índice de búsqueda
    /// (confirmado) → redb (confirmado) → caché. Si algo falla antes de confirmar redb, la
    /// transacción se descarta; un documento huérfano en el índice se filtra al buscar y se
    /// sobrescribe si el id se reutiliza.
    fn write<R>(&self, f: impl FnOnce(&WriteTransaction, &mut Pending) -> Result<R>) -> Result<R> {
        let _g = self.lock_writes();
        let tx = self.db.begin_write()?;
        let mut pending = Pending::default();
        let r = f(&tx, &mut pending)?;
        self.search.delete(&pending.unindex)?;
        if !pending.index.is_empty() {
            self.search.upsert(&pending.index)?;
        }
        tx.commit()?;
        self.apply_cache(pending.cache);
        Ok(r)
    }

    fn apply_cache(&self, ops: Vec<CacheOp>) {
        if ops.is_empty() {
            return;
        }
        let mut cache = self.folder_cache.write().unwrap_or_else(PoisonError::into_inner);
        for op in ops {
            match op {
                CacheOp::Insert(folder, key) => {
                    if let Some(v) = cache.get_mut(&folder) {
                        let v = Arc::make_mut(v);
                        if let Err(pos) = v.binary_search(&key) {
                            v.insert(pos, key);
                        }
                    }
                }
                CacheOp::Remove(folder, key) => {
                    if let Some(v) = cache.get_mut(&folder) {
                        let v = Arc::make_mut(v);
                        if let Ok(pos) = v.binary_search(&key) {
                            v.remove(pos);
                        }
                    }
                }
                CacheOp::Invalidate(folder) => {
                    cache.remove(&folder);
                }
            }
        }
    }

    fn read_rec<T: DeserializeOwned>(&self, table: TableDefinition<u64, &[u8]>, id: u64) -> Result<Option<T>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(table)?;
        let v = t.get(id)?;
        match v {
            Some(g) => Ok(Some(codec::decode(g.value())?)),
            None => Ok(None),
        }
    }

    fn read_all<T: DeserializeOwned>(&self, table: TableDefinition<u64, &[u8]>) -> Result<Vec<T>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(table)?;
        let mut out = Vec::new();
        for item in t.iter()? {
            let (_, v) = item?;
            out.push(codec::decode(v.value())?);
        }
        Ok(out)
    }

    fn set_meta(&self, key: &str, value: &[u8]) -> Result<()> {
        self.write(|tx, _| {
            let mut m = tx.open_table(tables::META)?;
            m.insert(key, value)?;
            Ok(())
        })
    }

    fn get_meta(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(tables::META)?;
        let v = t.get(key)?.map(|g| g.value().to_vec());
        Ok(v)
    }

    /// `true` si el índice se reconstruyó solo con los extractos y conviene llamar a
    /// [`Store::reindex_all`] con un extractor de texto.
    pub fn search_needs_body_reindex(&self) -> Result<bool> {
        Ok(self.get_meta(META_NEEDS_BODY_REINDEX)?.is_some_and(|v| v.first() == Some(&1)))
    }

    /// Compacta `meta.redb` (devuelve páginas libres al sistema). Requiere acceso exclusivo;
    /// pensado para mantenimiento o al cerrar.
    pub fn compact(&mut self) -> Result<bool> {
        self.db.compact().map_err(|e| StoreError::Db(Box::new(e.into())))
    }

    /// Estadísticas (recuentos y tamaño en disco).
    pub fn stats(&self) -> Result<StoreStats> {
        use redb::ReadableTableMetadata;
        let tx = self.db.begin_read()?;
        let len = |d: TableDefinition<u64, &[u8]>| -> Result<u64> { Ok(tx.open_table(d)?.len()?) };
        Ok(StoreStats {
            messages: len(tables::ENVELOPES)?,
            threads: len(tables::THREADS)?,
            folders: len(tables::FOLDERS)?,
            events: len(tables::EVENTS)?,
            contacts: len(tables::CONTACTS)?,
            indexed: self.search.num_docs(),
            queued_ops: len(tables::OPS_QUEUE)?,
            db_bytes: std::fs::metadata(self.root.join("meta.redb")).map(|m| m.len()).unwrap_or(0),
            blob_bytes: self.blobs.disk_usage(),
            search_bytes: blob::dir_size(&self.root.join("search"), 2),
        })
    }
}

/// Lee el siguiente id de un contador persistido (`seq:<tipo>`); empieza en 1.
fn next_id(meta: &mut redb::Table<'_, &str, &[u8]>, kind: &str) -> Result<u64> {
    let key = format!("seq:{kind}");
    let cur = meta.get(key.as_str())?.and_then(|g| <[u8; 8]>::try_from(g.value()).ok()).map_or(0, u64::from_le_bytes);
    let next = cur.checked_add(1).ok_or_else(|| StoreError::Invalid(format!("contador {kind} agotado")))?;
    meta.insert(key.as_str(), next.to_le_bytes().as_slice())?;
    Ok(next)
}

/// Asegura que el contador `kind` sea al menos `id` (cuando el llamador trae su propio id).
fn bump_seq(meta: &mut redb::Table<'_, &str, &[u8]>, kind: &str, id: u64) -> Result<()> {
    let key = format!("seq:{kind}");
    let cur = meta.get(key.as_str())?.and_then(|g| <[u8; 8]>::try_from(g.value()).ok()).map_or(0, u64::from_le_bytes);
    if id > cur {
        meta.insert(key.as_str(), id.to_le_bytes().as_slice())?;
    }
    Ok(())
}

/// Inserta un registro serializado.
fn put_rec<T: Serialize>(t: &mut redb::Table<'_, u64, &[u8]>, id: u64, v: &T) -> Result<()> {
    let bytes = codec::encode(v)?;
    t.insert(id, bytes.as_slice())?;
    Ok(())
}

/// Lee un registro dentro de una transacción de escritura.
fn get_rec<T: DeserializeOwned>(t: &impl ReadableTable<u64, &'static [u8]>, id: u64) -> Result<Option<T>> {
    match t.get(id)? {
        Some(g) => Ok(Some(codec::decode(g.value())?)),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn store_is_send_sync() {
        assert_send_sync::<Store>();
    }

    #[test]
    fn open_twice_and_schema_version() {
        let dir = tempfile::tempdir().unwrap();
        {
            let s = Store::open(dir.path()).unwrap();
            assert_eq!(s.stats().unwrap().messages, 0);
        }
        let s = Store::open(dir.path()).unwrap();
        assert_eq!(s.get_meta(META_SCHEMA).unwrap(), Some(SCHEMA_VERSION.to_le_bytes().to_vec()));
    }

    #[test]
    fn newer_schema_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        {
            let s = Store::open(dir.path()).unwrap();
            s.set_meta(META_SCHEMA, &(SCHEMA_VERSION + 1).to_le_bytes()).unwrap();
        }
        assert!(matches!(Store::open(dir.path()), Err(StoreError::SchemaTooNew { .. })));
    }
}
