//! R1.2: lo que el sistema operativo le pasa a IureDesk al abrirlo (o a la instancia abierta):
//! enlaces `mailto:`, archivos `.eml` / `.ics` / `.vcf` (por ruta o `file://`) y enlaces propios
//! `iuredesk://`:
//!
//! - `iuredesk://message/<Message-ID>` abre ese mensaje (con o sin `<>`, codificado con `%XX`).
//! - `iuredesk://compose?to=…&subject=…&body=…` abre el redactor (igual que `mailto:`).
//! - `iuredesk://event/<UID>` abre ese evento en el calendario.
//!
//! Todo lo de fuera es hostil: longitudes acotadas, archivos con tope de tamaño y sin pánicos.
//! Aquí no hay interfaz: la UI muestra lo que devuelve [`load`].

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use iuredesk_model::{Contact, Event, FolderId, Mailbox};

use crate::body::MessageBody;
use crate::calendar::invite::{Invitation, parse_ics, parse_invitation};
use crate::lang::Text;

/// Esquema de los enlaces propios.
pub const SCHEME: &str = "iuredesk";
/// Tope de un argumento (enlace o ruta).
const MAX_ARG: usize = 8 * 1024;
/// Tope de un `.eml`.
const MAX_EML: u64 = 64 * 1024 * 1024;
/// Tope de un `.ics` / `.vcf`.
const MAX_PIM: u64 = 16 * 1024 * 1024;
/// Elementos que se muestran de un archivo con muchos (el resto, con «Importar a…»).
pub const MAX_ITEMS: usize = 200;

/// Tipo de archivo que IureDesk sabe abrir.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    /// Mensaje RFC 822 (`.eml`).
    Eml,
    /// Calendario o invitación (`.ics`).
    Ics,
    /// Contactos (`.vcf`).
    Vcf,
}

impl FileKind {
    /// Por la extensión (sin distinguir mayúsculas).
    pub fn of(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "eml" => Some(Self::Eml),
            "ics" | "ical" | "icalendar" | "ifb" | "vcs" => Some(Self::Ics),
            "vcf" | "vcard" => Some(Self::Vcf),
            _ => None,
        }
    }
}

/// Qué pidió el sistema.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenTarget {
    /// `mailto:…` (o `iuredesk://compose?…` convertido): abrir el redactor.
    Mailto(String),
    /// Un archivo, con ruta absoluta.
    File { path: PathBuf, kind: FileKind },
    /// Un mensaje por su `Message-ID` (sin `<>`).
    Message(String),
    /// Un evento por su `UID`.
    Event(String),
}

impl OpenTarget {
    /// Forma de texto que se reenvía a la instancia abierta (y que [`parse_target`] vuelve a leer).
    pub fn to_arg(&self) -> String {
        match self {
            OpenTarget::Mailto(m) => m.clone(),
            OpenTarget::File { path, .. } => path.to_string_lossy().into_owned(),
            OpenTarget::Message(id) => message_link(id),
            OpenTarget::Event(uid) => event_link(uid),
        }
    }
}

/// `iuredesk://message/<Message-ID>` (para notificaciones y otras apps).
pub fn message_link(message_id: &str) -> String {
    format!("{SCHEME}://message/{}", percent_encode(clean_id(message_id)))
}

/// `iuredesk://event/<UID>`.
pub fn event_link(uid: &str) -> String {
    format!("{SCHEME}://event/{}", percent_encode(uid.trim()))
}

fn clean_id(s: &str) -> &str {
    let s = s.trim();
    s.strip_prefix('<').and_then(|x| x.strip_suffix('>')).unwrap_or(s).trim()
}

/// Lee un argumento de la línea de órdenes. `cwd` hace absolutas las rutas relativas (la
/// instancia abierta tiene otra carpeta de trabajo). `None` si no es nada que se sepa abrir.
pub fn parse_target(arg: &str, cwd: Option<&Path>) -> Option<OpenTarget> {
    let arg = arg.trim();
    if arg.is_empty() || arg.len() > MAX_ARG {
        return None;
    }
    let lower: String = arg.chars().take(16).collect::<String>().to_ascii_lowercase();
    if lower.starts_with("mailto:") {
        return Some(OpenTarget::Mailto(arg.to_string()));
    }
    if lower.starts_with("iuredesk:") {
        return parse_link(arg);
    }
    let path = if lower.starts_with("file://") { file_url_path(arg)? } else { PathBuf::from(arg) };
    let kind = FileKind::of(&path)?;
    let path = match (path.is_absolute(), cwd) {
        (false, Some(dir)) => dir.join(path),
        _ => path,
    };
    Some(OpenTarget::File { path, kind })
}

/// `file:///ruta/con%20espacios.eml` → ruta (también `file://localhost/…` y `file:///C:/…`).
fn file_url_path(url: &str) -> Option<PathBuf> {
    let rest = url.get("file://".len()..)?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    if !rest.starts_with('/') {
        return None; // `file://otro-equipo/…`: no es nuestro.
    }
    let decoded = percent_decode(rest);
    // Windows: `/C:/Users/…` → `C:/Users/…`.
    let b = decoded.as_bytes();
    let windows_drive = b.len() >= 3 && b.get(2) == Some(&b':') && b.get(1).is_some_and(u8::is_ascii_alphabetic);
    Some(PathBuf::from(if windows_drive { decoded.get(1..).unwrap_or(&decoded).to_string() } else { decoded }))
}

/// Enlace `iuredesk:` → destino.
pub fn parse_link(url: &str) -> Option<OpenTarget> {
    let rest = url.get(SCHEME.len().saturating_add(1)..)?;
    let rest = rest.trim_start_matches('/');
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    let path = path.trim_end_matches('/');
    let (head, tail) = path.split_once('/').unwrap_or((path, ""));
    match head.to_ascii_lowercase().as_str() {
        "message" | "mensaje" => {
            let id = percent_decode(tail);
            let id = clean_id(&id);
            (!id.is_empty() && id.len() <= 998).then(|| OpenTarget::Message(id.to_string()))
        }
        "event" | "evento" => {
            let uid = percent_decode(tail);
            let uid = uid.trim();
            (!uid.is_empty() && uid.len() <= 1_000).then(|| OpenTarget::Event(uid.to_string()))
        }
        "compose" | "redactar" => {
            // Mismos campos que `mailto:`; `to` va antes del `?`.
            let mut to = String::new();
            let mut others = Vec::new();
            for kv in query.split('&').filter(|s| !s.is_empty()).take(32) {
                match kv.split_once('=') {
                    Some((k, v)) if k.eq_ignore_ascii_case("to") => {
                        if !to.is_empty() {
                            to.push(',');
                        }
                        to.push_str(v);
                    }
                    _ => others.push(kv),
                }
            }
            let mut m = format!("mailto:{to}");
            if !others.is_empty() {
                m.push('?');
                m.push_str(&others.join("&"));
            }
            Some(OpenTarget::Mailto(m))
        }
        _ => None,
    }
}

/// `%XX` → bytes (UTF-8 con pérdidas; `+` se queda).
pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0usize;
    while i < b.len() {
        let c = b.get(i).copied().unwrap_or(0);
        if c == b'%'
            && let (Some(h), Some(l)) = (b.get(i + 1).copied().and_then(hex), b.get(i + 2).copied().and_then(hex))
        {
            out.push(h << 4 | l);
            i += 3;
            continue;
        }
        out.push(c);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Codifica todo salvo lo no reservado de RFC 3986 y `@` (frecuente en Message-ID).
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'@') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

// ------------------------------------------------------------------ archivos

/// Un archivo abierto, listo para mostrar.
#[derive(Clone, Debug)]
pub enum Opened {
    Message(Box<OpenedMessage>),
    Calendar(Box<OpenedCalendar>),
    Contacts(OpenedContacts),
}

impl Opened {
    /// Ruta del archivo.
    pub fn path(&self) -> &Path {
        match self {
            Opened::Message(m) => &m.path,
            Opened::Calendar(c) => &c.path,
            Opened::Contacts(c) => &c.path,
        }
    }
}

/// Un `.eml` de solo lectura.
#[derive(Clone, Debug)]
pub struct OpenedMessage {
    pub path: PathBuf,
    pub subject: String,
    pub from: Vec<Mailbox>,
    pub to: Vec<Mailbox>,
    pub cc: Vec<Mailbox>,
    pub date: DateTime<Utc>,
    pub body: MessageBody,
    /// Invitación adjunta (`text/calendar`), si trae.
    pub invitation: Option<Box<Invitation>>,
}

/// Un `.ics`: la invitación (si es una sola, con `METHOD`) y los eventos.
#[derive(Clone, Debug)]
pub struct OpenedCalendar {
    pub path: PathBuf,
    pub invitation: Option<Box<Invitation>>,
    /// Hasta [`MAX_ITEMS`] eventos.
    pub events: Vec<Event>,
    /// Eventos en el archivo.
    pub total: usize,
}

/// Un `.vcf` con una o muchas tarjetas.
#[derive(Clone, Debug)]
pub struct OpenedContacts {
    pub path: PathBuf,
    /// Hasta [`MAX_ITEMS`] contactos.
    pub contacts: Vec<Contact>,
    pub total: usize,
}

fn read_capped(path: &Path, max: u64) -> Result<Vec<u8>, Text> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let meta = std::fs::metadata(path).map_err(|_| {
        Text::new(
            format!("“{name}” could not be opened: it does not exist or you cannot read it."),
            format!("No se pudo abrir «{name}»: no existe o no tienes permiso para leerlo."),
        )
    })?;
    if !meta.is_file() {
        return Err(Text::new(format!("“{name}” is not a file."), format!("«{name}» no es un archivo.")));
    }
    if meta.len() > max {
        let mb = max / (1024 * 1024);
        return Err(Text::new(
            format!("“{name}” is too large to open here (more than {mb} MB). Use Import instead."),
            format!("«{name}» es demasiado grande para abrirlo aquí (más de {mb} MB). Usa Importar."),
        ));
    }
    std::fs::read(path).map_err(|e| {
        Text::new(format!("“{name}” could not be read ({e})."), format!("No se pudo leer «{name}» ({e})."))
    })
}

fn unreadable(path: &Path, what_en: &str, what_es: &str) -> Text {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    Text::new(format!("“{name}” does not look like {what_en}."), format!("«{name}» no parece {what_es}."))
}

/// Lee un archivo para mostrarlo. `mine` son mis direcciones (para la invitación) y `tz` la zona
/// del usuario.
pub fn load(path: &Path, kind: FileKind, mine: &[String], tz: chrono_tz::Tz) -> Result<Opened, Text> {
    match kind {
        FileKind::Eml => {
            let raw = read_capped(path, MAX_EML)?;
            if mail_parser::MessageParser::default().parse(&raw).is_none() {
                return Err(unreadable(path, "an email message", "un mensaje de correo"));
            }
            let (env, _) = iuredesk_sync::parse::envelope_from_raw(&raw, FolderId::default(), Utc::now());
            let invitation = parse_invitation(&raw, mine, tz).map(Box::new);
            Ok(Opened::Message(Box::new(OpenedMessage {
                path: path.to_path_buf(),
                subject: env.subject,
                from: env.from,
                to: env.to,
                cc: env.cc,
                date: env.date,
                body: crate::body::parse(&raw),
                invitation,
            })))
        }
        FileKind::Ics => {
            let raw = read_capped(path, MAX_PIM)?;
            let cal = iuredesk_dav::ICalendar::parse_bytes(&raw)
                .map_err(|_| unreadable(path, "a calendar file", "un archivo de calendario"))?;
            let all = iuredesk_pim_sync::map::ical::events_from_ics(&cal, tz);
            if all.is_empty() {
                return Err(Text::new(
                    "This calendar file has no events.",
                    "Este archivo de calendario no tiene eventos.",
                ));
            }
            // Sin `METHOD`, un `.ics` suelto es una publicación (agregar a mi calendario).
            let invitation = if all.len() == 1 {
                parse_ics(&raw, Some("text/calendar; method=PUBLISH"), mine, tz).map(Box::new)
            } else {
                None
            };
            let total = all.len();
            Ok(Opened::Calendar(Box::new(OpenedCalendar {
                path: path.to_path_buf(),
                invitation,
                events: all.into_iter().take(MAX_ITEMS).collect(),
                total,
            })))
        }
        FileKind::Vcf => {
            let raw = read_capped(path, MAX_PIM)?;
            let text = iuredesk_import::pim::unfold_quoted_printable(&String::from_utf8_lossy(&raw));
            let cards = iuredesk_dav::vcard::VCard::parse_all(&text)
                .map_err(|_| unreadable(path, "a contacts file", "un archivo de contactos"))?;
            if cards.is_empty() {
                return Err(unreadable(path, "a contacts file", "un archivo de contactos"));
            }
            let total = cards.len();
            let contacts =
                cards.iter().take(MAX_ITEMS).map(|v| iuredesk_pim_sync::map::vcard::contact_from_vcard(v).0).collect();
            Ok(Opened::Contacts(OpenedContacts { path: path.to_path_buf(), contacts, total }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets() {
        let cwd = Path::new("/home/ana");
        assert_eq!(parse_target("mailto:a@b.c", None), Some(OpenTarget::Mailto("mailto:a@b.c".into())));
        assert_eq!(
            parse_target("factura.EML", Some(cwd)),
            Some(OpenTarget::File { path: cwd.join("factura.EML"), kind: FileKind::Eml })
        );
        assert_eq!(
            parse_target("file:///tmp/mi%20cita.ics", None),
            Some(OpenTarget::File { path: PathBuf::from("/tmp/mi cita.ics"), kind: FileKind::Ics })
        );
        assert_eq!(
            parse_target("file:///C:/Users/Ana/tarjeta.vcf", None),
            Some(OpenTarget::File { path: PathBuf::from("C:/Users/Ana/tarjeta.vcf"), kind: FileKind::Vcf })
        );
        assert_eq!(parse_target("file://otro/x.eml", None), None);
        assert_eq!(parse_target("notas.txt", Some(cwd)), None);
        assert_eq!(parse_target("", None), None);
        assert_eq!(parse_target(&"a".repeat(MAX_ARG + 10), None), None);
    }

    #[test]
    fn links() {
        assert_eq!(
            parse_target("iuredesk://message/%3Cabc%40x.org%3E", None),
            Some(OpenTarget::Message("abc@x.org".into()))
        );
        assert_eq!(parse_target("IUREDESK://message/abc@x.org", None), Some(OpenTarget::Message("abc@x.org".into())));
        assert_eq!(parse_target("iuredesk://event/uid-1%2F2", None), Some(OpenTarget::Event("uid-1/2".into())));
        assert_eq!(
            parse_target("iuredesk://compose?to=ana%40x.org&subject=Hola", None),
            Some(OpenTarget::Mailto("mailto:ana%40x.org?subject=Hola".into()))
        );
        assert_eq!(parse_target("iuredesk://compose", None), Some(OpenTarget::Mailto("mailto:".into())));
        assert_eq!(parse_target("iuredesk://message/", None), None);
        assert_eq!(parse_target("iuredesk://borrar-todo", None), None);
        // Ida y vuelta.
        for t in [OpenTarget::Message("a+b/c@x".into()), OpenTarget::Event("é 1".into())] {
            assert_eq!(parse_target(&t.to_arg(), None), Some(t));
        }
        assert_eq!(message_link("<m1@x>"), "iuredesk://message/m1@x");
    }

    #[test]
    fn percent_hostil() {
        assert_eq!(percent_decode("%"), "%");
        assert_eq!(percent_decode("%zz%4"), "%zz%4");
        assert_eq!(percent_decode("%C3%A9"), "é");
        assert_eq!(percent_decode("%FF"), "\u{FFFD}");
    }

    #[test]
    fn archivos_rotos_o_grandes() {
        let dir = tempfile::tempdir().unwrap();
        let tz = chrono_tz::UTC;
        let missing = dir.path().join("no.eml");
        assert!(load(&missing, FileKind::Eml, &[], tz).is_err());
        let bad = dir.path().join("x.ics");
        std::fs::write(&bad, b"no es un calendario").unwrap();
        assert!(load(&bad, FileKind::Ics, &[], tz).is_err());
        let bad = dir.path().join("x.vcf");
        std::fs::write(&bad, b"\x00\x01\x02").unwrap();
        assert!(load(&bad, FileKind::Vcf, &[], tz).is_err());
        assert!(load(dir.path(), FileKind::Eml, &[], tz).is_err());
    }
}
