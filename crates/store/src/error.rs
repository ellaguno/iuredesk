//! Error del almacén.

/// Error de cualquier operación del almacén.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// Fallo de la base de metadatos (redb).
    #[error("base de datos: {0}")]
    Db(Box<redb::Error>),
    /// Fallo de E/S (blobs, directorios).
    #[error("E/S: {0}")]
    Io(#[from] std::io::Error),
    /// Fallo del índice de búsqueda (tantivy).
    #[error("índice de búsqueda: {0}")]
    Search(String),
    /// Un registro guardado no se pudo (de)serializar.
    #[error("registro dañado: {0}")]
    Codec(String),
    /// Un blob no coincide con su huella o su cabecera es inválida.
    #[error("blob dañado: {0}")]
    Corrupt(String),
    /// No existe el objeto pedido.
    #[error("no existe {what} {id}")]
    NotFound {
        /// Tipo de objeto.
        what: &'static str,
        /// Id buscado.
        id: String,
    },
    /// Un dato supera el tamaño máximo admitido.
    #[error("{what} demasiado grande: {size} bytes (máximo {max})")]
    TooLarge {
        /// Qué dato.
        what: &'static str,
        /// Tamaño recibido.
        size: u64,
        /// Máximo admitido.
        max: u64,
    },
    /// Argumento inválido (id sin asignar donde se requiere uno, rango invertido…).
    #[error("argumento inválido: {0}")]
    Invalid(String),
    /// La base la creó una versión más nueva de IureDesk.
    #[error("la base de datos es de una versión más nueva (esquema {found}; esta versión admite {supported})")]
    SchemaTooNew {
        /// Versión encontrada.
        found: u32,
        /// Versión admitida.
        supported: u32,
    },
}

/// Resultado del almacén.
pub type Result<T, E = StoreError> = std::result::Result<T, E>;

impl StoreError {
    pub(crate) fn not_found(what: &'static str, id: impl ToString) -> Self {
        Self::NotFound { what, id: id.to_string() }
    }
}

macro_rules! from_redb {
    ($($t:ty),*) => {
        $(impl From<$t> for StoreError {
            fn from(e: $t) -> Self {
                Self::Db(Box::new(redb::Error::from(e)))
            }
        })*
    };
}
from_redb!(redb::TransactionError, redb::TableError, redb::StorageError, redb::CommitError, redb::DatabaseError);

impl From<redb::Error> for StoreError {
    fn from(e: redb::Error) -> Self {
        Self::Db(Box::new(e))
    }
}

impl From<tantivy::TantivyError> for StoreError {
    fn from(e: tantivy::TantivyError) -> Self {
        Self::Search(e.to_string())
    }
}

impl From<tantivy::directory::error::OpenDirectoryError> for StoreError {
    fn from(e: tantivy::directory::error::OpenDirectoryError) -> Self {
        Self::Search(e.to_string())
    }
}

impl From<postcard::Error> for StoreError {
    fn from(e: postcard::Error) -> Self {
        Self::Codec(e.to_string())
    }
}
