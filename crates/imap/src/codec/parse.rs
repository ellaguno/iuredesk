//! Interpretación de respuestas IMAP completas (una línea lógica con sus literales dentro).
//!
//! Todo es hostil: profundidad de listas acotada, longitudes de literal comprobadas contra el
//! búfer, números con tope de dígitos y ningún índice directo. Se es tolerante con los desvíos
//! comunes (minúsculas, LF sin CR, espacios dobles) y una respuesta que no se entiende se
//! devuelve como [`Untagged::Other`] en vez de abortar la sesión.

use chrono::{DateTime, FixedOffset};

use super::types::*;
use crate::seqset::SeqSet;

/// Profundidad máxima de listas anidadas.
pub const MAX_DEPTH: usize = 64;
/// Elementos máximos de una lista.
const MAX_LIST_ITEMS: usize = 2_000_000;
/// Caracteres máximos de un texto de estado que se conservan.
const MAX_TEXT: usize = 1_000;
/// Direcciones máximas por campo de un `ENVELOPE`.
const MAX_ADDRESSES: usize = 500;
/// Parámetros máximos por parte.
const MAX_PARAMS: usize = 100;
/// Partes máximas por `multipart`.
const MAX_PARTS: usize = 1_000;

/// Respuesta que no respeta la gramática.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("respuesta IMAP no válida: {0}")]
pub struct ParseError(pub String);

fn err<T>(m: &str) -> Result<T, ParseError> {
    Err(ParseError(m.to_string()))
}

/// Cursor sobre los bytes de una respuesta.
pub struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    /// Cursor al inicio de `buf`.
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn peek(&self) -> Option<u8> {
        self.buf.get(self.pos).copied()
    }

    fn peek_at(&self, off: usize) -> Option<u8> {
        self.buf.get(self.pos.saturating_add(off)).copied()
    }

    /// `true` si ya no quedan bytes (salvo espacios y fin de línea).
    pub fn at_end(&self) -> bool {
        self.buf.get(self.pos..).unwrap_or_default().iter().all(|b| matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
    }

    /// Salta espacios.
    pub fn skip_sp(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t')) {
            self.pos = self.pos.saturating_add(1);
        }
    }

    /// Resto de la línea como texto recortado.
    pub fn rest_text(&mut self) -> String {
        let rest = self.buf.get(self.pos..).unwrap_or_default();
        self.pos = self.buf.len();
        let s = String::from_utf8_lossy(rest);
        s.trim_end_matches(['\r', '\n']).chars().take(MAX_TEXT).collect()
    }

    /// Palabra hasta el siguiente espacio (etiqueta, palabra clave).
    pub fn word(&mut self) -> String {
        let start = self.pos;
        while let Some(b) = self.peek() {
            if matches!(b, b' ' | b'\t' | b'\r' | b'\n') {
                break;
            }
            self.pos = self.pos.saturating_add(1);
        }
        String::from_utf8_lossy(self.buf.get(start..self.pos).unwrap_or_default()).into_owned()
    }

    /// Un valor: lista, cadena, literal, `NIL` o átomo.
    pub fn item(&mut self, depth: usize) -> Result<Item, ParseError> {
        if depth > MAX_DEPTH {
            return err("listas demasiado anidadas");
        }
        self.skip_sp();
        match self.peek() {
            None => err("faltan datos"),
            Some(b'(') => {
                self.pos = self.pos.saturating_add(1);
                let mut items = Vec::new();
                loop {
                    self.skip_sp();
                    match self.peek() {
                        None => return err("lista sin cerrar"),
                        Some(b')') => {
                            self.pos = self.pos.saturating_add(1);
                            return Ok(Item::List(items));
                        }
                        Some(b'\r' | b'\n') => return err("lista sin cerrar"),
                        Some(_) => {
                            if items.len() >= MAX_LIST_ITEMS {
                                return err("lista demasiado larga");
                            }
                            items.push(self.item(depth.saturating_add(1))?);
                        }
                    }
                }
            }
            Some(b'"') => self.quoted().map(Item::Str),
            Some(b'{') => self.literal().map(Item::Str),
            Some(b'~') if self.peek_at(1) == Some(b'{') => {
                self.pos = self.pos.saturating_add(1);
                self.literal().map(Item::Str)
            }
            Some(b')') => err("paréntesis de cierre inesperado"),
            Some(_) => {
                let a = self.atom()?;
                if a.eq_ignore_ascii_case("NIL") { Ok(Item::Nil) } else { Ok(Item::Atom(a)) }
            }
        }
    }

    fn quoted(&mut self) -> Result<Vec<u8>, ParseError> {
        self.pos = self.pos.saturating_add(1);
        let mut out = Vec::new();
        loop {
            match self.peek() {
                None => return err("cadena sin cerrar"),
                Some(b'"') => {
                    self.pos = self.pos.saturating_add(1);
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.pos = self.pos.saturating_add(1);
                    match self.peek() {
                        Some(c) => {
                            out.push(c);
                            self.pos = self.pos.saturating_add(1);
                        }
                        None => return err("cadena sin cerrar"),
                    }
                }
                Some(c) => {
                    out.push(c);
                    self.pos = self.pos.saturating_add(1);
                }
            }
        }
    }

    fn literal(&mut self) -> Result<Vec<u8>, ParseError> {
        // `{` dígitos [`+`|`-`] `}` CRLF bytes
        self.pos = self.pos.saturating_add(1);
        let start = self.pos;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.pos = self.pos.saturating_add(1);
        }
        let digits = self.buf.get(start..self.pos).unwrap_or_default();
        if digits.is_empty() || digits.len() > 12 {
            return err("longitud de literal no válida");
        }
        let n: usize = std::str::from_utf8(digits)
            .ok()
            .and_then(|d| d.parse().ok())
            .ok_or_else(|| ParseError("longitud de literal no válida".into()))?;
        if matches!(self.peek(), Some(b'+' | b'-')) {
            self.pos = self.pos.saturating_add(1);
        }
        if self.peek() != Some(b'}') {
            return err("literal sin `}`");
        }
        self.pos = self.pos.saturating_add(1);
        if self.peek() == Some(b'\r') {
            self.pos = self.pos.saturating_add(1);
        }
        if self.peek() != Some(b'\n') {
            return err("literal sin fin de línea");
        }
        self.pos = self.pos.saturating_add(1);
        let end = self.pos.checked_add(n).ok_or_else(|| ParseError("literal demasiado largo".into()))?;
        let data = self.buf.get(self.pos..end).ok_or_else(|| ParseError("literal truncado".into()))?;
        self.pos = end;
        Ok(data.to_vec())
    }

    fn atom(&mut self) -> Result<String, ParseError> {
        let start = self.pos;
        let mut brackets = 0usize;
        while let Some(b) = self.peek() {
            if brackets > 0 {
                match b {
                    b']' => brackets = brackets.saturating_sub(1),
                    b'[' => brackets = brackets.saturating_add(1),
                    b'\r' | b'\n' => break,
                    _ => {}
                }
            } else {
                match b {
                    b' ' | b'\t' | b'(' | b')' | b'\r' | b'\n' | b'"' => break,
                    b'[' => brackets = 1,
                    _ => {}
                }
            }
            self.pos = self.pos.saturating_add(1);
        }
        if self.pos == start {
            return err("se esperaba un átomo");
        }
        Ok(String::from_utf8_lossy(self.buf.get(start..self.pos).unwrap_or_default()).into_owned())
    }

    /// Código entre corchetes, si lo hay en la posición actual.
    fn code(&mut self) -> Option<Code> {
        self.skip_sp();
        if self.peek() != Some(b'[') {
            return None;
        }
        let start = self.pos.saturating_add(1);
        let mut i = start;
        let mut in_quote = false;
        let mut depth = 0usize;
        loop {
            let b = *self.buf.get(i)?;
            match b {
                b'"' => in_quote = !in_quote,
                b'\\' if in_quote => i = i.saturating_add(1),
                b'[' if !in_quote => depth = depth.saturating_add(1),
                b']' if !in_quote => {
                    if depth == 0 {
                        break;
                    }
                    depth = depth.saturating_sub(1);
                }
                b'\r' | b'\n' => return None,
                _ => {}
            }
            i = i.saturating_add(1);
        }
        let inner = self.buf.get(start..i)?;
        self.pos = i.saturating_add(1);
        Some(parse_code(inner))
    }
}

/// Interpreta el contenido de un código de respuesta (sin corchetes).
pub fn parse_code(inner: &[u8]) -> Code {
    let mut c = Cursor::new(inner);
    let name = c.word().to_ascii_uppercase();
    c.skip_sp();
    let num32 = |c: &mut Cursor<'_>| c.item(0).ok().and_then(|i| i.number()).and_then(|n| u32::try_from(n).ok());
    let seq = |c: &mut Cursor<'_>| c.item(0).ok().and_then(|i| i.text()).and_then(|t| SeqSet::parse(&t));
    let other = |c: &mut Cursor<'_>, name: String| {
        let rest = c.rest_text();
        Code::Other(name, if rest.is_empty() { None } else { Some(rest) })
    };
    match name.as_str() {
        "UIDVALIDITY" => num32(&mut c).map_or_else(|| Code::Other(name.clone(), None), Code::UidValidity),
        "UIDNEXT" => num32(&mut c).map_or_else(|| Code::Other(name.clone(), None), Code::UidNext),
        "UNSEEN" => num32(&mut c).map_or_else(|| Code::Other(name.clone(), None), Code::Unseen),
        "HIGHESTMODSEQ" => {
            c.item(0).ok().and_then(|i| i.number()).map_or_else(|| Code::Other(name.clone(), None), Code::HighestModSeq)
        }
        "NOMODSEQ" => Code::NoModSeq,
        "READ-ONLY" => Code::ReadOnly,
        "READ-WRITE" => Code::ReadWrite,
        "TRYCREATE" => Code::TryCreate,
        "CLOSED" => Code::Closed,
        "PERMANENTFLAGS" => Code::PermanentFlags(
            c.item(0).map(|i| i.list().iter().filter_map(Item::text).take(500).collect()).unwrap_or_default(),
        ),
        "CAPABILITY" => Code::Capability(split_caps(&c.rest_text())),
        "APPENDUID" => match (num32(&mut c), seq(&mut c)) {
            (Some(v), Some(uids)) => Code::AppendUid { uidvalidity: v, uids },
            _ => other(&mut c, name),
        },
        "COPYUID" => match (num32(&mut c), seq(&mut c), seq(&mut c)) {
            (Some(v), Some(source), Some(dest)) => Code::CopyUid { uidvalidity: v, source, dest },
            _ => other(&mut c, name),
        },
        "MODIFIED" => seq(&mut c).map_or_else(|| Code::Other(name.clone(), None), Code::Modified),
        _ => other(&mut c, name),
    }
}

fn split_caps(s: &str) -> Vec<String> {
    s.split_ascii_whitespace().take(500).map(|c| c.to_ascii_uppercase()).collect()
}

fn status_word(w: &str) -> Option<Status> {
    Some(match w.to_ascii_uppercase().as_str() {
        "OK" => Status::Ok,
        "NO" => Status::No,
        "BAD" => Status::Bad,
        "PREAUTH" => Status::PreAuth,
        "BYE" => Status::Bye,
        _ => return None,
    })
}

/// Interpreta una respuesta completa (sin el CRLF final, con los literales dentro).
pub fn parse_response(buf: &[u8]) -> Result<Response, ParseError> {
    let mut c = Cursor::new(buf);
    match c.peek() {
        None => err("respuesta vacía"),
        Some(b'+') => {
            c.pos = 1;
            c.skip_sp();
            Ok(Response::Continue(c.rest_text()))
        }
        Some(b'*') => {
            c.pos = 1;
            c.skip_sp();
            Ok(Response::Untagged(untagged(&mut c)))
        }
        Some(_) => {
            let tag = c.word();
            c.skip_sp();
            let st = c.word();
            let status = status_word(&st).ok_or_else(|| ParseError(format!("estado desconocido «{st}»")))?;
            let code = c.code();
            c.skip_sp();
            Ok(Response::Tagged { tag, status, code, text: c.rest_text() })
        }
    }
}

/// Todos los valores de una línea (lo usa el servidor de pruebas para leer órdenes).
pub fn parse_items(buf: &[u8]) -> Result<Vec<Item>, ParseError> {
    let mut c = Cursor::new(buf);
    let mut out = Vec::new();
    loop {
        c.skip_sp();
        if c.at_end() {
            return Ok(out);
        }
        if out.len() >= MAX_LIST_ITEMS {
            return err("demasiados elementos");
        }
        out.push(c.item(0)?);
    }
}

fn untagged(c: &mut Cursor<'_>) -> Untagged {
    let start = c.pos;
    match untagged_inner(c) {
        Some(u) => u,
        None => {
            c.pos = start;
            Untagged::Other(c.rest_text())
        }
    }
}

fn untagged_inner(c: &mut Cursor<'_>) -> Option<Untagged> {
    let w = c.word();
    c.skip_sp();
    if !w.is_empty() && w.len() <= 10 && w.bytes().all(|b| b.is_ascii_digit()) {
        let n: u32 = w.parse().ok()?;
        let kw = c.word().to_ascii_uppercase();
        c.skip_sp();
        return match kw.as_str() {
            "EXISTS" => Some(Untagged::Exists(n)),
            "RECENT" => Some(Untagged::Recent(n)),
            "EXPUNGE" => Some(Untagged::Expunge(n)),
            "FETCH" => {
                let item = c.item(0).ok()?;
                Some(Untagged::Fetch(Box::new(fetch_data(n, item.list()))))
            }
            _ => None,
        };
    }
    let kw = w.to_ascii_uppercase();
    if let Some(status) = status_word(&kw) {
        let code = c.code();
        c.skip_sp();
        return Some(Untagged::State { status, code, text: c.rest_text() });
    }
    match kw.as_str() {
        "CAPABILITY" => Some(Untagged::Capability(split_caps(&c.rest_text()))),
        "ENABLED" => Some(Untagged::Enabled(split_caps(&c.rest_text()))),
        "LIST" | "LSUB" | "XLIST" => {
            let attrs = c.item(0).ok()?;
            let delim = c.item(0).ok()?;
            let name = c.item(0).ok()?;
            Some(Untagged::List(ListEntry {
                attributes: attrs.list().iter().filter_map(Item::text).take(100).collect(),
                delimiter: delim.nstring().and_then(|d| d.first().map(|b| char::from(*b))),
                raw_name: name.nstring()?,
            }))
        }
        "STATUS" => {
            let name = c.item(0).ok()?;
            let items = c.item(0).ok()?;
            let mut st = MailboxStatus { raw_name: name.nstring()?, ..Default::default() };
            for pair in items.list().chunks(2) {
                let (Some(k), Some(v)) = (pair.first().and_then(Item::text), pair.get(1).and_then(Item::number)) else {
                    continue;
                };
                let v32 = u32::try_from(v).ok();
                match k.to_ascii_uppercase().as_str() {
                    "MESSAGES" => st.messages = v32,
                    "UIDNEXT" => st.uid_next = v32,
                    "UIDVALIDITY" => st.uid_validity = v32,
                    "UNSEEN" => st.unseen = v32,
                    "RECENT" => st.recent = v32,
                    "HIGHESTMODSEQ" => st.highest_modseq = Some(v),
                    _ => {}
                }
            }
            Some(Untagged::Status(st))
        }
        "SEARCH" | "SORT" => {
            let mut nums = Vec::new();
            let mut modseq = None;
            while !c.at_end() {
                match c.item(0).ok()? {
                    Item::List(l) => {
                        if l.first().and_then(Item::text).is_some_and(|t| t.eq_ignore_ascii_case("MODSEQ")) {
                            modseq = l.get(1).and_then(Item::number);
                        }
                    }
                    other => {
                        if let Some(n) = other.number().and_then(|n| u32::try_from(n).ok()) {
                            nums.push(n);
                        }
                    }
                }
            }
            Some(Untagged::Search(nums, modseq))
        }
        "ESEARCH" => {
            let mut es = ESearch::default();
            let items = parse_rest_items(c)?;
            let mut i = 0usize;
            while let Some(it) = items.get(i) {
                match it {
                    Item::List(l) => {
                        if l.first().and_then(Item::text).is_some_and(|t| t.eq_ignore_ascii_case("TAG")) {
                            es.tag = l.get(1).and_then(Item::text);
                        }
                        i = i.saturating_add(1);
                    }
                    other => {
                        let k = other.text().unwrap_or_default().to_ascii_uppercase();
                        let v = items.get(i.saturating_add(1));
                        let mut step = 2usize;
                        match k.as_str() {
                            "UID" => {
                                es.uid = true;
                                step = 1;
                            }
                            "ALL" => es.all = v.and_then(Item::text).and_then(|t| SeqSet::parse(&t)),
                            "COUNT" => es.count = v.and_then(Item::number).and_then(|n| u32::try_from(n).ok()),
                            "MIN" => es.min = v.and_then(Item::number).and_then(|n| u32::try_from(n).ok()),
                            "MAX" => es.max = v.and_then(Item::number).and_then(|n| u32::try_from(n).ok()),
                            "MODSEQ" => es.modseq = v.and_then(Item::number),
                            _ => {}
                        }
                        i = i.saturating_add(step);
                    }
                }
            }
            Some(Untagged::ESearch(es))
        }
        "VANISHED" => {
            let items = parse_rest_items(c)?;
            let mut earlier = false;
            let mut uids = None;
            for it in &items {
                match it {
                    Item::List(l) => {
                        earlier |= l.iter().any(|x| x.text().is_some_and(|t| t.eq_ignore_ascii_case("EARLIER")));
                    }
                    other => uids = other.text().and_then(|t| SeqSet::parse(&t)),
                }
            }
            Some(Untagged::Vanished { earlier, uids: uids? })
        }
        "FLAGS" => {
            let l = c.item(0).ok()?;
            Some(Untagged::Flags(l.list().iter().filter_map(Item::text).take(500).collect()))
        }
        _ => None,
    }
}

fn parse_rest_items(c: &mut Cursor<'_>) -> Option<Vec<Item>> {
    let mut out = Vec::new();
    while !c.at_end() {
        if out.len() >= MAX_LIST_ITEMS {
            return None;
        }
        out.push(c.item(0).ok()?);
    }
    Some(out)
}

/// Interpreta `17-Jul-1996 02:44:25 -0700` (el día puede venir con espacio delante).
pub fn parse_internal_date(s: &str) -> Option<DateTime<FixedOffset>> {
    let t = s.trim();
    if t.len() > 64 {
        return None;
    }
    let padded = if t.as_bytes().get(1) == Some(&b'-') { format!("0{t}") } else { t.to_string() };
    DateTime::parse_from_str(&padded, "%d-%b-%Y %H:%M:%S %z").ok()
}

fn fetch_data(seq: u32, items: &[Item]) -> FetchData {
    let mut f = FetchData { seq, ..Default::default() };
    let mut i = 0usize;
    while let Some(name_item) = items.get(i) {
        let Some(name) = name_item.text() else {
            i = i.saturating_add(1);
            continue;
        };
        let val = items.get(i.saturating_add(1));
        i = i.saturating_add(2);
        let Some(val) = val else { break };
        let upper = name.to_ascii_uppercase();
        match upper.as_str() {
            "UID" => f.uid = val.number().and_then(|n| u32::try_from(n).ok()),
            "FLAGS" => f.flags = Some(val.list().iter().filter_map(Item::text).take(500).collect()),
            "INTERNALDATE" => f.internal_date = val.text().and_then(|t| parse_internal_date(&t)),
            "RFC822.SIZE" => f.size = val.number(),
            "ENVELOPE" => f.envelope = envelope(val.list()),
            "BODYSTRUCTURE" | "BODY" => f.body_structure = body_structure(val, 0),
            "MODSEQ" => f.modseq = val.list().first().and_then(Item::number),
            "X-GM-LABELS" => f.gm_labels = Some(val.list().iter().filter_map(Item::text).take(500).collect()),
            "X-GM-MSGID" => f.gm_msgid = val.number(),
            "X-GM-THRID" => f.gm_thrid = val.number(),
            _ if upper.starts_with("BODY[")
                || upper.starts_with("BINARY[")
                || upper.starts_with("RFC822")
                || upper.starts_with("BODY.PEEK[") =>
            {
                let name = upper.replacen(".PEEK", "", 1);
                f.sections.push(BodySection { name, data: val.nstring() });
            }
            _ => {}
        }
    }
    f
}

fn addresses(item: &Item) -> Vec<Address> {
    let mut out = Vec::new();
    for a in item.list() {
        if out.len() >= MAX_ADDRESSES {
            break;
        }
        let l = a.list();
        let get = |k: usize| l.get(k).and_then(Item::nstring);
        let (mailbox, host) = (get(2), get(3));
        // Marcadores de grupo (RFC 3501 §7.4.2): se omiten.
        if host.is_none() {
            continue;
        }
        out.push(Address { name: get(0), mailbox, host });
    }
    out
}

fn envelope(l: &[Item]) -> Option<Envelope> {
    if l.len() < 10 {
        return None;
    }
    let ns = |k: usize| l.get(k).and_then(Item::nstring);
    let ad = |k: usize| l.get(k).map(addresses).unwrap_or_default();
    Some(Envelope {
        date: ns(0),
        subject: ns(1),
        from: ad(2),
        sender: ad(3),
        reply_to: ad(4),
        to: ad(5),
        cc: ad(6),
        bcc: ad(7),
        in_reply_to: ns(8),
        message_id: ns(9),
    })
}

fn params(item: Option<&Item>) -> Vec<(String, String)> {
    let Some(item) = item else { return Vec::new() };
    item.list()
        .chunks(2)
        .take(MAX_PARAMS)
        .filter_map(|p| Some((p.first()?.text()?.to_ascii_lowercase(), p.get(1)?.text()?)))
        .collect()
}

fn disposition(item: Option<&Item>) -> Option<Disposition> {
    let l = item?.list();
    let kind = l.first()?.text()?.to_ascii_lowercase();
    Some(Disposition { kind, params: params(l.get(1)) })
}

/// Interpreta un `BODYSTRUCTURE` (o `BODY`) con profundidad acotada.
pub fn body_structure(item: &Item, depth: usize) -> Option<BodyStructure> {
    if depth > MAX_DEPTH {
        return None;
    }
    let l = item.list();
    match l.first()? {
        Item::List(_) => {
            let mut parts = Vec::new();
            let mut idx = 0usize;
            while let Some(p @ Item::List(_)) = l.get(idx) {
                if parts.len() < MAX_PARTS
                    && let Some(b) = body_structure(p, depth.saturating_add(1))
                {
                    parts.push(b);
                }
                idx = idx.saturating_add(1);
            }
            let subtype = l.get(idx).and_then(Item::text).unwrap_or_default().to_ascii_lowercase();
            let disp = disposition(l.get(idx.saturating_add(2)));
            Some(BodyStructure::Multi { parts, subtype, disposition: disp })
        }
        first => {
            let media_type = first.text()?.to_ascii_lowercase();
            let subtype = l.get(1).and_then(Item::text).unwrap_or_default().to_ascii_lowercase();
            let ext = match (media_type.as_str(), subtype.as_str()) {
                ("text", _) => 8usize,
                ("message", "rfc822" | "global") => 10,
                _ => 7,
            };
            Some(BodyStructure::Single {
                params: params(l.get(2)),
                id: l.get(3).and_then(Item::text),
                encoding: l.get(5).and_then(Item::text).unwrap_or_default().to_ascii_lowercase(),
                size: l.get(6).and_then(Item::number).unwrap_or(0),
                disposition: disposition(l.get(ext.saturating_add(1))),
                media_type,
                subtype,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Response {
        parse_response(s.as_bytes()).unwrap()
    }

    #[test]
    fn tagged_and_codes() {
        assert_eq!(
            p("a1 OK [UIDVALIDITY 3857529045] UIDs valid"),
            Response::Tagged {
                tag: "a1".into(),
                status: Status::Ok,
                code: Some(Code::UidValidity(3857529045)),
                text: "UIDs valid".into()
            }
        );
        let Response::Tagged { code: Some(Code::CopyUid { uidvalidity, source, dest }), .. } =
            p("a2 OK [COPYUID 38505 304,319:320 3956:3958] Done")
        else {
            panic!()
        };
        assert_eq!(uidvalidity, 38505);
        assert_eq!(source.to_vec(0, 10), vec![304, 319, 320]);
        assert_eq!(dest.to_vec(0, 10), vec![3956, 3957, 3958]);
        let Response::Untagged(Untagged::State { code: Some(Code::PermanentFlags(f)), .. }) =
            p("* OK [PERMANENTFLAGS (\\Deleted \\Seen \\*)] Limited")
        else {
            panic!()
        };
        assert_eq!(f, vec!["\\Deleted", "\\Seen", "\\*"]);
        assert!(matches!(p("+ idling"), Response::Continue(t) if t == "idling"));
        assert!(matches!(
            p("* BYE [UNAVAILABLE] Too many"),
            Response::Untagged(Untagged::State { status: Status::Bye, code: Some(Code::Other(..)), .. })
        ));
    }

    #[test]
    fn fetch_with_literals() {
        let raw = b"* 12 FETCH (UID 4827 FLAGS (\\Seen $Forwarded) INTERNALDATE \" 7-Jul-2026 02:44:25 -0700\" RFC822.SIZE 4286 MODSEQ (12121) BODY[HEADER.FIELDS (REFERENCES IN-REPLY-TO)] {21}\r\nReferences: <a@b>\r\n\r\n)";
        let Response::Untagged(Untagged::Fetch(f)) = parse_response(raw).unwrap() else { panic!() };
        assert_eq!(f.seq, 12);
        assert_eq!(f.uid, Some(4827));
        assert_eq!(f.flags.as_deref(), Some(&["\\Seen".to_string(), "$Forwarded".to_string()][..]));
        assert_eq!(f.size, Some(4286));
        assert_eq!(f.modseq, Some(12121));
        assert_eq!(f.internal_date.unwrap().to_rfc3339(), "2026-07-07T02:44:25-07:00");
        assert_eq!(f.section("BODY[HEADER"), Some(&b"References: <a@b>\r\n\r\n"[..]));
    }

    #[test]
    fn envelope_and_bodystructure() {
        let raw = br#"* 1 FETCH (UID 1 ENVELOPE ("Wed, 17 Jul 1996 02:23:25 -0700" "=?utf-8?q?Hola?=" (("Ana" NIL "ana" "example.com")) NIL NIL ((NIL NIL "grupo" NIL)("Bo" NIL "bo" "x.org")(NIL NIL NIL NIL)) NIL NIL "<p@x>" "<m@x>") BODYSTRUCTURE (("TEXT" "PLAIN" ("CHARSET" "utf-8") NIL NIL "7BIT" 10 1 NIL NIL NIL NIL)("APPLICATION" "PDF" ("NAME" "f.pdf") NIL NIL "BASE64" 400 NIL ("ATTACHMENT" ("FILENAME" "factura.pdf")) NIL NIL) "MIXED" ("BOUNDARY" "x") NIL NIL NIL))"#;
        let Response::Untagged(Untagged::Fetch(f)) = parse_response(raw).unwrap() else { panic!() };
        let e = f.envelope.unwrap();
        assert_eq!(e.from.first().unwrap().email(), "ana@example.com");
        assert_eq!(e.to.len(), 1);
        assert_eq!(e.message_id.as_deref(), Some(&b"<m@x>"[..]));
        let bs = f.body_structure.unwrap();
        let att = bs.attachments();
        assert_eq!(att.len(), 1);
        assert_eq!(att[0].filename, "factura.pdf");
        assert_eq!(att[0].mime_type, "application/pdf");
        assert!(bs.has_attachments());
    }

    #[test]
    fn list_status_search_vanished() {
        let Response::Untagged(Untagged::List(l)) = p(r#"* LIST (\HasNoChildren \Sent) "/" "Elementos enviados""#)
        else {
            panic!()
        };
        assert_eq!(l.delimiter, Some('/'));
        assert_eq!(l.raw_name, b"Elementos enviados");
        let Response::Untagged(Untagged::List(l)) = p(r#"* LIST () "\\" INBOX"#) else { panic!() };
        assert_eq!(l.delimiter, Some('\\'));
        let Response::Untagged(Untagged::Status(s)) =
            p("* STATUS INBOX (MESSAGES 231 UIDNEXT 44292 UIDVALIDITY 1 HIGHESTMODSEQ 7011231777)")
        else {
            panic!()
        };
        assert_eq!((s.messages, s.uid_next, s.highest_modseq), (Some(231), Some(44292), Some(7011231777)));
        assert_eq!(
            p("* SEARCH 2 84 882 (MODSEQ 917162500)"),
            Response::Untagged(Untagged::Search(vec![2, 84, 882], Some(917162500)))
        );
        let Response::Untagged(Untagged::ESearch(e)) = p(r#"* ESEARCH (TAG "a5") UID ALL 1:3,7 COUNT 4"#) else {
            panic!()
        };
        assert!(e.uid);
        assert_eq!(e.all.unwrap().to_vec(0, 10), vec![1, 2, 3, 7]);
        assert_eq!(e.count, Some(4));
        let Response::Untagged(Untagged::Vanished { earlier, uids }) = p("* VANISHED (EARLIER) 41,43:116") else {
            panic!()
        };
        assert!(earlier);
        assert_eq!(uids.len(), 75);
    }

    #[test]
    fn garbage_is_tolerated() {
        assert!(matches!(p("* 3 FETCH ((((("), Response::Untagged(Untagged::Other(_))));
        assert!(matches!(p("* XYZZY whatever"), Response::Untagged(Untagged::Other(_))));
        assert!(parse_response(b"a1 MAYBE").is_err());
        assert!(parse_response(b"* 1 FETCH (BODY[] {99999}\r\nabc)").is_ok());
        let deep = format!("* 1 FETCH {}", "(".repeat(10_000));
        assert!(parse_response(deep.as_bytes()).is_ok());
    }

    proptest::proptest! {
        #[test]
        fn never_panics(bytes in proptest::collection::vec(proptest::num::u8::ANY, 0..600)) {
            let _ = parse_response(&bytes);
            let _ = parse_items(&bytes);
            let _ = parse_code(&bytes);
        }

        #[test]
        fn never_panics_on_imapish(s in r#"(\* |a1 |\+ )?([0-9]{1,3} )?(FETCH|OK|NO|LIST|STATUS|ESEARCH|VANISHED|SEARCH|BYE)? ?[\(\)\[\] "{}0-9A-Z\\~+*:,.]{0,200}"#) {
            let _ = parse_response(s.as_bytes());
        }
    }
}
