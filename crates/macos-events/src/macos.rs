//! Manejadores de Apple Events con `NSAppleEventManager` (el único `unsafe` del workspace).
//!
//! AppKit instala sus propios manejadores de `odoc` (y de `GURL`, para `application:openURLs:`)
//! en `-[NSApplication finishLaunching]`, justo antes de avisar `WillFinishLaunching`, y los
//! eventos del arranque se despachan después. Por eso, como recomienda Apple, registramos los
//! nuestros al recibir `NSApplicationWillFinishLaunchingNotification` (además de una vez al
//! instalar, por si la app ya terminó de arrancar). Así no tocamos el delegado de winit.
//!
//! R1.5: el delegado de `UNUserNotificationCenter` ([`NotificationDelegate`]) se pone en
//! [`install`], antes de que la app termine de arrancar, como pide Apple para recibir también el
//! clic que lanzó la app en frío. El delegado de winit tampoco se toca aquí.

use std::cell::OnceCell;

use block2::DynBlock;
use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AllocAnyThread, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_foundation::{
    NSAppleEventDescriptor, NSAppleEventManager, NSBundle, NSDictionary, NSNotification, NSNotificationCenter, NSString,
};
use objc2_user_notifications::{
    UNMutableNotificationContent, UNNotification, UNNotificationPresentationOptions, UNNotificationResponse,
    UNUserNotificationCenter, UNUserNotificationCenterDelegate,
};

use crate::{INBOX, MAX_PENDING, MacEventsError, NOTIFICATION_LINK_KEY, deliver_click, notification_link};

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

define_class!(
    // SAFETY:
    // - NSObject no tiene requisitos para subclases.
    // - `NotificationDelegate` no implementa `Drop` ni tiene variables de instancia, y solo toca la
    //   bandeja (segura entre hilos), así que puede vivir en cualquier hilo.
    #[unsafe(super(NSObject))]
    #[name = "IureDeskNotificationDelegate"]
    struct NotificationDelegate;

    // SAFETY: `NSObjectProtocol` no pide nada más a una subclase de NSObject.
    unsafe impl NSObjectProtocol for NotificationDelegate {}

    // SAFETY: los dos métodos llevan el selector y la firma que declara el protocolo en
    // objc2-user-notifications; los demás son opcionales.
    unsafe impl UNUserNotificationCenterDelegate for NotificationDelegate {
        // SAFETY: `- (void)userNotificationCenter:(UNUserNotificationCenter *)center
        // willPresentNotification:(UNNotification *)notification
        // withCompletionHandler:(void (^)(UNNotificationPresentationOptions))completionHandler`.
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn will_present(
            &self,
            _center: &UNUserNotificationCenter,
            _notification: &UNNotification,
            completion: &DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            // Con IureDesk al frente el sistema calla los avisos salvo que se pida mostrarlos.
            completion.call((UNNotificationPresentationOptions::Banner
                | UNNotificationPresentationOptions::List
                | UNNotificationPresentationOptions::Sound,));
        }

        // SAFETY: `- (void)userNotificationCenter:(UNUserNotificationCenter *)center
        // didReceiveNotificationResponse:(UNNotificationResponse *)response
        // withCompletionHandler:(void (^)(void))completionHandler`.
        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn did_receive(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            completion: &DynBlock<dyn Fn()>,
        ) {
            let action = response.actionIdentifier().to_string();
            let link = clicked_link(response);
            let _ = deliver_click(&INBOX, &action, link.as_deref());
            // Apple exige llamarlo siempre, y una sola vez.
            completion.call(());
        }
    }
);

impl NotificationDelegate {
    fn new() -> Retained<Self> {
        let this = Self::alloc().set_ivars(());
        // SAFETY: `init` de NSObject sobre un objeto recién reservado con sus (cero) variables
        // de instancia ya puestas; devuelve el objeto inicializado y retenido.
        unsafe { msg_send![super(this), init] }
    }
}

/// Texto guardado bajo [`NOTIFICATION_LINK_KEY`] en el `userInfo` del aviso (lo valida
/// [`deliver_click`]); cualquier otro tipo de valor se ignora.
fn clicked_link(response: &UNNotificationResponse) -> Option<String> {
    let info = response.notification().request().content().userInfo();
    let key = NSString::from_str(NOTIFICATION_LINK_KEY);
    let value = info.objectForKey(&key)?.downcast::<NSString>().ok()?;
    Some(value.to_string())
}

/// Guarda en el `userInfo` del aviso el enlace que abre su clic (si [`notification_link`] lo
/// acepta; si no, no hace nada y el clic solo trae la app al frente).
pub fn set_notification_link(content: &UNMutableNotificationContent, link: &str) {
    let Some(link) = notification_link(link) else {
        return;
    };
    let key = NSString::from_str(NOTIFICATION_LINK_KEY);
    let value = NSString::from_str(&link);
    let info = NSDictionary::<NSString, NSString>::from_slices(&[&*key], &[&*value]);
    // SAFETY: `userInfo` debe ser un diccionario de objetos de lista de propiedades: aquí es
    // `NSString → NSString`. Reinterpretar los genéricos como `AnyObject` (lo que pide la firma)
    // es válido porque todo objeto de Objective-C lo es; el contenido copia el diccionario.
    unsafe {
        content.setUserInfo(info.cast_unchecked());
    }
}

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
    /// `UNUserNotificationCenter.delegate` es débil: el delegado vive aquí hasta el final del
    /// proceso (hilo principal, que no termina antes que la app).
    static NOTIFICATION_DELEGATE: OnceCell<Retained<NotificationDelegate>> = const { OnceCell::new() };
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
    install_notification_delegate();
    Ok(())
}

/// Pone el delegado de notificaciones (una vez). Fuera de un `.app` (p. ej. `cargo run`) el centro
/// de notificaciones aborta el proceso, así que entonces no se toca.
fn install_notification_delegate() {
    if NSBundle::mainBundle().bundleIdentifier().is_none() {
        return;
    }
    NOTIFICATION_DELEGATE.with(|cell| {
        if cell.get().is_some() {
            return;
        }
        let delegate = cell.get_or_init(NotificationDelegate::new);
        UNUserNotificationCenter::currentNotificationCenter().setDelegate(Some(ProtocolObject::from_ref(&**delegate)));
    });
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
