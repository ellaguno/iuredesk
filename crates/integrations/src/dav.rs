//! IureDav: unidades WebDAV de Iurefficient montadas en este equipo.
//!
//! - [`Suite::dav_mounts`]: los perfiles de IureDav (`apps::iuredav_mounts` de connect, sin red).
//! - [`Suite::iurefficient_folder`]: la primera unidad montada y con escritura.
//! - [`Suite::attachment_destination`] / [`Suite::save_attachment`]: «Guardar adjunto en
//!   Iurefficient» (nombre saneado, sin pisar archivos existentes).
//! - [`Suite::large_attachment_advice`]: si un adjunto es grande, proponer mandarlo como enlace.

use std::path::{Path, PathBuf};

pub use iurefficient_connect::apps::DavMount;

use crate::error::Result;
use crate::suite::{InstallHint, Outcome, SisterApp, Suite, Text};

/// A partir de aquí (suma de adjuntos) se propone enviar un enlace: muchos servidores rechazan
/// más de 20–25 MB.
pub const LARGE_ATTACHMENT_BYTES: u64 = 20 * 1024 * 1024;
/// Largo máximo de un nombre de archivo saneado (en caracteres).
const MAX_NAME_CHARS: usize = 150;

/// Qué proponer con adjuntos grandes.
#[derive(Clone, Debug)]
pub enum LinkAdvice {
    /// Caben: adjuntar normalmente.
    Attach,
    /// Guardarlos en esta unidad de Iurefficient y mandar el enlace.
    UseIureDav {
        /// Unidad.
        mount: DavMount,
        /// Texto de la sugerencia.
        text: Text,
    },
    /// IureDav está instalado pero no hay una unidad montada con escritura.
    MountIureDav(Text),
    /// IureDav no está instalado.
    InstallIureDav(InstallHint),
}

/// Nombre de archivo seguro en cualquier sistema: sin rutas, controles ni caracteres reservados
/// de Windows, sin nombres reservados (`CON`, `NUL`…), sin puntos/espacios al final, acotado.
pub fn sanitize_file_name(name: &str) -> String {
    // Solo el último componente: «../../x» → «x».
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let mut out: String = base
        .chars()
        .map(|c| if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*') { '_' } else { c })
        .collect();
    out = out.trim().trim_end_matches(['.', ' ']).trim_start_matches('.').to_string();
    if out.chars().count() > MAX_NAME_CHARS {
        // Conserva la extensión si es corta.
        let ext = out.rsplit_once('.').map(|(_, e)| e.to_string()).filter(|e| e.chars().count() <= 10);
        let keep = MAX_NAME_CHARS.saturating_sub(ext.as_ref().map_or(0, |e| e.chars().count() + 1));
        let mut stem: String = out.chars().take(keep).collect();
        if let Some(e) = ext {
            stem.push('.');
            stem.push_str(&e);
        }
        out = stem;
    }
    let stem_upper = out.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(stem_upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem_upper.starts_with("COM") || stem_upper.starts_with("LPT"))
            && stem_upper.len() == 4
            && stem_upper.chars().last().is_some_and(|c| c.is_ascii_digit()));
    if reserved {
        out.insert(0, '_');
    }
    if out.is_empty() { "adjunto".to_string() } else { out }
}

/// `dir/name`, o `dir/name (2).ext`, `(3)`… si ya existe (hasta 999; luego un sufijo de tiempo).
pub(crate) fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.to_string(), String::new()),
    };
    for n in 2..1000 {
        let p = dir.join(format!("{stem} ({n}){ext}"));
        if !p.exists() {
            return p;
        }
    }
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
    dir.join(format!("{stem} ({stamp}){ext}"))
}

fn no_mount() -> Text {
    Text::new(
        "Mount your Iurefficient drive in IureDav (with write access) to save files there.",
        "Monta tu unidad de Iurefficient en IureDav (con escritura) para guardar archivos ahí.",
    )
}

impl Suite {
    /// Perfiles de IureDav de este equipo (montados o no).
    pub fn dav_mounts(&self) -> Vec<DavMount> {
        (self.mounts)()
    }

    /// La unidad de Iurefficient donde guardar: la primera montada y con escritura.
    pub fn iurefficient_folder(&self) -> Outcome<DavMount> {
        if let Some(m) = self.dav_mounts().into_iter().find(|m| m.mounted && m.writable) {
            return Outcome::Ready(m);
        }
        if self.installed(SisterApp::Dav) {
            Outcome::NotAvailable(no_mount())
        } else {
            Outcome::NotInstalled(SisterApp::Dav.install_hint())
        }
    }

    /// Dónde quedaría un adjunto guardado en Iurefficient (sin escribir nada).
    pub fn attachment_destination(&self, file_name: &str) -> Outcome<PathBuf> {
        self.iurefficient_folder().map(|m| unique_path(Path::new(&m.mount_point), &sanitize_file_name(file_name)))
    }

    /// «Guardar adjunto en Iurefficient»: lo escribe en la unidad y devuelve la ruta.
    pub fn save_attachment(&self, file_name: &str, bytes: &[u8]) -> Result<Outcome<PathBuf>> {
        match self.attachment_destination(file_name) {
            Outcome::Ready(path) => {
                std::fs::write(&path, bytes)?;
                Ok(Outcome::Ready(path))
            }
            Outcome::NotInstalled(h) => Ok(Outcome::NotInstalled(h)),
            Outcome::NotAvailable(t) => Ok(Outcome::NotAvailable(t)),
        }
    }

    /// ¿Conviene mandar los adjuntos (`total_bytes`) como enlace?
    pub fn large_attachment_advice(&self, total_bytes: u64) -> LinkAdvice {
        if total_bytes < LARGE_ATTACHMENT_BYTES {
            return LinkAdvice::Attach;
        }
        let mb = total_bytes / (1024 * 1024);
        match self.iurefficient_folder() {
            Outcome::Ready(mount) => {
                let text = Text::new(
                    format!(
                        "The attachments add up to {mb} MB and may be rejected. Save them to “{}” and send a link instead?",
                        mount.name
                    ),
                    format!(
                        "Los adjuntos suman {mb} MB y pueden ser rechazados. ¿Guardarlos en «{}» y enviar un enlace?",
                        mount.name
                    ),
                );
                LinkAdvice::UseIureDav { mount, text }
            }
            Outcome::NotAvailable(t) => LinkAdvice::MountIureDav(t),
            Outcome::NotInstalled(h) => LinkAdvice::InstallIureDav(h),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mount(dir: &Path, mounted: bool, writable: bool) -> DavMount {
        DavMount {
            id: "1".into(),
            name: "Despacho".into(),
            url: "https://x.example/dav".into(),
            user: "ana".into(),
            mount_point: dir.to_string_lossy().into_owned(),
            mounted,
            writable,
        }
    }

    #[test]
    fn nombres_saneados() {
        assert_eq!(sanitize_file_name("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_file_name("C:\\x\\a:b?.pdf"), "a_b_.pdf");
        assert_eq!(sanitize_file_name("CON.txt"), "_CON.txt");
        assert_eq!(sanitize_file_name("com1"), "_com1");
        assert_eq!(sanitize_file_name("..."), "adjunto");
        assert_eq!(sanitize_file_name("  .oculto. "), "oculto");
        assert_eq!(sanitize_file_name("a\u{0}b"), "a_b");
        let long = format!("{}.pdf", "x".repeat(400));
        let s = sanitize_file_name(&long);
        assert!(s.ends_with(".pdf") && s.chars().count() <= MAX_NAME_CHARS);
    }

    #[test]
    fn guardar_sin_pisar() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().to_path_buf();
        let s = Suite::empty()
            .with_app(SisterApp::Dav, "/x/iuredav-app")
            .with_mounts(move || vec![mount(Path::new("/no/montada"), false, true), mount(&d, true, true)]);
        let p1 = s.save_attachment("informe.pdf", b"1").unwrap().ready().unwrap();
        let p2 = s.save_attachment("informe.pdf", b"2").unwrap().ready().unwrap();
        assert_eq!(p1, dir.path().join("informe.pdf"));
        assert_eq!(p2, dir.path().join("informe (2).pdf"));
        assert_eq!(std::fs::read(&p2).unwrap(), b"2");
        assert!(matches!(s.large_attachment_advice(1024), LinkAdvice::Attach));
        assert!(matches!(s.large_attachment_advice(30 * 1024 * 1024), LinkAdvice::UseIureDav { .. }));
    }

    #[test]
    fn sin_iuredav_o_sin_unidad() {
        let s = Suite::empty();
        assert!(matches!(s.iurefficient_folder(), Outcome::NotInstalled(_)));
        assert!(matches!(s.large_attachment_advice(LARGE_ATTACHMENT_BYTES), LinkAdvice::InstallIureDav(_)));
        let s = s.with_app(SisterApp::Dav, "/x").with_mounts(|| vec![mount(Path::new("/r"), true, false)]);
        assert!(matches!(s.save_attachment("a", b"").unwrap(), Outcome::NotAvailable(_)));
        assert!(matches!(s.large_attachment_advice(u64::MAX), LinkAdvice::MountIureDav(_)));
    }
}
