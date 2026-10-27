//! Qué apps de la suite hay en este equipo y dónde.
//!
//! La detección combina el catálogo de `iurefficient-connect` (`apps::locate`, que ya conoce
//! IureEditor, IureTranscribe, IureDav e IureOCR) con una búsqueda propia para IureVoz e
//! IureProject (aún no están en el catálogo de connect) en las mismas ubicaciones: `PATH`,
//! `/usr/bin`, `/usr/local/bin`, `~/.local/bin`, `/opt/<Producto>` en Linux; `/Applications` y
//! `~/Applications` en macOS; `%LOCALAPPDATA%\Programs`, `%ProgramFiles%` en Windows.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use iurefficient_connect::apps::{self as capps, AppId};

use crate::dav::DavMount;
use crate::mcp_client::{McpClient, McpOptions};
use crate::opener::{Opener, SystemOpener};

/// Texto en los dos idiomas de la interfaz.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Text {
    /// Inglés.
    pub en: String,
    /// Español.
    pub es: String,
}

impl Text {
    /// Texto bilingüe.
    pub fn new(en: impl Into<String>, es: impl Into<String>) -> Self {
        Self { en: en.into(), es: es.into() }
    }

    /// El del idioma pedido.
    pub fn get(&self, spanish: bool) -> &str {
        if spanish { &self.es } else { &self.en }
    }
}

/// Una app hermana.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SisterApp {
    /// IureEditor (Markdown, PDF, DOCX).
    Editor,
    /// IureTranscribe (transcripción local, minutas).
    Transcribe,
    /// IureVoz (voz y dictado).
    Voz,
    /// IureOCR (PDF buscables).
    Ocr,
    /// IureDav (unidad WebDAV de Iurefficient).
    Dav,
    /// IureProject (planes de proyecto).
    Project,
}

/// Nombres de binario por sistema: (Linux, producto/carpeta y `.app`, `.exe` de Windows).
struct Bins {
    linux: &'static str,
    product: &'static str,
    windows_exe: &'static str,
}

impl SisterApp {
    /// Todas, en el orden en que se muestran.
    pub const ALL: [SisterApp; 6] =
        [SisterApp::Editor, SisterApp::Transcribe, SisterApp::Voz, SisterApp::Ocr, SisterApp::Dav, SisterApp::Project];

    /// Nombre visible.
    pub fn name(self) -> &'static str {
        match self {
            Self::Editor => "IureEditor",
            Self::Transcribe => "IureTranscribe",
            Self::Voz => "IureVoz",
            Self::Ocr => "IureOCR",
            Self::Dav => "IureDav",
            Self::Project => "IureProject",
        }
    }

    /// Para qué sirve, en los dos idiomas.
    pub fn description(self) -> Text {
        match self {
            Self::Editor => Text::new(
                "Edit long notes and documents in Markdown, export to PDF and DOCX.",
                "Edita notas y documentos largos en Markdown y exporta a PDF y DOCX.",
            ),
            Self::Transcribe => Text::new(
                "Record meetings and transcribe them on this computer.",
                "Graba reuniones y las transcribe en este equipo.",
            ),
            Self::Voz => {
                Text::new("Dictate emails and hear them read aloud.", "Dicta correos y escúchalos en voz alta.")
            }
            Self::Ocr => Text::new(
                "Turn scanned attachments into searchable PDFs.",
                "Convierte adjuntos escaneados en PDF buscables.",
            ),
            Self::Dav => Text::new(
                "Save attachments to your Iurefficient drive and share large files as links.",
                "Guarda adjuntos en tu unidad de Iurefficient y comparte archivos grandes como enlace.",
            ),
            Self::Project => Text::new(
                "Turn emails into project tasks and see milestones.",
                "Convierte correos en tareas del plan y ve los hitos.",
            ),
        }
    }

    /// Repositorio de GitHub (`dueño/repo`).
    pub fn repo(self) -> &'static str {
        match self {
            Self::Editor => "ellaguno/iureditor",
            Self::Transcribe => "ellaguno/iuretranscribe",
            Self::Voz => "ellaguno/iurevoz",
            Self::Ocr => "ellaguno/iureocr",
            Self::Dav => "ellaguno/iuredav",
            Self::Project => "ellaguno/iureproject",
        }
    }

    /// Página de descarga (última versión publicada).
    pub fn download_url(self) -> String {
        format!("https://github.com/{}/releases/latest", self.repo())
    }

    /// Esquema de deep link, si la app tiene.
    pub fn scheme(self) -> Option<&'static str> {
        match self {
            Self::Editor => Some("iureditor"),
            Self::Transcribe => Some("iuretranscribe"),
            Self::Ocr => Some("iureocr"),
            Self::Dav => Some("iuredav"),
            Self::Voz | Self::Project => None,
        }
    }

    /// La app en el catálogo de connect, si está.
    fn connect_id(self) -> Option<AppId> {
        match self {
            Self::Editor => Some(AppId::Editor),
            Self::Transcribe => Some(AppId::Transcribe),
            Self::Ocr => Some(AppId::Ocr),
            Self::Dav => Some(AppId::Dav),
            Self::Voz | Self::Project => None,
        }
    }

    fn bins(self) -> Bins {
        match self {
            Self::Editor => Bins { linux: "iureditor", product: "iureditor", windows_exe: "iureditor" },
            Self::Transcribe => {
                Bins { linux: "iuretranscribe", product: "IureTranscribe", windows_exe: "IureTranscribe" }
            }
            Self::Voz => Bins { linux: "iurevoz", product: "IureVoz", windows_exe: "iurevoz" },
            Self::Ocr => Bins { linux: "iureocr", product: "IureOCR", windows_exe: "iureocr" },
            Self::Dav => Bins { linux: "iuredav-app", product: "IureDav", windows_exe: "iuredav-app" },
            Self::Project => Bins { linux: "iureproject", product: "IureProject", windows_exe: "iureproject" },
        }
    }

    /// Texto para invitar a instalarla.
    pub fn install_hint(self) -> InstallHint {
        let name = self.name();
        let d = self.description();
        InstallHint {
            app: self,
            text: Text::new(
                format!("{} is not installed. {} Install it for free.", name, d.en),
                format!("{} no está instalada. {} Instálala gratis.", name, d.es),
            ),
            download_url: self.download_url(),
        }
    }
}

/// Invitación discreta a instalar una app que falta.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallHint {
    /// La app.
    pub app: SisterApp,
    /// Qué decir (es/en).
    pub text: Text,
    /// Dónde descargarla.
    pub download_url: String,
}

/// Resultado de una acción con una app hermana.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome<T> {
    /// Hecho.
    Ready(T),
    /// La app no está instalada: se ofrece instalarla (no es un error).
    NotInstalled(InstallHint),
    /// Instalada pero no disponible ahora (no está abierta, sin unidad montada, sistema no
    /// compatible…). El texto explica qué hacer.
    NotAvailable(Text),
}

impl<T> Outcome<T> {
    /// `Some` si se hizo.
    pub fn ready(self) -> Option<T> {
        match self {
            Self::Ready(t) => Some(t),
            _ => None,
        }
    }

    /// `true` si se hizo.
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready(_))
    }

    /// Transforma el valor si se hizo.
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Outcome<U> {
        match self {
            Self::Ready(t) => Outcome::Ready(f(t)),
            Self::NotInstalled(h) => Outcome::NotInstalled(h),
            Self::NotAvailable(t) => Outcome::NotAvailable(t),
        }
    }
}

/// Estado de una app para el panel «Apps de Iurefficient».
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppStatus {
    /// La app.
    pub app: SisterApp,
    /// Dónde está, si está.
    pub path: Option<PathBuf>,
    /// Invitación a instalarla (si falta).
    pub hint: Option<InstallHint>,
}

type MountSource = Arc<dyn Fn() -> Vec<DavMount> + Send + Sync>;

/// Las apps hermanas de este equipo y cómo hablarles.
#[derive(Clone)]
pub struct Suite {
    paths: HashMap<SisterApp, PathBuf>,
    pub(crate) opener: Arc<dyn Opener>,
    pub(crate) voz_socket: Option<PathBuf>,
    pub(crate) mounts: MountSource,
    pub(crate) cache_dir: PathBuf,
    pub(crate) mcp: McpOptions,
}

impl std::fmt::Debug for Suite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Suite")
            .field("paths", &self.paths)
            .field("voz_socket", &self.voz_socket)
            .finish_non_exhaustive()
    }
}

impl Suite {
    /// Detecta las apps instaladas (sin red; unos `stat` del disco).
    pub fn detect() -> Self {
        let mut s = Self::empty();
        for app in SisterApp::ALL {
            if let Some(p) = locate(app) {
                s.paths.insert(app, p);
            }
        }
        s.voz_socket = default_voz_socket();
        s.mounts = Arc::new(capps::iuredav_mounts);
        s
    }

    /// Ninguna app instalada, sin socket de voz ni unidades (base para pruebas o para desactivar
    /// las integraciones).
    pub fn empty() -> Self {
        Self {
            paths: HashMap::new(),
            opener: Arc::new(SystemOpener),
            voz_socket: None,
            mounts: Arc::new(Vec::new),
            cache_dir: dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("iuredesk").join("suite"),
            mcp: McpOptions::default(),
        }
    }

    /// Fija la ruta de una app (pruebas, o ajuste manual del usuario).
    pub fn with_app(mut self, app: SisterApp, path: impl Into<PathBuf>) -> Self {
        self.paths.insert(app, path.into());
        self
    }

    /// Cambia quién abre enlaces y lanza programas.
    pub fn with_opener(mut self, opener: Arc<dyn Opener>) -> Self {
        self.opener = opener;
        self
    }

    /// Cambia la ruta del socket de control de IureVoz.
    pub fn with_voz_socket(mut self, path: Option<PathBuf>) -> Self {
        self.voz_socket = path;
        self
    }

    /// Cambia de dónde salen las unidades de IureDav.
    pub fn with_mounts(mut self, f: impl Fn() -> Vec<DavMount> + Send + Sync + 'static) -> Self {
        self.mounts = Arc::new(f);
        self
    }

    /// Carpeta para archivos temporales de las integraciones (documentos para IureEditor…).
    pub fn with_cache_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cache_dir = dir.into();
        self
    }

    /// Cambia las opciones del cliente MCP (tiempos, entorno del hijo).
    pub fn with_mcp_options(mut self, opts: McpOptions) -> Self {
        self.mcp = opts;
        self
    }

    /// Lanza `<app> --mcp` y negocia. Para herramientas que este crate no envuelve (p. ej.
    /// `list_recordings` de IureTranscribe o `pdf_info` de IureOCR).
    pub fn connect_mcp(&self, app: SisterApp) -> crate::Result<Outcome<McpClient>> {
        let Some(program) = self.mcp_program(app) else {
            return Ok(Outcome::NotInstalled(app.install_hint()));
        };
        match McpClient::spawn(&program, &["--mcp"], &self.mcp) {
            Ok(c) => Ok(Outcome::Ready(c)),
            // El ejecutable desapareció o no arranca: no es culpa del usuario.
            Err(crate::IntegrationError::Io(_)) => Ok(Outcome::NotAvailable(Text::new(
                format!("{} could not be started. Reinstalling it may help.", app.name()),
                format!("No se pudo iniciar {}. Reinstalarla puede ayudar.", app.name()),
            ))),
            Err(e) => Err(e),
        }
    }

    /// ¿Está instalada?
    pub fn installed(&self, app: SisterApp) -> bool {
        self.paths.contains_key(&app)
    }

    /// Ruta del ejecutable (o del `.app` en macOS).
    pub fn path(&self, app: SisterApp) -> Option<&Path> {
        self.paths.get(&app).map(PathBuf::as_path)
    }

    /// Estado de todas, para el panel de apps.
    pub fn apps(&self) -> Vec<AppStatus> {
        SisterApp::ALL
            .iter()
            .map(|&app| {
                let path = self.paths.get(&app).cloned();
                let hint = path.is_none().then(|| app.install_hint());
                AppStatus { app, path, hint }
            })
            .collect()
    }

    /// El ejecutable que habla MCP por stdio (dentro del `.app` en macOS).
    pub(crate) fn mcp_program(&self, app: SisterApp) -> Option<PathBuf> {
        let p = self.paths.get(&app)?;
        Some(bundle_executable(p, app.bins().product))
    }

    /// Abre un archivo `.iurp` (o cualquiera que entienda) con IureProject.
    pub fn open_in_iureproject(&self, file: &Path) -> crate::Result<Outcome<()>> {
        let Some(program) = self.paths.get(&SisterApp::Project) else {
            return Ok(Outcome::NotInstalled(SisterApp::Project.install_hint()));
        };
        let file = absolute(file)?;
        self.opener.launch(program, &[file.to_string_lossy().into_owned()])?;
        Ok(Outcome::Ready(()))
    }

    /// Lanza una app sin argumentos (botón «Abrir» del panel).
    pub fn launch(&self, app: SisterApp) -> crate::Result<Outcome<()>> {
        let Some(program) = self.paths.get(&app) else {
            return Ok(Outcome::NotInstalled(app.install_hint()));
        };
        self.opener.launch(program, &[])?;
        Ok(Outcome::Ready(()))
    }
}

/// Ruta absoluta y existente (las apps hermanas exigen rutas absolutas).
pub(crate) fn absolute(p: &Path) -> crate::Result<PathBuf> {
    p.canonicalize().map_err(|e| crate::IntegrationError::InvalidInput(format!("{}: {e}", p.display())))
}

/// En macOS, `X.app` → `X.app/Contents/MacOS/<producto>` (o el primer ejecutable que haya).
fn bundle_executable(p: &Path, product: &str) -> PathBuf {
    if !p.extension().is_some_and(|e| e.eq_ignore_ascii_case("app")) {
        return p.to_path_buf();
    }
    let macos = p.join("Contents").join("MacOS");
    let named = macos.join(product);
    if named.is_file() {
        return named;
    }
    std::fs::read_dir(&macos).ok().and_then(|d| d.flatten().map(|e| e.path()).find(|x| x.is_file())).unwrap_or(named)
}

fn which(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(bin)).find(|p| p.is_file())
}

/// Busca una app: primero el catálogo de connect, luego las ubicaciones habituales.
fn locate(app: SisterApp) -> Option<PathBuf> {
    if let Some(id) = app.connect_id()
        && let Some(p) = capps::locate(capps::APPS.iter().find(|d| d.id == id)?)
    {
        return Some(p);
    }
    let b = app.bins();
    let mut candidates: Vec<PathBuf> = Vec::new();
    if cfg!(target_os = "macos") {
        candidates.push(PathBuf::from("/Applications").join(format!("{}.app", b.product)));
        if let Some(h) = dirs::home_dir() {
            candidates.push(h.join("Applications").join(format!("{}.app", b.product)));
        }
        candidates.extend(which(b.linux));
    } else if cfg!(target_os = "windows") {
        let exe = format!("{}.exe", b.windows_exe);
        for var in ["LOCALAPPDATA", "ProgramFiles", "ProgramFiles(x86)"] {
            if let Ok(base) = std::env::var(var) {
                let base = PathBuf::from(base);
                candidates.push(base.join("Programs").join(b.product).join(&exe));
                candidates.push(base.join(b.product).join(&exe));
            }
        }
        candidates.extend(which(&exe));
    } else {
        candidates.extend(which(b.linux));
        candidates.push(PathBuf::from("/usr/bin").join(b.linux));
        candidates.push(PathBuf::from("/usr/local/bin").join(b.linux));
        if let Some(h) = dirs::home_dir() {
            candidates.push(h.join(".local/bin").join(b.linux));
        }
        candidates.push(PathBuf::from("/opt").join(b.product).join(b.linux));
    }
    candidates.into_iter().find(|p| p.exists())
}

/// Socket de control de IureVoz: `$XDG_RUNTIME_DIR/iurevoz.sock` (o el temporal del usuario).
fn default_voz_socket() -> Option<PathBuf> {
    Some(dirs::runtime_dir().unwrap_or_else(std::env::temp_dir).join("iurevoz.sock"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogo_completo_y_enlaces_de_descarga() {
        for app in SisterApp::ALL {
            assert!(!app.name().is_empty());
            assert!(app.download_url().starts_with("https://github.com/ellaguno/"));
            let h = app.install_hint();
            assert!(h.text.es.contains(app.name()) && h.text.en.contains(app.name()));
        }
        assert_eq!(SisterApp::Editor.scheme(), Some("iureditor"));
    }

    #[test]
    fn suite_vacia_no_tiene_nada() {
        let s = Suite::empty();
        assert!(SisterApp::ALL.iter().all(|a| !s.installed(*a)));
        assert!(s.apps().iter().all(|a| a.hint.is_some()));
        let s = s.with_app(SisterApp::Ocr, "/x/iureocr");
        assert!(s.installed(SisterApp::Ocr));
        assert_eq!(s.apps().iter().filter(|a| a.hint.is_none()).count(), 1);
    }

    #[test]
    fn detectar_no_falla() {
        // En la máquina de pruebas puede haber o no apps; solo debe responder sin fallar.
        let s = Suite::detect();
        assert_eq!(s.apps().len(), SisterApp::ALL.len());
    }

    #[test]
    fn ejecutable_dentro_del_bundle() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let app = dir.path().join("IureOCR.app");
        let macos = app.join("Contents").join("MacOS");
        std::fs::create_dir_all(&macos).unwrap_or_else(|e| panic!("{e}"));
        std::fs::write(macos.join("iureocr"), b"").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(bundle_executable(&app, "IureOCR"), macos.join("iureocr"));
        assert_eq!(bundle_executable(Path::new("/usr/bin/iureocr"), "IureOCR"), PathBuf::from("/usr/bin/iureocr"));
    }

    #[test]
    fn iureproject_no_instalado() {
        let r = Suite::empty().open_in_iureproject(Path::new("/tmp/x.iurp"));
        assert!(matches!(r, Ok(Outcome::NotInstalled(h)) if h.app == SisterApp::Project));
    }
}
