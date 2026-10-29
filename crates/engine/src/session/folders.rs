//! Carpetas propias (F7.1): crear, renombrar y borrar en local primero; el servidor se entera
//! por la cola (`CREATE`/`RENAME`/`DELETE` en IMAP, etiquetas en Gmail, `mailFolders` en
//! Microsoft 365). Sin sincronización en marcha (buzón de ejemplo, sin red) se usa la misma cola.

use iuredesk_model::account::Protocol;
use iuredesk_model::{AccountId, FolderId, FolderRole};
use iuredesk_sync::SyncError;
use iuredesk_sync::ops::FolderStyle;

use super::{Notice, Outcome, Session};
use crate::lang::Text;
use crate::{EngineError, Result};

fn sync_text(e: &SyncError) -> EngineError {
    EngineError::Invalid(e.message(crate::lang::spanish()))
}

impl Session {
    /// Cómo nombra carpetas el servidor de la cuenta; `None` si no tiene (POP3).
    pub(crate) fn folder_style(&self, account: AccountId) -> Option<FolderStyle> {
        if let Some(h) = &self.live.handle
            && h.has_account(account)
        {
            return h.folder_style(account);
        }
        let pop3 = self
            .accounts
            .iter()
            .find(|a| self.account_ids.get(&a.id) == Some(&account))
            .is_some_and(|a| a.incoming.protocol == Protocol::Pop3);
        (!pop3).then(|| FolderStyle::Imap { delimiter: iuredesk_sync::ops::imap_delimiter(&self.store, account) })
    }

    /// ¿Se pueden crear carpetas en la cuenta?
    pub fn can_manage_folders(&self, account: AccountId) -> bool {
        self.folder_style(account).is_some()
    }

    /// ¿Se puede renombrar o borrar la carpeta? (las del sistema, no).
    pub fn is_custom_folder(&self, folder: FolderId) -> bool {
        self.folder(folder).is_some_and(|f| f.role == FolderRole::Custom && self.can_manage_folders(f.account))
    }

    fn after_folder_change(&mut self, account: AccountId) {
        if let Ok(f) = self.store.all_folders() {
            self.folders = f;
        }
        self.counts.clear();
        self.data_gen = self.data_gen.wrapping_add(1);
        self.kick(account);
    }

    /// Crea una carpeta (en Gmail, una etiqueta) dentro de `parent` o en la raíz de la cuenta.
    pub fn create_folder(&mut self, account: AccountId, parent: Option<FolderId>, name: &str) -> Result<FolderId> {
        let style = self.folder_style(account).ok_or_else(|| sync_text(&SyncError::NoFolders))?;
        let id =
            iuredesk_sync::ops::create_folder(&self.store, account, parent, name, style).map_err(|e| sync_text(&e))?;
        self.after_folder_change(account);
        Ok(id)
    }

    /// Cambia el nombre de una carpeta propia.
    pub fn rename_folder(&mut self, folder: FolderId, name: &str) -> Result<Outcome> {
        let account = self.folder(folder).map(|f| f.account).ok_or(EngineError::NoSelection)?;
        let style = self.folder_style(account).ok_or_else(|| sync_text(&SyncError::NoFolders))?;
        iuredesk_sync::ops::rename_folder(&self.store, folder, name, style).map_err(|e| sync_text(&e))?;
        self.after_folder_change(account);
        Ok(Outcome::Notice(Notice::success("Folder renamed", "Carpeta renombrada")))
    }

    /// Borra una carpeta propia con todo lo que tiene. Sin `confirmed` pide confirmación (no se
    /// puede deshacer).
    pub fn delete_folder(&mut self, folder: FolderId, confirmed: bool) -> Result<Outcome> {
        let f = self.folder(folder).cloned().ok_or(EngineError::NoSelection)?;
        if f.role != FolderRole::Custom {
            return Err(sync_text(&SyncError::SystemFolder));
        }
        if !confirmed {
            let n = self.store.count(folder).map(|c| c.total).unwrap_or(0);
            let name = f.name.clone();
            return Ok(Outcome::Confirm {
                text: Text::new(
                    format!(
                        "Delete the folder \"{name}\" and its {n} messages? It is also deleted on the server and can't be undone."
                    ),
                    format!(
                        "¿Eliminar la carpeta «{name}» y sus {n} mensajes? También se elimina en el servidor y no se puede deshacer."
                    ),
                ),
                yes: Text::new("Delete folder", "Eliminar carpeta"),
                command: format!("mail.delete_folder:{}:yes", folder.get()),
            });
        }
        iuredesk_sync::ops::delete_folder(&self.store, folder).map_err(|e| sync_text(&e))?;
        if self.folder_sel() == super::FolderSel::Folder(folder) {
            self.select_folder(super::FolderSel::Unified(FolderRole::Inbox));
        }
        self.after_folder_change(f.account);
        Ok(Outcome::Notice(Notice::success(
            format!("Folder \"{}\" deleted", f.name),
            format!("Carpeta «{}» eliminada", f.name),
        )))
    }

    /// `mail.new_folder:<cuenta>:<nombre>`, `mail.rename_folder:<carpeta>:<nombre>`,
    /// `mail.delete_folder:<carpeta>[:yes]` (paleta, CLI, MCP; sin diálogos).
    pub(crate) fn run_folder_command(&mut self, base: &str, arg: Option<&str>) -> Result<Outcome> {
        let (first, rest) = match arg.and_then(|a| a.split_once(':')) {
            Some((a, b)) => (a, Some(b)),
            None => (arg.unwrap_or_default(), None),
        };
        let num = first.trim().parse::<u64>().ok();
        match (base, num, rest) {
            ("mail.new_folder", Some(a), Some(name)) => {
                let account = AccountId(a);
                self.create_folder(account, None, name)?;
                Ok(Outcome::Notice(Notice::success(
                    format!("Folder \"{}\" created", name.trim()),
                    format!("Carpeta «{}» creada", name.trim()),
                )))
            }
            ("mail.rename_folder", Some(f), Some(name)) => self.rename_folder(FolderId(f), name),
            ("mail.delete_folder", Some(f), confirm) => self.delete_folder(FolderId(f), confirm == Some("yes")),
            _ => Ok(Outcome::Notice(Notice::info(
                "Right-click a folder in the folder pane to create, rename or delete folders.",
                "Haz clic derecho en una carpeta del panel para crear, renombrar o eliminar carpetas.",
            ))),
        }
    }

    /// F7.1: carpeta «Pospuestos» de la cuenta; si no existe y el servidor tiene carpetas, se
    /// crea (así lo pospuesto se ve igual en los demás equipos).
    pub(crate) fn ensure_snooze_folder(&mut self, account: AccountId) -> Option<FolderId> {
        if let Some(f) = iuredesk_rules::snooze::snooze_folder(&self.folders, account) {
            return Some(f);
        }
        // Solo con la sincronización de esa cuenta en marcha (en el ejemplo o sin red se oculta
        // con `$Snoozed`, como antes).
        let live = self.live.handle.as_ref().is_some_and(|h| h.has_account(account));
        if !live || !self.can_manage_folders(account) {
            return None;
        }
        let name = crate::lang::pick("Snoozed", "Pospuestos");
        self.create_folder(account, None, name).ok()
    }
}
