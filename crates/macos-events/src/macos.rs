//! Manejadores de Apple Events con `NSAppleEventManager` (el único `unsafe` del workspace).
//!
//! AppKit instala sus propios manejadores de `odoc` (y de `GURL`, para `application:openURLs:`)
//! en `-[NSApplication finishLaunching]`, justo antes de avisar `WillFinishLaunching`, y los
//! eventos del arranque se despachan después. Por eso, como recomienda Apple, registramos los
//! nuestros al recibir `NSApplicationWillFinishLaunchingNotification` (además de una vez al
//! instalar, por si la app ya terminó de arrancar). Así no tocamos el delegado de winit.

use std::cell::OnceCell;

use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_foundation::{NSAppleEventDescriptor, NSAppleEventManager, NSNotification, NSNotificationCenter, NSString};

use crate::{INBOX, MAX_PENDING, MacEventsError};

/// Código de cuatro letras de Carbon (`'GURL'` → `0x4755524C`).
const fn code(c: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*c)
}

/// `kInternetEventClass` / `kAEGetURL` (los dos son `'GURL'`).
const INTERNET_EVENT_CLASS: u32 = code(b"GURL");
const AE_GET_URL: u32 = code(b"GURL");
/// `kCoreEventClass` / `kAEOpenDocuments`.
const CORE_EVENT_CLASS: u32 = code(b"aevt");
const AE_OPEN_DOCUMENTS: u32 = code(b"odoc");
/// `keyDirectObject` (`'----'`): el parámetro con el enlace o la lista de archivos.
const KEY_DIRECT_OBJECT: u32 = code(b"----");

/// El valor de `NSApplicationWillFinishLaunchingNotification` es su propio nombre; usarlo como
/// texto evita leer un `extern static` de AppKit (otro `unsafe`) y depender de objc2-app-kit.
const WILL_FINISH_LAUNCHING: &str = "NSApplicationWillFinishLaunchingNotification";

define_class!(
    // SAFETY:
    // - NSObject no tiene requisitos para subclases.
    // - `Handler` no implementa `Drop` ni tiene variables de instancia.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "IureDeskAppleEventHandler"]
    struct Handler;

    // SAFETY: `NSObjectProtocol` no pide nada más a una subclase de NSObject.
    unsafe impl NSObjectProtocol for Handler {}

    impl Handler {
        // SAFETY: firma que exige NSAppleEventManager para el selector:
        // `- (void)handle:(NSAppleEventDescriptor *)event withReplyEvent:(NSAppleEventDescriptor *)reply`.
        // Los dos son objetos (pueden llegar nulos: `Option`); no devuelve nada.
        #[unsafe(method(handleGetURL:withReplyEvent:))]
        fn handle_get_url(&self, event: Option<&NSAppleEventDescriptor>, _reply: Option<&NSAppleEventDescriptor>) {
            if let Some(url) = event.and_then(get_url) {
                let _ = INBOX.push(&url);
            }
        }

        // SAFETY: misma firma que el anterior.
        #[unsafe(method(handleOpenDocuments:withReplyEvent:))]
        fn handle_open_documents(
            &self,
            event: Option<&NSAppleEventDescriptor>,
            _reply: Option<&NSAppleEventDescriptor>,
        ) {
            for path in event.map(documents).unwrap_or_default() {
                let _ = INBOX.push(&path);
            }
        }

        // SAFETY: firma de un observador de NSNotificationCenter:
        // `- (void)willFinish:(NSNotification *)note`; no devuelve nada.
        #[unsafe(method(willFinishLaunching:))]
        fn will_finish_launching(&self, _note: Option<&NSNotification>) {
            register(self);
        }
    }
);

impl Handler {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: `init` de NSObject sobre un objeto recién reservado con sus (cero) variables
        // de instancia ya puestas; devuelve el objeto inicializado y retenido.
        unsafe { msg_send![super(this), init] }
    }
}

thread_local! {
    /// El manejador vive lo que el proceso: ni NSAppleEventManager ni NSNotificationCenter lo
    /// retienen, así que se guarda aquí (hilo principal) y nunca se suelta.
    static HANDLER: OnceCell<Retained<Handler>> = const { OnceCell::new() };
}

pub(crate) fn install() -> Result<(), MacEventsError> {
    let mtm = MainThreadMarker::new().ok_or(MacEventsError::NotMainThread)?;
    HANDLER.with(|cell| {
        if cell.get().is_some() {
            return;
        }
        let handler = cell.get_or_init(|| Handler::new(mtm));
        register(handler);
        let center = NSNotificationCenter::defaultCenter();
        let name = NSString::from_str(WILL_FINISH_LAUNCHING);
        // SAFETY: `handler` implementa `willFinishLaunching:` con la firma de un observador
        // (arriba) y vive hasta el final del proceso (`HANDLER`), así que el centro nunca llama
        // a un objeto liberado. `name` lo copia el centro; `object` nulo = de cualquier emisor.
        unsafe {
            center.addObserver_selector_name_object(handler, sel!(willFinishLaunching:), Some(&name), None);
        }
    });
    Ok(())
}

/// Registra (o vuelve a registrar, reemplazando los de AppKit) los dos manejadores.
fn register(handler: &Handler) {
    let manager = NSAppleEventManager::sharedAppleEventManager();
    // SAFETY: `handler` implementa los dos selectores con la firma que pide NSAppleEventManager
    // (`(NSAppleEventDescriptor *, NSAppleEventDescriptor *) -> void`, ver `define_class!`) y vive
    // hasta el final del proceso (`HANDLER`); el gestor no lo retiene pero nunca queda colgando.
    unsafe {
        manager.setEventHandler_andSelector_forEventClass_andEventID(
            handler,
            sel!(handleGetURL:withReplyEvent:),
            INTERNET_EVENT_CLASS,
            AE_GET_URL,
        );
        manager.setEventHandler_andSelector_forEventClass_andEventID(
            handler,
            sel!(handleOpenDocuments:withReplyEvent:),
            CORE_EVENT_CLASS,
            AE_OPEN_DOCUMENTS,
        );
    }
}

/// `GURL`: el enlace va como texto en el objeto directo.
fn get_url(event: &NSAppleEventDescriptor) -> Option<String> {
    let s = event.paramDescriptorForKeyword(KEY_DIRECT_OBJECT)?.stringValue()?.to_string();
    Some(s)
}

/// `odoc`: el objeto directo es una lista de alias/URL de archivo (o uno solo). Devuelve las
/// rutas absolutas, como mucho [`MAX_PENDING`].
fn documents(event: &NSAppleEventDescriptor) -> Vec<String> {
    let Some(direct) = event.paramDescriptorForKeyword(KEY_DIRECT_OBJECT) else {
        return Vec::new();
    };
    let n = direct.numberOfItems();
    if n <= 0 {
        return file_path(&direct).into_iter().collect();
    }
    let n = usize::try_from(n).unwrap_or(0).min(MAX_PENDING);
    // Las listas de Apple Events empiezan en 1.
    (1..=n)
        .filter_map(|i| isize::try_from(i).ok())
        .filter_map(|i| direct.descriptorAtIndex(i))
        .filter_map(|d| file_path(&d))
        .collect()
}

/// Ruta de un descriptor de archivo (alias, bookmark o `furl`); si no se puede convertir a URL de
/// archivo, su texto (`parse_target` también acepta `file://…`).
fn file_path(d: &NSAppleEventDescriptor) -> Option<String> {
    if let Some(url) = d.fileURLValue()
        && let Some(path) = url.path()
    {
        return Some(path.to_string());
    }
    d.stringValue().map(|s| s.to_string())
}
