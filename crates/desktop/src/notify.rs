//! Notificaciones del sistema con clic que abre el elemento (un enlace `iuredesk://`).
//!
//! - **Linux**: D-Bus (`org.freedesktop.Notifications`) con `notify-rust` sobre zbus. La acción
//!   `default` (clic en la notificación) llama a `on_click` con el enlace; se espera en un hilo
//!   por notificación, como mucho [`MAX_WAITING`] a la vez.
//! - **Windows**: aviso de WinRT (`ToastNotificationManager::CreateToastNotifierWithId`) con el
//!   `AppUserModelID` [`crate::APP_ID`]. El clic se activa como protocolo: Windows abre
//!   `iuredesk://…`, que lanza otro `iuredesk.exe` y este se lo pasa a la ventana abierta por el
//!   puente (una sola instancia). Para eso el instalador registra el protocolo `iuredesk` y el
//!   acceso directo lleva el mismo AUMID; además, al arrancar se registra el AUMID en
//!   `HKCU\Software\Classes\AppUserModelId\…` para que los avisos salgan aun sin acceso directo.
//! - **macOS**: `UNUserNotificationCenter` (objc2), solo si la app corre dentro de su `.app`
//!   (fuera de un paquete el sistema lo rechaza). El clic trae IureDesk al frente.
//! - Otros: `None` (solo los avisos dentro de la ventana).

use std::sync::Arc;
#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicUsize, Ordering};

/// Muestra una notificación: (título, texto, enlace que abre el clic).
pub type Notify = Arc<dyn Fn(&str, &str, Option<&str>) + Send + Sync>;
/// Clic en una notificación: el enlace (`None` = solo traer la ventana al frente).
pub type OnClick = Arc<dyn Fn(Option<String>) + Send + Sync>;

/// Notificaciones esperando el clic a la vez (Linux).
pub const MAX_WAITING: usize = 16;
/// Tope del texto de una notificación.
const MAX_TEXT: usize = 1_000;
/// Enlace del clic cuando no hay elemento (solo trae la ventana).
pub const FOCUS_LINK: &str = "iuredesk://open";

fn clip(s: &str) -> String {
    s.chars().take(MAX_TEXT).collect()
}

/// XML del aviso de Windows (se prueba en todos los sistemas).
pub fn toast_xml(title: &str, body: &str, link: Option<&str>) -> String {
    fn esc(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for ch in s.chars() {
            match ch {
                '&' => out.push_str("&amp;"),
                '<' => out.push_str("&lt;"),
                '>' => out.push_str("&gt;"),
                '"' => out.push_str("&quot;"),
                '\'' => out.push_str("&apos;"),
                '\n' | '\t' => out.push(' '),
                c if c.is_control() => {}
                c => out.push(c),
            }
        }
        out
    }
    let launch = link.filter(|l| l.to_ascii_lowercase().starts_with("iuredesk:")).unwrap_or(FOCUS_LINK);
    format!(
        "<toast activationType=\"protocol\" launch=\"{}\"><visual><binding template=\"ToastGeneric\">\
         <text>{}</text><text>{}</text></binding></visual></toast>",
        esc(launch),
        esc(&clip(title)),
        esc(&clip(body))
    )
}

/// Notificaciones del sistema, si este las tiene. `open_label` es el texto del botón «Abrir»
/// (Linux).
#[allow(unused_variables)]
pub fn system_notifier(open_label: &str, on_click: OnClick) -> Option<Notify> {
    #[cfg(target_os = "linux")]
    {
        Some(linux(open_label.to_string(), on_click))
    }
    #[cfg(windows)]
    {
        windows_register_aumid();
        Some(Arc::new(|title: &str, body: &str, link: Option<&str>| {
            let xml = toast_xml(title, body, link);
            let _ = std::thread::Builder::new().name("iuredesk-notify".into()).spawn(move || {
                let _ = windows_show(&xml);
            });
        }))
    }
    #[cfg(target_os = "macos")]
    {
        macos::notifier()
    }
    #[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
    {
        None
    }
}

#[cfg(target_os = "linux")]
fn linux(open_label: String, on_click: OnClick) -> Notify {
    let waiting = Arc::new(AtomicUsize::new(0));
    Arc::new(move |title: &str, body: &str, link: Option<&str>| {
        let (t, b, l) = (clip(title), clip(body), link.map(str::to_string));
        let (cb, w, label) = (Arc::clone(&on_click), Arc::clone(&waiting), open_label.clone());
        // `show` habla con el bus de sesión: en su propio hilo para no frenar la interfaz.
        let _ = std::thread::Builder::new().name("iuredesk-notify".into()).spawn(move || {
            let mut n = notify_rust::Notification::new();
            n.appname("IureDesk")
                .summary(&t)
                .body(&b)
                .icon("iuredesk")
                .hint(notify_rust::Hint::DesktopEntry("iuredesk".into()))
                .action("default", &label);
            let Ok(handle) = n.show() else { return };
            if w.fetch_add(1, Ordering::SeqCst) < MAX_WAITING {
                handle.wait_for_action(|action| {
                    if action == "default" {
                        cb(l);
                    }
                });
            }
            w.fetch_sub(1, Ordering::SeqCst);
        });
    })
}

/// Registra el AUMID para el usuario (nombre visible del aviso). Sin esto, y sin acceso directo
/// con el AUMID, Windows no muestra los avisos de una app sin paquete.
#[cfg(windows)]
fn windows_register_aumid() {
    let path = format!(r"Software\Classes\AppUserModelId\{}", crate::APP_ID);
    if let Ok(key) = windows_registry::CURRENT_USER.create(&path) {
        let _ = key.set_string("DisplayName", "IureDesk");
        if let Some(icon) = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join("iuredesk.ico")))
            && icon.is_file()
        {
            let _ = key.set_string("IconUri", icon.to_string_lossy());
        }
    }
}

#[cfg(windows)]
fn windows_show(xml: &str) -> windows::core::Result<()> {
    use windows::Data::Xml::Dom::XmlDocument;
    use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};
    use windows::core::HSTRING;
    let doc = XmlDocument::new()?;
    doc.LoadXml(&HSTRING::from(xml))?;
    let toast = ToastNotification::CreateToastNotification(&doc)?;
    ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(crate::APP_ID))?.Show(&toast)
}

#[cfg(target_os = "macos")]
mod macos {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2_foundation::{NSBundle, NSError, NSString};
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNMutableNotificationContent, UNNotificationRequest, UNUserNotificationCenter,
    };

    use super::{Notify, clip};

    pub(super) fn notifier() -> Option<Notify> {
        // Fuera de un `.app` (p. ej. `cargo run`) el centro de notificaciones aborta el proceso.
        NSBundle::mainBundle().bundleIdentifier()?;
        let center = UNUserNotificationCenter::currentNotificationCenter();
        let done = RcBlock::new(|_granted: Bool, _err: *mut NSError| {});
        center.requestAuthorizationWithOptions_completionHandler(
            UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
            &done,
        );
        let seq = Arc::new(AtomicU64::new(0));
        Some(Arc::new(move |title: &str, body: &str, _link: Option<&str>| {
            let content = UNMutableNotificationContent::new();
            content.setTitle(&NSString::from_str(&clip(title)));
            content.setBody(&NSString::from_str(&clip(body)));
            let n = seq.fetch_add(1, Ordering::Relaxed);
            let id = NSString::from_str(&format!("iuredesk-{}-{n}", std::process::id()));
            let request = UNNotificationRequest::requestWithIdentifier_content_trigger(&id, &content, None);
            UNUserNotificationCenter::currentNotificationCenter()
                .addNotificationRequest_withCompletionHandler(&request, None);
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aviso_de_windows() {
        let x = toast_xml("Correo <nuevo>", "Ana & Luis — \"Hola\"\nmás", Some("iuredesk://message/a@b"));
        assert!(x.starts_with("<toast activationType=\"protocol\" launch=\"iuredesk://message/a@b\">"));
        assert!(x.contains("<text>Correo &lt;nuevo&gt;</text>"));
        assert!(x.contains("<text>Ana &amp; Luis — &quot;Hola&quot; más</text>"));
        // Un enlace que no es nuestro no se lanza.
        let x = toast_xml("t", "b", Some("https://malo.example"));
        assert!(x.contains(&format!("launch=\"{FOCUS_LINK}\"")));
        let x = toast_xml(&"a".repeat(5_000), "", None);
        assert!(x.len() < 1_400);
    }
}
