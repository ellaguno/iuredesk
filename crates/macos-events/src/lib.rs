//! Apple Events de macOS que winit 0.30 no entrega (capa L3, sin interfaz), R1.4.
//!
//! En macOS el sistema no pasa por argumento lo que el usuario abre con IureDesk: lo manda como
//! Apple Event a la app (en marcha o recién lanzada).
//!
//! - `kAEGetURL` (`GURL`/`GURL`): `mailto:…` e `iuredesk://…` (de otras apps y de la suite).
//! - `kAEOpenDocuments` (`aevt`/`odoc`): `.eml`, `.ics` y `.vcf` desde el Finder o «Abrir con».
//!
//! [`install`] registra los manejadores (hay que llamarla en el hilo principal **antes** de que
//! arranque el bucle de eventos); cada enlace o ruta recibida queda en una bandeja acotada
//! ([`drain`]) que la app vacía en cada cuadro, así que lo que llega antes de que exista la
//! interfaz (arranque en frío) no se pierde. [`set_waker`] despierta a la interfaz al llegar algo.
//!
//! **Excepción acotada** (decisión del dueño 2026-10-29, `docs/excepciones.md`): este es el único
//! crate del workspace con `unsafe`, y solo en `src/macos.rs` (registrar un selector en
//! `NSAppleEventManager` y en `NSNotificationCenter` y declarar la clase que lo implementa). Fuera
//! de macOS el crate es Rust seguro (`forbid(unsafe_code)`) y [`install`] no hace nada.
#![cfg_attr(not(target_os = "macos"), forbid(unsafe_code))]
#![cfg_attr(target_os = "macos", deny(unsafe_op_in_unsafe_fn))]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod macos;

/// Máximo de enlaces/rutas pendientes (como `MAX_OPEN_ARGS` del puente IPC): una ráfaga hostil
/// no hace crecer la bandeja sin límite; lo que sobra se descarta.
pub const MAX_PENDING: usize = 64;

/// Largo máximo de cada enlace o ruta, en bytes (como el puente IPC).
pub const MAX_LEN: usize = 8 * 1024;

/// Función que despierta a la interfaz (p. ej. `egui::Context::request_repaint`).
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// Errores de [`install`].
#[derive(Debug, thiserror::Error)]
pub enum MacEventsError {
    /// AppKit exige el hilo principal para registrar manejadores.
    #[error("los Apple Events solo se pueden registrar desde el hilo principal")]
    NotMainThread,
}

/// Bandeja acotada de enlaces y rutas recibidos, segura entre hilos.
#[derive(Default)]
pub struct Inbox {
    items: Mutex<VecDeque<String>>,
    waker: Mutex<Option<Waker>>,
}

/// Recupera el contenido aunque otro hilo haya entrado en pánico con el candado (no se cae).
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Inbox {
    /// Bandeja vacía (constante: sirve para la global).
    pub const fn new() -> Self {
        Self { items: Mutex::new(VecDeque::new()), waker: Mutex::new(None) }
    }

    /// Guarda un enlace o ruta. Se descarta (y devuelve `false`) si está vacío, si pasa de
    /// [`MAX_LEN`], si contiene caracteres de control o si la bandeja está llena.
    pub fn push(&self, item: &str) -> bool {
        let item = item.trim();
        if item.is_empty() || item.len() > MAX_LEN || item.chars().any(char::is_control) {
            return false;
        }
        {
            let mut q = lock(&self.items);
            if q.len() >= MAX_PENDING {
                return false;
            }
            q.push_back(item.to_string());
        }
        // El candado ya se soltó: el despertador puede volver a llamar a la bandeja.
        let waker = lock(&self.waker).clone();
        if let Some(w) = waker {
            w();
        }
        true
    }

    /// Saca todo lo pendiente, en orden de llegada.
    pub fn drain(&self) -> Vec<String> {
        lock(&self.items).drain(..).collect()
    }

    /// Cuántos hay pendientes.
    pub fn len(&self) -> usize {
        lock(&self.items).len()
    }

    /// ¿Nada pendiente?
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Despertador que se llama tras cada [`Inbox::push`] aceptado. Si ya hay pendientes al
    /// ponerlo (llegaron antes de la interfaz), se llama enseguida.
    pub fn set_waker(&self, waker: Waker) {
        *lock(&self.waker) = Some(Arc::clone(&waker));
        if !self.is_empty() {
            waker();
        }
    }
}

/// La bandeja que llenan los manejadores de macOS.
static INBOX: Inbox = Inbox::new();

/// Registra los manejadores de Apple Events (macOS; en los demás sistemas no hace nada y
/// devuelve `Ok`). Llamar una vez, en el hilo principal, antes de `eframe::run_native`; las
/// llamadas siguientes no hacen nada.
pub fn install() -> Result<(), MacEventsError> {
    #[cfg(target_os = "macos")]
    {
        macos::install()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(())
    }
}

/// Enlaces (`mailto:`, `iuredesk://`) y rutas absolutas recibidos desde la última llamada.
pub fn drain() -> Vec<String> {
    INBOX.drain()
}

/// Despertador de la interfaz para lo que llegue (ver [`Inbox::set_waker`]).
pub fn set_waker(waker: Waker) {
    INBOX.set_waker(waker);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn guarda_en_orden_y_vacia() {
        let inbox = Inbox::new();
        assert!(inbox.push("mailto:ana@example.com"));
        assert!(inbox.push("  /Users/ana/cita.ics \n"));
        assert_eq!(inbox.len(), 2);
        assert_eq!(inbox.drain(), vec!["mailto:ana@example.com".to_string(), "/Users/ana/cita.ics".to_string()]);
        assert!(inbox.is_empty());
        assert!(inbox.drain().is_empty());
    }

    #[test]
    fn descarta_lo_hostil() {
        let inbox = Inbox::new();
        assert!(!inbox.push(""));
        assert!(!inbox.push("   "));
        assert!(!inbox.push(&"a".repeat(MAX_LEN + 1)));
        assert!(inbox.push(&"a".repeat(MAX_LEN)));
        assert!(!inbox.push("iuredesk://message/x\u{0}y"));
        assert!(!inbox.push("mailto:a@b\r\nBcc: c@d"));
        assert_eq!(inbox.len(), 1);
    }

    #[test]
    fn acotada() {
        let inbox = Inbox::new();
        for i in 0..MAX_PENDING + 10 {
            let _ = inbox.push(&format!("/tmp/{i}.eml"));
        }
        let got = inbox.drain();
        assert_eq!(got.len(), MAX_PENDING);
        assert_eq!(got.first().map(String::as_str), Some("/tmp/0.eml"));
        // Tras vaciar vuelve a aceptar.
        assert!(inbox.push("/tmp/otra.eml"));
    }

    #[test]
    fn arranque_en_frio_despierta_al_poner_el_despertador() {
        let inbox = Inbox::new();
        // Llega antes de que exista la interfaz (sin despertador): se guarda.
        assert!(inbox.push("iuredesk://event/abc"));
        let hits = Arc::new(AtomicUsize::new(0));
        let h = Arc::clone(&hits);
        inbox.set_waker(Arc::new(move || {
            h.fetch_add(1, Ordering::SeqCst);
        }));
        assert_eq!(hits.load(Ordering::SeqCst), 1, "había pendientes: despierta enseguida");
        assert!(inbox.push("mailto:x@example.com"));
        assert!(!inbox.push(""));
        assert_eq!(hits.load(Ordering::SeqCst), 2, "solo despierta con lo aceptado");
        assert_eq!(inbox.drain().len(), 2);
    }

    #[test]
    fn despertador_que_vacia_la_bandeja_no_se_bloquea() {
        let inbox = Arc::new(Inbox::new());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (i, s) = (Arc::clone(&inbox), Arc::clone(&seen));
        inbox.set_waker(Arc::new(move || lock(&s).extend(i.drain())));
        assert!(inbox.push("/tmp/a.vcf"));
        assert_eq!(*lock(&seen), vec!["/tmp/a.vcf".to_string()]);
    }

    #[test]
    fn segura_entre_hilos() {
        let inbox = Arc::new(Inbox::new());
        let hilos: Vec<_> = (0..8)
            .map(|t| {
                let i = Arc::clone(&inbox);
                std::thread::spawn(move || {
                    for n in 0..20 {
                        let _ = i.push(&format!("/tmp/{t}-{n}.eml"));
                    }
                })
            })
            .collect();
        for h in hilos {
            assert!(h.join().is_ok());
        }
        assert_eq!(inbox.drain().len(), MAX_PENDING);
    }

    #[test]
    fn instalar_fuera_de_macos_no_hace_nada() {
        #[cfg(not(target_os = "macos"))]
        {
            assert!(install().is_ok());
            assert!(drain().is_empty());
        }
    }
}
