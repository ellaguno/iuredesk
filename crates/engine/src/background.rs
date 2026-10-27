//! Trabajo lento fuera del hilo de la interfaz (PLAN.md D6): un runtime tokio de fondo y un canal
//! de resultados que la UI consulta en cada cuadro con [`Background::poll`]. La UI nunca espera:
//! encola el trabajo, sigue pintando y, cuando llega el resultado, el despertador pide un cuadro.
//!
//! Lo lento: render de HTML (lo encola la UI con [`Background::spawn_blocking`] y un cierre que
//! devuelve [`JobOutput::Custom`], porque `html-view` es de una capa superior), búsqueda,
//! agrupar por conversación, leer cuerpos, autoconfig, prueba de conexión y OAuth.

use std::any::Any;
use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, PoisonError};

use crate::EngineError;

/// Identificador de un trabajo.
pub type JobId = u64;

/// Función que pide un cuadro a la UI cuando llega un resultado.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// Resultado de un trabajo.
pub enum JobOutput {
    /// Lo interpreta la sesión (búsqueda, conversaciones, cuerpos, asistente de alta…).
    Engine(crate::session::EngineJob),
    /// Lo interpreta quien lo encoló (p. ej. la UI con un render de HTML).
    Custom(Box<dyn Any + Send>),
}

impl std::fmt::Debug for JobOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JobOutput::Engine(_) => f.write_str("JobOutput::Engine"),
            JobOutput::Custom(_) => f.write_str("JobOutput::Custom"),
        }
    }
}

/// Un trabajo terminado.
#[derive(Debug)]
pub struct Done {
    pub id: JobId,
    pub output: JobOutput,
}

/// Runtime de fondo y buzón de resultados.
pub struct Background {
    rt: tokio::runtime::Runtime,
    tx: Sender<Done>,
    rx: Receiver<Done>,
    pending: Arc<AtomicUsize>,
    waker: Arc<Mutex<Option<Waker>>>,
    next: JobId,
}

impl std::fmt::Debug for Background {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Background").field("pending", &self.pending()).finish_non_exhaustive()
    }
}

/// Envía el resultado y despierta a la UI; descuenta el trabajo aunque el receptor ya no exista.
struct Reply {
    id: JobId,
    tx: Sender<Done>,
    pending: Arc<AtomicUsize>,
    waker: Arc<Mutex<Option<Waker>>>,
}

impl Reply {
    fn send(self, output: JobOutput) {
        let _ = self.tx.send(Done { id: self.id, output });
        self.pending.fetch_sub(1, Ordering::AcqRel);
        let w = self.waker.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if let Some(w) = w {
            w();
        }
    }
}

impl Background {
    /// Crea el runtime (2 hilos de trabajo; lo bloqueante va al grupo de `spawn_blocking`).
    pub fn new() -> Result<Self, EngineError> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .max_blocking_threads(8)
            .thread_name("iuredesk-bg")
            .enable_all()
            .build()
            .map_err(|e| EngineError::Io(format!("runtime de fondo: {e}")))?;
        let (tx, rx) = channel();
        Ok(Background {
            rt,
            tx,
            rx,
            pending: Arc::new(AtomicUsize::new(0)),
            waker: Arc::new(Mutex::new(None)),
            next: 0,
        })
    }

    /// Fija el despertador (la UI pasa `ctx.request_repaint`).
    pub fn set_waker(&self, waker: Waker) {
        *self.waker.lock().unwrap_or_else(PoisonError::into_inner) = Some(waker);
    }

    fn reply(&mut self) -> Reply {
        self.next = self.next.wrapping_add(1);
        self.pending.fetch_add(1, Ordering::AcqRel);
        Reply { id: self.next, tx: self.tx.clone(), pending: Arc::clone(&self.pending), waker: Arc::clone(&self.waker) }
    }

    /// Ejecuta `f` (bloqueante: disco, CPU) en el grupo de hilos de fondo.
    pub fn spawn_blocking(&mut self, f: impl FnOnce() -> JobOutput + Send + 'static) -> JobId {
        let reply = self.reply();
        let id = reply.id;
        self.rt.spawn_blocking(move || {
            // Un pánico dentro del trabajo no debe dejar la UI esperando para siempre.
            let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|_| {
                JobOutput::Engine(crate::session::EngineJob::Failed(crate::lang::Text::new(
                    "Something went wrong in the background. Try again.",
                    "Algo falló en segundo plano. Inténtalo de nuevo.",
                )))
            });
            reply.send(out);
        });
        id
    }

    /// Ejecuta un futuro (red) en el runtime.
    pub fn spawn<F>(&mut self, fut: F) -> JobId
    where
        F: Future<Output = JobOutput> + Send + 'static,
    {
        let reply = self.reply();
        let id = reply.id;
        self.rt.spawn(async move {
            let out = fut.await;
            reply.send(out);
        });
        id
    }

    /// Función que despierta a la UI (para tareas propias que avisan por su cuenta).
    pub fn waker(&self) -> Waker {
        let w = Arc::clone(&self.waker);
        Arc::new(move || {
            let f = w.lock().unwrap_or_else(PoisonError::into_inner).clone();
            if let Some(f) = f {
                f();
            }
        })
    }

    /// Resultados listos (no bloquea).
    pub fn poll(&self) -> Vec<Done> {
        self.rx.try_iter().collect()
    }

    /// Trabajos en curso.
    pub fn pending(&self) -> usize {
        self.pending.load(Ordering::Acquire)
    }

    /// Manejador del runtime (para futuros que la sesión arma por su cuenta).
    pub fn handle(&self) -> tokio::runtime::Handle {
        self.rt.handle().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jobs_report_back_and_count() {
        let mut bg = Background::new().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let h = Arc::clone(&hits);
        bg.set_waker(Arc::new(move || {
            h.fetch_add(1, Ordering::Relaxed);
        }));
        let a = bg.spawn_blocking(|| JobOutput::Custom(Box::new(41u32)));
        let b = bg.spawn(async { JobOutput::Custom(Box::new("listo")) });
        let mut got = Vec::new();
        let start = std::time::Instant::now();
        while got.len() < 2 && start.elapsed().as_secs() < 10 {
            got.extend(bg.poll());
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(got.len(), 2);
        assert_eq!(bg.pending(), 0);
        assert!(hits.load(Ordering::Relaxed) >= 2);
        let ids: Vec<JobId> = got.iter().map(|d| d.id).collect();
        assert!(ids.contains(&a) && ids.contains(&b));
    }

    #[test]
    fn panicking_job_still_reports() {
        let mut bg = Background::new().unwrap();
        #[allow(clippy::panic)]
        bg.spawn_blocking(|| panic!("boom"));
        let start = std::time::Instant::now();
        let mut got = Vec::new();
        while got.is_empty() && start.elapsed().as_secs() < 10 {
            got.extend(bg.poll());
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(got.len(), 1);
        assert_eq!(bg.pending(), 0);
    }
}
