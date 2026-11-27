//! Instalación desde Microsoft Store (paquete MSIX, `packaging/msix/AppxManifest.xml`).
//!
//! Empaquetada, IureDesk:
//! - no se actualiza ni avisa de versiones nuevas (las da la Store; política 10.2.2), ni ofrece
//!   instaladores de GitHub para las apps hermanas (las busca en la Store);
//! - no escribe en el registro para declararse cliente de correo, protocolo `iuredesk:` o tipos
//!   de archivo, ni el AppUserModelID de los avisos: todo eso lo declara el manifiesto;
//! - arranca con la sesión por la StartupTask [`STARTUP_TASK`] (la clave `Run` de `HKCU` no
//!   sirve dentro de un paquete), ver [`crate::autostart`];
//! - se registra en Claude Desktop y VS Code con el alias de ejecución
//!   `%LOCALAPPDATA%\Microsoft\WindowsApps\iuredesk.exe` ([`execution_alias`]): la carpeta del
//!   paquete cambia con cada versión y, lanzado por su ruta, el ejecutable corre sin la
//!   identidad del paquete (y no vería el almacén, que está en el `AppData` del paquete).
//!
//! Se detecta con `Windows.ApplicationModel.Package.Current` (WinRT, sin `unsafe`): falla si el
//! proceso no tiene identidad de paquete.

use std::path::PathBuf;

/// Nombre del alias de ejecución que declara el manifiesto (`windows.appExecutionAlias`).
pub const ALIAS: &str = "iuredesk.exe";

/// `TaskId` de la StartupTask del manifiesto (`windows.startupTask`).
pub const STARTUP_TASK: &str = "IureDeskInicio";

/// ¿Corre instalada como paquete MSIX (Microsoft Store, o en las pruebas con
/// `Add-AppxPackage`)? Fuera de Windows siempre es `false`.
pub fn packaged() -> bool {
    #[cfg(windows)]
    {
        use std::sync::OnceLock;
        static PACKAGED: OnceLock<bool> = OnceLock::new();
        *PACKAGED.get_or_init(detect)
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(windows)]
fn detect() -> bool {
    if windows::ApplicationModel::Package::Current().is_ok() {
        return true;
    }
    // Lanzado por su ruta dentro de WindowsApps (sin identidad de paquete): también es la copia
    // de la Store y no debe actualizarse sola.
    std::env::current_exe().is_ok_and(|p| in_windows_apps(&p.to_string_lossy()))
}

/// ¿Está la ruta dentro de una carpeta `WindowsApps` (paquetes instalados)?
pub fn in_windows_apps(path: &str) -> bool {
    path.to_ascii_lowercase().replace('/', "\\").contains("\\windowsapps\\")
}

/// Ruta estable con que otros programas (Claude Desktop, VS Code) lanzan IureDesk empaquetada:
/// el alias de ejecución. `None` fuera del paquete o si el alias no existe.
pub fn execution_alias() -> Option<PathBuf> {
    if !packaged() {
        return None;
    }
    let alias = dirs::data_local_dir()?.join("Microsoft").join("WindowsApps").join(ALIAS);
    alias.exists().then_some(alias)
}

/// El alias de ejecución de otra app de la suite instalada desde la Store
/// (`%LOCALAPPDATA%\Microsoft\WindowsApps\<exe>`), si existe. Lanzada por el alias corre con su
/// propia identidad de paquete, no dentro del contenedor de IureDesk. Solo en Windows.
pub fn sister_alias(exe: &str) -> Option<PathBuf> {
    if !cfg!(windows) {
        return None;
    }
    let alias = dirs::data_local_dir()?.join("Microsoft").join("WindowsApps").join(exe);
    alias.exists().then_some(alias)
}

/// Enlace de la Store que busca una app por su nombre (para invitar a instalar una app hermana
/// sin remitir a un instalador de fuera de la Store).
pub fn store_search_url(name: &str) -> String {
    let q: String = name.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    format!("https://apps.microsoft.com/search?query={q}")
}

/// ¿Abrió Windows la app con la StartupTask del paquete al iniciar la sesión?
pub fn launched_by_startup_task() -> bool {
    #[cfg(windows)]
    {
        use windows::ApplicationModel::Activation::ActivationKind;
        use windows::ApplicationModel::AppInstance;
        packaged()
            && AppInstance::GetActivatedEventArgs()
                .and_then(|a| a.Kind())
                .is_ok_and(|k| k == ActivationKind::StartupTask)
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ruta_de_windows_apps() {
        assert!(in_windows_apps(r"C:\Program Files\WindowsApps\LaloAbueliux.IureDesk_1.1.312.0_x64__abc\iuredesk.exe"));
        assert!(in_windows_apps(r"c:/program files/windowsapps/x/iuredesk.exe"));
        assert!(!in_windows_apps(r"C:\Users\Ana\AppData\Local\IureDesk\iuredesk.exe"));
        assert!(!in_windows_apps("/usr/bin/iuredesk"));
    }

    #[test]
    fn fuera_del_paquete() {
        // Las pruebas nunca corren empaquetadas.
        assert!(!packaged());
        assert!(execution_alias().is_none());
        assert!(!launched_by_startup_task());
    }

    #[test]
    fn busqueda_en_la_store() {
        assert_eq!(store_search_url("IureTranscribe"), "https://apps.microsoft.com/search?query=IureTranscribe");
        assert_eq!(store_search_url("Iure OCR&x=1"), "https://apps.microsoft.com/search?query=IureOCRx1");
    }
}
