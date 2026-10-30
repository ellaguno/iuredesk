//! Apple Events de macOS que winit 0.30 no entrega y clics en notificaciones (capa L3, sin
//! interfaz), R1.4 y R1.5.
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
//! R1.5: [`install`] también pone el delegado de `UNUserNotificationCenter` (solo dentro del
//! `.app`): al hacer clic en un aviso, el enlace guardado en su `userInfo` bajo
//! [`NOTIFICATION_LINK_KEY`] (`iuredesk://message/<id>` o `iuredesk://event/<uid>`, ver
//! [`notification_link`]) entra en la misma bandeja; y los avisos se muestran aunque IureDesk esté
//! al frente. `iuredesk-desktop` guarda el enlace con `set_notification_link` (solo macOS).
//!
//! **Excepción acotada** (decisión del dueño 2026-10-29, `docs/excepciones.md`): este es el único
//! crate del workspace con `unsafe`, y solo en `src/macos.rs` (registrar un selector en
//! `NSAppleEventManager` y en `NSNotificationCenter`, declarar las clases que lo implementan y el
//! delegado de notificaciones, y poner el `userInfo` del aviso). Fuera
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

/// Clave del `userInfo` de un aviso de macOS con el enlace que abre su clic.
pub const NOTIFICATION_LINK_KEY: &str = "iuredesk-link";

/// `UNNotificationDismissActionIdentifier` (su valor es este texto; así no se lee un `extern
/// static`): el usuario cerró el aviso sin abrirlo.
pub const DISMISS_ACTION: &str = "com.apple.UNNotificationDismissActionIdentifier";

/// Destinos que puede abrir el clic en un aviso.
const NOTIFICATION_TARGETS: &[&str] = &["iuredesk://message/", "iuredesk://event/"];

/// Enlace válido para guardar en (o leer de) el `userInfo` de un aviso: solo
/// `iuredesk://message/<id>` o `iuredesk://event/<uid>` (esquema sin distinguir mayúsculas), no
/// vacío tras el prefijo, de ≤ [`MAX_LEN`] bytes y sin caracteres de control. Lo demás, `None`.
pub fn notification_link(link: &str) -> Option<String> {
    let link = link.trim();
    if link.len() > MAX_LEN || link.chars().any(char::is_control) {
        return None;
    }
    let ok = NOTIFICATION_TARGETS.iter().any(|p| {
        link.get(..p.len()).is_some_and(|head| head.eq_ignore_ascii_case(p))
            && link.get(p.len()..).is_some_and(|rest| !rest.trim_start_matches('/').is_empty())
    });
    ok.then(|| link.to_string())
}

/// Clic en un aviso: `action` es el `actionIdentifier` de la respuesta y `value`, lo guardado bajo
/// [`NOTIFICATION_LINK_KEY`]. Si no se descartó y el enlace es válido, entra en `inbox`.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn deliver_click(inbox: &Inbox, action: &str, value: Option<&str>) -> bool {
    if action == DISMISS_ACTION {
        return false;
    }
    value.and_then(notification_link).is_some_and(|link| inbox.push(&link))
}

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

/// macOS: guarda en el `userInfo` del aviso el enlace que abre su clic (si [`notification_link`]
/// lo acepta; si no, el clic solo trae la app al frente).
#[cfg(target_os = "macos")]
pub use macos::set_notification_link;

/// Registra los manejadores de Apple Events y, dentro del `.app`, el delegado de notificaciones
/// (macOS; en los demás sistemas no hace nada y devuelve `Ok`). Llamar una vez, en el hilo principal, antes de `eframe::run_native`; las
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

/// Enlaces (`mailto:`, `iuredesk://`, clics en avisos) y rutas absolutas recibidos desde la última
/// llamada.
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
    fn enlace_de_aviso_valido() {
        assert_eq!(
            notification_link("iuredesk://message/a@b.example").as_deref(),
            Some("iuredesk://message/a@b.example")
        );
        assert_eq!(notification_link(" iuredesk://event/uid-1 \n").as_deref(), Some("iuredesk://event/uid-1"));
        assert_eq!(notification_link("IureDesk://Message/X").as_deref(), Some("IureDesk://Message/X"));
        assert!(notification_link(&format!("iuredesk://message/{}", "a".repeat(MAX_LEN - 19))).is_some());
    }

    #[test]
    fn enlace_de_aviso_hostil() {
        for bad in [
            "",
            "iuredesk://message/",
            "iuredesk://message///",
            "iuredesk://event",
            "iuredesk://open",
            "iuredesk://compose?to=a@b",
            "mailto:a@b",
            "https://malo.example/iuredesk://message/x",
            "file:///etc/passwd",
            "iuredesk://message/x\u{0}y",
            "iuredesk://message/x\r\nmás",
            "iuredesk://mes",
            "ñiuredesk://message/x",
        ] {
            assert!(notification_link(bad).is_none(), "{bad:?}");
        }
        assert!(notification_link(&format!("iuredesk://message/{}", "a".repeat(MAX_LEN))).is_none());
    }

    #[test]
    fn clic_en_aviso_va_a_la_bandeja() {
        let inbox = Inbox::new();
        let default = "com.apple.UNNotificationDefaultActionIdentifier";
        assert!(deliver_click(&inbox, default, Some("iuredesk://message/a@b")));
        assert!(deliver_click(&inbox, "otra-accion", Some("iuredesk://event/u1")));
        assert!(!deliver_click(&inbox, DISMISS_ACTION, Some("iuredesk://message/c@d")));
        assert!(!deliver_click(&inbox, default, None));
        assert!(!deliver_click(&inbox, default, Some("https://malo.example")));
        assert_eq!(inbox.drain(), vec!["iuredesk://message/a@b".to_string(), "iuredesk://event/u1".to_string()]);
    }

    #[test]
    fn clics_acotados() {
        let inbox = Inbox::new();
        for i in 0..MAX_PENDING + 5 {
            let _ = deliver_click(&inbox, "x", Some(&format!("iuredesk://event/{i}")));
        }
        assert_eq!(inbox.len(), MAX_PENDING);
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
