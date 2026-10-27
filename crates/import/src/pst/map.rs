//! Elementos de Outlook (propiedades MAPI) → mensaje RFC 822, contacto, evento, tarea o nota.
//!
//! Funciones puras sobre [`PstItem`] (una vista neutral de las propiedades), así se prueban sin
//! archivos PST y no dependen del lector.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, TimeZone, Utc};
use chrono_tz::Tz;
use iuredesk_model::{
    Attendee, AttendeeRole, Contact, Event, EventStatus, EventTime, Flags, Labeled, Mailbox, Note, PartStat,
    PostalAddress, Reminder, Task, TaskStatus, Transparency,
};
use mail_builder::MessageBuilder;
use mail_builder::headers::address::Address;
use mail_builder::headers::content_type::ContentType;
use mail_builder::mime::{BodyPart, MimePart};
use sha2::{Digest, Sha256};

use super::{recur, rtf};

/// Valor de propiedad.
#[derive(Clone, Debug, PartialEq)]
pub enum Prop {
    /// Entero.
    Int(i64),
    /// Booleano.
    Bool(bool),
    /// `FILETIME` (100 ns desde 1601).
    Time(i64),
    /// Real.
    Float(f64),
    /// Texto.
    Str(String),
    /// Binario.
    Bin(Vec<u8>),
    /// Lista de textos.
    Strs(Vec<String>),
}

/// Conjunto de propiedades con nombre que se usan.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PropSet {
    /// `PSETID_Appointment` {00062002-0000-0000-C000-000000000046}.
    Appointment,
    /// `PSETID_Task` {00062003-…}.
    Task,
    /// `PSETID_Address` {00062004-…}.
    Address,
    /// `PSETID_Common` {00062008-…}.
    Common,
    /// `PSETID_Meeting` {6ED8DA90-450B-101B-98DA-00AA003F1305}.
    Meeting,
    /// `PS_PUBLIC_STRINGS` {00020329-…} (categorías: `Keywords`).
    PublicStrings,
}

/// Clave de una propiedad con nombre.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Named {
    /// Por número (LID).
    Id(PropSet, u32),
    /// Por nombre.
    Name(PropSet, String),
}

/// Destinatario.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Recipient {
    /// `PR_RECIPIENT_TYPE` (1 Para, 2 CC, 3 CCO/recurso).
    pub kind: i64,
    /// Nombre.
    pub name: String,
    /// Dirección SMTP (o la que haya).
    pub email: String,
    /// `PR_RECIPIENT_TRACKSTATUS`.
    pub track: i64,
}

/// Adjunto.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Attachment {
    /// Nombre de archivo.
    pub filename: String,
    /// Tipo MIME.
    pub mime: String,
    /// `Content-ID` (adjuntos en línea).
    pub content_id: Option<String>,
    /// Contenido.
    pub data: Vec<u8>,
    /// Mensaje incrustado (método 5), ya convertido.
    pub embedded: Option<Box<PstItem>>,
    /// Es la foto de un contacto.
    pub contact_photo: bool,
    /// Oculto (`PR_ATTACHMENT_HIDDEN`).
    pub hidden: bool,
}

/// Elemento de Outlook.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PstItem {
    /// `PR_MESSAGE_CLASS` (`IPM.Note`, `IPM.Contact`…).
    pub class: String,
    /// Propiedades por id.
    pub props: BTreeMap<u16, Prop>,
    /// Propiedades con nombre resueltas.
    pub named: HashMap<Named, Prop>,
    /// Destinatarios.
    pub recipients: Vec<Recipient>,
    /// Adjuntos.
    pub attachments: Vec<Attachment>,
}

// Ids de propiedades MAPI.
const PR_IMPORTANCE: u16 = 0x0017;
const PR_SUBJECT: u16 = 0x0037;
const PR_CLIENT_SUBMIT_TIME: u16 = 0x0039;
const PR_SENT_REPRESENTING_NAME: u16 = 0x0042;
const PR_SENT_REPRESENTING_EMAIL: u16 = 0x0065;
const PR_TRANSPORT_HEADERS: u16 = 0x007D;
const PR_SENDER_NAME: u16 = 0x0C1A;
const PR_SENDER_EMAIL: u16 = 0x0C1F;
const PR_MESSAGE_DELIVERY_TIME: u16 = 0x0E06;
const PR_MESSAGE_FLAGS: u16 = 0x0E07;
const PR_BODY: u16 = 0x1000;
const PR_RTF_COMPRESSED: u16 = 0x1009;
const PR_HTML: u16 = 0x1013;
const PR_INTERNET_MESSAGE_ID: u16 = 0x1035;
const PR_INTERNET_REFERENCES: u16 = 0x1039;
const PR_IN_REPLY_TO: u16 = 0x1042;
const PR_ICON_INDEX: u16 = 0x1080;
const PR_LAST_VERB: u16 = 0x1081;
const PR_FLAG_STATUS: u16 = 0x1090;
const PR_DISPLAY_NAME: u16 = 0x3001;
const PR_CREATION_TIME: u16 = 0x3007;
const PR_LAST_MODIFICATION_TIME: u16 = 0x3008;
const PR_INTERNET_CPID: u16 = 0x3FDE;
const PR_GIVEN_NAME: u16 = 0x3A06;
const PR_BUSINESS_PHONE: u16 = 0x3A08;
const PR_HOME_PHONE: u16 = 0x3A09;
const PR_SURNAME: u16 = 0x3A11;
const PR_COMPANY: u16 = 0x3A16;
const PR_TITLE: u16 = 0x3A17;
const PR_MOBILE_PHONE: u16 = 0x3A1C;
const PR_BUSINESS_FAX: u16 = 0x3A24;
const PR_BIRTHDAY: u16 = 0x3A42;
const PR_NICKNAME: u16 = 0x3A4F;
const PR_SENDER_SMTP: u16 = 0x5D01;
const PR_SENT_REPRESENTING_SMTP: u16 = 0x5D02;
/// (calle, ciudad, región, código postal, país) de las direcciones de trabajo y casa.
const ADDRESSES: [(&str, [u16; 5]); 2] =
    [("work", [0x3A29, 0x3A27, 0x3A28, 0x3A2A, 0x3A26]), ("home", [0x3A5D, 0x3A59, 0x3A5C, 0x3A5B, 0x3A5A])];

/// `FILETIME` → UTC.
pub fn filetime(ft: i64) -> Option<DateTime<Utc>> {
    // 100 ns desde 1601 → segundos desde 1970.
    let secs = ft.checked_div(10_000_000)?.checked_sub(11_644_473_600)?;
    let nanos = u32::try_from(ft.rem_euclid(10_000_000).checked_mul(100)?).ok()?;
    let t = Utc.timestamp_opt(secs, nanos).single()?;
    // Outlook usa 4501-01-01 como «sin fecha».
    (t.timestamp() > 0 && t.timestamp() < 32_503_680_000).then_some(t)
}

impl PstItem {
    /// Texto de una propiedad.
    pub fn str(&self, id: u16) -> Option<&str> {
        match self.props.get(&id) {
            Some(Prop::Str(s)) => Some(s.as_str()).filter(|s| !s.is_empty()),
            _ => None,
        }
    }

    fn int(&self, id: u16) -> Option<i64> {
        match self.props.get(&id) {
            Some(Prop::Int(i)) => Some(*i),
            Some(Prop::Bool(b)) => Some(i64::from(*b)),
            _ => None,
        }
    }

    fn time(&self, id: u16) -> Option<DateTime<Utc>> {
        match self.props.get(&id) {
            Some(Prop::Time(t)) => filetime(*t),
            _ => None,
        }
    }

    fn bin(&self, id: u16) -> Option<&[u8]> {
        match self.props.get(&id) {
            Some(Prop::Bin(b)) => Some(b),
            _ => None,
        }
    }

    fn named(&self, set: PropSet, lid: u32) -> Option<&Prop> {
        self.named.get(&Named::Id(set, lid))
    }

    fn named_str(&self, set: PropSet, lid: u32) -> Option<&str> {
        match self.named(set, lid) {
            Some(Prop::Str(s)) => Some(s.as_str()).filter(|s| !s.trim().is_empty()),
            _ => None,
        }
    }

    fn named_time(&self, set: PropSet, lid: u32) -> Option<DateTime<Utc>> {
        match self.named(set, lid) {
            Some(Prop::Time(t)) => filetime(*t),
            _ => None,
        }
    }

    fn named_bool(&self, set: PropSet, lid: u32) -> Option<bool> {
        match self.named(set, lid) {
            Some(Prop::Bool(b)) => Some(*b),
            Some(Prop::Int(i)) => Some(*i != 0),
            _ => None,
        }
    }

    fn named_int(&self, set: PropSet, lid: u32) -> Option<i64> {
        match self.named(set, lid) {
            Some(Prop::Int(i)) => Some(*i),
            Some(Prop::Bool(b)) => Some(i64::from(*b)),
            _ => None,
        }
    }

    /// Asunto (sin el prefijo de control `\x01\x05` que a veces guarda Outlook).
    pub fn subject(&self) -> String {
        let s = self.str(PR_SUBJECT).unwrap_or("");
        let s = if s.starts_with('\u{1}') { s.chars().skip(2).collect() } else { s.to_string() };
        s.trim_end_matches('\0').to_string()
    }

    /// Cuerpo de texto (o, en su defecto, el texto del RTF).
    pub fn body_text(&self) -> String {
        if let Some(b) = self.str(PR_BODY) {
            return b.to_string();
        }
        match self.rtf() {
            Some((t, false)) => t,
            _ => String::new(),
        }
    }

    fn rtf(&self) -> Option<(String, bool)> {
        let data = self.bin(PR_RTF_COMPRESSED)?;
        let raw = rtf::decompress(data, 32 * 1024 * 1024)?;
        Some(rtf::extract(&raw, 32 * 1024 * 1024))
    }

    /// Categorías (`Keywords` de `PS_PUBLIC_STRINGS`).
    pub fn categories(&self) -> Vec<String> {
        match self.named.get(&Named::Name(PropSet::PublicStrings, "Keywords".into())) {
            Some(Prop::Strs(v)) => v.iter().filter(|s| !s.trim().is_empty()).take(100).cloned().collect(),
            Some(Prop::Str(s)) if !s.trim().is_empty() => vec![s.clone()],
            _ => Vec::new(),
        }
    }

    /// Fecha del mensaje (envío, entrega o creación).
    pub fn date(&self) -> Option<DateTime<Utc>> {
        self.time(PR_CLIENT_SUBMIT_TIME)
            .or_else(|| self.time(PR_MESSAGE_DELIVERY_TIME))
            .or_else(|| self.time(PR_CREATION_TIME))
    }

    /// Fecha de recepción.
    pub fn received(&self) -> Option<DateTime<Utc>> {
        self.time(PR_MESSAGE_DELIVERY_TIME).or_else(|| self.date())
    }
}

fn smtp(s: Option<&str>) -> Option<&str> {
    s.filter(|s| s.contains('@') && !s.starts_with('/'))
}

/// Remitente (nombre, dirección).
fn sender(item: &PstItem) -> Option<(String, String)> {
    let email = smtp(item.str(PR_SENT_REPRESENTING_SMTP))
        .or_else(|| smtp(item.str(PR_SENDER_SMTP)))
        .or_else(|| smtp(item.str(PR_SENT_REPRESENTING_EMAIL)))
        .or_else(|| smtp(item.str(PR_SENDER_EMAIL)))?;
    let name = item.str(PR_SENT_REPRESENTING_NAME).or_else(|| item.str(PR_SENDER_NAME)).unwrap_or("");
    Some((name.to_string(), email.to_string()))
}

/// Banderas de un mensaje.
pub fn flags(item: &PstItem) -> Flags {
    let mf = item.int(PR_MESSAGE_FLAGS).unwrap_or(0);
    let verb = item.int(PR_LAST_VERB).unwrap_or(0);
    let icon = item.int(PR_ICON_INDEX).unwrap_or(0);
    Flags {
        seen: mf & 0x01 != 0,
        draft: mf & 0x08 != 0,
        flagged: item.int(PR_FLAG_STATUS) == Some(2),
        answered: matches!(verb, 102 | 103) || matches!(icon, 0x105),
        forwarded: verb == 104 || icon == 0x106,
        deleted: false,
    }
}

/// Juego de caracteres MIME de un código de página de Windows.
fn charset_of(cpid: i64) -> &'static str {
    match cpid {
        65001 => "utf-8",
        20127 => "us-ascii",
        1250 => "windows-1250",
        1251 => "windows-1251",
        1253 => "windows-1253",
        1254 => "windows-1254",
        1255 => "windows-1255",
        1256 => "windows-1256",
        1257 => "windows-1257",
        1258 => "windows-1258",
        28591 => "iso-8859-1",
        28592 => "iso-8859-2",
        28595 => "iso-8859-5",
        28597 => "iso-8859-7",
        28605 => "iso-8859-15",
        932 => "shift_jis",
        936 => "gbk",
        949 => "euc-kr",
        950 => "big5",
        50220 => "iso-2022-jp",
        51932 => "euc-jp",
        20866 => "koi8-r",
        21866 => "koi8-u",
        _ => "windows-1252",
    }
}

/// Cabeceras de transporte que se conservan (sin las MIME, que se regeneran).
fn transport_headers(raw: &str) -> String {
    let mut out = String::new();
    let mut keep = false;
    for line in raw.lines().take(5_000) {
        if line.is_empty() {
            break;
        }
        if line.starts_with([' ', '\t']) {
            if keep {
                out.push_str(line);
                out.push_str("\r\n");
            }
            continue;
        }
        let name = line.split(':').next().unwrap_or("").trim().to_ascii_lowercase();
        keep = line.contains(':')
            && !name.is_empty()
            && !name.starts_with("content-")
            && name != "mime-version"
            && !name.contains(' ');
        if keep {
            out.push_str(line);
            out.push_str("\r\n");
        }
        if out.len() > 1024 * 1024 {
            break;
        }
    }
    out
}

/// Cuerpo MIME (texto/HTML + adjuntos).
fn body_part(item: &PstItem, depth: usize) -> MimePart<'static> {
    let text = item.str(PR_BODY).map(str::to_string);
    let cpid = item.int(PR_INTERNET_CPID).unwrap_or(65001);
    let html: Option<MimePart<'static>> = match item.props.get(&PR_HTML) {
        Some(Prop::Bin(b)) if !b.is_empty() => Some(MimePart::new(
            ContentType::new("text/html").attribute("charset", charset_of(cpid)),
            BodyPart::Binary(b.clone().into()),
        )),
        Some(Prop::Str(s)) if !s.is_empty() => Some(MimePart::new("text/html", BodyPart::Text(s.clone().into()))),
        _ => None,
    };
    let (text, html) = match (text, html) {
        (None, None) => match item.rtf() {
            Some((h, true)) => (None, Some(MimePart::new("text/html", BodyPart::Text(h.into())))),
            Some((t, false)) => (Some(t), None),
            None => (Some(String::new()), None),
        },
        other => other,
    };
    let text = text.map(|t| MimePart::new("text/plain", BodyPart::Text(t.into())));
    let body = match (text, html) {
        (Some(t), Some(h)) => MimePart::new("multipart/alternative", vec![t, h]),
        (Some(t), None) => t,
        (None, Some(h)) => h,
        (None, None) => MimePart::new("text/plain", BodyPart::Text(String::new().into())),
    };
    let mut parts = vec![body];
    for a in &item.attachments {
        if parts.len() > 500 {
            break;
        }
        if let Some(e) = &a.embedded {
            if depth < 3 {
                let raw = to_rfc822(e, depth.saturating_add(1));
                let name = if a.filename.is_empty() { format!("{}.eml", e.subject()) } else { a.filename.clone() };
                parts.push(MimePart::new("message/rfc822", BodyPart::Binary(raw.into())).attachment(name));
            }
            continue;
        }
        let mime = if a.mime.contains('/') { a.mime.clone() } else { guess_mime(&a.filename).to_string() };
        let part = MimePart::new(ContentType::new(mime), BodyPart::Binary(a.data.clone().into()));
        let part = match &a.content_id {
            Some(cid) if !cid.is_empty() => part.inline().cid(cid.trim_matches(['<', '>']).to_string()),
            _ => part.attachment(if a.filename.is_empty() { "adjunto".to_string() } else { a.filename.clone() }),
        };
        parts.push(part);
    }
    if parts.len() == 1 {
        parts.pop().unwrap_or_else(|| MimePart::new("text/plain", BodyPart::Text(String::new().into())))
    } else {
        MimePart::new("multipart/mixed", parts)
    }
}

fn guess_mime(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "txt" => "text/plain",
        "htm" | "html" => "text/html",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "zip" => "application/zip",
        "ics" => "text/calendar",
        "vcf" => "text/vcard",
        "eml" => "message/rfc822",
        _ => "application/octet-stream",
    }
}

fn clean_id(s: &str) -> String {
    s.trim().trim_start_matches('<').trim_end_matches('>').to_string()
}

/// Mensaje RFC 822 a partir de un elemento. Si trae las cabeceras de transporte originales, se
/// conservan (con `Received`, firmas DKIM, listas…) y solo se regenera el MIME.
pub fn to_rfc822(item: &PstItem, depth: usize) -> Vec<u8> {
    let body = body_part(item, depth);
    if let Some(h) = item.str(PR_TRANSPORT_HEADERS) {
        let headers = transport_headers(h);
        if headers.to_ascii_lowercase().contains("from:") {
            let mut out = headers.into_bytes();
            out.extend_from_slice(b"MIME-Version: 1.0\r\n");
            if body.clone().write_part(&mut out).is_ok() {
                return out;
            }
        }
    }
    let mut b = MessageBuilder::new().subject(item.subject());
    if let Some((name, email)) = sender(item) {
        b = b.from(Address::new_address(Some(name).filter(|n| !n.is_empty()), email));
    }
    let list = |kind: i64| -> Vec<Address<'static>> {
        item.recipients
            .iter()
            .filter(|r| r.kind == kind && !r.email.is_empty())
            .take(500)
            .map(|r| Address::new_address(Some(r.name.clone()).filter(|n| !n.is_empty()), r.email.clone()))
            .collect()
    };
    for (kind, set) in [(1, 0), (2, 1), (3, 2)] {
        let l = list(kind);
        if l.is_empty() {
            continue;
        }
        let a = Address::new_list(l);
        b = match set {
            0 => b.to(a),
            1 => b.cc(a),
            _ => b.bcc(a),
        };
    }
    if let Some(d) = item.date() {
        b = b.date(d.timestamp());
    }
    let mid = item.str(PR_INTERNET_MESSAGE_ID).map(clean_id).filter(|m| !m.is_empty()).unwrap_or_else(|| {
        // Determinista: el mismo elemento da el mismo Message-ID al reimportar.
        format!("{}@pst.iuredesk.invalid", dedup_key(item).trim_start_matches("imp:"))
    });
    b = b.message_id(mid);
    if let Some(r) = item.str(PR_IN_REPLY_TO).map(clean_id).filter(|m| !m.is_empty()) {
        b = b.in_reply_to(r);
    }
    if let Some(refs) = item.str(PR_INTERNET_REFERENCES) {
        let ids: Vec<String> = refs.split_whitespace().map(clean_id).filter(|m| !m.is_empty()).take(100).collect();
        if !ids.is_empty() {
            b = b.references(ids);
        }
    }
    b = b.body(body);
    b.write_to_vec().unwrap_or_default()
}

/// Clave de deduplicación de un mensaje de PST (estable entre importaciones).
pub fn dedup_key(item: &PstItem) -> String {
    let mut h = Sha256::new();
    h.update(b"pst\0");
    h.update(item.str(PR_INTERNET_MESSAGE_ID).map(clean_id).unwrap_or_default().as_bytes());
    h.update(b"\0");
    h.update(item.subject().as_bytes());
    h.update(b"\0");
    h.update(item.date().map(|d| d.timestamp()).unwrap_or(0).to_le_bytes());
    let body = item.body_text();
    h.update(body.get(..body.len().min(64 * 1024)).unwrap_or("").as_bytes());
    for a in &item.attachments {
        h.update(a.filename.as_bytes());
        h.update((a.data.len() as u64).to_le_bytes());
    }
    let d = h.finalize();
    let mut s = String::from("imp:");
    for b in d.iter().take(20) {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Contacto (`IPM.Contact`) y su foto.
pub fn contact(item: &PstItem) -> (Contact, Option<Vec<u8>>) {
    let s = |id| item.str(id).unwrap_or("").trim().to_string();
    let mut c = Contact {
        full_name: s(PR_DISPLAY_NAME),
        given_name: s(PR_GIVEN_NAME),
        family_name: s(PR_SURNAME),
        nickname: s(PR_NICKNAME),
        org: s(PR_COMPANY),
        title: s(PR_TITLE),
        notes: item.body_text().trim().to_string(),
        birthday: item.time(PR_BIRTHDAY).map(|t| t.date_naive()),
        updated: item.time(PR_LAST_MODIFICATION_TIME),
        ..Default::default()
    };
    if c.full_name.is_empty() {
        c.full_name = item.subject();
    }
    for (lid, label) in [(0x8083u32, "work"), (0x8093, "home"), (0x80A3, "other")] {
        if let Some(e) = smtp(item.named_str(PropSet::Address, lid)) {
            c.emails.push(Labeled::new(label, e));
        }
    }
    for (id, label) in
        [(PR_BUSINESS_PHONE, "work"), (PR_HOME_PHONE, "home"), (PR_MOBILE_PHONE, "cell"), (PR_BUSINESS_FAX, "fax")]
    {
        let v = s(id);
        if !v.is_empty() {
            c.phones.push(Labeled::new(label, v));
        }
    }
    for (label, ids) in ADDRESSES {
        let [street, city, region, postal, country] = ids.map(s);
        if !(street.is_empty() && city.is_empty() && postal.is_empty() && country.is_empty()) {
            c.addresses.push(PostalAddress { label: label.into(), street, city, region, postal_code: postal, country });
        }
    }
    let photo = item.attachments.iter().find(|a| a.contact_photo).map(|a| a.data.clone());
    (c, photo)
}

/// Nombre de zona de Windows de un `TZDEFINITION` (`PidLidAppointmentTimeZoneDefinitionStartDisplay`).
pub fn tzdef_key_name(b: &[u8]) -> Option<String> {
    // MajorVersion(1) MinorVersion(1) cbHeader(2) Reserved(2) cchKeyName(2) KeyName(UTF-16).
    if b.first() != Some(&2) {
        return None;
    }
    let cch = usize::from(u16::from_le_bytes(b.get(6..8)?.try_into().ok()?));
    let bytes = b.get(8..8usize.checked_add(cch.checked_mul(2)?)?)?;
    let units: Vec<u16> = bytes.chunks_exact(2).filter_map(|c| c.try_into().ok().map(u16::from_le_bytes)).collect();
    Some(String::from_utf16_lossy(&units))
}

/// Zona IANA de una cita, si se conoce.
fn appointment_tz(item: &PstItem) -> Option<(Tz, String)> {
    for lid in [0x825E_u32, 0x825F, 0x8233] {
        if let Some(Prop::Bin(b)) = item.named(PropSet::Appointment, lid)
            && let Some(name) = tzdef_key_name(b)
            && let Some(iana) = iuredesk_dav::tz::windows_to_iana(&name)
            && let Ok(tz) = iana.parse::<Tz>()
        {
            return Some((tz, iana.to_string()));
        }
    }
    None
}

/// Evento (`IPM.Appointment`) y cuántas repeticiones modificadas no se importaron.
pub fn event(item: &PstItem, user_tz: Tz) -> (Event, usize) {
    let start = item.named_time(PropSet::Appointment, 0x820D).or_else(|| item.time(0x0060));
    let end = item.named_time(PropSet::Appointment, 0x820E).or_else(|| item.time(0x0061)).or(start);
    let all_day = item.named_bool(PropSet::Appointment, 0x8215).unwrap_or(false);
    let zone = appointment_tz(item);
    let tz = zone.as_ref().map_or(user_tz, |(t, _)| *t);
    let to_time = |t: Option<DateTime<Utc>>| -> EventTime {
        let t = t.unwrap_or(DateTime::<Utc>::UNIX_EPOCH);
        if all_day {
            EventTime::Date(t.with_timezone(&tz).date_naive())
        } else {
            EventTime::DateTime { utc: t, tzid: zone.as_ref().map(|(_, n)| n.clone()) }
        }
    };
    let busy = item.named_int(PropSet::Appointment, 0x8205).unwrap_or(2);
    let mut ev = Event {
        uid: global_uid(item).unwrap_or_default(),
        summary: item.subject(),
        location: item.named_str(PropSet::Appointment, 0x8208).unwrap_or("").to_string(),
        description: item.body_text().trim().to_string(),
        start: to_time(start),
        end: to_time(end),
        status: if busy == 1 { EventStatus::Tentative } else { EventStatus::Confirmed },
        transparency: if busy == 0 { Transparency::Transparent } else { Transparency::Opaque },
        updated: item.time(PR_LAST_MODIFICATION_TIME),
        ..Default::default()
    };
    if let Some((name, email)) = sender(item)
        && !item.recipients.is_empty()
    {
        ev.organizer = Some(Mailbox::new(name, email));
    }
    for r in item.recipients.iter().filter(|r| !r.email.is_empty()).take(1_000) {
        if ev.organizer.as_ref().is_some_and(|o| o.addr.eq_ignore_ascii_case(&r.email)) {
            continue;
        }
        ev.attendees.push(Attendee {
            mailbox: Mailbox::new(r.name.clone(), r.email.clone()),
            partstat: match r.track {
                2 => PartStat::Tentative,
                3 => PartStat::Accepted,
                4 => PartStat::Declined,
                _ => PartStat::NeedsAction,
            },
            role: match r.kind {
                2 => AttendeeRole::Optional,
                3 => AttendeeRole::NonParticipant,
                _ => AttendeeRole::Required,
            },
            rsvp: false,
        });
    }
    if item.named_bool(PropSet::Common, 0x8503) == Some(true) {
        ev.reminders.push(Reminder { minutes_before: item.named_int(PropSet::Common, 0x8501).unwrap_or(15) });
    }
    let mut modified = 0;
    if item.named_bool(PropSet::Appointment, 0x8223) == Some(true)
        && let Some(Prop::Bin(b)) = item.named(PropSet::Appointment, 0x8216)
        && let Some(r) = recur::parse(b, all_day, tz)
    {
        ev.rrule = Some(r.rrule);
        ev.exdates = r.exdates;
        modified = r.modified;
    }
    (ev, modified)
}

/// `UID` de iCalendar a partir de `PidLidGlobalObjectId` (como lo exporta Outlook: hexadecimal).
fn global_uid(item: &PstItem) -> Option<String> {
    let b = match item.named(PropSet::Meeting, 0x0023).or_else(|| item.named(PropSet::Meeting, 0x0003)) {
        Some(Prop::Bin(b)) if !b.is_empty() => b,
        _ => return None,
    };
    Some(b.iter().take(256).map(|x| format!("{x:02X}")).collect())
}

/// Tarea (`IPM.Task`).
pub fn task(item: &PstItem) -> Task {
    let status = match item.named_int(PropSet::Task, 0x8101) {
        Some(1) => TaskStatus::InProcess,
        Some(2) => TaskStatus::Completed,
        _ => TaskStatus::NeedsAction,
    };
    let complete = item.named_bool(PropSet::Task, 0x811C) == Some(true) || status == TaskStatus::Completed;
    let percent = match item.named(PropSet::Task, 0x8102) {
        Some(Prop::Float(f)) if f.is_finite() => (f * 100.0).clamp(0.0, 100.0) as u8,
        _ => 0,
    };
    Task {
        summary: item.subject(),
        description: item.body_text().trim().to_string(),
        due: item.named_time(PropSet::Task, 0x8105).map(|t| EventTime::Date(t.date_naive())),
        priority: match item.int(PR_IMPORTANCE) {
            Some(2) => 1,
            Some(0) => 9,
            _ => 0,
        },
        status: if complete { TaskStatus::Completed } else { status },
        percent_complete: if complete { 100 } else { percent },
        completed: if complete {
            item.named_time(PropSet::Task, 0x810F).or_else(|| item.time(PR_LAST_MODIFICATION_TIME))
        } else {
            None
        },
        created: item.time(PR_CREATION_TIME),
        updated: item.time(PR_LAST_MODIFICATION_TIME),
        ..Default::default()
    }
}

/// Nota (`IPM.StickyNote`).
pub fn note(item: &PstItem) -> Note {
    let created = item.time(PR_CREATION_TIME).unwrap_or(DateTime::<Utc>::UNIX_EPOCH);
    let body = item.body_text();
    let title = {
        let s = item.subject();
        if s.trim().is_empty() { body.lines().next().unwrap_or("").chars().take(100).collect() } else { s }
    };
    Note {
        title,
        body,
        tags: item.categories(),
        created,
        updated: item.time(PR_LAST_MODIFICATION_TIME).unwrap_or(created),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ft(t: DateTime<Utc>) -> Prop {
        Prop::Time((t.timestamp() + 11_644_473_600) * 10_000_000)
    }

    fn base() -> PstItem {
        let mut i = PstItem { class: "IPM.Note".into(), ..Default::default() };
        i.props.insert(PR_SUBJECT, Prop::Str("\u{1}\u{5}RE: Presupuesto".into()));
        i.props.insert(PR_SENT_REPRESENTING_NAME, Prop::Str("Ana Ejemplo".into()));
        i.props.insert(PR_SENT_REPRESENTING_EMAIL, Prop::Str("/O=EXCHANGELABS/OU=X/CN=ANA".into()));
        i.props.insert(PR_SENT_REPRESENTING_SMTP, Prop::Str("ana@example.com".into()));
        i.props.insert(PR_BODY, Prop::Str("Hola, va el presupuesto.".into()));
        i.props.insert(PR_INTERNET_MESSAGE_ID, Prop::Str("<p1@example.com>".into()));
        i.props.insert(PR_CLIENT_SUBMIT_TIME, ft(Utc.with_ymd_and_hms(2025, 5, 4, 10, 0, 0).unwrap()));
        i.props.insert(PR_MESSAGE_FLAGS, Prop::Int(0x01));
        i.props.insert(PR_FLAG_STATUS, Prop::Int(2));
        i.props.insert(PR_LAST_VERB, Prop::Int(102));
        i.recipients.push(Recipient { kind: 1, name: "Luis".into(), email: "luis@example.com".into(), track: 0 });
        i.recipients.push(Recipient { kind: 2, name: String::new(), email: "cc@example.com".into(), track: 0 });
        i
    }

    #[test]
    fn filetimes() {
        let t = filetime(133_750_080_000_000_000).unwrap();
        assert_eq!(t.to_rfc3339(), "2024-11-02T08:00:00+00:00");
        assert!(filetime(0).is_none());
        assert!(filetime(i64::MAX).is_none());
        assert!(filetime(-5).is_none());
    }

    #[test]
    fn message_without_transport_headers() {
        let mut i = base();
        i.attachments.push(Attachment { filename: "factura.pdf".into(), data: b"%PDF".to_vec(), ..Default::default() });
        let raw = to_rfc822(&i, 0);
        let (env, text) = iuredesk_sync::parse::envelope_from_raw(&raw, iuredesk_model::FolderId(1), Utc::now());
        assert_eq!(env.subject, "RE: Presupuesto");
        assert_eq!(env.from[0].addr, "ana@example.com");
        assert_eq!(env.to[0].addr, "luis@example.com");
        assert_eq!(env.cc[0].addr, "cc@example.com");
        assert_eq!(env.message_id.as_deref(), Some("p1@example.com"));
        assert_eq!(env.date.to_rfc3339(), "2025-05-04T10:00:00+00:00");
        assert!(text.contains("presupuesto"));
        assert_eq!(env.attachments[0].filename, "factura.pdf");
        assert_eq!(env.attachments[0].mime_type, "application/pdf");
        let f = flags(&i);
        assert!(f.seen && f.flagged && f.answered && !f.draft);
        // Estable al reimportar.
        assert_eq!(dedup_key(&i), dedup_key(&i.clone()));
    }

    #[test]
    fn message_with_transport_headers_and_cp1252_html() {
        let mut i = base();
        i.props.remove(&PR_BODY);
        i.props.insert(
            PR_TRANSPORT_HEADERS,
            Prop::Str("Received: from mx\r\n by x; Mon, 1 Jan 2024 00:00:00 +0000\r\nFrom: Ana <ana@example.com>\r\nTo: luis@example.com\r\nSubject: Original\r\nMessage-ID: <orig@x>\r\nContent-Type: text/plain\r\nMIME-Version: 1.0\r\n\r\n".into()),
        );
        i.props.insert(PR_HTML, Prop::Bin(b"<p>Se\xf1or</p>".to_vec()));
        i.props.insert(PR_INTERNET_CPID, Prop::Int(1252));
        let raw = to_rfc822(&i, 0);
        let s = String::from_utf8_lossy(&raw);
        assert!(s.starts_with("Received: from mx\r\n by x;"));
        assert_eq!(s.matches("MIME-Version").count(), 1);
        let m = mail_parser::MessageParser::default().parse(&raw).unwrap();
        assert_eq!(m.subject(), Some("Original"));
        assert!(m.body_html(0).unwrap().contains("Señor"));
    }

    #[test]
    fn contact_mapping() {
        let mut i = PstItem { class: "IPM.Contact".into(), ..Default::default() };
        i.props.insert(PR_DISPLAY_NAME, Prop::Str("Luis Ejemplo".into()));
        i.props.insert(PR_GIVEN_NAME, Prop::Str("Luis".into()));
        i.props.insert(PR_MOBILE_PHONE, Prop::Str("+52 55 0000".into()));
        i.props.insert(0x3A27, Prop::Str("Monterrey".into()));
        i.named.insert(Named::Id(PropSet::Address, 0x8083), Prop::Str("luis@example.com".into()));
        i.named.insert(Named::Id(PropSet::Address, 0x8093), Prop::Str("/o=ex/cn=luis".into()));
        i.attachments.push(Attachment { contact_photo: true, data: vec![0xFF, 0xD8], ..Default::default() });
        let (c, photo) = contact(&i);
        assert_eq!(c.full_name, "Luis Ejemplo");
        assert_eq!(c.emails, vec![Labeled::new("work", "luis@example.com")]);
        assert_eq!(c.phones[0].label, "cell");
        assert_eq!(c.addresses[0].city, "Monterrey");
        assert_eq!(photo, Some(vec![0xFF, 0xD8]));
    }

    #[test]
    fn appointment_mapping() {
        let mut i = PstItem { class: "IPM.Appointment".into(), ..Default::default() };
        i.props.insert(PR_SUBJECT, Prop::Str("Comité".into()));
        let s = Utc.with_ymd_and_hms(2026, 1, 5, 15, 0, 0).unwrap();
        i.named.insert(Named::Id(PropSet::Appointment, 0x820D), ft(s));
        i.named.insert(Named::Id(PropSet::Appointment, 0x820E), ft(s + chrono::Duration::hours(1)));
        i.named.insert(Named::Id(PropSet::Appointment, 0x8208), Prop::Str("Sala 3".into()));
        i.named.insert(Named::Id(PropSet::Appointment, 0x8205), Prop::Int(0));
        i.named.insert(Named::Id(PropSet::Meeting, 0x0003), Prop::Bin(vec![0x04, 0x00, 0xAB]));
        let mut tzdef = vec![2u8, 1, 0, 0, 2, 0];
        let name: Vec<u16> = "Central Standard Time (Mexico)".encode_utf16().collect();
        tzdef.extend((name.len() as u16).to_le_bytes());
        for u in name {
            tzdef.extend(u.to_le_bytes());
        }
        i.named.insert(Named::Id(PropSet::Appointment, 0x825E), Prop::Bin(tzdef));
        i.named.insert(Named::Id(PropSet::Appointment, 0x8223), Prop::Bool(true));
        let start = recur::tests::mins(2026, 1, 5);
        let del = [recur::tests::mins(2026, 1, 12)];
        i.named.insert(
            Named::Id(PropSet::Appointment, 0x8216),
            Prop::Bin(recur::tests::pattern(0x200B, 1, 1, &[0b10], 0x2022, 4, &del, &del, start, start, 540)),
        );
        i.named.insert(Named::Id(PropSet::Common, 0x8503), Prop::Bool(true));
        i.named.insert(Named::Id(PropSet::Common, 0x8501), Prop::Int(10));
        let (e, modified) = event(&i, chrono_tz::UTC);
        assert_eq!(e.summary, "Comité");
        assert_eq!(e.location, "Sala 3");
        assert_eq!(e.uid, "0400AB");
        assert_eq!(e.transparency, Transparency::Transparent);
        assert_eq!(e.start, EventTime::DateTime { utc: s, tzid: Some("America/Mexico_City".into()) });
        assert_eq!(e.rrule.as_deref(), Some("FREQ=WEEKLY;BYDAY=MO;COUNT=4"));
        assert!(e.exdates.is_empty());
        assert_eq!(modified, 1);
        assert_eq!(e.reminders, vec![Reminder { minutes_before: 10 }]);

        // Día completo: la fecha local de la zona de la cita.
        i.named.insert(Named::Id(PropSet::Appointment, 0x8215), Prop::Bool(true));
        let midnight_mx = Utc.with_ymd_and_hms(2026, 1, 5, 6, 0, 0).unwrap();
        i.named.insert(Named::Id(PropSet::Appointment, 0x820D), ft(midnight_mx));
        let (e, _) = event(&i, chrono_tz::UTC);
        assert_eq!(e.start, EventTime::Date(chrono::NaiveDate::from_ymd_opt(2026, 1, 5).unwrap()));
    }

    #[test]
    fn task_and_note() {
        let mut i = PstItem { class: "IPM.Task".into(), ..Default::default() };
        i.props.insert(PR_SUBJECT, Prop::Str("Enviar informe".into()));
        i.props.insert(PR_IMPORTANCE, Prop::Int(2));
        i.named.insert(Named::Id(PropSet::Task, 0x8101), Prop::Int(1));
        i.named.insert(Named::Id(PropSet::Task, 0x8102), Prop::Float(0.5));
        let t = task(&i);
        assert_eq!((t.status.clone(), t.percent_complete, t.priority), (TaskStatus::InProcess, 50, 1));
        i.named.insert(Named::Id(PropSet::Task, 0x811C), Prop::Bool(true));
        assert!(task(&i).is_done());

        let mut n = PstItem { class: "IPM.StickyNote".into(), ..Default::default() };
        n.props.insert(PR_BODY, Prop::Str("Comprar café\nmañana".into()));
        n.named.insert(Named::Name(PropSet::PublicStrings, "Keywords".into()), Prop::Strs(vec!["Casa".into()]));
        let note = note(&n);
        assert_eq!(note.title, "Comprar café");
        assert_eq!(note.tags, vec!["Casa".to_string()]);
    }
}
