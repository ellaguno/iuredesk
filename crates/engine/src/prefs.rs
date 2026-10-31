//! Preferencias de la interfaz (tema, densidad, atajos, conversaciones), guardadas como JSON en la
//! carpeta de configuración (`~/.config/iuredesk/prefs.json`). Un archivo dañado o de otra versión
//! nunca impide arrancar: se usan los valores por omisión.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Tamaño máximo del archivo de preferencias.
const MAX_PREFS_BYTES: u64 = 256 * 1024;

/// Tema elegido.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemePref {
    /// Sigue al escritorio (portal XDG en Linux).
    #[default]
    System,
    Light,
    Dark,
    HighContrast,
}

impl ThemePref {
    pub const ALL: [ThemePref; 4] = [ThemePref::System, ThemePref::Light, ThemePref::Dark, ThemePref::HighContrast];

    pub fn key(self) -> &'static str {
        match self {
            ThemePref::System => "system",
            ThemePref::Light => "light",
            ThemePref::Dark => "dark",
            ThemePref::HighContrast => "high_contrast",
        }
    }

    pub fn from_key(s: &str) -> Option<ThemePref> {
        ThemePref::ALL.into_iter().find(|t| t.key() == s)
    }
}

/// Densidad de listas y controles.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DensityPref {
    Comfortable,
    #[default]
    Compact,
    /// Filas de 22 px, una línea, columnas (más denso que Outlook).
    Dense,
}

impl DensityPref {
    pub const ALL: [DensityPref; 3] = [DensityPref::Comfortable, DensityPref::Compact, DensityPref::Dense];

    pub fn key(self) -> &'static str {
        match self {
            DensityPref::Comfortable => "comfortable",
            DensityPref::Compact => "compact",
            DensityPref::Dense => "dense",
        }
    }

    pub fn from_key(s: &str) -> Option<DensityPref> {
        DensityPref::ALL.into_iter().find(|d| d.key() == s)
    }
}

/// Esquema de atajos de teclado, para que nadie tenga que reaprender (PLAN.md §6).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShortcutScheme {
    #[default]
    Outlook,
    Gmail,
    AppleMail,
}

impl ShortcutScheme {
    pub const ALL: [ShortcutScheme; 3] = [ShortcutScheme::Outlook, ShortcutScheme::Gmail, ShortcutScheme::AppleMail];

    pub fn key(self) -> &'static str {
        match self {
            ShortcutScheme::Outlook => "outlook",
            ShortcutScheme::Gmail => "gmail",
            ShortcutScheme::AppleMail => "apple_mail",
        }
    }

    pub fn from_key(s: &str) -> Option<ShortcutScheme> {
        ShortcutScheme::ALL.into_iter().find(|d| d.key() == s)
    }

    /// Nombre para mostrar (marcas de terceros solo como texto descriptivo).
    pub fn label(self) -> &'static str {
        match self {
            ShortcutScheme::Outlook => "Outlook",
            ShortcutScheme::Gmail => "Gmail",
            ShortcutScheme::AppleMail => "Apple Mail",
        }
    }
}

/// Qué se ve al abrir IureDesk (F6.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartPref {
    /// «Mi día»: el resumen del día.
    #[default]
    Today,
    /// Donde se dejó la última vez (módulo y carpeta).
    LastPlace,
}

fn yes() -> bool {
    true
}

/// Preferencias persistidas.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prefs {
    #[serde(default)]
    pub theme: ThemePref,
    #[serde(default)]
    pub density: DensityPref,
    #[serde(default)]
    pub scheme: ShortcutScheme,
    /// Agrupar la lista por conversación.
    #[serde(default = "yes")]
    pub conversations: bool,
    /// Idioma de la interfaz (`es`/`en`); `None` = el del sistema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
    /// Mostrar el panel «Hoy» al pie de las carpetas.
    #[serde(default = "yes")]
    pub show_today: bool,
    /// Marcar como leído al abrir un mensaje.
    #[serde(default = "yes")]
    pub mark_read_on_open: bool,
    /// Espera antes de marcar como leído (ms): repasar la lista con las flechas no marca nada.
    #[serde(default = "default_read_delay")]
    pub mark_read_delay_ms: u32,
    /// Ventana para deshacer un envío (segundos, 0–30).
    #[serde(default = "default_undo_send")]
    pub send_undo_secs: u32,
    /// Avisar con una notificación del escritorio cuando llega correo.
    #[serde(default = "yes")]
    pub notify_new_mail: bool,
    /// Cada cuántos segundos se consultan los avisos y menciones de Iurefficient (60–3600).
    #[serde(default = "default_iure_poll")]
    pub iure_poll_secs: u32,
    /// Vista del calendario.
    #[serde(default)]
    pub cal_view: crate::calendar::nav::CalView,
    /// Calendarios ocultos (ids locales).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hidden_calendars: Vec<u64>,
    /// Calendarios duplicados (misma colección que otra cuenta) que el usuario quiere ver; los
    /// demás duplicados se ocultan solos (BUG-DUP).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shown_duplicate_calendars: Vec<u64>,
    /// Segunda zona horaria en la rejilla del calendario (IANA).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secondary_tz: Option<String>,
    /// Avisar con una notificación del escritorio de los recordatorios.
    #[serde(default = "yes")]
    pub notify_reminders: bool,
    /// Usar la barra de título del sistema en vez de la integrada (se aplica al reiniciar).
    #[serde(default)]
    pub native_decorations: bool,
    /// Qué se ve al abrir.
    #[serde(default)]
    pub start: StartPref,
    /// Último módulo visto (`mail`, `calendar`…), para «Donde lo dejé».
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_module: Option<String>,
    /// Última carpeta del correo (`unified:inbox`, `folder:12`…).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_folder: Option<String>,
    /// Tipografía de la interfaz: `inter` (incluida), `system` (la del escritorio) o
    /// `family:<nombre>` (una familia instalada). La interpreta `iurefficient_ui::fonts::FontChoice`.
    #[serde(default = "default_font")]
    pub font: String,
    /// Tamaño del texto en % (90–130).
    #[serde(default = "default_text_scale")]
    pub text_scale_pct: u32,
    /// Texto nítido: letras alineadas a píxeles enteros (más nítido con escalado fraccionario).
    #[serde(default)]
    pub crisp_text: bool,
    /// Mostrar en Contactos los contactos sin ningún dato.
    #[serde(default)]
    pub show_empty_contacts: bool,
    /// UX-6: carpeta de las notas en Markdown (`None` = `Documentos/IureDesk/Notas`). Puede ser
    /// una unidad de IureDav para que las notas lleguen a Iurefficient.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes_dir: Option<String>,
    /// UX-3: ancho del panel de lectura (puntos) que dejó el usuario; `None` = automático.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reading_width: Option<u32>,
    /// UX-3: zoom del cuerpo del mensaje en % ([`READING_ZOOM_RANGE`]).
    #[serde(default = "default_reading_zoom")]
    pub reading_zoom_pct: u32,
    /// UX-3: panel contextual (Iurefficient, contacto) plegado a un riel delgado.
    #[serde(default)]
    pub context_collapsed: bool,
    /// UX-3: dónde va el panel contextual.
    #[serde(default)]
    pub context_layout: ContextLayout,
    /// UX-3: tamaño de la ventana de lectura propia (puntos), recordado al cerrarla.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reader_size: Option<[u32; 2]>,
    /// UX-5: cuándo se cargan solas las imágenes remotas.
    #[serde(default)]
    pub remote_images: crate::remote_images::RemoteImages,
    /// UX-5: remitentes cuyas imágenes se muestran siempre (minúsculas).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub image_senders: Vec<String>,
    /// UX-5: dominios cuyas imágenes se muestran siempre (minúsculas; incluye subdominios).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub image_domains: Vec<String>,
}

/// UX-3: dónde se muestra el panel contextual.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextLayout {
    /// Columna a la derecha si la ventana mide al menos [`CONTEXT_STRIP_BELOW`] puntos; si no,
    /// franja bajo la cabecera del mensaje.
    #[default]
    Auto,
    /// Siempre columna a la derecha (si cabe).
    Side,
    /// Siempre franja compacta bajo la cabecera del mensaje.
    Strip,
}

impl ContextLayout {
    pub const ALL: [ContextLayout; 3] = [ContextLayout::Auto, ContextLayout::Side, ContextLayout::Strip];

    pub fn label(self) -> &'static str {
        match self {
            ContextLayout::Auto => crate::lang::pick("Automatic", "Automático"),
            ContextLayout::Side => crate::lang::pick("Column on the right", "Columna a la derecha"),
            ContextLayout::Strip => crate::lang::pick("Strip under the header", "Franja bajo la cabecera"),
        }
    }

    /// ¿Franja (y no columna) con una ventana de este ancho?
    pub fn strip_at(self, window_width: f32) -> bool {
        match self {
            ContextLayout::Auto => window_width < CONTEXT_STRIP_BELOW,
            ContextLayout::Side => false,
            ContextLayout::Strip => true,
        }
    }
}

/// Por debajo de este ancho de ventana (puntos), el panel contextual automático va en franja.
pub const CONTEXT_STRIP_BELOW: f32 = 1300.0;

/// Zoom aceptado del cuerpo del mensaje (%).
pub const READING_ZOOM_RANGE: std::ops::RangeInclusive<u32> = 50..=300;
/// Paso del zoom (%).
pub const READING_ZOOM_STEP: u32 = 10;

fn default_reading_zoom() -> u32 {
    100
}

fn default_font() -> String {
    "inter".into()
}

fn default_text_scale() -> u32 {
    100
}

/// Tamaños de texto aceptados (%).
pub const TEXT_SCALE_RANGE: std::ops::RangeInclusive<u32> = 90..=130;

impl Prefs {
    /// Tamaño del texto acotado a [`TEXT_SCALE_RANGE`], como factor (1.0 = 100 %).
    pub fn text_scale(&self) -> f32 {
        let pct = self.text_scale_pct.clamp(*TEXT_SCALE_RANGE.start(), *TEXT_SCALE_RANGE.end());
        pct as f32 / 100.0
    }

    /// Zoom del cuerpo acotado, como factor (1.0 = 100 %).
    pub fn reading_zoom(&self) -> f32 {
        self.reading_zoom_pct.clamp(*READING_ZOOM_RANGE.start(), *READING_ZOOM_RANGE.end()) as f32 / 100.0
    }

    /// Sube (`+1`), baja (`-1`) o restablece (`0`) el zoom del cuerpo; devuelve el nuevo %.
    pub fn step_reading_zoom(&mut self, dir: i32) -> u32 {
        let cur = self.reading_zoom_pct.clamp(*READING_ZOOM_RANGE.start(), *READING_ZOOM_RANGE.end());
        let next = match dir.signum() {
            0 => 100,
            1 => cur.saturating_add(READING_ZOOM_STEP),
            _ => cur.saturating_sub(READING_ZOOM_STEP),
        };
        self.reading_zoom_pct = next.clamp(*READING_ZOOM_RANGE.start(), *READING_ZOOM_RANGE.end());
        self.reading_zoom_pct
    }
}

fn default_iure_poll() -> u32 {
    120
}

fn default_read_delay() -> u32 {
    1_000
}

fn default_undo_send() -> u32 {
    10
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            theme: ThemePref::default(),
            density: DensityPref::default(),
            scheme: ShortcutScheme::default(),
            conversations: true,
            lang: None,
            show_today: true,
            mark_read_on_open: true,
            mark_read_delay_ms: default_read_delay(),
            send_undo_secs: default_undo_send(),
            notify_new_mail: true,
            iure_poll_secs: default_iure_poll(),
            cal_view: crate::calendar::nav::CalView::default(),
            hidden_calendars: Vec::new(),
            shown_duplicate_calendars: Vec::new(),
            secondary_tz: None,
            notify_reminders: true,
            native_decorations: false,
            start: StartPref::Today,
            last_module: None,
            last_folder: None,
            font: default_font(),
            text_scale_pct: default_text_scale(),
            crisp_text: false,
            show_empty_contacts: false,
            notes_dir: None,
            reading_width: None,
            reading_zoom_pct: default_reading_zoom(),
            context_collapsed: false,
            context_layout: ContextLayout::Auto,
            reader_size: None,
            remote_images: crate::remote_images::RemoteImages::Ask,
            image_senders: Vec::new(),
            image_domains: Vec::new(),
        }
    }
}

impl Prefs {
    /// Ruta por omisión (`<config>/iuredesk/prefs.json`).
    pub fn default_path() -> Option<PathBuf> {
        iuredesk_accounts::store::config_dir().map(|d| d.join("prefs.json"))
    }

    /// Lee las preferencias; ante cualquier problema devuelve las de omisión.
    pub fn load(path: &Path) -> Prefs {
        let too_big = std::fs::metadata(path).map(|m| m.len() > MAX_PREFS_BYTES).unwrap_or(true);
        if too_big {
            return Prefs::default();
        }
        std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    /// Guarda las preferencias de forma atómica.
    pub fn save(&self, path: &Path) -> Result<(), crate::EngineError> {
        let json = serde_json::to_vec_pretty(self).map_err(|e| crate::EngineError::Io(e.to_string()))?;
        iuredesk_accounts::store::write_atomic(path, &json).map_err(|e| crate::EngineError::Io(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("prefs.json");
        assert_eq!(Prefs::load(&p), Prefs::default(), "sin archivo: omisión");
        let prefs = Prefs { density: DensityPref::Dense, scheme: ShortcutScheme::Gmail, ..Default::default() };
        prefs.save(&p).unwrap();
        assert_eq!(Prefs::load(&p), prefs);
        std::fs::write(&p, b"{no es json").unwrap();
        assert_eq!(Prefs::load(&p), Prefs::default());
        // Campos que faltan toman su valor por omisión.
        std::fs::write(&p, br#"{"theme":"dark"}"#).unwrap();
        let p2 = Prefs::load(&p);
        assert_eq!(p2.theme, ThemePref::Dark);
        assert!(p2.conversations);
    }

    #[test]
    fn typography_round_trip_and_clamp() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("prefs.json");
        let prefs = Prefs {
            font: "family:IBM Plex Sans".into(),
            text_scale_pct: 115,
            crisp_text: true,
            show_empty_contacts: true,
            ..Default::default()
        };
        prefs.save(&p).unwrap();
        let back = Prefs::load(&p);
        assert_eq!(back, prefs);
        assert!((back.text_scale() - 1.15).abs() < 1e-6);
        // Lo viejo (sin estos campos) toma Inter al 100 %.
        std::fs::write(&p, br#"{"theme":"dark"}"#).unwrap();
        let old = Prefs::load(&p);
        assert_eq!((old.font.as_str(), old.text_scale_pct, old.crisp_text), ("inter", 100, false));
        assert_eq!(Prefs { text_scale_pct: 500, ..Default::default() }.text_scale(), 1.3);
        assert_eq!(Prefs { text_scale_pct: 0, ..Default::default() }.text_scale(), 0.9);
    }

    #[test]
    fn reading_layout_round_trip_and_zoom_steps() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("prefs.json");
        let prefs = Prefs {
            reading_width: Some(720),
            reading_zoom_pct: 130,
            context_collapsed: true,
            context_layout: ContextLayout::Strip,
            reader_size: Some([900, 700]),
            ..Default::default()
        };
        prefs.save(&p).unwrap();
        assert_eq!(Prefs::load(&p), prefs);
        std::fs::write(&p, br#"{"theme":"dark"}"#).unwrap();
        let old = Prefs::load(&p);
        assert_eq!(
            (old.reading_zoom_pct, old.context_collapsed, old.context_layout),
            (100, false, ContextLayout::Auto)
        );
        let mut z = Prefs { reading_zoom_pct: 290, ..Default::default() };
        assert_eq!(z.step_reading_zoom(1), 300);
        assert_eq!(z.step_reading_zoom(1), 300, "tope");
        assert_eq!(z.step_reading_zoom(0), 100);
        z.reading_zoom_pct = 0;
        assert_eq!(z.step_reading_zoom(-1), 50, "basura acotada");
        assert!(ContextLayout::Auto.strip_at(1280.0) && !ContextLayout::Auto.strip_at(1440.0));
        assert!(!ContextLayout::Side.strip_at(800.0) && ContextLayout::Strip.strip_at(2000.0));
    }

    #[test]
    fn keys() {
        for d in DensityPref::ALL {
            assert_eq!(DensityPref::from_key(d.key()), Some(d));
        }
        for t in ThemePref::ALL {
            assert_eq!(ThemePref::from_key(t.key()), Some(t));
        }
        for s in ShortcutScheme::ALL {
            assert_eq!(ShortcutScheme::from_key(s.key()), Some(s));
        }
    }
}
