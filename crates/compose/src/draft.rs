//! El borrador: todo lo que el usuario edita antes de enviar.
//!
//! Se serializa a JSON (borradores guardados, plantillas y la bandeja de salida). Los adjuntos se
//! refieren a su contenido ([`AttachmentSource`]): un archivo del disco, un blob del almacén o los
//! bytes mismos; la bandeja de salida los copia al almacén al encolar para que el envío no dependa
//! de que el archivo siga ahí.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use iuredesk_model::account::Identity;
use iuredesk_model::{BlobHash, MAX_RECIPIENTS, Mailbox, MessageId};
use serde::{Deserialize, Serialize};

use crate::address::{self, parse_address_list};
use crate::error::{ComposeError, Result};
use crate::rich::RichBody;
use crate::signature::SignaturePlacement;

/// Prioridad del mensaje (`X-Priority`, `Importance`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Low,
    #[default]
    Normal,
    High,
}

/// De dónde sale el contenido de un adjunto.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "from", content = "value")]
pub enum AttachmentSource {
    /// Archivo local.
    Path(PathBuf),
    /// Blob del almacén (adjunto de un correo recibido, o copia hecha al encolar).
    Blob(BlobHash),
    /// Bytes en memoria (p. ej. una imagen pegada). Se pasan a blob al encolar.
    Bytes(Vec<u8>),
}

/// Cómo va un adjunto.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum AttachmentKind {
    /// Adjunto normal (`Content-Disposition: attachment`).
    #[default]
    Regular,
    /// Imagen en línea, referida en el cuerpo como `cid:<content_id>`.
    Inline { content_id: String },
}

/// Un adjunto del borrador.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    /// Nombre del archivo como lo verá el destinatario.
    pub filename: String,
    /// Tipo MIME (`application/pdf`). Vacío = se deduce de la extensión.
    #[serde(default)]
    pub mime_type: String,
    pub source: AttachmentSource,
    #[serde(default)]
    pub kind: AttachmentKind,
    /// Tamaño conocido (para avisar antes de leerlo). 0 = desconocido.
    #[serde(default)]
    pub size: u64,
}

impl Attachment {
    /// Adjunto normal desde un archivo.
    pub fn file(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let filename =
            path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "adjunto".to_string());
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        Self {
            mime_type: guess_mime(&filename).to_string(),
            filename,
            source: AttachmentSource::Path(path),
            kind: AttachmentKind::Regular,
            size,
        }
    }

    /// Adjunto normal desde bytes.
    pub fn bytes(filename: impl Into<String>, mime_type: impl Into<String>, data: Vec<u8>) -> Self {
        let size = data.len() as u64;
        Self {
            filename: filename.into(),
            mime_type: mime_type.into(),
            source: AttachmentSource::Bytes(data),
            kind: AttachmentKind::Regular,
            size,
        }
    }

    /// Imagen en línea desde bytes.
    pub fn inline_image(
        filename: impl Into<String>,
        mime_type: impl Into<String>,
        content_id: impl Into<String>,
        data: Vec<u8>,
    ) -> Self {
        let mut a = Self::bytes(filename, mime_type, data);
        a.kind = AttachmentKind::Inline { content_id: content_id.into() };
        a
    }

    /// Tipo MIME efectivo.
    pub fn effective_mime(&self) -> String {
        let t = self.mime_type.trim();
        if is_valid_mime(t) { t.to_ascii_lowercase() } else { guess_mime(&self.filename).to_string() }
    }
}

/// ¿Parece un tipo MIME `tipo/subtipo` sin caracteres raros?
fn is_valid_mime(t: &str) -> bool {
    let Some((a, b)) = t.split_once('/') else { return false };
    let ok = |s: &str| {
        !s.is_empty() && s.len() <= 127 && s.chars().all(|c| c.is_ascii_alphanumeric() || "!#$&-^_.+".contains(c))
    };
    ok(a) && ok(b)
}

/// Tipo MIME por la extensión del nombre.
pub fn guess_mime(filename: &str) -> &'static str {
    let ext = filename.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "txt" => "text/plain",
        "csv" => "text/csv",
        "html" | "htm" => "text/html",
        "md" => "text/markdown",
        "ics" => "text/calendar",
        "vcf" => "text/vcard",
        "eml" => "message/rfc822",
        "zip" => "application/zip",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "odt" => "application/vnd.oasis.opendocument.text",
        "ods" => "application/vnd.oasis.opendocument.spreadsheet",
        "json" => "application/json",
        "xml" => "application/xml",
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        _ => "application/octet-stream",
    }
}

/// Cómo se reenvía el original.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForwardMode {
    /// El cuerpo citado en el texto y sus adjuntos copiados.
    #[default]
    Inline,
    /// El correo original entero como adjunto `message/rfc822`.
    AsAttachment,
}

/// Correo reenviado como adjunto.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Forwarded {
    /// Correo crudo (RFC 5322).
    pub source: AttachmentSource,
    /// Nombre del adjunto (`Asunto.eml`).
    pub filename: String,
}

/// Qué relación tiene el borrador con el correo original.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OriginalKind {
    Reply,
    Forward,
}

/// Correo original al que responde o que reenvía (para marcarlo tras enviar).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OriginalRef {
    /// Mensaje en el almacén local, si se conoce.
    #[serde(default)]
    pub message: Option<MessageId>,
    pub kind: OriginalKind,
}

/// Método iTIP de una invitación.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CalendarMethod {
    Request,
    Reply,
    Cancel,
    Counter,
    Publish,
}

impl CalendarMethod {
    /// Valor para `method=` y la propiedad `METHOD:`.
    pub fn as_str(self) -> &'static str {
        match self {
            CalendarMethod::Request => "REQUEST",
            CalendarMethod::Reply => "REPLY",
            CalendarMethod::Cancel => "CANCEL",
            CalendarMethod::Counter => "COUNTER",
            CalendarMethod::Publish => "PUBLISH",
        }
    }
}

/// Invitación (o respuesta) de calendario: el `.ics` que produce `iuredesk-dav`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarPart {
    pub method: CalendarMethod,
    /// iCalendar en UTF-8.
    pub ics: Vec<u8>,
    /// Además de la parte en línea, adjuntar una copia `invite.ics` (la piden algunos clientes).
    #[serde(default = "yes")]
    pub attach_copy: bool,
}

fn yes() -> bool {
    true
}

impl CalendarPart {
    /// Crea la parte comprobando que es un iCalendar con el mismo `METHOD`.
    pub fn new(method: CalendarMethod, ics: Vec<u8>) -> Result<Self> {
        let p = Self { method, ics, attach_copy: true };
        p.validate()?;
        Ok(p)
    }

    /// Comprueba que es UTF-8, empieza por `BEGIN:VCALENDAR` y su `METHOD` coincide (si lo trae).
    pub fn validate(&self) -> Result<()> {
        let text = std::str::from_utf8(&self.ics).map_err(|_| ComposeError::Calendar("no es UTF-8".to_string()))?;
        if !text.trim_start().to_ascii_uppercase().starts_with("BEGIN:VCALENDAR") {
            return Err(ComposeError::Calendar("falta BEGIN:VCALENDAR".to_string()));
        }
        for line in text.lines().take(10_000) {
            if let Some(m) = line.trim().to_ascii_uppercase().strip_prefix("METHOD:")
                && m.trim() != self.method.as_str()
            {
                return Err(ComposeError::Calendar(format!("METHOD:{} ≠ {}", m.trim(), self.method.as_str())));
            }
        }
        Ok(())
    }
}

/// Un borrador.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Draft {
    /// Identidad «De:».
    pub identity: Identity,
    #[serde(default)]
    pub to: Vec<Mailbox>,
    #[serde(default)]
    pub cc: Vec<Mailbox>,
    #[serde(default)]
    pub bcc: Vec<Mailbox>,
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub body: RichBody,
    /// Enviar solo `text/plain` (sin HTML).
    #[serde(default)]
    pub plain_only: bool,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    /// `In-Reply-To` (con `<…>`).
    #[serde(default)]
    pub in_reply_to: Option<String>,
    /// `References` (cada uno con `<…>`).
    #[serde(default)]
    pub references: Vec<String>,
    /// Original reenviado como adjunto.
    #[serde(default)]
    pub forwarded: Option<Forwarded>,
    /// Original al que responde o que reenvía.
    #[serde(default)]
    pub original: Option<OriginalRef>,
    #[serde(default)]
    pub priority: Priority,
    /// Pedir acuse de lectura (`Disposition-Notification-To`).
    #[serde(default)]
    pub request_read_receipt: bool,
    /// Envío programado. `None` = en cuanto pase la ventana para deshacer.
    #[serde(default)]
    pub send_at: Option<DateTime<Utc>>,
    /// Invitación de calendario.
    #[serde(default)]
    pub calendar: Option<CalendarPart>,
    /// Firmar / cifrar al enviar (F5.4).
    #[serde(default)]
    pub security: crate::secure::SecurityRequest,
}

impl Draft {
    /// Borrador vacío para una identidad.
    pub fn new(identity: Identity) -> Self {
        Self {
            identity,
            to: Vec::new(),
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: String::new(),
            body: RichBody::default(),
            plain_only: false,
            attachments: Vec::new(),
            in_reply_to: None,
            references: Vec::new(),
            forwarded: None,
            original: None,
            priority: Priority::Normal,
            request_read_receipt: false,
            send_at: None,
            calendar: None,
            security: Default::default(),
        }
    }

    /// Fija «Para» desde el texto que escribió el usuario.
    pub fn set_to(&mut self, input: &str) -> Result<()> {
        self.to = parse_address_list(input)?;
        Ok(())
    }

    /// Fija «CC» desde texto.
    pub fn set_cc(&mut self, input: &str) -> Result<()> {
        self.cc = parse_address_list(input)?;
        Ok(())
    }

    /// Fija «CCO» desde texto.
    pub fn set_bcc(&mut self, input: &str) -> Result<()> {
        self.bcc = parse_address_list(input)?;
        Ok(())
    }

    /// Todos los destinatarios del sobre (Para + CC + CCO), sin repetir.
    pub fn all_recipients(&self) -> Vec<Mailbox> {
        let mut all: Vec<Mailbox> = self.to.iter().chain(&self.cc).chain(&self.bcc).cloned().collect();
        address::dedupe(&mut all);
        all
    }

    /// Cambia de identidad y sustituye la firma por la de la nueva (si se da).
    pub fn switch_identity(&mut self, identity: Identity, signature: Option<&RichBody>, placement: SignaturePlacement) {
        self.identity = identity;
        crate::signature::apply(&mut self.body, signature, placement);
    }

    /// Comprueba que se puede enviar: identidad válida, al menos un destinatario, todos válidos y
    /// la invitación (si hay) bien formada.
    pub fn validate(&self) -> Result<()> {
        address::validate_address(&self.identity.email)
            .map_err(|_| ComposeError::Identity(self.identity.email.clone()))?;
        let all = self.all_recipients();
        if all.is_empty() {
            return Err(ComposeError::NoRecipients);
        }
        if all.len() > MAX_RECIPIENTS {
            return Err(ComposeError::TooManyRecipients { count: all.len(), max: MAX_RECIPIENTS });
        }
        for m in &all {
            address::validate_address(&m.addr)?;
        }
        if let Some(cal) = &self.calendar {
            cal.validate()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ident() -> Identity {
        Identity { name: "Ana".into(), email: "ana@acme.mx".into(), signature: None, reply_to: None }
    }

    #[test]
    fn recipients_and_validation() {
        let mut d = Draft::new(ident());
        assert_eq!(d.validate(), Err(ComposeError::NoRecipients));
        d.set_to("Bob <bob@x.mx>, carla@y.mx").unwrap();
        d.set_cc("BOB@x.mx").unwrap();
        d.set_bcc("dan@z.mx").unwrap();
        assert_eq!(d.all_recipients().len(), 3);
        assert!(d.validate().is_ok());
        assert!(d.set_to("malo").is_err());
        d.identity.email = "nadie".into();
        assert!(matches!(d.validate(), Err(ComposeError::Identity(_))));
    }

    #[test]
    fn calendar_validation() {
        let ok = b"BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nEND:VCALENDAR\r\n".to_vec();
        assert!(CalendarPart::new(CalendarMethod::Request, ok.clone()).is_ok());
        assert!(CalendarPart::new(CalendarMethod::Reply, ok).is_err());
        assert!(CalendarPart::new(CalendarMethod::Request, b"hola".to_vec()).is_err());
        assert!(CalendarPart::new(CalendarMethod::Request, vec![0xff, 0xfe]).is_err());
    }

    #[test]
    fn mime_guess() {
        let a = Attachment::bytes("Informe.PDF", "", vec![1]);
        assert_eq!(a.effective_mime(), "application/pdf");
        let a = Attachment::bytes("x", "text/plain\r\nBcc: x", vec![1]);
        assert_eq!(a.effective_mime(), "application/octet-stream");
        let j = serde_json::to_string(&Draft::new(ident())).unwrap();
        let d: Draft = serde_json::from_str(&j).unwrap();
        assert_eq!(d, Draft::new(ident()));
    }
}
