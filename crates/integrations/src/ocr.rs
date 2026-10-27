//! Adjuntos escaneados → PDF buscable + texto, con IureOCR (`iureocr --mcp`, herramienta
//! `ocr_file`). IureOCR escribe el PDF y un `.txt` junto al original (o en `output_dir`) y nunca
//! toca el original.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::error::{IntegrationError, Result};
use crate::suite::{Outcome, SisterApp, Suite, absolute};

/// Tiempo máximo de un OCR (un PDF escaneado largo tarda).
pub const OCR_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Opciones del OCR.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OcrOptions {
    /// Idiomas de Tesseract unidos con `+` (`spa+eng`). `None` = los de IureOCR.
    pub languages: Option<String>,
    /// Carpeta de salida (absoluta). `None` = la de IureOCR (junto al original).
    pub output_dir: Option<PathBuf>,
    /// Hacer OCR aunque el PDF ya tenga texto.
    pub force: bool,
    /// Máximo de caracteres de texto a devolver (`None` = 20 000, el de IureOCR).
    pub max_chars: Option<u64>,
}

/// Resultado del OCR.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OcrResult {
    /// PDF buscable escrito (no hay si se omitió porque ya tenía texto).
    pub pdf_path: Option<PathBuf>,
    /// `.txt` escrito.
    pub txt_path: Option<PathBuf>,
    /// Texto reconocido (o el que ya tenía el PDF).
    pub text: String,
    /// El texto se recortó.
    pub text_truncated: bool,
    /// Se omitió: el PDF ya tenía capa de texto.
    pub skipped: bool,
    /// Páginas.
    pub pages: u64,
    /// Nota de IureOCR, si la hay.
    pub note: Option<String>,
}

impl OcrResult {
    fn from_value(v: &Value) -> Self {
        let path = |k: &str| v.get(k).and_then(Value::as_str).filter(|s| !s.is_empty()).map(PathBuf::from);
        Self {
            pdf_path: path("pdfPath"),
            txt_path: path("txtPath"),
            text: v.get("text").and_then(Value::as_str).unwrap_or("").to_string(),
            text_truncated: v.get("textTruncated").and_then(Value::as_bool).unwrap_or(false),
            skipped: v.get("skipped").and_then(Value::as_bool).unwrap_or(false),
            pages: v.get("pages").and_then(Value::as_u64).unwrap_or(0),
            note: v.get("note").and_then(Value::as_str).map(String::from),
        }
    }
}

impl Suite {
    /// OCR de un adjunto (ya guardado en disco: ver [`Suite::stage_file`]). Bloquea hasta
    /// [`OCR_TIMEOUT`].
    pub fn ocr_attachment(&self, file: &Path, opts: &OcrOptions) -> Result<Outcome<OcrResult>> {
        let mut client = match self.connect_mcp(SisterApp::Ocr)? {
            Outcome::Ready(c) => c,
            Outcome::NotInstalled(h) => return Ok(Outcome::NotInstalled(h)),
            Outcome::NotAvailable(t) => return Ok(Outcome::NotAvailable(t)),
        };
        let input = absolute(file)?;
        let mut args = Map::new();
        args.insert("path".into(), json!(input));
        args.insert("include_text".into(), json!(true));
        if let Some(l) = &opts.languages {
            args.insert("languages".into(), json!(l));
        }
        if let Some(d) = &opts.output_dir {
            if !d.is_absolute() {
                return Err(IntegrationError::InvalidInput(format!("ruta relativa: {}", d.display())));
            }
            args.insert("output_dir".into(), json!(d));
        }
        if opts.force {
            args.insert("force".into(), json!(true));
        }
        if let Some(m) = opts.max_chars {
            args.insert("max_chars".into(), json!(m));
        }
        let r = client.call_tool("ocr_file", Value::Object(args), Some(OCR_TIMEOUT))?.into_ok()?;
        let data = r.data().ok_or_else(|| IntegrationError::Protocol("respuesta sin datos".into()))?;
        Ok(Outcome::Ready(OcrResult::from_value(&data)))
    }

    /// Escribe unos bytes (un adjunto) en la carpeta de caché para pasarlos a otra app. Devuelve
    /// la ruta (nombre saneado y sin pisar otro).
    pub fn stage_file(&self, file_name: &str, bytes: &[u8]) -> Result<PathBuf> {
        let dir = self.cache_dir.join("adjuntos");
        std::fs::create_dir_all(&dir)?;
        let path = crate::dav::unique_path(&dir, &crate::dav::sanitize_file_name(file_name));
        std::fs::write(&path, bytes)?;
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lee_el_resultado_de_iureocr() {
        let r = OcrResult::from_value(&json!({
            "skipped": false, "pdfPath": "/a/x-ocr.pdf", "txtPath": "/a/x-ocr.txt", "pages": 3,
            "chars": 10, "elapsedSecs": 1.0, "note": null, "text": "hola", "textTruncated": false
        }));
        assert_eq!(r.pdf_path, Some(PathBuf::from("/a/x-ocr.pdf")));
        assert_eq!(r.pages, 3);
        assert_eq!(r.text, "hola");
        assert!(r.note.is_none());
    }

    #[test]
    fn no_instalado() {
        let r = Suite::empty().ocr_attachment(Path::new("/tmp/x.pdf"), &OcrOptions::default());
        assert!(matches!(r, Ok(Outcome::NotInstalled(h)) if h.app == SisterApp::Ocr));
    }
}
