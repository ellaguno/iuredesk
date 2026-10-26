//! Tipos de las respuestas IMAP ya interpretadas.

use chrono::{DateTime, FixedOffset};

use crate::seqset::SeqSet;

/// Valor genérico de la gramática IMAP.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    /// Átomo (incluye números, banderas `\Seen` y nombres con sección `BODY[...]<0>`).
    Atom(String),
    /// Cadena entrecomillada o literal (bytes tal cual).
    Str(Vec<u8>),
    /// `NIL`.
    Nil,
    /// Lista entre paréntesis.
    List(Vec<Item>),
}

impl Item {
    /// Texto de un átomo o cadena (con pérdida si no es UTF-8).
    pub fn text(&self) -> Option<String> {
        match self {
            Item::Atom(a) => Some(a.clone()),
            Item::Str(s) => Some(String::from_utf8_lossy(s).into_owned()),
            _ => None,
        }
    }

    /// Bytes de una cadena, átomo, o `None` para `NIL`.
    pub fn nstring(&self) -> Option<Vec<u8>> {
        match self {
            Item::Atom(a) => Some(a.as_bytes().to_vec()),
            Item::Str(s) => Some(s.clone()),
            _ => None,
        }
    }

    /// Número sin signo (átomo o cadena de dígitos).
    pub fn number(&self) -> Option<u64> {
        match self {
            Item::Atom(a) if !a.is_empty() && a.len() <= 20 && a.bytes().all(|b| b.is_ascii_digit()) => a.parse().ok(),
            Item::Str(s) if !s.is_empty() && s.len() <= 20 && s.iter().all(u8::is_ascii_digit) => {
                std::str::from_utf8(s).ok()?.parse().ok()
            }
            _ => None,
        }
    }

    /// Elementos de una lista (vacío si no lo es).
    pub fn list(&self) -> &[Item] {
        match self {
            Item::List(l) => l,
            _ => &[],
        }
    }

    /// `true` si es `NIL`.
    pub fn is_nil(&self) -> bool {
        matches!(self, Item::Nil)
    }
}

/// Estado de una respuesta (`OK`, `NO`, `BAD`, `PREAUTH`, `BYE`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// Éxito.
    Ok,
    /// Fallo de la operación.
    No,
    /// Orden no válida.
    Bad,
    /// Saludo ya autenticado.
    PreAuth,
    /// El servidor cierra.
    Bye,
}

/// Código de respuesta entre corchetes (`[UIDVALIDITY 3]`…).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Code {
    /// `UIDVALIDITY`.
    UidValidity(u32),
    /// `UIDNEXT`.
    UidNext(u32),
    /// `UNSEEN` (número de secuencia del primer no leído).
    Unseen(u32),
    /// `HIGHESTMODSEQ` (CONDSTORE).
    HighestModSeq(u64),
    /// `NOMODSEQ`: el buzón no guarda modseq.
    NoModSeq,
    /// `PERMANENTFLAGS`.
    PermanentFlags(Vec<String>),
    /// `READ-ONLY`.
    ReadOnly,
    /// `READ-WRITE`.
    ReadWrite,
    /// `TRYCREATE`: el buzón destino no existe.
    TryCreate,
    /// `APPENDUID` (UIDPLUS).
    AppendUid {
        /// UIDVALIDITY del destino.
        uidvalidity: u32,
        /// UIDs asignados.
        uids: SeqSet,
    },
    /// `COPYUID` (UIDPLUS).
    CopyUid {
        /// UIDVALIDITY del destino.
        uidvalidity: u32,
        /// UIDs de origen.
        source: SeqSet,
        /// UIDs en el destino, en el mismo orden.
        dest: SeqSet,
    },
    /// `CAPABILITY` dentro de un código.
    Capability(Vec<String>),
    /// `CLOSED` (QRESYNC: se cerró el buzón anterior).
    Closed,
    /// `MODIFIED` (CONDSTORE: no se cambiaron estos mensajes).
    Modified(SeqSet),
    /// Cualquier otro (`ALERT`, `AUTHENTICATIONFAILED`, `UNAVAILABLE`, `LIMIT`…), en mayúsculas.
    Other(String, Option<String>),
}

/// Dirección de un `ENVELOPE`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Address {
    /// Nombre visible (puede venir codificado RFC 2047).
    pub name: Option<Vec<u8>>,
    /// Parte local.
    pub mailbox: Option<Vec<u8>>,
    /// Dominio.
    pub host: Option<Vec<u8>>,
}

impl Address {
    /// `buzón@dominio` (con pérdida si no es UTF-8).
    pub fn email(&self) -> String {
        let m = self.mailbox.as_deref().map(String::from_utf8_lossy).unwrap_or_default();
        match self.host.as_deref() {
            Some(h) if !h.is_empty() => format!("{m}@{}", String::from_utf8_lossy(h)),
            _ => m.into_owned(),
        }
    }
}

/// `ENVELOPE` de RFC 3501 (cadenas en bruto; la decodificación RFC 2047 la hace quien lo usa).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Envelope {
    /// `Date:`.
    pub date: Option<Vec<u8>>,
    /// `Subject:`.
    pub subject: Option<Vec<u8>>,
    /// `From:`.
    pub from: Vec<Address>,
    /// `Sender:`.
    pub sender: Vec<Address>,
    /// `Reply-To:`.
    pub reply_to: Vec<Address>,
    /// `To:`.
    pub to: Vec<Address>,
    /// `Cc:`.
    pub cc: Vec<Address>,
    /// `Bcc:`.
    pub bcc: Vec<Address>,
    /// `In-Reply-To:`.
    pub in_reply_to: Option<Vec<u8>>,
    /// `Message-ID:`.
    pub message_id: Option<Vec<u8>>,
}

/// `Content-Disposition` de una parte.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Disposition {
    /// `attachment`, `inline`… en minúsculas.
    pub kind: String,
    /// Parámetros (nombre en minúsculas).
    pub params: Vec<(String, String)>,
}

/// `BODYSTRUCTURE`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BodyStructure {
    /// Parte simple.
    Single {
        /// Tipo (`text`), en minúsculas.
        media_type: String,
        /// Subtipo (`plain`), en minúsculas.
        subtype: String,
        /// Parámetros (`charset`, `name`), nombre en minúsculas.
        params: Vec<(String, String)>,
        /// `Content-ID`.
        id: Option<String>,
        /// `Content-Transfer-Encoding`, en minúsculas.
        encoding: String,
        /// Tamaño codificado en bytes.
        size: u64,
        /// `Content-Disposition`.
        disposition: Option<Disposition>,
    },
    /// Parte `multipart/*`.
    Multi {
        /// Partes.
        parts: Vec<BodyStructure>,
        /// Subtipo (`mixed`, `alternative`), en minúsculas.
        subtype: String,
        /// `Content-Disposition`.
        disposition: Option<Disposition>,
    },
}

/// Adjunto deducido de un `BODYSTRUCTURE`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PartInfo {
    /// Nombre declarado (en bruto; puede venir codificado RFC 2047/2231).
    pub filename: String,
    /// `tipo/subtipo`.
    pub mime_type: String,
    /// Tamaño codificado (base64 ≈ 4/3 del real).
    pub size: u64,
    /// `Content-ID`.
    pub content_id: Option<String>,
    /// `true` si es en línea.
    pub inline: bool,
}

impl BodyStructure {
    /// Adjuntos (y partes en línea con nombre), recorriendo el árbol con tope.
    pub fn attachments(&self) -> Vec<PartInfo> {
        let mut out = Vec::new();
        self.walk(&mut out, 0);
        out
    }

    fn walk(&self, out: &mut Vec<PartInfo>, depth: usize) {
        if depth > 32 || out.len() >= 500 {
            return;
        }
        match self {
            BodyStructure::Multi { parts, .. } => {
                for p in parts {
                    p.walk(out, depth.saturating_add(1));
                }
            }
            BodyStructure::Single { media_type, subtype, params, id, size, disposition, .. } => {
                let disp = disposition.as_ref().map(|d| d.kind.as_str()).unwrap_or("");
                let name =
                    disposition.as_ref().and_then(|d| param(&d.params, "filename")).or_else(|| param(params, "name"));
                let is_body_text = media_type == "text" && (subtype == "plain" || subtype == "html");
                let attachment = disp == "attachment" || (name.is_some() && disp != "inline" && !is_body_text);
                let inline_named = disp == "inline" && name.is_some();
                if attachment || inline_named || (id.is_some() && media_type == "image") {
                    out.push(PartInfo {
                        filename: name.unwrap_or_default(),
                        mime_type: format!("{media_type}/{subtype}"),
                        size: *size,
                        content_id: id.clone(),
                        inline: !attachment,
                    });
                }
            }
        }
    }

    /// `true` si hay algún adjunto que no es en línea.
    pub fn has_attachments(&self) -> bool {
        self.attachments().iter().any(|a| !a.inline)
    }
}

fn param(params: &[(String, String)], key: &str) -> Option<String> {
    params.iter().find(|(k, _)| k == key || k.strip_suffix('*').is_some_and(|b| b == key)).map(|(_, v)| v.clone())
}

/// Sección de cuerpo devuelta por `FETCH` (`BODY[HEADER]`, `BODY[]<0>`…).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BodySection {
    /// Nombre completo del atributo en mayúsculas, sin `.PEEK` (`BODY[HEADER.FIELDS (REFERENCES)]`).
    pub name: String,
    /// Contenido (`None` si el servidor respondió `NIL`).
    pub data: Option<Vec<u8>>,
}

/// Datos de un `* n FETCH (...)`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FetchData {
    /// Número de secuencia.
    pub seq: u32,
    /// `UID`.
    pub uid: Option<u32>,
    /// `FLAGS` (tal cual, p. ej. `\Seen`, `$Forwarded`).
    pub flags: Option<Vec<String>>,
    /// `INTERNALDATE`.
    pub internal_date: Option<DateTime<FixedOffset>>,
    /// `RFC822.SIZE`.
    pub size: Option<u64>,
    /// `ENVELOPE`.
    pub envelope: Option<Envelope>,
    /// `BODYSTRUCTURE`.
    pub body_structure: Option<BodyStructure>,
    /// `MODSEQ`.
    pub modseq: Option<u64>,
    /// Secciones de cuerpo.
    pub sections: Vec<BodySection>,
    /// `X-GM-LABELS` (Gmail).
    pub gm_labels: Option<Vec<String>>,
    /// `X-GM-MSGID` (Gmail).
    pub gm_msgid: Option<u64>,
    /// `X-GM-THRID` (Gmail).
    pub gm_thrid: Option<u64>,
}

impl FetchData {
    /// Primera sección cuyo nombre empieza por `prefix` (`BODY[]`, `BODY[HEADER`).
    pub fn section(&self, prefix: &str) -> Option<&[u8]> {
        self.sections.iter().find(|s| s.name.starts_with(prefix)).and_then(|s| s.data.as_deref())
    }

    /// Mensaje completo (`BODY[]` o `RFC822`).
    pub fn full_body(&self) -> Option<&[u8]> {
        self.sections
            .iter()
            .find(|s| s.name == "BODY[]" || s.name == "RFC822" || s.name == "BINARY[]")
            .and_then(|s| s.data.as_deref())
    }
}

/// Una línea de `LIST`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ListEntry {
    /// Atributos (`\HasChildren`, `\Sent`…), tal cual.
    pub attributes: Vec<String>,
    /// Separador de jerarquía.
    pub delimiter: Option<char>,
    /// Nombre en bruto (UTF-7 modificado en IMAP4rev1).
    pub raw_name: Vec<u8>,
}

/// Respuesta de `STATUS`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MailboxStatus {
    /// Nombre en bruto.
    pub raw_name: Vec<u8>,
    /// `MESSAGES`.
    pub messages: Option<u32>,
    /// `UIDNEXT`.
    pub uid_next: Option<u32>,
    /// `UIDVALIDITY`.
    pub uid_validity: Option<u32>,
    /// `UNSEEN`.
    pub unseen: Option<u32>,
    /// `HIGHESTMODSEQ`.
    pub highest_modseq: Option<u64>,
    /// `RECENT`.
    pub recent: Option<u32>,
}

/// `* ESEARCH`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ESearch {
    /// Etiqueta de la orden (`TAG "a3"`).
    pub tag: Option<String>,
    /// Resultado en UIDs.
    pub uid: bool,
    /// `ALL`.
    pub all: Option<SeqSet>,
    /// `COUNT`.
    pub count: Option<u32>,
    /// `MIN`.
    pub min: Option<u32>,
    /// `MAX`.
    pub max: Option<u32>,
    /// `MODSEQ`.
    pub modseq: Option<u64>,
}

/// Respuesta sin etiqueta.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Untagged {
    /// `* OK/NO/BAD/BYE/PREAUTH [code] texto`.
    State {
        /// Estado.
        status: Status,
        /// Código.
        code: Option<Code>,
        /// Texto.
        text: String,
    },
    /// `* CAPABILITY`.
    Capability(Vec<String>),
    /// `* ENABLED`.
    Enabled(Vec<String>),
    /// `* LIST` / `* LSUB`.
    List(ListEntry),
    /// `* STATUS`.
    Status(MailboxStatus),
    /// `* n EXISTS`.
    Exists(u32),
    /// `* n RECENT`.
    Recent(u32),
    /// `* n EXPUNGE`.
    Expunge(u32),
    /// `* n FETCH`.
    Fetch(Box<FetchData>),
    /// `* SEARCH` (con `MODSEQ` opcional).
    Search(Vec<u32>, Option<u64>),
    /// `* ESEARCH`.
    ESearch(ESearch),
    /// `* VANISHED [(EARLIER)] uids`.
    Vanished {
        /// `(EARLIER)`: respuesta a QRESYNC/CHANGEDSINCE, no un borrado nuevo.
        earlier: bool,
        /// UIDs borrados.
        uids: SeqSet,
    },
    /// `* FLAGS`.
    Flags(Vec<String>),
    /// Cualquier otra (se ignora), recortada.
    Other(String),
}

/// Una respuesta completa del servidor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Response {
    /// `+ texto`.
    Continue(String),
    /// Sin etiqueta.
    Untagged(Untagged),
    /// Con etiqueta: fin de una orden.
    Tagged {
        /// Etiqueta.
        tag: String,
        /// Estado.
        status: Status,
        /// Código.
        code: Option<Code>,
        /// Texto.
        text: String,
    },
}
