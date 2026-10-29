//! R1.2: abrir lo que pasa el sistema (archivos y enlaces `iuredesk://`). La lectura de archivos
//! corre en segundo plano; la UI recibe un [`FileOpened`] por [`super::Event::Custom`].

use std::path::PathBuf;

use iuredesk_model::{FolderRole, MessageId};

use super::{FolderSel, Module, Session};
use crate::JobId;
use crate::JobOutput;
use crate::lang::Text;
use crate::open::{FileKind, Opened};

/// Resultado de leer un archivo abierto ([`Session::open_file`]).
pub struct FileOpened {
    pub path: PathBuf,
    pub result: Result<Opened, Text>,
}

impl Session {
    /// Lee el archivo en segundo plano; el resultado llega como [`FileOpened`].
    pub fn open_file(&mut self, path: PathBuf, kind: FileKind) -> JobId {
        let mine = self.my_addresses();
        let tz = crate::time::user_tz(self.zone);
        self.bg.spawn_blocking(move || {
            let result = crate::open::load(&path, kind, &mine, tz);
            JobOutput::Custom(Box::new(FileOpened { path, result }))
        })
    }

    /// El mensaje con ese `Message-ID`, si está en el almacén.
    pub fn message_by_message_id(&self, message_id: &str) -> Option<MessageId> {
        self.store.message_by_msgid(message_id).ok().flatten()
    }

    /// Lleva al mensaje con ese `Message-ID` (su carpeta, seleccionado). `false` si no está.
    pub fn open_message_id(&mut self, message_id: &str) -> bool {
        let Some(id) = self.message_by_message_id(message_id) else { return false };
        let Some(env) = self.store.get_envelope(id).ok().flatten() else { return false };
        self.select_folder(FolderSel::Folder(env.folder));
        self.module = Module::Mail;
        // En una bandeja de entrada, la pestaña de su categoría (si no, no se vería).
        if self.folder(env.folder).is_some_and(|f| f.role == FolderRole::Inbox)
            && let Some((c, _)) = self.category_of(id)
            && self.category_tab() != c
        {
            self.set_category_tab(c);
        }
        self.request_focus(id);
        true
    }
}
