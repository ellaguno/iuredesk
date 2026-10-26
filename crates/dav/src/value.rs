//! Valores de iCalendar: texto escapado, fechas, fechas-hora y duraciones (RFC 5545 §3.3).

use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};

use crate::ical::Property;
use crate::{Error, Result};

/// Quita los escapes de un valor de texto (`\n`, `\,`, `\;`, `\\`).
pub fn unescape_text(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n' | 'N') => out.push('\n'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// Escapa un texto para usarlo como valor (`\`, `;`, `,`, saltos de línea).
pub fn escape_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len().saturating_add(8));
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    for c in normalized.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out
}

/// Parte un valor crudo por un separador no escapado (`,` en CATEGORIES, `;` en N/ADR).
/// Cada parte queda sin escapes.
pub fn split_unescaped(raw: &str, sep: char) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut escaped = false;
    for c in raw.chars() {
        if escaped {
            cur.push('\\');
            cur.push(c);
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == sep {
            parts.push(unescape_text(&cur));
            cur.clear();
        } else {
            cur.push(c);
        }
    }
    if escaped {
        cur.push('\\');
    }
    parts.push(unescape_text(&cur));
    parts
}

/// Fecha o fecha-hora tal como viene en el archivo.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DateTimeValue {
    /// `VALUE=DATE`: día completo, sin hora ni zona.
    Date(NaiveDate),
    /// Hora «flotante» (sin `Z` ni `TZID`): la misma hora de reloj en cualquier zona.
    Floating(NaiveDateTime),
    /// Hora UTC (`…Z`).
    Utc(DateTime<Utc>),
    /// Hora local en la zona `TZID`.
    Zoned {
        /// Hora de reloj.
        local: NaiveDateTime,
        /// `TZID` tal cual (puede ser IANA, de Windows o propio de un `VTIMEZONE`).
        tzid: String,
    },
}

impl DateTimeValue {
    /// ¿Es un día completo?
    pub fn is_date(&self) -> bool {
        matches!(self, Self::Date(_))
    }

    /// Hora de reloj (las fechas a medianoche; UTC en UTC).
    pub fn naive(&self) -> NaiveDateTime {
        match self {
            Self::Date(d) => d.and_time(NaiveTime::MIN),
            Self::Floating(n) | Self::Zoned { local: n, .. } => *n,
            Self::Utc(u) => u.naive_utc(),
        }
    }

    /// Lee el valor de una propiedad de fecha (`DTSTART`, `DTEND`, `RECURRENCE-ID`, `DUE`…).
    pub fn from_property(prop: &Property) -> Result<Self> {
        let first = prop.value.split(',').next().unwrap_or_default();
        parse_date_time(first, prop.param("TZID"), prop.param("VALUE"))
    }

    /// Lee todos los valores de una propiedad multivalor (`EXDATE`, `RDATE`). Los periodos
    /// (`inicio/fin`) devuelven el inicio.
    pub fn list_from_property(prop: &Property, max: usize) -> Result<Vec<Self>> {
        let mut out = Vec::new();
        for v in prop.value.split(',').filter(|v| !v.trim().is_empty()) {
            if out.len() >= max {
                return Err(Error::Limit(format!("más de {max} fechas en {}", prop.name)));
            }
            let start = v.split('/').next().unwrap_or_default();
            let value = if prop.param("VALUE").is_some_and(|p| p.eq_ignore_ascii_case("PERIOD")) {
                None
            } else {
                prop.param("VALUE")
            };
            out.push(parse_date_time(start, prop.param("TZID"), value)?);
        }
        Ok(out)
    }

    /// Escribe el valor como propiedad `name` (con `VALUE=DATE` o `TZID` según el caso).
    pub fn to_property(&self, name: &str) -> Property {
        match self {
            Self::Date(d) => Property::new(name, &d.format("%Y%m%d").to_string()).with_param("VALUE", "DATE"),
            Self::Floating(n) => Property::new(name, &n.format("%Y%m%dT%H%M%S").to_string()),
            Self::Utc(u) => Property::new(name, &u.format("%Y%m%dT%H%M%SZ").to_string()),
            Self::Zoned { local, tzid } => {
                Property::new(name, &local.format("%Y%m%dT%H%M%S").to_string()).with_param("TZID", tzid)
            }
        }
    }
}

/// Lee una fecha (`AAAAMMDD`) o fecha-hora (`AAAAMMDDTHHMMSS[Z]`). Tolera guiones y dos puntos
/// (`2026-03-08T09:00:00`), que algunos generadores escriben.
pub fn parse_date_time(raw: &str, tzid: Option<&str>, value_type: Option<&str>) -> Result<DateTimeValue> {
    let cleaned: String = raw.trim().chars().filter(|c| *c != '-' && *c != ':').collect();
    let bad = || Error::Parse(format!("fecha no válida «{}»", raw.chars().take(40).collect::<String>()));
    let (date_part, time_part) = match cleaned.split_once(['T', 't']) {
        Some((d, t)) => (d, Some(t)),
        None => (cleaned.as_str(), None),
    };
    let date = parse_date(date_part).ok_or_else(bad)?;
    let wants_date = value_type.is_some_and(|v| v.eq_ignore_ascii_case("DATE"));
    let Some(time_part) = time_part.filter(|_| !wants_date) else {
        return Ok(DateTimeValue::Date(date));
    };
    let (time_digits, utc) = match time_part.strip_suffix(['Z', 'z']) {
        Some(t) => (t, true),
        None => (time_part, false),
    };
    let digits = time_digits.get(..6).unwrap_or(time_digits);
    if digits.len() < 4 || !digits.chars().all(|c| c.is_ascii_digit()) {
        return Err(bad());
    }
    let num = |a: usize, b: usize| digits.get(a..b).and_then(|s| s.parse::<u32>().ok());
    let (h, m) = (num(0, 2).ok_or_else(bad)?, num(2, 4).ok_or_else(bad)?);
    // Segundo 60 (segundo intercalar) se trata como 59.
    let s = num(4, 6).unwrap_or(0).min(59);
    let time = NaiveTime::from_hms_opt(h, m, s).ok_or_else(bad)?;
    let local = date.and_time(time);
    if utc {
        return Ok(DateTimeValue::Utc(Utc.from_utc_datetime(&local)));
    }
    match tzid.map(|t| t.trim().trim_matches('"')).filter(|t| !t.is_empty()) {
        Some(t) => Ok(DateTimeValue::Zoned { local, tzid: t.to_string() }),
        None => Ok(DateTimeValue::Floating(local)),
    }
}

fn parse_date(s: &str) -> Option<NaiveDate> {
    if s.len() != 8 || !s.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let y = s.get(0..4)?.parse().ok()?;
    let m = s.get(4..6)?.parse().ok()?;
    let d = s.get(6..8)?.parse().ok()?;
    NaiveDate::from_ymd_opt(y, m, d)
}

/// Duración máxima aceptada (≈ 100 años); más es un dato hostil o roto.
const MAX_DURATION_SECS: i64 = 100 * 366 * 86_400;

/// Duración RFC 5545 (`P1D`, `-PT15M`, `P1W`, `P1DT2H`). Acotada a ±100 años.
pub fn parse_duration(raw: &str) -> Result<Duration> {
    let bad = || Error::Parse(format!("duración no válida «{}»", raw.chars().take(40).collect::<String>()));
    let s = raw.trim();
    let (neg, s) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let s = s.strip_prefix(['P', 'p']).ok_or_else(bad)?;
    let mut total: i64 = 0;
    let mut num: i64 = 0;
    let mut have_digit = false;
    let mut in_time = false;
    for c in s.chars() {
        match c {
            '0'..='9' => {
                num = num.saturating_mul(10).saturating_add(i64::from(c as u8 - b'0'));
                have_digit = true;
            }
            'T' | 't' => in_time = true,
            'W' | 'w' | 'D' | 'd' | 'H' | 'h' | 'M' | 'm' | 'S' | 's' => {
                if !have_digit {
                    return Err(bad());
                }
                let unit = match (c.to_ascii_uppercase(), in_time) {
                    ('W', false) => 7 * 86_400,
                    ('D', false) => 86_400,
                    ('H', true) => 3_600,
                    ('M', true) => 60,
                    ('S', true) => 1,
                    _ => return Err(bad()),
                };
                total = total.saturating_add(num.saturating_mul(unit));
                num = 0;
                have_digit = false;
            }
            _ => return Err(bad()),
        }
    }
    if have_digit || total > MAX_DURATION_SECS {
        return Err(bad());
    }
    Ok(Duration::seconds(if neg { -total } else { total }))
}

/// Escribe una duración (`P2D`, `PT1H30M`, `-PT15M`).
pub fn format_duration(d: Duration) -> String {
    let mut secs = d.num_seconds();
    let mut out = String::new();
    if secs < 0 {
        out.push('-');
        secs = secs.saturating_neg();
    }
    out.push('P');
    let days = secs / 86_400;
    let rest = secs % 86_400;
    if days > 0 {
        out.push_str(&format!("{days}D"));
    }
    if rest > 0 || days == 0 {
        out.push('T');
        let (h, m, s) = (rest / 3600, (rest % 3600) / 60, rest % 60);
        if h > 0 {
            out.push_str(&format!("{h}H"));
        }
        if m > 0 {
            out.push_str(&format!("{m}M"));
        }
        if s > 0 || (h == 0 && m == 0) {
            out.push_str(&format!("{s}S"));
        }
    }
    out
}

/// Formatea una hora UTC al estilo iCalendar (`20260308T140000Z`).
pub fn format_utc(t: DateTime<Utc>) -> String {
    t.format("%Y%m%dT%H%M%SZ").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_escapes() {
        assert_eq!(unescape_text("a\\,b\\;c\\nd\\\\e"), "a,b;c\nd\\e");
        assert_eq!(escape_text("a,b;c\nd\\e"), "a\\,b\\;c\\nd\\\\e");
        assert_eq!(split_unescaped("Pérez;Ana;;Dra.;", ';'), vec!["Pérez", "Ana", "", "Dra.", ""]);
        assert_eq!(split_unescaped("a\\,b,c", ','), vec!["a,b", "c"]);
    }

    #[test]
    fn dates() {
        assert_eq!(
            parse_date_time("20260308", None, Some("DATE")).unwrap(),
            DateTimeValue::Date(NaiveDate::from_ymd_opt(2026, 3, 8).unwrap())
        );
        assert!(matches!(parse_date_time("20260308T090000Z", None, None).unwrap(), DateTimeValue::Utc(_)));
        assert!(matches!(
            parse_date_time("2026-03-08T09:00:00", Some("Europe/Madrid"), None).unwrap(),
            DateTimeValue::Zoned { .. }
        ));
        assert!(matches!(parse_date_time("20260308T0900", None, None).unwrap(), DateTimeValue::Floating(_)));
        assert!(parse_date_time("2026", None, None).is_err());
        assert!(parse_date_time("20261340", None, None).is_err());
        assert!(parse_date_time("20260101T2500", None, None).is_err());
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("PT1H30M").unwrap(), Duration::minutes(90));
        assert_eq!(parse_duration("-PT15M").unwrap(), Duration::minutes(-15));
        assert_eq!(parse_duration("P1W").unwrap(), Duration::days(7));
        assert_eq!(parse_duration("P1DT2H").unwrap(), Duration::hours(26));
        assert!(parse_duration("P99999999999999999D").is_err());
        assert!(parse_duration("PT").is_ok());
        assert!(parse_duration("P1H").is_err());
        assert_eq!(format_duration(Duration::minutes(90)), "PT1H30M");
        assert_eq!(format_duration(Duration::days(2)), "P2D");
        assert_eq!(format_duration(Duration::minutes(-15)), "-PT15M");
        assert_eq!(format_duration(Duration::zero()), "PT0S");
    }

    proptest::proptest! {
        #[test]
        fn date_parse_never_panics(s in "\\PC{0,40}") {
            let _ = parse_date_time(&s, Some("X"), None);
            let _ = parse_duration(&s);
        }

        #[test]
        fn escape_round_trip(s in "\\PC{0,100}") {
            proptest::prop_assert_eq!(unescape_text(&escape_text(&s)), s);
        }
    }
}
