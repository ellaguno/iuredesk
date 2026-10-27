//! Asistente de importación (F4.2) sobre `iuredesk-import`: detecta los orígenes instalados
//! (Thunderbird, Apple Mail, Outlook…), importa en segundo plano con progreso, se puede cancelar
//! y reanudar (el punto de control lo guarda `iuredesk-import`) y termina con el informe.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, PoisonError};

use iuredesk_import::{DetectedSource, ImportEvent, ImportOptions, ImportReport, Importer, Progress};
use iuredesk_store::Store;

use crate::background::Waker;
use crate::lang::Text;
use crate::session::Session;

/// Estado del asistente.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum MigrateState {
    /// Sin empezar.
    #[default]
    Idle,
    /// Importando.
    Running,
    /// Terminó (o se canceló: `report.cancelled`).
    Finished(Box<ImportReport>),
    /// No se pudo seguir.
    Failed(Text),
}

enum Msg {
    Detected(Vec<DetectedSource>),
    Finished(Result<ImportReport, Text>),
}

/// La importación en la aplicación.
pub struct Migrate {
    store: Arc<Store>,
    rt: tokio::runtime::Handle,
    waker: Waker,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    detected: Option<Vec<DetectedSource>>,
    detecting: bool,
    state: MigrateState,
    progress: Arc<Mutex<Progress>>,
    cancel: Arc<AtomicBool>,
    last: Option<(Vec<PathBuf>, Option<String>)>,
}

impl std::fmt::Debug for Migrate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Migrate").field("state", &self.state).finish_non_exhaustive()
    }
}

impl Migrate {
    pub fn new(session: &Session) -> Self {
        let (tx, rx) = channel();
        Migrate {
            store: Arc::clone(session.store()),
            rt: session.runtime(),
            waker: session.waker(),
            tx,
            rx,
            detected: None,
            detecting: false,
            state: MigrateState::Idle,
            progress: Arc::new(Mutex::new(Progress::default())),
            cancel: Arc::new(AtomicBool::new(false)),
            last: None,
        }
    }

    /// Busca Thunderbird, Apple Mail y archivos de Outlook en este equipo (en segundo plano).
    pub fn detect(&mut self) {
        if self.detecting {
            return;
        }
        self.detecting = true;
        let tx = self.tx.clone();
        let waker = self.waker.clone();
        self.rt.spawn_blocking(move || {
            let found = std::panic::catch_unwind(iuredesk_import::find_installed).unwrap_or_default();
            let _ = tx.send(Msg::Detected(found));
            waker();
        });
    }

    /// Orígenes detectados (`None` = aún buscando o sin buscar).
    pub fn detected(&self) -> Option<&[DetectedSource]> {
        self.detected.as_deref()
    }

    /// Fija los orígenes detectados (capturas y pruebas, sin buscar en el equipo).
    pub fn set_detected(&mut self, found: Vec<DetectedSource>) {
        self.detected = Some(found);
    }

    pub fn is_detecting(&self) -> bool {
        self.detecting
    }

    /// Qué tipo de origen es una ruta elegida a mano.
    pub fn kind_of(path: &std::path::Path) -> Option<iuredesk_import::SourceKind> {
        iuredesk_import::detect(path)
    }

    /// Importa `paths` a cuentas locales (`account_name` para el correo suelto). Con `resume`
    /// continúa donde quedó la anterior.
    pub fn start(&mut self, paths: Vec<PathBuf>, account_name: Option<String>, resume: bool) -> Result<(), Text> {
        if self.state == MigrateState::Running {
            return Err(Text::new("An import is already running.", "Ya hay una importación en curso."));
        }
        if paths.is_empty() {
            return Err(Text::new("Choose what to import first.", "Primero elige qué importar."));
        }
        self.last = Some((paths.clone(), account_name.clone()));
        self.cancel = Arc::new(AtomicBool::new(false));
        *self.progress.lock().unwrap_or_else(PoisonError::into_inner) = Progress::default();
        self.state = MigrateState::Running;
        let opts = ImportOptions {
            resume,
            account_name: account_name.filter(|n| !n.trim().is_empty()),
            lang: if crate::lang::spanish() { "es".into() } else { "en".into() },
            time_zone: crate::time::user_tz(crate::time::Zone::Local),
            ..ImportOptions::default()
        };
        let store = Arc::clone(&self.store);
        let cancel = Arc::clone(&self.cancel);
        let progress = Arc::clone(&self.progress);
        let tx = self.tx.clone();
        let waker = self.waker.clone();
        self.rt.spawn_blocking(move || {
            let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                Importer::new(&store).options(opts).cancel_flag(cancel).run(&paths, |e| {
                    if let ImportEvent::Progress(p) = e {
                        *progress.lock().unwrap_or_else(PoisonError::into_inner) = p.clone();
                        waker();
                    }
                })
            }));
            let r = match run {
                Ok(Ok(rep)) => Ok(rep),
                Ok(Err(e)) => Err(Text::new(
                    format!("The import stopped: {e}. Run it again to continue where it left off."),
                    format!("La importación se detuvo: {e}. Vuelve a lanzarla para seguir donde quedó."),
                )),
                Err(_) => Err(Text::new(
                    "The import stopped unexpectedly. Run it again to continue.",
                    "La importación se detuvo de forma inesperada. Vuelve a lanzarla para seguir.",
                )),
            };
            let _ = tx.send(Msg::Finished(r));
            waker();
        });
        Ok(())
    }

    /// Reanuda la última importación (cancelada o detenida).
    pub fn resume(&mut self) -> Result<(), Text> {
        let Some((paths, name)) = self.last.clone() else {
            return Err(Text::new("Nothing to resume.", "No hay nada que reanudar."));
        };
        self.start(paths, name, true)
    }

    /// Pide cancelar (lo hecho se conserva; se puede reanudar).
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }

    pub fn state(&self) -> &MigrateState {
        &self.state
    }

    /// Progreso actual.
    pub fn progress(&self) -> Progress {
        self.progress.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Vuelve al principio (tras ver el informe).
    pub fn reset(&mut self) {
        if self.state != MigrateState::Running {
            self.state = MigrateState::Idle;
        }
    }

    /// Nada en curso.
    pub fn is_idle(&self) -> bool {
        !self.detecting && self.state != MigrateState::Running
    }

    /// Atiende los resultados. `true` si acaba de terminar una importación (recargar la vista).
    pub fn poll(&mut self) -> bool {
        let mut finished = false;
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Detected(d) => {
                    self.detecting = false;
                    self.detected = Some(d);
                }
                Msg::Finished(r) => {
                    finished = true;
                    self.state = match r {
                        Ok(rep) => MigrateState::Finished(Box::new(rep)),
                        Err(t) => MigrateState::Failed(t),
                    };
                }
            }
        }
        finished
    }
}
