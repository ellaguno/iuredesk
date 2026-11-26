//! Lo que piden los atajos de Gmail que faltaban (UX-26): siguiente y anterior (`j`/`k`), ir a
//! una carpeta (`g i`, `g s`, `g t`, `g d`) y «Es spam» (`!`). Como todo, son comandos: la
//! paleta, la CLI y MCP los usan igual.

use iuredesk_model::FolderRole;

use super::{FolderSel, Notice, NoticeKind, Outcome, Session};
use crate::Result;
use crate::lang::Text;

impl Session {
    /// `mail.next` / `mail.prev`: abre el mensaje siguiente o anterior de la lista (sin abierto,
    /// el primero). Al llegar al final se queda en el último.
    pub(crate) fn run_step(&mut self, forward: bool) -> Result<Outcome> {
        let rows = self.rows_shared();
        if rows.is_empty() {
            return Ok(Outcome::Notice(Notice::info("This list is empty.", "Esta lista está vacía.")));
        }
        // Con filas marcadas el abierto no se mueve: se sigue desde el último paso mientras el
        // abierto sea el mismo de entonces.
        let from = match self.step_cursor {
            Some((was, cursor)) if was == self.focused => Some(cursor),
            _ => self.focused,
        };
        let at = from.and_then(|f| rows.iter().position(|r| r.id == f));
        let next = match at {
            None => 0,
            Some(i) if forward => i.saturating_add(1).min(rows.len().saturating_sub(1)),
            Some(i) => i.saturating_sub(1),
        };
        if let Some(r) = rows.get(next) {
            self.request_focus(r.id);
            self.step_request = true;
            self.step_cursor = Some((self.focused, r.id));
        }
        Ok(Outcome::Done)
    }

    /// ¿La petición de foco que se acaba de tomar vino de `j`/`k`? (una vez)
    pub fn take_step_request(&mut self) -> bool {
        std::mem::take(&mut self.step_request)
    }

    /// `mail.go.inbox|flagged|sent|drafts`: cambia a esa vista (en todas las cuentas).
    pub(crate) fn run_go(&mut self, to: &str) -> Result<Outcome> {
        let sel = match to {
            "inbox" => FolderSel::Unified(FolderRole::Inbox),
            "flagged" => FolderSel::Flagged,
            "sent" => FolderSel::Unified(FolderRole::Sent),
            "drafts" => FolderSel::Unified(FolderRole::Drafts),
            _ => return Err(crate::EngineError::UnknownCommand(format!("mail.go.{to}"))),
        };
        self.select_folder(sel);
        Ok(Outcome::Done)
    }

    /// `mail.spam`: mueve a «No deseado» (con deshacer). Lo que ya está ahí se queda.
    pub(crate) fn run_spam(&mut self) -> Result<Outcome> {
        let ids = self.targets();
        let ids: Vec<_> = ids
            .into_iter()
            .filter(|id| {
                let folder = self.envelope(*id).map(|e| e.folder);
                !folder.and_then(|f| self.folder(f)).is_some_and(|f| f.role == FolderRole::Junk)
            })
            .collect();
        if ids.is_empty() {
            return Ok(Outcome::Notice(Notice::info("It's already in Junk.", "Ya está en «No deseado».")));
        }
        let (n, steps) = self.move_with(&ids, |s, acc| s.role_folder(acc, FolderRole::Junk))?;
        if n == 0 {
            return Ok(Outcome::Notice(Notice::error(Text::new(
                "This account has no junk folder.",
                "Esta cuenta no tiene carpeta de correo no deseado.",
            ))));
        }
        let text = Self::plural(
            n,
            "Message marked as spam",
            "messages marked as spam",
            "Mensaje enviado a «No deseado»",
            "mensajes enviados a «No deseado»",
        );
        self.push_undo(text.clone(), steps);
        self.after_removal(&ids);
        Ok(Outcome::Notice(Notice { kind: NoticeKind::Success, text, undo: true, secs: None }))
    }
}
