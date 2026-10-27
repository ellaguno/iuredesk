//! Patrón de repetición binario de Outlook (`PidLidAppointmentRecur`, [MS-OXOCAL] 2.2.1.44) →
//! `RRULE` de iCalendar + `EXDATE`.
//!
//! Se traducen los patrones comunes (diario, semanal con días, mensual por día o «n-ésimo día»,
//! anual, último día del mes) con su fin (fecha, número de veces o nunca). Las repeticiones
//! borradas pasan a `EXDATE`; las *modificadas* (movidas o editadas una a una) no se importan como
//! excepciones: se cuentan para avisar al usuario.

use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, Utc};
use chrono_tz::Tz;

/// Resultado de traducir un patrón.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Recurrence {
    /// Regla (`FREQ=WEEKLY;BYDAY=MO,WE`…), sin el prefijo `RRULE:`.
    pub rrule: String,
    /// Repeticiones borradas (`YYYYMMDD` en día completo, `YYYYMMDDTHHMMSSZ` si no).
    pub exdates: Vec<String>,
    /// Repeticiones modificadas que no se importan.
    pub modified: usize,
}

const DAYS: [&str; 7] = ["SU", "MO", "TU", "WE", "TH", "FR", "SA"];
/// Máximo de fechas borradas/modificadas que se leen.
const MAX_INSTANCES: usize = 2_000;

struct Cur<'a> {
    b: &'a [u8],
    at: usize,
}

impl Cur<'_> {
    fn u16(&mut self) -> Option<u16> {
        let v = u16::from_le_bytes(self.b.get(self.at..self.at.checked_add(2)?)?.try_into().ok()?);
        self.at = self.at.saturating_add(2);
        Some(v)
    }
    fn u32(&mut self) -> Option<u32> {
        let v = u32::from_le_bytes(self.b.get(self.at..self.at.checked_add(4)?)?.try_into().ok()?);
        self.at = self.at.saturating_add(4);
        Some(v)
    }
}

/// Minutos desde 1601-01-01 → fecha y hora (sin zona).
pub fn minutes_to_naive(m: u32) -> Option<NaiveDateTime> {
    let base = NaiveDate::from_ymd_opt(1601, 1, 1)?.and_hms_opt(0, 0, 0)?;
    base.checked_add_signed(Duration::minutes(i64::from(m)))
}

fn byday(mask: u32) -> String {
    DAYS.iter().enumerate().filter(|(i, _)| mask & (1 << i) != 0).map(|(_, d)| *d).collect::<Vec<_>>().join(",")
}

/// Traduce el patrón. `all_day` y `tz` (zona de la cita) deciden el formato de `EXDATE`/`UNTIL`.
pub fn parse(b: &[u8], all_day: bool, tz: Tz) -> Option<Recurrence> {
    let mut c = Cur { b, at: 0 };
    let _reader = c.u16()?;
    let _writer = c.u16()?;
    let freq = c.u16()?;
    let pattern = c.u16()?;
    let _calendar = c.u16()?;
    let _first = c.u32()?;
    let period = c.u32()?;
    let _sliding = c.u32()?;
    let (days, nth, dom) = match pattern {
        0x0000 => (0, 0, 0),
        0x0001 => (c.u32()?, 0, 0),
        0x0002 | 0x0003 | 0x000A | 0x000C => (0, 0, c.u32()?),
        0x0004 | 0x000B => {
            let d = c.u32()?;
            (d, c.u32()?, 0)
        }
        _ => return None,
    };
    let end_type = c.u32()?;
    let count = c.u32()?;
    let first_dow = c.u32()?;
    let deleted_n = usize::try_from(c.u32()?).ok()?.min(MAX_INSTANCES);
    let mut deleted = Vec::with_capacity(deleted_n);
    for _ in 0..deleted_n {
        deleted.push(c.u32()?);
    }
    let modified_n = usize::try_from(c.u32()?).ok()?.min(MAX_INSTANCES);
    let mut modified = Vec::with_capacity(modified_n);
    for _ in 0..modified_n {
        modified.push(c.u32()?);
    }
    let start_date = c.u32()?;
    let end_date = c.u32()?;
    // Parte de `AppointmentRecurrencePattern`: desplazamiento de la hora de inicio (si existe).
    let _reader2 = c.u32();
    let _writer2 = c.u32();
    let start_offset = c.u32().unwrap_or(0);

    let mut rule: Vec<String> = Vec::new();
    match (freq, pattern) {
        (0x200A, 0x0000) => {
            rule.push("FREQ=DAILY".into());
            let n = (period / 1440).max(1);
            if n > 1 {
                rule.push(format!("INTERVAL={n}"));
            }
        }
        (0x200A | 0x200B, 0x0001) => {
            rule.push("FREQ=WEEKLY".into());
            if period > 1 {
                rule.push(format!("INTERVAL={period}"));
            }
            let d = byday(days);
            if !d.is_empty() {
                rule.push(format!("BYDAY={d}"));
            }
            if let Some(w) = DAYS.get(usize::try_from(first_dow).ok()?).filter(|_| first_dow != 1) {
                rule.push(format!("WKST={w}"));
            }
        }
        (0x200C | 0x200D, _) => {
            let yearly = freq == 0x200D || period % 12 == 0 && period >= 12;
            if yearly {
                rule.push("FREQ=YEARLY".into());
                let n = (period / 12).max(1);
                if n > 1 {
                    rule.push(format!("INTERVAL={n}"));
                }
                // Sin BYMONTH, `FREQ=YEARLY;BYMONTHDAY=1` sería «cada día 1 de cada mes».
                if let Some(start) = minutes_to_naive(start_date) {
                    rule.push(format!("BYMONTH={}", chrono::Datelike::month(&start.date())));
                }
            } else {
                rule.push("FREQ=MONTHLY".into());
                if period > 1 {
                    rule.push(format!("INTERVAL={period}"));
                }
            }
            match pattern {
                0x0002 | 0x000A => {
                    if (1..=31).contains(&dom) {
                        rule.push(format!("BYMONTHDAY={dom}"));
                    }
                }
                0x0003 | 0x000C => rule.push("BYMONTHDAY=-1".into()),
                0x0004 | 0x000B => {
                    let d = byday(days);
                    if !d.is_empty() {
                        rule.push(format!("BYDAY={d}"));
                    }
                    rule.push(format!("BYSETPOS={}", if nth >= 5 { -1 } else { i64::from(nth.max(1)) }));
                }
                _ => {}
            }
        }
        _ => return None,
    }
    let time_of = |date_minutes: u32| -> Option<DateTime<Utc>> {
        let local = minutes_to_naive(date_minutes.checked_add(start_offset)?)?;
        Some(iuredesk_dav::tz::local_to_utc(local, tz))
    };
    match end_type {
        0x2021 => {
            let d = minutes_to_naive(end_date)?.date();
            if all_day {
                rule.push(format!("UNTIL={}", d.format("%Y%m%d")));
            } else {
                let until =
                    time_of(end_date).unwrap_or_else(|| d.and_hms_opt(23, 59, 59).unwrap_or_default().and_utc());
                rule.push(format!("UNTIL={}", until.format("%Y%m%dT%H%M%SZ")));
            }
        }
        0x2022 => rule.push(format!("COUNT={}", count.max(1))),
        _ => {}
    }
    let exdates = deleted
        .iter()
        .filter(|d| !modified.contains(d))
        .filter_map(|&d| {
            if all_day {
                minutes_to_naive(d).map(|n| n.date().format("%Y%m%d").to_string())
            } else {
                time_of(d).map(|t| t.format("%Y%m%dT%H%M%SZ").to_string())
            }
        })
        .collect();
    Some(Recurrence { rrule: rule.join(";"), exdates, modified: modified.len() })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Minutos desde 1601 de una fecha.
    pub(crate) fn mins(y: i32, m: u32, d: u32) -> u32 {
        let base = NaiveDate::from_ymd_opt(1601, 1, 1).unwrap();
        let date = NaiveDate::from_ymd_opt(y, m, d).unwrap();
        u32::try_from((date - base).num_minutes()).unwrap()
    }

    /// Patrón binario de prueba.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn pattern(
        freq: u16,
        ptype: u16,
        period: u32,
        specific: &[u32],
        end_type: u32,
        count: u32,
        deleted: &[u32],
        modified: &[u32],
        start: u32,
        end: u32,
        start_offset: u32,
    ) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend(0x3004u16.to_le_bytes());
        v.extend(0x3004u16.to_le_bytes());
        v.extend(freq.to_le_bytes());
        v.extend(ptype.to_le_bytes());
        v.extend(0u16.to_le_bytes());
        v.extend(0u32.to_le_bytes());
        v.extend(period.to_le_bytes());
        v.extend(0u32.to_le_bytes());
        for s in specific {
            v.extend(s.to_le_bytes());
        }
        v.extend(end_type.to_le_bytes());
        v.extend(count.to_le_bytes());
        v.extend(1u32.to_le_bytes());
        v.extend((deleted.len() as u32).to_le_bytes());
        for d in deleted {
            v.extend(d.to_le_bytes());
        }
        v.extend((modified.len() as u32).to_le_bytes());
        for d in modified {
            v.extend(d.to_le_bytes());
        }
        v.extend(start.to_le_bytes());
        v.extend(end.to_le_bytes());
        v.extend(0x3006u32.to_le_bytes());
        v.extend(0x3009u32.to_le_bytes());
        v.extend(start_offset.to_le_bytes());
        v.extend((start_offset + 60).to_le_bytes());
        v.extend(0u16.to_le_bytes());
        v
    }

    #[test]
    fn weekly_with_count_and_exceptions() {
        let tz: Tz = "America/Mexico_City".parse().unwrap();
        let start = mins(2026, 1, 5);
        let del = [mins(2026, 1, 12), mins(2026, 1, 19)];
        let modi = [mins(2026, 1, 19)];
        let b = pattern(0x200B, 0x0001, 1, &[0b0010_1010], 0x2022, 10, &del, &modi, start, start, 9 * 60);
        let r = parse(&b, false, tz).unwrap();
        assert_eq!(r.rrule, "FREQ=WEEKLY;BYDAY=MO,WE,FR;COUNT=10");
        // 12 de enero, 9:00 en Ciudad de México = 15:00 UTC.
        assert_eq!(r.exdates, vec!["20260112T150000Z".to_string()]);
        assert_eq!(r.modified, 1);
    }

    #[test]
    fn daily_monthly_yearly() {
        let tz = chrono_tz::UTC;
        let s = mins(2026, 3, 1);
        let b = pattern(0x200A, 0x0000, 2 * 1440, &[], 0x2023, 0, &[], &[], s, 0x5AE9_80DF, 0);
        assert_eq!(parse(&b, true, tz).unwrap().rrule, "FREQ=DAILY;INTERVAL=2");
        let b = pattern(0x200C, 0x0002, 1, &[15], 0x2021, 0, &[], &[], s, mins(2026, 12, 31), 600);
        assert_eq!(parse(&b, false, tz).unwrap().rrule, "FREQ=MONTHLY;BYMONTHDAY=15;UNTIL=20261231T100000Z");
        let b = pattern(0x200C, 0x0004, 1, &[0b0000_0010, 5], 0x2023, 0, &[], &[], s, 0, 0);
        assert_eq!(parse(&b, false, tz).unwrap().rrule, "FREQ=MONTHLY;BYDAY=MO;BYSETPOS=-1");
        let b = pattern(0x200D, 0x0002, 12, &[1], 0x2021, 0, &[mins(2027, 3, 1)], &[], s, mins(2030, 3, 1), 0);
        let r = parse(&b, true, tz).unwrap();
        assert_eq!(r.rrule, "FREQ=YEARLY;BYMONTH=3;BYMONTHDAY=1;UNTIL=20300301");
        assert_eq!(r.exdates, vec!["20270301".to_string()]);
        let b = pattern(0x200C, 0x0003, 1, &[31], 0x2023, 0, &[], &[], s, 0, 0);
        assert_eq!(parse(&b, true, tz).unwrap().rrule, "FREQ=MONTHLY;BYMONTHDAY=-1");
    }

    proptest::proptest! {
        #[test]
        fn never_panics(b in proptest::collection::vec(proptest::num::u8::ANY, 0..300)) {
            let _ = parse(&b, false, chrono_tz::UTC);
            let _ = parse(&b, true, chrono_tz::UTC);
        }
    }
}
