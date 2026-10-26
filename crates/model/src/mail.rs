//! Correo: carpetas, sobres de mensaje, adjuntos e hilos.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{AccountId, BlobHash, FolderId, MessageId, ThreadId};

/// Longitud máxima (en caracteres) del extracto que se muestra en la lista de mensajes.
pub const MAX_PREVIEW_CHARS: usize = 200;
/// Longitud máxima (en caracteres) que se conserva de un asunto.
pub const MAX_SUBJECT_CHARS: usize = 1_000;
/// Número máximo de destinatarios por campo que se conservan en un sobre.
pub const MAX_RECIPIENTS: usize = 500;
/// Número máximo de referencias (`References:`) que se conservan.
pub const MAX_REFERENCES: usize = 100;

/// Buzón de correo: nombre visible opcional y dirección.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Mailbox {
    /// Nombre visible («Ana Pérez»), si lo hay.
    pub name: Option<String>,
    /// Dirección (`ana@example.com`). No se valida aquí.
    pub addr: String,
}

impl Mailbox {
    /// Crea un buzón con nombre visible.
    pub fn new(name: impl Into<String>, addr: impl Into<String>) -> Self {
        let name = name.into();
        Self { name: if name.trim().is_empty() { None } else { Some(name) }, addr: addr.into() }
    }

    /// Crea un buzón solo con dirección.
    pub fn addr(addr: impl Into<String>) -> Self {
        Self { name: None, addr: addr.into() }
    }

    /// Texto para mostrar: el nombre si existe, si no la dirección.
    pub fn display(&self) -> &str {
        match &self.name {
            Some(n) if !n.trim().is_empty() => n,
            _ => &self.addr,
        }
    }
}

/// Función especial de una carpeta (RFC 6154 `SPECIAL-USE`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FolderRole {
    /// Bandeja de entrada.
    Inbox,
    /// Enviados.
    Sent,
    /// Borradores.
    Drafts,
    /// Papelera.
    Trash,
    /// Correo no deseado.
    Junk,
    /// Archivo.
    Archive,
    /// Carpeta creada por el usuario.
    #[default]
    Custom,
}

impl FolderRole {
    /// Nombre corto en inglés (`inbox`, `sent`…), útil para `in:` en la búsqueda.
    pub fn slug(&self) -> &'static str {
        match self {
            Self::Inbox => "inbox",
            Self::Sent => "sent",
            Self::Drafts => "drafts",
            Self::Trash => "trash",
            Self::Junk => "junk",
            Self::Archive => "archive",
            Self::Custom => "custom",
        }
    }
}

/// Carpeta de correo de una cuenta.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Folder {
    /// Id local (`UNASSIGNED` al crearla; el almacén lo asigna).
    pub id: FolderId,
    /// Cuenta a la que pertenece.
    pub account: AccountId,
    /// Nombre visible (último segmento de la ruta, ya decodificado).
    pub name: String,
    /// Ruta remota completa (`INBOX/Proyectos`, id de Graph…), opaca.
    pub remote_path: String,
    /// Función especial.
    pub role: FolderRole,
    /// Carpeta padre, si la jerarquía es conocida.
    pub parent: Option<FolderId>,
    /// No leídos según el servidor (el almacén mantiene además su propio recuento local).
    pub unread: u32,
    /// Total según el servidor.
    pub total: u32,
    /// Estado de sincronización opaco del proveedor (p. ej. `UIDVALIDITY`/`HIGHESTMODSEQ` de IMAP,
    /// `deltaLink` de Graph, `historyId` de Gmail). Solo lo interpreta su proveedor.
    pub sync_token: Option<String>,
}

/// Banderas estándar de un mensaje (IMAP `\Seen`, `\Flagged`… y `$Forwarded`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Flags {
    /// Leído.
    pub seen: bool,
    /// Destacado.
    pub flagged: bool,
    /// Respondido.
    pub answered: bool,
    /// Borrador.
    pub draft: bool,
    /// Marcado para borrar.
    pub deleted: bool,
    /// Reenviado.
    pub forwarded: bool,
}

impl Flags {
    /// Representación compacta en bits (para índices).
    pub fn bits(self) -> u8 {
        u8::from(self.seen)
            | u8::from(self.flagged) << 1
            | u8::from(self.answered) << 2
            | u8::from(self.draft) << 3
            | u8::from(self.deleted) << 4
            | u8::from(self.forwarded) << 5
    }

    /// Inversa de [`Flags::bits`]; los bits desconocidos se ignoran.
    pub fn from_bits(b: u8) -> Self {
        Self {
            seen: b & 1 != 0,
            flagged: b & (1 << 1) != 0,
            answered: b & (1 << 2) != 0,
            draft: b & (1 << 3) != 0,
            deleted: b & (1 << 4) != 0,
            forwarded: b & (1 << 5) != 0,
        }
    }
}

/// Metadatos de un adjunto (el contenido vive en el almacén de blobs).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentMeta {
    /// Nombre de archivo declarado (no confiable: sanear antes de escribir a disco).
    pub filename: String,
    /// Tipo MIME declarado (`application/pdf`).
    pub mime_type: String,
    /// Tamaño en bytes (decodificado, si se conoce).
    pub size: u64,
    /// `Content-ID` para imágenes en línea.
    pub content_id: Option<String>,
    /// `true` si es en línea (`Content-Disposition: inline`).
    pub inline: bool,
    /// Huella del contenido extraído, si ya se guardó como blob propio.
    pub blob: Option<BlobHash>,
}

/// Sobre de un mensaje: todo lo necesario para listar, ordenar, enhebrar y buscar sin abrir el
/// correo crudo.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageEnvelope {
    /// Id local (`UNASSIGNED` al crearlo; el almacén lo asigna).
    pub id: MessageId,
    /// Cuenta.
    pub account: AccountId,
    /// Carpeta donde está.
    pub folder: FolderId,
    /// Hilo asignado por el almacén (`None` hasta que se enhebra).
    pub thread: Option<ThreadId>,
    /// Id remoto opaco: UID de IMAP (como texto), id de Graph/Gmail…
    pub remote_id: Option<String>,
    /// `Message-ID` de RFC 5322, sin los `<>`.
    pub message_id: Option<String>,
    /// `In-Reply-To`, sin los `<>`.
    pub in_reply_to: Option<String>,
    /// `References`, sin los `<>`, del más antiguo al más reciente.
    pub references: Vec<String>,
    /// Remitentes (`From:`).
    pub from: Vec<Mailbox>,
    /// Destinatarios (`To:`).
    pub to: Vec<Mailbox>,
    /// Copia (`Cc:`).
    pub cc: Vec<Mailbox>,
    /// Copia oculta (`Bcc:`, solo en enviados/borradores).
    pub bcc: Vec<Mailbox>,
    /// `Reply-To:`.
    pub reply_to: Vec<Mailbox>,
    /// Asunto decodificado.
    pub subject: String,
    /// Fecha (`Date:` o, si falta o es absurda, la de recepción).
    pub date: DateTime<Utc>,
    /// Banderas estándar.
    pub flags: Flags,
    /// Palabras clave IMAP / etiquetas (Gmail, categorías de Outlook).
    pub keywords: Vec<String>,
    /// Tamaño del mensaje crudo en bytes.
    pub size: u64,
    /// `true` si tiene adjuntos no en línea.
    pub has_attachments: bool,
    /// Metadatos de los adjuntos.
    pub attachments: Vec<AttachmentMeta>,
    /// Extracto de texto, como mucho [`MAX_PREVIEW_CHARS`] caracteres (ver [`make_preview`]).
    pub preview: String,
    /// Huella del mensaje crudo (RFC 822) en el almacén de blobs, si ya se descargó.
    pub blob: Option<BlobHash>,
}

impl MessageEnvelope {
    /// Recorta los campos a los límites del modelo (datos externos hostiles).
    pub fn clamp(&mut self) {
        truncate_chars(&mut self.subject, MAX_SUBJECT_CHARS);
        if self.preview.chars().count() > MAX_PREVIEW_CHARS {
            self.preview = make_preview(&self.preview);
        }
        for list in [&mut self.from, &mut self.to, &mut self.cc, &mut self.bcc, &mut self.reply_to] {
            list.truncate(MAX_RECIPIENTS);
        }
        if self.references.len() > MAX_REFERENCES {
            let extra = self.references.len().saturating_sub(MAX_REFERENCES);
            self.references.drain(..extra);
        }
        self.keywords.truncate(MAX_RECIPIENTS);
        self.attachments.truncate(MAX_RECIPIENTS);
    }

    /// Texto del remitente principal para la lista.
    pub fn sender_display(&self) -> &str {
        self.from.first().map_or("", Mailbox::display)
    }
}

/// Hilo de conversación (resumen; los miembros los da el almacén).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Thread {
    /// Id local.
    pub id: ThreadId,
    /// Cuenta.
    pub account: AccountId,
    /// Asunto normalizado (sin `Re:`/`Fwd:`).
    pub subject: String,
    /// Fecha del mensaje más reciente.
    pub last_date: DateTime<Utc>,
    /// Número de mensajes.
    pub message_count: u32,
    /// Número de mensajes no leídos.
    pub unread_count: u32,
}

/// Recorta `s` a como mucho `max` caracteres, sin partir un carácter UTF-8.
pub fn truncate_chars(s: &mut String, max: usize) {
    if let Some((idx, _)) = s.char_indices().nth(max) {
        s.truncate(idx);
    }
}

/// Construye el extracto de la lista: colapsa espacios y saltos de línea y recorta a
/// [`MAX_PREVIEW_CHARS`] caracteres (con `…` si se cortó).
pub fn make_preview(text: &str) -> String {
    let mut out = String::with_capacity(MAX_PREVIEW_CHARS.saturating_add(4));
    let mut count = 0usize;
    let mut pending_space = false;
    for c in text.chars() {
        if c.is_whitespace() || c.is_control() {
            pending_space = count > 0;
            continue;
        }
        let needed = if pending_space { 2 } else { 1 };
        if count.saturating_add(needed) > MAX_PREVIEW_CHARS {
            // Deja sitio para la elipsis.
            while count >= MAX_PREVIEW_CHARS {
                if out.pop().is_none() {
                    break;
                }
                count = count.saturating_sub(1);
            }
            out.push('…');
            return out;
        }
        if pending_space {
            out.push(' ');
            count = count.saturating_add(1);
            pending_space = false;
        }
        out.push(c);
        count = count.saturating_add(1);
    }
    out
}

/// Asunto normalizado para agrupar hilos: sin prefijos de respuesta/reenvío (`Re:`, `RE:`, `Fwd:`,
/// `Fw:`, `RV:`, `Rif:`, `AW:`, `SV:`, `Re[2]:`…), sin etiquetas de lista (`[equipo]`) al inicio y
/// con espacios colapsados. Devuelve también si tenía algún prefijo de respuesta.
pub fn normalize_subject(subject: &str) -> (String, bool) {
    const PREFIXES: &[&str] = &["re", "fwd", "fw", "rv", "res", "enc", "rif", "aw", "wg", "sv", "vs", "tr", "antw"];
    let mut s = subject.trim();
    let mut was_reply = false;
    // Límite de iteraciones: un asunto hostil no puede hacernos girar sin fin.
    for _ in 0..32 {
        let before = s;
        if let Some(rest) = s.strip_prefix('[')
            && let Some(end) = rest.find(']')
        {
            let after = rest.get(end.saturating_add(1)..).unwrap_or("").trim_start();
            // Solo se quita la etiqueta si queda asunto detrás.
            if !after.is_empty() {
                s = after;
            }
        }
        if let Some(colon) = s.find(':').filter(|&c| c <= 8) {
            let head = s.get(..colon).unwrap_or("");
            // `Re[2]` / `Re(2)` / `Re 2`.
            let word: String = head.chars().take_while(|c| c.is_alphabetic()).collect::<String>().to_lowercase();
            let rest_ok = head
                .chars()
                .skip(word.chars().count())
                .all(|c| c.is_ascii_digit() || matches!(c, '[' | ']' | '(' | ')' | ' '));
            if !word.is_empty() && rest_ok && PREFIXES.contains(&word.as_str()) {
                was_reply = true;
                s = s.get(colon.saturating_add(1)..).unwrap_or("").trim_start();
            }
        }
        if s == before {
            break;
        }
    }
    let mut out = String::with_capacity(s.len());
    for w in s.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(w);
    }
    truncate_chars(&mut out, MAX_SUBJECT_CHARS);
    (out, was_reply)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_collapses_and_truncates() {
        assert_eq!(make_preview("  hola\n\n  mundo\t "), "hola mundo");
        let long = "palabra ".repeat(100);
        let p = make_preview(&long);
        assert!(p.chars().count() <= MAX_PREVIEW_CHARS);
        assert!(p.ends_with('…'));
        assert_eq!(make_preview(""), "");
    }

    #[test]
    fn subject_normalization() {
        assert_eq!(normalize_subject("Re: RE: Fwd: Hola"), ("Hola".into(), true));
        assert_eq!(normalize_subject("[equipo] Re: Presupuesto  2027"), ("Presupuesto 2027".into(), true));
        assert_eq!(normalize_subject("RV: Informe"), ("Informe".into(), true));
        assert_eq!(normalize_subject("Re[2]: Informe"), ("Informe".into(), true));
        assert_eq!(normalize_subject("Nota: importante"), ("Nota: importante".into(), false));
        assert_eq!(normalize_subject("[solo]"), ("[solo]".into(), false));
        assert_eq!(normalize_subject(""), (String::new(), false));
    }

    #[test]
    fn flags_bits_roundtrip() {
        let f = Flags { seen: true, flagged: false, answered: true, draft: false, deleted: true, forwarded: true };
        assert_eq!(Flags::from_bits(f.bits()), f);
    }

    #[test]
    fn clamp_limits() {
        let mut e = MessageEnvelope {
            subject: "x".repeat(5_000),
            references: (0..300).map(|i| format!("r{i}")).collect(),
            preview: "y".repeat(1_000),
            ..Default::default()
        };
        e.clamp();
        assert_eq!(e.subject.chars().count(), MAX_SUBJECT_CHARS);
        assert_eq!(e.references.len(), MAX_REFERENCES);
        assert_eq!(e.references.last().map(String::as_str), Some("r299"));
        assert!(e.preview.chars().count() <= MAX_PREVIEW_CHARS);
    }

    proptest::proptest! {
        #[test]
        fn preview_bounded(s in "\\PC{0,600}") {
            proptest::prop_assert!(make_preview(&s).chars().count() <= MAX_PREVIEW_CHARS);
        }

        #[test]
        fn normalize_never_panics(s in "\\PC{0,200}") {
            let (n, _) = normalize_subject(&s);
            proptest::prop_assert!(n.len() <= s.len());
        }
    }
}
