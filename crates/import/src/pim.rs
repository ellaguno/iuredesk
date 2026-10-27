//! Calendarios (`.ics`) y contactos (`.vcf`) por flujo.
//!
//! Un `.vcf` exportado de Apple Contactos o de Google puede pesar cientos de MB (fotos); un `.ics`
//! de años de calendario, decenas. Aquí se leen línea a línea: cada tarjeta se parsea sola y los
//! componentes de un calendario se agrupan en tandas (con su cabecera y sus `VTIMEZONE`) que se
//! parsean con el lector sin pérdida de `iuredesk-dav` y se mapean con el de `iuredesk-pim-sync`,
//! los mismos que usa la sincronización CalDAV/CardDAV.

use std::collections::HashSet;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use chrono::{DateTime, Utc};
use iuredesk_dav::calendar::{EventView, ICalendar};
use iuredesk_dav::ical::Component;
use iuredesk_dav::value::DateTimeValue;
use iuredesk_dav::vcard::VCard;
use iuredesk_model::{AccountId, Note};
use iuredesk_pim_sync::map::ical::{events_from_ics, tasks_from_ics};
use iuredesk_pim_sync::map::vcard::contact_from_vcard;
use iuredesk_store::PimRef;

use crate::error::{ImportError, Result};
use crate::report::{Notice, SkipReason};
use crate::sink::Sink;
use crate::util::{decode_text, decode_utf16_bom, read_line_capped};

/// Componentes por tanda.
const CHUNK_COMPONENTS: usize = 300;
/// Bytes por tanda.
const CHUNK_BYTES: usize = 4 * 1024 * 1024;
/// Líneas máximas de la cabecera de un `VCALENDAR`.
const MAX_HEADER_LINES: usize = 200;
/// `VTIMEZONE` máximos que se conservan.
const MAX_TIMEZONES: usize = 500;
/// Anidamiento máximo de `BEGIN`.
const MAX_NESTING: usize = 16;

/// Fuente de líneas: por flujo, o en memoria (archivos UTF-16).
enum Lines {
    Stream(BufReader<File>, Vec<u8>, usize),
    Mem(std::vec::IntoIter<String>),
}

impl Lines {
    fn open(path: &Path, max_line: usize, max_utf16: u64) -> Result<Self> {
        let head = crate::util::head(path, 2);
        if head == [0xFF, 0xFE] || head == [0xFE, 0xFF] {
            let data = crate::util::read_bounded(path, max_utf16).map_err(|e| match e {
                crate::util::ReadError::TooLarge(_) => {
                    ImportError::io(path, std::io::Error::other("UTF-16 demasiado grande"))
                }
                crate::util::ReadError::Io(s) => ImportError::io(path, std::io::Error::other(s)),
            })?;
            let text = decode_utf16_bom(&data).unwrap_or_default();
            let lines: Vec<String> = text.lines().map(|l| format!("{l}\n")).collect();
            return Ok(Self::Mem(lines.into_iter()));
        }
        let f = File::open(path).map_err(|e| ImportError::io(path, e))?;
        Ok(Self::Stream(BufReader::with_capacity(256 * 1024, f), Vec::new(), max_line))
    }

    /// Siguiente línea sin salto, bytes consumidos y si se truncó.
    fn next(&mut self, path: &Path) -> Result<Option<(String, u64, bool)>> {
        match self {
            Self::Stream(r, buf, cap) => {
                let (n, truncated) = read_line_capped(r, buf, *cap).map_err(|e| ImportError::io(path, e))?;
                if n == 0 {
                    return Ok(None);
                }
                let mut s = decode_text(buf);
                while s.ends_with(['\n', '\r']) {
                    s.pop();
                }
                if let Some(rest) = s.strip_prefix('\u{feff}') {
                    s = rest.to_string();
                }
                Ok(Some((s, n, truncated)))
            }
            Self::Mem(it) => Ok(it.next().map(|l| {
                let n = l.len() as u64;
                (l.trim_end_matches(['\n', '\r']).to_string(), n, false)
            })),
        }
    }
}

fn begin_name(line: &str) -> Option<String> {
    let (k, v) = line.split_once(':')?;
    if k.trim().eq_ignore_ascii_case("BEGIN") { Some(v.trim().to_ascii_uppercase()) } else { None }
}

fn end_name(line: &str) -> Option<String> {
    let (k, v) = line.split_once(':')?;
    if k.trim().eq_ignore_ascii_case("END") { Some(v.trim().to_ascii_uppercase()) } else { None }
}

// ---------------------------------------------------------------------------- vCard

/// Importa un `.vcf` (una o muchas tarjetas) en la libreta `book`.
pub(crate) fn import_vcf(sink: &mut Sink<'_>, path: &Path, account: AccountId, book: &str) -> Result<()> {
    let max = sink.opts.max_object_bytes;
    let mut lines = Lines::open(path, max, sink.opts.max_message_bytes)?;
    let mut depth = 0usize;
    let mut card = String::new();
    let mut too_big = 0u64;
    let mut index = 0u64;
    while let Some((line, n, truncated)) = lines.next(path)? {
        sink.advance(n);
        if begin_name(&line).as_deref() == Some("VCARD") {
            depth = depth.saturating_add(1);
            if depth == 1 {
                card.clear();
                too_big = 0;
            }
        }
        if depth > 0 {
            too_big = too_big.saturating_add(n);
            if truncated || card.len().saturating_add(line.len()) > max {
                too_big = too_big.max(max as u64 + 1);
            } else {
                card.push_str(&line);
                card.push_str("\r\n");
            }
        }
        if depth > 0 && end_name(&line).as_deref() == Some("VCARD") {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                index = index.saturating_add(1);
                let loc = || format!("{}#{index}", path.display());
                if too_big > max as u64 {
                    sink.skip(loc(), SkipReason::TooLarge { size: too_big, max: max as u64 });
                    continue;
                }
                let text = unfold_quoted_printable(&card);
                match VCard::parse(&text) {
                    Ok(v) => {
                        let (c, photo) = contact_from_vcard(&v);
                        sink.contact(account, book, c, photo)?;
                    }
                    Err(e) => sink.skip(loc(), SkipReason::Malformed { detail: e.to_string() }),
                }
            }
        }
        depth = depth.min(MAX_NESTING);
    }
    if index == 0 {
        sink.skip(path.display().to_string(), SkipReason::Malformed { detail: "sin tarjetas".into() });
    }
    Ok(())
}

/// Decodifica las propiedades `ENCODING=QUOTED-PRINTABLE` de vCard 2.1 (Outlook, teléfonos
/// antiguos), con sus continuaciones `=` al final de línea y su `CHARSET`.
pub fn unfold_quoted_printable(card: &str) -> String {
    let mut out = String::with_capacity(card.len());
    let mut lines = card.split("\r\n").peekable();
    while let Some(line) = lines.next() {
        let Some((head, value)) = line.split_once(':') else {
            if !line.is_empty() {
                out.push_str(line);
                out.push_str("\r\n");
            }
            continue;
        };
        if !head.to_ascii_uppercase().contains("QUOTED-PRINTABLE") {
            out.push_str(line);
            out.push_str("\r\n");
            continue;
        }
        let mut value = value.to_string();
        let mut guard = 0;
        while value.ends_with('=') && guard < 10_000 {
            guard += 1;
            value.pop();
            match lines.next() {
                Some(next) => value.push_str(next.trim_start()),
                None => break,
            }
        }
        let bytes = decode_qp(value.as_bytes());
        let charset = head
            .split(';')
            .find_map(|p| p.split_once('=').filter(|(k, _)| k.trim().eq_ignore_ascii_case("CHARSET")))
            .map(|(_, v)| v.trim().to_ascii_lowercase())
            .unwrap_or_default();
        let text = if charset.is_empty() || charset == "utf-8" || charset == "utf8" {
            decode_text(&bytes)
        } else {
            crate::util::decode_cp1252(&bytes)
        };
        let params: Vec<&str> = head
            .split(';')
            .filter(|p| {
                let u = p.trim().to_ascii_uppercase();
                u != "QUOTED-PRINTABLE" && u != "ENCODING=QUOTED-PRINTABLE" && !u.starts_with("CHARSET=")
            })
            .collect();
        out.push_str(&params.join(";"));
        out.push(':');
        // Saltos de línea del valor decodificado → escapes de vCard.
        out.push_str(&text.replace("\r\n", "\\n").replace('\n', "\\n"));
        out.push_str("\r\n");
    }
    out
}

fn decode_qp(v: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len());
    let mut i = 0usize;
    while i < v.len() {
        let b = v.get(i).copied().unwrap_or(0);
        if b == b'=' {
            let hex = v.get(i.saturating_add(1)..i.saturating_add(3)).and_then(|h| std::str::from_utf8(h).ok());
            if let Some(n) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(n);
                i = i.saturating_add(3);
                continue;
            }
        }
        out.push(b);
        i = i.saturating_add(1);
    }
    out
}

// ---------------------------------------------------------------------------- iCalendar

struct IcsState<'p> {
    path: &'p Path,
    account: AccountId,
    default_name: String,
    header: Vec<String>,
    timezones: Vec<String>,
    chunk: Vec<String>,
    chunk_bytes: usize,
    seen_masters: HashSet<String>,
    dropped_exceptions: u64,
    index: u64,
}

impl IcsState<'_> {
    fn name(&self) -> String {
        for l in &self.header {
            if let Some((k, v)) = l.split_once(':')
                && k.trim().eq_ignore_ascii_case("X-WR-CALNAME")
            {
                let v = iuredesk_dav::value::unescape_text(v.trim());
                if !v.trim().is_empty() {
                    return v.trim().chars().take(200).collect();
                }
            }
        }
        self.default_name.clone()
    }

    fn calendar_text(&self, comps: &[String]) -> String {
        let mut s = String::from("BEGIN:VCALENDAR\r\n");
        for l in &self.header {
            s.push_str(l);
            s.push_str("\r\n");
        }
        for t in &self.timezones {
            s.push_str(t);
        }
        for c in comps {
            s.push_str(c);
        }
        s.push_str("END:VCALENDAR\r\n");
        s
    }

    fn flush(&mut self, sink: &mut Sink<'_>) -> Result<()> {
        if self.chunk.is_empty() {
            return Ok(());
        }
        let comps = std::mem::take(&mut self.chunk);
        self.chunk_bytes = 0;
        let text = self.calendar_text(&comps);
        let cal = match ICalendar::parse(&text) {
            Ok(c) => c,
            Err(e) => {
                // Una tanda rota: se reintenta componente a componente para salvar el resto.
                if comps.len() > 1 {
                    for c in comps {
                        self.chunk.push(c);
                        self.flush(sink)?;
                    }
                } else {
                    sink.skip(
                        format!("{}#{}", self.path.display(), self.index),
                        SkipReason::Malformed { detail: e.to_string() },
                    );
                }
                return Ok(());
            }
        };
        let name = self.name();
        let tz = sink.opts.time_zone;
        for ev in events_from_ics(&cal, tz) {
            let comps_of_uid: Vec<&Component> =
                cal.root.children("VEVENT").filter(|c| EventView::new(c).uid().unwrap_or_default() == ev.uid).collect();
            let has_master = comps_of_uid.iter().any(|c| c.prop("RECURRENCE-ID").is_none());
            if !has_master && !ev.uid.is_empty() && self.seen_masters.contains(&ev.uid) {
                // Excepción de una serie que llegó en otra tanda: no debe pisar la serie.
                self.dropped_exceptions = self.dropped_exceptions.saturating_add(1);
                continue;
            }
            if has_master {
                self.seen_masters.insert(ev.uid.clone());
            }
            let keep_raw = ev.is_recurring() || comps_of_uid.len() > 1;
            let uid = ev.uid.clone();
            let id = sink.event(self.account, &name, ev)?;
            if keep_raw {
                // El `.ics` de la serie (con sus excepciones y zonas) para expandirla sin pérdida.
                let mut one = ICalendar::new();
                one.root.properties = cal.root.properties.clone();
                one.root.components = cal.root.children("VTIMEZONE").cloned().collect();
                one.root.components.extend(comps_of_uid.into_iter().cloned());
                let data = one.to_ics();
                sink.store.put_raw_object(PimRef::Event(id), data.as_bytes(), &format!("import:{uid}"), None)?;
            }
        }
        for t in tasks_from_ics(&cal, tz) {
            sink.task(self.account, &name, t)?;
        }
        for j in cal.root.children("VJOURNAL") {
            sink.note(journal_note(&cal, j, tz))?;
        }
        Ok(())
    }
}

fn utc_of(cal: &ICalendar, comp: &Component, name: &str, tz: chrono_tz::Tz) -> Option<DateTime<Utc>> {
    let p = comp.prop(name)?;
    let v = DateTimeValue::from_property(p).ok()?;
    Some(cal.to_utc(&v, tz))
}

/// `VJOURNAL` → nota.
fn journal_note(cal: &ICalendar, j: &Component, tz: chrono_tz::Tz) -> Note {
    let created = utc_of(cal, j, "CREATED", tz)
        .or_else(|| utc_of(cal, j, "DTSTART", tz))
        .or_else(|| utc_of(cal, j, "DTSTAMP", tz))
        .unwrap_or(DateTime::<Utc>::UNIX_EPOCH);
    let updated = utc_of(cal, j, "LAST-MODIFIED", tz).unwrap_or(created);
    let tags = j
        .props("CATEGORIES")
        .flat_map(|p| iuredesk_dav::value::split_unescaped(&p.value, ','))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .take(100)
        .collect();
    Note {
        title: j.text("SUMMARY").unwrap_or_default(),
        body: j.props("DESCRIPTION").map(|p| p.text_value()).collect::<Vec<_>>().join("\n\n"),
        tags,
        created,
        updated,
        ..Default::default()
    }
}

/// Importa un `.ics` (eventos → calendario, `VTODO` → lista de tareas, `VJOURNAL` → notas). El
/// nombre es `X-WR-CALNAME` o `default_name`.
pub(crate) fn import_ics(sink: &mut Sink<'_>, path: &Path, account: AccountId, default_name: &str) -> Result<()> {
    let max = sink.opts.max_object_bytes;
    let mut lines = Lines::open(path, max, sink.opts.max_message_bytes)?;
    let mut st = IcsState {
        path,
        account,
        default_name: default_name.to_string(),
        header: Vec::new(),
        timezones: Vec::new(),
        chunk: Vec::new(),
        chunk_bytes: 0,
        seen_masters: HashSet::new(),
        dropped_exceptions: 0,
        index: 0,
    };
    // Pila de componentes abiertos; `cur` acumula el componente hijo de VCALENDAR en curso.
    let mut stack: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cur_size = 0u64;
    let mut found = false;
    while let Some((line, n, truncated)) = lines.next(path)? {
        sink.advance(n);
        let continuation = line.starts_with([' ', '\t']);
        if !continuation && let Some(b) = begin_name(&line) {
            if stack.len() >= MAX_NESTING {
                return Err(ImportError::io(path, std::io::Error::other("anidamiento excesivo")));
            }
            if stack.is_empty() && b != "VCALENDAR" {
                // Componente fuera de VCALENDAR: no es un .ics válido; se ignora.
                stack.push(b);
                continue;
            }
            if stack.len() == 1 && stack.first().is_some_and(|s| s == "VCALENDAR") {
                cur.clear();
                cur_size = 0;
                st.index = st.index.saturating_add(1);
            }
            stack.push(b);
        }
        let depth = stack.len();
        let in_vcal = stack.first().is_some_and(|s| s == "VCALENDAR");
        if in_vcal && depth == 1 && !continuation && begin_name(&line).is_none() && end_name(&line).is_none() {
            if st.header.len() < MAX_HEADER_LINES && !line.is_empty() {
                st.header.push(line.clone());
            }
        } else if in_vcal && depth == 1 && continuation {
            if let Some(last) = st.header.last_mut()
                && last.len() < 64 * 1024
            {
                last.push_str("\r\n");
                last.push_str(&line);
            }
        } else if in_vcal && depth >= 2 {
            cur_size = cur_size.saturating_add(n);
            if !truncated && cur.len().saturating_add(line.len()) <= max {
                cur.push_str(&line);
                cur.push_str("\r\n");
            }
        }
        if !continuation && let Some(e) = end_name(&line) {
            // Cierra hasta el componente con ese nombre (tolerante a END mal anidados).
            if let Some(pos) = stack.iter().rposition(|s| *s == e) {
                let closing_child = in_vcal && pos == 1;
                stack.truncate(pos);
                if closing_child {
                    found = true;
                    if cur_size > max as u64 {
                        sink.skip(
                            format!("{}#{}", path.display(), st.index),
                            SkipReason::TooLarge { size: cur_size, max: max as u64 },
                        );
                    } else if e == "VTIMEZONE" {
                        if st.timezones.len() < MAX_TIMEZONES {
                            st.timezones.push(std::mem::take(&mut cur));
                        }
                    } else {
                        st.chunk_bytes = st.chunk_bytes.saturating_add(cur.len());
                        st.chunk.push(std::mem::take(&mut cur));
                        if st.chunk.len() >= CHUNK_COMPONENTS || st.chunk_bytes >= CHUNK_BYTES {
                            st.flush(sink)?;
                        }
                    }
                } else if in_vcal && pos == 0 {
                    // Fin de un VCALENDAR: procesa lo pendiente; el siguiente empieza limpio.
                    st.flush(sink)?;
                    st.header.clear();
                }
            }
        }
    }
    st.flush(sink)?;
    if st.dropped_exceptions > 0 {
        sink.notice(Notice::RecurrenceExceptionsDropped { count: st.dropped_exceptions });
    }
    if !found {
        sink.skip(path.display().to_string(), SkipReason::Malformed { detail: "sin eventos ni tareas".into() });
    }
    Ok(())
}

/// Nombre por omisión de un calendario o libreta a partir del archivo (`Trabajo.ics` → `Trabajo`).
pub(crate) fn stem_name(path: &Path, fallback: &str) -> String {
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let stem = stem.trim();
    if stem.is_empty() { fallback.to_string() } else { stem.chars().take(200).collect() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoted_printable_vcard21() {
        let card = "BEGIN:VCARD\r\nVERSION:2.1\r\nN;CHARSET=UTF-8;ENCODING=QUOTED-PRINTABLE:Pe=C3=B1a;Jos=\r\n=C3=A9\r\nNOTE;ENCODING=QUOTED-PRINTABLE;CHARSET=ISO-8859-1:l=EDnea 1=0D=0Al=EDnea 2\r\nEND:VCARD\r\n";
        let out = unfold_quoted_printable(card);
        assert!(out.contains("N:Peña;José\r\n"), "{out}");
        assert!(out.contains("NOTE:línea 1\\nlínea 2\r\n"), "{out}");
        let v = VCard::parse(&out).unwrap();
        assert_eq!(v.name().unwrap().given, "José");
    }

    #[test]
    fn line_helpers() {
        assert_eq!(begin_name("begin:vevent").as_deref(), Some("VEVENT"));
        assert_eq!(end_name("END:VEVENT ").as_deref(), Some("VEVENT"));
        assert_eq!(begin_name("SUMMARY:BEGIN"), None);
        assert_eq!(stem_name(Path::new("/x/Trabajo.ics"), "C"), "Trabajo");
    }
}
