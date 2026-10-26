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
}
