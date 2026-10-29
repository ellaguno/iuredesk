//! «Abrir / editar en IureEditor».
//!
//! Deep links que entiende IureEditor (ver `iureditor/src-tauri/src/lib.rs`):
//! - `iureditor://open?path=<ruta absoluta>`
//! - `iureditor://insert?text=<texto>&path=<ruta opcional>`: inserta en el cursor del documento.
//! - `iureditor://iurefficient/doc?case=<id>&doc=<id>&name=<archivo>&title=<proyecto>`: abre un
//!   documento de la instancia (IureEditor lo baja como espejo local).
//!
//! El enlace se abre con el manejador del SO; si eso falla (esquema sin registrar) y la app está
//! instalada, se lanza el ejecutable con el enlace como argumento, que IureEditor también acepta.

use std::path::{Path, PathBuf};

use url::form_urlencoded::Serializer;

use crate::error::{IntegrationError, Result};
use crate::suite::{Outcome, SisterApp, Suite, absolute};

/// Tope del texto que viaja dentro de un enlace `insert` (más largo: [`EditorTarget::NewDocument`]).
pub const MAX_INSERT_CHARS: usize = 16 * 1024;

/// Qué abrir en IureEditor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditorTarget {
    /// Un archivo local existente.
    File(PathBuf),
    /// Insertar texto en el documento activo (o en `path`).
    Insert {
        /// Texto (≤ [`MAX_INSERT_CHARS`]).
        text: String,
        /// Documento destino.
        path: Option<PathBuf>,
    },
    /// Un documento nuevo con este Markdown (p. ej. «redactar un correo largo en IureEditor»): se
    /// escribe en la carpeta de caché y se abre; [`EditorOpened::path`] dice dónde, para leerlo
    /// de vuelta.
    NewDocument {
        /// Nombre sugerido (se sanea; se añade `.md` si falta).
        file_name: String,
        /// Contenido.
        markdown: String,
    },
    /// Un documento de Iurefficient.
    IurefficientDoc {
        /// Id del caso/proyecto.
        case_id: Option<String>,
        /// Id del documento.
        doc_id: String,
        /// Nombre del archivo.
        file_name: Option<String>,
        /// Título del caso/proyecto.
        case_title: Option<String>,
    },
}

/// Lo que se abrió.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorOpened {
    /// El deep link usado.
    pub link: String,
    /// Archivo local, si lo hay (con [`EditorTarget::NewDocument`], el que hay que releer).
    pub path: Option<PathBuf>,
}

/// El deep link para un destino (sin tocar el disco; `NewDocument` necesita
/// [`Suite::open_in_iureditor`]).
pub fn editor_link(target: &EditorTarget) -> Result<String> {
    let mut q = Serializer::new(String::new());
    let route = match target {
        EditorTarget::File(p) => {
            if !p.is_absolute() {
                return Err(IntegrationError::InvalidInput(format!("ruta relativa: {}", p.display())));
            }
            q.append_pair("path", &p.to_string_lossy());
            "open"
        }
        EditorTarget::Insert { text, path } => {
            if text.is_empty() {
                return Err(IntegrationError::InvalidInput("texto vacío".into()));
            }
            if text.chars().count() > MAX_INSERT_CHARS {
                return Err(IntegrationError::InvalidInput(format!(
                    "texto de más de {MAX_INSERT_CHARS} caracteres: ábrelo como documento nuevo"
                )));
            }
            q.append_pair("text", text);
            if let Some(p) = path {
                q.append_pair("path", &p.to_string_lossy());
            }
            "insert"
        }
        EditorTarget::IurefficientDoc { case_id, doc_id, file_name, case_title } => {
            if doc_id.trim().is_empty() {
                return Err(IntegrationError::InvalidInput("falta el id del documento".into()));
            }
            if let Some(c) = case_id {
                q.append_pair("case", c);
            }
            q.append_pair("doc", doc_id);
            if let Some(n) = file_name {
                q.append_pair("name", n);
            }
            if let Some(t) = case_title {
                q.append_pair("title", t);
            }
            "iurefficient/doc"
        }
        EditorTarget::NewDocument { .. } => {
            return Err(IntegrationError::InvalidInput("un documento nuevo se abre con open_in_iureditor".into()));
        }
    };
    // `form_urlencoded` codifica el espacio como `+`; en un deep link es más seguro %20.
    Ok(format!("iureditor://{route}?{}", q.finish().replace('+', "%20")))
}

impl Suite {
    /// Abre algo en IureEditor. Si no está instalado: [`Outcome::NotInstalled`].
    pub fn open_in_iureditor(&self, target: &EditorTarget) -> Result<Outcome<EditorOpened>> {
        let Some(program) = self.path(SisterApp::Editor).map(Path::to_path_buf) else {
            return Ok(Outcome::NotInstalled(SisterApp::Editor.install_hint()));
        };
        let (target, path) = match target {
            EditorTarget::File(p) => {
                let abs = absolute(p)?;
                (EditorTarget::File(abs.clone()), Some(abs))
            }
            EditorTarget::NewDocument { file_name, markdown } => {
                let dir = self.cache_dir.join("iureditor");
                std::fs::create_dir_all(&dir)?;
                let mut name = crate::dav::sanitize_file_name(file_name);
                if !name.to_ascii_lowercase().ends_with(".md") {
                    name.push_str(".md");
                }
                let file = crate::dav::unique_path(&dir, &name);
                std::fs::write(&file, markdown)?;
                let abs = absolute(&file)?;
                (EditorTarget::File(abs.clone()), Some(abs))
            }
            EditorTarget::Insert { path, .. } => (target.clone(), path.clone()),
            other => (other.clone(), None),
        };
        let link = editor_link(&target)?;
        if self.opener.open_url(&link).is_err() {
            self.opener.launch(&program, std::slice::from_ref(&link))?;
        }
        Ok(Outcome::Ready(EditorOpened { link, path }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enlaces() {
        // Ruta absoluta según el sistema (en Windows «/tmp/…» no es absoluta).
        let (path, encoded) = if cfg!(windows) {
            (r"C:\tmp\a b.md", "C%3A%5Ctmp%5Ca%20b.md")
        } else {
            ("/tmp/a b.md", "%2Ftmp%2Fa%20b.md")
        };
        let l = editor_link(&EditorTarget::File(PathBuf::from(path))).unwrap();
        assert_eq!(l, format!("iureditor://open?path={encoded}"));
        let l = editor_link(&EditorTarget::Insert { text: "Hola, mundo.\n".into(), path: None }).unwrap();
        assert_eq!(l, "iureditor://insert?text=Hola%2C%20mundo.%0A");
        let l = editor_link(&EditorTarget::IurefficientDoc {
            case_id: Some("12".into()),
            doc_id: "99".into(),
            file_name: Some("demanda.md".into()),
            case_title: Some("Pérez".into()),
        })
        .unwrap();
        assert_eq!(l, "iureditor://iurefficient/doc?case=12&doc=99&name=demanda.md&title=P%C3%A9rez");
        // El enlace se puede volver a leer como lo hace IureEditor.
        let u = url::Url::parse(&l).unwrap();
        assert_eq!(format!("{}{}", u.host_str().unwrap_or(""), u.path()), "iurefficient/doc");
    }

    #[test]
    fn entradas_invalidas() {
        assert!(editor_link(&EditorTarget::File(PathBuf::from("rel.md"))).is_err());
        assert!(editor_link(&EditorTarget::Insert { text: String::new(), path: None }).is_err());
        let big = "x".repeat(MAX_INSERT_CHARS + 1);
        assert!(editor_link(&EditorTarget::Insert { text: big, path: None }).is_err());
        let doc =
            EditorTarget::IurefficientDoc { case_id: None, doc_id: " ".into(), file_name: None, case_title: None };
        assert!(editor_link(&doc).is_err());
    }
}
