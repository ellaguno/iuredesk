//! Notas en Markdown.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{AccountId, NoteId};

/// Nota del usuario (local, o sincronizada por WebDAV / IMAP «Notes»).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    /// Id local.
    pub id: NoteId,
    /// Cuenta con la que se sincroniza; `None` = solo local.
    pub account: Option<AccountId>,
    /// Título.
    pub title: String,
    /// Cuerpo en Markdown.
    pub body: String,
    /// Etiquetas.
    pub tags: Vec<String>,
    /// Fijada arriba.
    pub pinned: bool,
    /// Creación.
    pub created: DateTime<Utc>,
    /// Última modificación.
    pub updated: DateTime<Utc>,
    /// Id o `href` remoto, opaco.
    pub remote_id: Option<String>,
    /// UX-6: archivo Markdown de la nota en la carpeta de notas (`None` = aún no se escribió).
    pub file: Option<NoteFile>,
}

/// UX-6: archivo `.md` de una nota en la carpeta de notas (el almacén es el índice).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteFile {
    /// Nombre del archivo dentro de la carpeta (sin ruta).
    pub name: String,
    /// Huella SHA-256 (hex) del contenido del archivo la última vez que se escribió o leyó.
    pub hash: String,
    /// `Note::updated` en ese momento: si la nota cambió después, hay cambios sin escribir.
    pub synced: DateTime<Utc>,
}
