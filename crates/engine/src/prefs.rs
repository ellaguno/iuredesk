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
            secondary_tz: None,
            notify_reminders: true,
            native_decorations: false,
            start: StartPref::Today,
            last_module: None,
            last_folder: None,
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
