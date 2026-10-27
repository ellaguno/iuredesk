//! Lector de mbox por flujo (mboxo/mboxrd, Thunderbird, Gmail Takeout, Apple «Exportar buzón»).
//!
//! - Lee línea a línea con un tope por mensaje: un archivo de 20 GB o sin saltos de línea nunca se
//!   carga entero en memoria. Un mensaje que supera el tope se descarta (se informa su tamaño) y la
//!   lectura sigue en el siguiente separador.
//! - Separador `From `: al principio del archivo, o tras una línea vacía y con forma de línea
//!   `From_` (`From remitente fecha` con hora `hh:mm` y año). Un «From » suelto en un cuerpo mal
//!   escapado no parte el mensaje.
//! - `>From ` (y `>>From `…) se des-escapa quitando un `>` (mboxrd; en mboxo también es lo
//!   correcto en la práctica).
//! - CRLF y LF se conservan tal cual; el separador vacío final se quita.

use std::io::{self, BufRead};

use chrono::{DateTime, NaiveDate, NaiveTime, TimeZone, Utc};

/// Mensaje leído de un mbox.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MboxMessage {
    /// Mensaje RFC 822 (vacío si `oversized`).
    pub raw: Vec<u8>,
    /// Fecha de la línea `From_`, si se pudo leer.
    pub from_date: Option<DateTime<Utc>>,
    /// Desplazamiento (bytes desde el inicio del archivo) de la línea `From_`.
    pub offset: u64,
    /// Desplazamiento donde termina (donde empieza el siguiente).
    pub end: u64,
    /// Tamaño real si superó el tope (el contenido se descartó).
    pub oversized: Option<u64>,
    /// Posición del mensaje en el archivo (0, 1, 2…), contando desde `offset` inicial.
    pub index: u64,
}

/// Lector de mbox.
pub struct MboxReader<R> {
    r: R,
    pos: u64,
    max_message: usize,
    line: Vec<u8>,
    line_size: u64,
    cur: Option<MboxMessage>,
    cur_size: u64,
    prev_blank: bool,
    at_start: bool,
    done: bool,
    next_index: u64,
    /// Bytes antes del primer separador (basura o no es un mbox).
    pub leading_garbage: u64,
}

impl<R: BufRead> MboxReader<R> {
    /// Lector sobre `r`, que empieza en el desplazamiento `start` del archivo (para reanudar: debe
    /// ser el inicio de una línea `From_`). `max_message` acota cada mensaje.
    pub fn new(r: R, start: u64, max_message: usize) -> Self {
        Self {
            r,
            pos: start,
            max_message: max_message.max(1024),
            line: Vec::new(),
            line_size: 0,
            cur: None,
            cur_size: 0,
            prev_blank: true,
            at_start: true,
            done: false,
            next_index: 0,
            leading_garbage: 0,
        }
    }

    /// Lee una línea en `self.line` (con su `\n`), guardando como mucho `max_message + 1` bytes.
    /// Devuelve `false` al final del archivo.
    fn read_line(&mut self) -> io::Result<bool> {
        self.line.clear();
        self.line_size = 0;
        let cap = self.max_message.saturating_add(1);
        loop {
            let buf = match self.r.fill_buf() {
                Ok(b) => b,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            if buf.is_empty() {
                return Ok(self.line_size > 0);
            }
            let (take, found) = match buf.iter().position(|&b| b == b'\n') {
                Some(i) => (i.saturating_add(1), true),
                None => (buf.len(), false),
            };
            let room = cap.saturating_sub(self.line.len());
            if room > 0 {
                self.line.extend_from_slice(buf.get(..take.min(room)).unwrap_or(&[]));
            }
            self.line_size = self.line_size.saturating_add(take as u64);
            self.r.consume(take);
            if found {
                return Ok(true);
            }
        }
    }

    fn finish(&mut self) -> Option<MboxMessage> {
        let mut m = self.cur.take()?;
        m.end = self.pos;
        // El separador: la línea vacía que precede a la siguiente `From_` (o el final).
        if m.raw.ends_with(b"\r\n\r\n") {
            m.raw.truncate(m.raw.len().saturating_sub(2));
        } else if m.raw.ends_with(b"\n\n") {
            m.raw.truncate(m.raw.len().saturating_sub(1));
        }
        if self.cur_size > self.max_message as u64 {
            m.oversized = Some(self.cur_size);
            m.raw = Vec::new();
        }
        Some(m)
    }

    fn start(&mut self, offset: u64) {
        let from_date = parse_from_line_date(&self.line);
        self.cur = Some(MboxMessage { offset, from_date, index: self.next_index, ..Default::default() });
        self.next_index = self.next_index.saturating_add(1);
        self.cur_size = 0;
    }

    /// Siguiente mensaje (o `None` al terminar).
    pub fn next_message(&mut self) -> io::Result<Option<MboxMessage>> {
        if self.done {
            return Ok(None);
        }
        loop {
            let line_start = self.pos;
            if !self.read_line()? {
                self.done = true;
                return Ok(self.finish());
            }
            self.pos = self.pos.saturating_add(self.line_size);
            let is_from = self.line.starts_with(b"From ")
                && (self.at_start || (self.prev_blank && looks_like_from_line(&self.line)));
            self.at_start = false;
            let blank = self.line == b"\n" || self.line == b"\r\n";
            if is_from {
                self.prev_blank = false;
                let done = self.finish();
                self.start(line_start);
                if let Some(m) = done {
                    // Corrige el final: termina donde empieza esta línea.
                    let mut m = m;
                    m.end = line_start;
                    return Ok(Some(m));
                }
                continue;
            }
            self.prev_blank = blank;
            let Some(cur) = self.cur.as_mut() else {
                self.leading_garbage = self.leading_garbage.saturating_add(self.line_size);
                continue;
            };
            let unescape = is_escaped_from(&self.line);
            let size = self.line_size.saturating_sub(u64::from(unescape));
            self.cur_size = self.cur_size.saturating_add(size);
            if self.cur_size <= self.max_message as u64 {
                let line = if unescape { self.line.get(1..).unwrap_or(&[]) } else { &self.line[..] };
                cur.raw.extend_from_slice(line);
            } else if !cur.raw.is_empty() {
                cur.raw = Vec::new();
            }
        }
    }
}

/// `>From `, `>>From `…
fn is_escaped_from(line: &[u8]) -> bool {
    let n = line.iter().take_while(|&&b| b == b'>').count();
    n > 0 && line.get(n..).is_some_and(|r| r.starts_with(b"From "))
}

/// ¿Parece una línea `From_` de verdad? (`From algo … hh:mm … año`).
pub fn looks_like_from_line(line: &[u8]) -> bool {
    let text = String::from_utf8_lossy(line.get(..line.len().min(512)).unwrap_or(&[]));
    let mut tokens = text.split_whitespace();
    if tokens.next() != Some("From") {
        return false;
    }
    let rest: Vec<&str> = tokens.collect();
    if rest.len() < 2 {
        return false;
    }
    let has_time = rest.iter().any(|t| {
        let parts: Vec<&str> = t.split(':').collect();
        (parts.len() == 2 || parts.len() == 3)
            && parts.iter().all(|p| p.len() == 2 && p.bytes().all(|b| b.is_ascii_digit()))
    });
    let has_year = rest.iter().any(|t| t.len() == 4 && t.parse::<u32>().is_ok_and(|y| (1970..=2200).contains(&y)));
    has_time && has_year
}

const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];

/// Fecha de una línea `From_` (`From x Tue Jan  1 00:00:00 [+0000] 2024`), en UTC si no trae zona.
pub fn parse_from_line_date(line: &[u8]) -> Option<DateTime<Utc>> {
    let text = String::from_utf8_lossy(line.get(..line.len().min(512)).unwrap_or(&[])).into_owned();
    let tokens: Vec<&str> = text.split_whitespace().skip(2).collect();
    let mut month = None;
    let mut day = None;
    let mut time = None;
    let mut year = None;
    let mut offset_secs: i64 = 0;
    for t in tokens.iter().take(16) {
        let l = t.to_ascii_lowercase();
        if month.is_none()
            && let Some(m) = MONTHS.iter().position(|m| l.starts_with(m))
        {
            month = u32::try_from(m).ok().map(|m| m.saturating_add(1));
            continue;
        }
        if time.is_none() && t.contains(':') {
            time = NaiveTime::parse_from_str(t, "%H:%M:%S").or_else(|_| NaiveTime::parse_from_str(t, "%H:%M")).ok();
            continue;
        }
        if (t.starts_with('+') || t.starts_with('-')) && t.len() == 5 {
            let sign = if t.starts_with('-') { -1 } else { 1 };
            let hh: i64 = t.get(1..3).and_then(|s| s.parse().ok()).unwrap_or(0);
            let mm: i64 = t.get(3..5).and_then(|s| s.parse().ok()).unwrap_or(0);
            offset_secs = sign * (hh * 3600 + mm * 60);
            continue;
        }
        if let Ok(n) = t.parse::<u32>() {
            if t.len() == 4 {
                year = i32::try_from(n).ok();
            } else if day.is_none() && (1..=31).contains(&n) {
                day = Some(n);
            }
        }
    }
    let date = NaiveDate::from_ymd_opt(year?, month?, day?)?;
    let naive = date.and_time(time.unwrap_or(NaiveTime::MIN));
    let utc = Utc.from_utc_datetime(&naive);
    Some(utc - chrono::Duration::seconds(offset_secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_all(data: &[u8], max: usize) -> Vec<MboxMessage> {
        let mut r = MboxReader::new(std::io::Cursor::new(data.to_vec()), 0, max);
        let mut v = Vec::new();
        while let Some(m) = r.next_message().unwrap() {
            v.push(m);
            assert!(v.len() < 10_000);
        }
        v
    }

    #[test]
    fn splits_and_unescapes() {
        let data = b"From a@x Tue Jan  2 10:00:00 2024\nSubject: uno\n\nhola\n>From aqui\n>>From alla\n\nFrom b@x Wed Jan  3 11:00:00 2024\nSubject: dos\n\nadios\n";
        let v = read_all(data, 1 << 20);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].raw, b"Subject: uno\n\nhola\nFrom aqui\n>From alla\n");
        assert_eq!(v[1].raw, b"Subject: dos\n\nadios\n");
        assert_eq!(v[0].offset, 0);
        assert_eq!(v[0].end, v[1].offset);
        assert_eq!(v[1].end, data.len() as u64);
        assert_eq!(v[0].from_date.unwrap().to_rfc3339(), "2024-01-02T10:00:00+00:00");
    }

    #[test]
    fn crlf_and_unescaped_from_in_body() {
        let data = b"From - Tue Jan 02 10:00:00 2024\r\nSubject: uno\r\n\r\nFrom the desk of Ana\r\n\r\nFrom here on\r\n\r\nFrom - Wed Jan 03 11:00:00 2024\r\nSubject: dos\r\n\r\nx\r\n";
        let v = read_all(data, 1 << 20);
        assert_eq!(v.len(), 2, "un «From» sin fecha no parte el mensaje");
        assert!(v[0].raw.ends_with(b"From here on\r\n"));
        assert_eq!(v[1].raw, b"Subject: dos\r\n\r\nx\r\n");
    }

    #[test]
    fn oversized_is_reported_and_skipped() {
        let mut data = b"From a@x Tue Jan  2 10:00:00 2024\nSubject: grande\n\n".to_vec();
        data.extend(std::iter::repeat_n(b'a', 5000));
        data.extend_from_slice(b"\n\nFrom b@x Tue Jan  2 10:00:00 2024\nSubject: chico\n\nok\n");
        let v = read_all(&data, 2048);
        assert_eq!(v.len(), 2);
        assert!(v[0].oversized.is_some_and(|s| s > 5000));
        assert!(v[0].raw.is_empty());
        assert_eq!(v[1].raw, b"Subject: chico\n\nok\n");
    }

    #[test]
    fn resume_from_offset() {
        let data = b"From a@x Tue Jan  2 10:00:00 2024\nSubject: uno\n\nx\n\nFrom b@x Wed Jan  3 11:00:00 2024\nSubject: dos\n\ny\n";
        let v = read_all(data, 1 << 20);
        let off = v[1].offset;
        let mut r = MboxReader::new(std::io::Cursor::new(data[off as usize..].to_vec()), off, 1 << 20);
        let m = r.next_message().unwrap().unwrap();
        assert_eq!(m.raw, b"Subject: dos\n\ny\n");
        assert_eq!(m.offset, off);
        assert!(r.next_message().unwrap().is_none());
    }

    #[test]
    fn garbage_and_empty() {
        assert!(read_all(b"", 1024).is_empty());
        let mut r = MboxReader::new(std::io::Cursor::new(b"hola\nmundo\n".to_vec()), 0, 1024);
        assert!(r.next_message().unwrap().is_none());
        assert_eq!(r.leading_garbage, 11);
    }

    #[test]
    fn from_line_dates() {
        assert!(looks_like_from_line(b"From MAILER-DAEMON Fri Jul  8 12:08:34 2011\n"));
        assert!(looks_like_from_line(b"From 1234@xxx Mon Sep 30 12:00:00 +0000 2019\n"));
        assert!(!looks_like_from_line(b"From the desk of Ana\n"));
        let d = parse_from_line_date(b"From 1234@xxx Mon Sep 30 12:00:00 +0200 2019").unwrap();
        assert_eq!(d.to_rfc3339(), "2019-09-30T10:00:00+00:00");
        assert!(parse_from_line_date(b"From x y").is_none());
    }

    proptest::proptest! {
        #[test]
        fn never_panics(b in proptest::collection::vec(proptest::num::u8::ANY, 0..3000)) {
            let mut data = b"From a@x Tue Jan  2 10:00:00 2024\n".to_vec();
            data.extend(b);
            let v = read_all(&data, 1024);
            for m in &v {
                proptest::prop_assert!(m.raw.len() <= 1024 + 2);
                proptest::prop_assert!(m.end >= m.offset);
            }
        }
    }
}
