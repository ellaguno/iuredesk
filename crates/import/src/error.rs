//! Errores de la importación.
//!
//! Un error de este tipo **detiene** la importación (no se puede abrir el almacén, el origen no
//! existe…). Lo que falla en un elemento suelto (un mensaje ilegible, un contacto roto) no es un
//! error: se omite y queda en el informe ([`crate::ImportReport::skipped`]).

use std::path::PathBuf;

/// Fallo que impide seguir importando.
#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    /// El origen no existe o no se puede leer.
    #[error("no se pudo leer «{path}»: {source}")]
    Io {
        /// Ruta.
        path: PathBuf,
        /// Error del sistema.
        source: std::io::Error,
    },
    /// No se reconoce el formato del origen.
    #[error("no se reconoce el formato de «{0}»")]
    Unrecognized(PathBuf),
    /// Almacén local.
    #[error("almacén local: {0}")]
    Store(#[from] iuredesk_store::StoreError),
    /// Se pidió cancelar. [`crate::Importer::run`] no lo devuelve: lo convierte en
    /// [`crate::ImportReport::cancelled`].
    #[error("importación cancelada")]
    Cancelled,
    /// El archivo de reanudación está dañado o no se puede escribir.
    #[error("estado de reanudación: {0}")]
    Checkpoint(String),
}

impl ImportError {
    /// Error de E/S con su ruta.
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io { path: path.into(), source }
    }

    /// Mensaje para la interfaz en el idioma pedido (`es` o `en`), sin jerga.
    pub fn human(&self, lang: &str) -> String {
        let en = lang.starts_with("en");
        match self {
            Self::Io { path, .. } => {
                if en {
                    format!("Couldn't read “{}”. Check that it exists and that you can open it.", path.display())
                } else {
                    format!("No se pudo leer «{}». Revisa que exista y que puedas abrirlo.", path.display())
                }
            }
            Self::Unrecognized(p) => {
                if en {
                    format!("“{}” isn't a mailbox, address book or calendar IureDesk can import.", p.display())
                } else {
                    format!("«{}» no es un buzón, libreta o calendario que IureDesk sepa importar.", p.display())
                }
            }
            Self::Store(_) => {
                if en {
                    "IureDesk couldn't save the imported data. Check free disk space.".into()
                } else {
                    "IureDesk no pudo guardar lo importado. Revisa el espacio libre en disco.".into()
                }
            }
            Self::Cancelled => {
                if en {
                    "Import cancelled. Start it again to continue where it stopped.".into()
                } else {
                    "Importación cancelada. Vuelve a lanzarla para seguir donde se quedó.".into()
                }
            }
            Self::Checkpoint(_) => {
                if en {
                    "The import progress file is damaged; the import will start over (nothing is duplicated).".into()
                } else {
                    "El archivo de progreso está dañado; la importación empezará de nuevo (no se duplica nada).".into()
                }
            }
        }
    }
}

/// Resultado de la importación.
pub type Result<T, E = ImportError> = std::result::Result<T, E>;
