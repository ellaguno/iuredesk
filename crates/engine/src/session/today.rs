//! «Mi día» en la sesión (F6.1): el resumen se calcula en segundo plano desde el almacén (nunca
//! en el hilo de la interfaz) y se vuelve a pedir cuando cambian los datos (sincronización,
//! tareas, avisos de Iurefficient) o pasa un minuto. También el arranque: abrir en «Mi día» o
//! donde se dejó la última vez.

use std::sync::Arc;

use iuredesk_model::{FolderId, FolderRole};

use super::{EngineJob, FolderSel, Module, Session};
use crate::background::{JobId, JobOutput};
use crate::prefs::StartPref;
use crate::today::{self, DaySummary, GatherArgs, IureDigest};

/// Clave de vigencia: datos de PIM, lista de correo, avisos, minuto.
type DayKey = (u64, u64, u64, usize, i64);

/// Parte «Mi día» de la sesión.
#[derive(Default)]
pub(crate) struct DayState {
    summary: Option<Arc<DaySummary>>,
    job: Option<JobId>,
    key: Option<DayKey>,
}

const ROLES: [FolderRole; 7] = [
    FolderRole::Inbox,
    FolderRole::Sent,
    FolderRole::Drafts,
    FolderRole::Trash,
    FolderRole::Junk,
    FolderRole::Archive,
    FolderRole::Custom,
];

/// Clave de texto de una carpeta del correo (para las preferencias).
pub fn folder_key(f: FolderSel) -> String {
    match f {
        FolderSel::Unified(r) => format!("unified:{}", r.slug()),
        FolderSel::Flagged => "flagged".into(),
        FolderSel::Folder(id) => format!("folder:{}", id.get()),
        FolderSel::Outbox => "outbox".into(),
    }
}

/// La carpeta de una clave de [`folder_key`] (sin comprobar que exista).
pub fn folder_from_key(s: &str) -> Option<FolderSel> {
    match s.split_once(':') {
        None if s == "flagged" => Some(FolderSel::Flagged),
        None if s == "outbox" => Some(FolderSel::Outbox),
        Some(("unified", r)) => ROLES.into_iter().find(|x| x.slug() == r).map(FolderSel::Unified),
        Some(("folder", n)) => n.parse::<u64>().ok().map(|n| FolderSel::Folder(FolderId(n))),
        _ => None,
    }
}

impl Session {
    fn day_key(&self) -> DayKey {
        let feed = self.iure_feed();
        (self.data_gen, self.generation, feed.unread, feed.items.len(), self.now().timestamp().div_euclid(60))
    }

    /// El resumen del día más reciente. Si los datos cambiaron o pasó un minuto, pide otro en
    /// segundo plano (mientras tanto devuelve el anterior; `None` solo la primera vez).
    pub fn day_summary(&mut self) -> Option<Arc<DaySummary>> {
        let key = self.day_key();
        if self.day.job.is_none() && self.day.key != Some(key) {
            self.day.key = Some(key);
            let args = self.day_args();
            let store = Arc::clone(&self.store);
            self.day.job = Some(self.bg.spawn_blocking(move || {
                let s = today::build(&today::gather(&store, args));
                JobOutput::Engine(EngineJob::Day(Box::new(s)))
            }));
        }
        self.day.summary.clone()
    }

    /// Llega un resumen calculado en segundo plano.
    pub(crate) fn on_day(&mut self, id: JobId, s: DaySummary) {
        if self.day.job == Some(id) {
            self.day.job = None;
        }
        self.day.summary = Some(Arc::new(s));
    }

    /// Un trabajo falló: si era el del resumen, se podrá volver a pedir.
    pub(crate) fn day_job_failed(&mut self, id: JobId) {
        if self.day.job == Some(id) {
            self.day.job = None;
            self.day.key = None;
        }
    }

    fn day_args(&self) -> GatherArgs {
        let iure = match self.iure_status() {
            crate::iure::IureStatus::Ready { host, .. } => {
                let feed = self.iure_feed();
                let mut items: Vec<_> = feed.items.iter().filter(|i| !i.read).take(6).cloned().collect();
                items.extend(feed.items.iter().filter(|i| i.read).take(6usize.saturating_sub(items.len())).cloned());
                Some(IureDigest { host: host.clone(), unread: feed.unread, items })
            }
            _ => None,
        };
        let user = self
            .labels
            .iter()
            .map(|l| l.name.as_str())
            .chain(self.accounts.iter().map(|a| a.display_name.as_str()))
            .find(|n| !n.trim().is_empty())
            .and_then(|n| n.split_whitespace().next())
            .unwrap_or("")
            .chars()
            .take(40)
            .collect();
        GatherArgs {
            now: self.now(),
            zone: self.zone,
            me: self.my_addresses(),
            user,
            labels: self.labels.clone(),
            folders: self.folders.clone(),
            calendars: self.calendars.clone(),
            hidden: self.hidden_calendar_ids(),
            iure,
        }
    }

    /// Al abrir la aplicación: «Mi día» o donde se dejó (según las preferencias).
    pub fn open_at_start(&mut self) {
        match self.prefs.start {
            StartPref::Today => self.module = Module::Today,
            StartPref::LastPlace => {
                let folder = self.prefs.last_folder.as_deref().and_then(folder_from_key).filter(|f| match f {
                    FolderSel::Folder(id) => self.folder(*id).is_some(),
                    _ => true,
                });
                if let Some(f) = folder {
                    self.select_folder(f);
                }
                self.module = self.prefs.last_module.as_deref().and_then(Module::from_key).unwrap_or(Module::Today);
            }
        }
    }

    /// Recuerda el módulo y la carpeta actuales (los guarda solo si cambiaron).
    pub fn remember_place(&mut self) {
        let module = Some(self.module.key().to_string());
        let folder = Some(folder_key(self.folder));
        if self.prefs.last_module != module || self.prefs.last_folder != folder {
            self.prefs.last_module = module;
            self.prefs.last_folder = folder;
            self.save_prefs();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_keys_round_trip() {
        for f in [
            FolderSel::Unified(FolderRole::Inbox),
            FolderSel::Unified(FolderRole::Archive),
            FolderSel::Flagged,
            FolderSel::Outbox,
            FolderSel::Folder(FolderId(42)),
        ] {
            assert_eq!(folder_from_key(&folder_key(f)), Some(f));
        }
        for bad in ["", "unified:nada", "folder:x", "folder:", "otra:1", "flagged:1"] {
            assert_eq!(folder_from_key(bad), None, "{bad}");
        }
    }
}
