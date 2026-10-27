//! Repeticiones de eventos para las vistas de calendario.
//!
//! El almacén devuelve los eventos recurrentes como su evento maestro; aquí se expanden las
//! reglas sencillas (`FREQ=DAILY|WEEKLY|MONTHLY|YEARLY`, `INTERVAL`, `BYDAY` semanal, `COUNT`,
//! `UNTIL`) dentro de la ventana visible. La expansión completa de RFC 5545 (excepciones,
//! `RECURRENCE-ID`, zonas) vive en `iuredesk-dav` y se conecta con la sincronización CalDAV (F2).

use chrono::{DateTime, Datelike, Duration, Months, NaiveDate, Utc, Weekday};
use iuredesk_model::{CalendarId, Event, EventId, EventStatus, Transparency};

pub mod draft;
pub mod drag;
pub mod invite;
pub mod nav;
pub mod reminders;
pub mod repeat;
pub mod schedule;

/// Repeticiones máximas que se generan por serie y ventana.
const MAX_PER_SERIES: usize = 400;

/// Una aparición concreta de un evento.
#[derive(Clone, Debug, PartialEq)]
pub struct Occurrence {
    pub id: EventId,
    pub summary: String,
    pub location: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub all_day: bool,
    /// Primer día y día siguiente al último (solo días completos).
    pub days: Option<(NaiveDate, NaiveDate)>,
    /// Color del calendario (0xRRGGBB).
    pub color: u32,
    pub tentative: bool,
    pub recurring: bool,
    /// Calendario del evento.
    pub calendar: CalendarId,
    /// Inicio original de la aparición en su serie (`RECURRENCE-ID`), si se repite.
    pub recurrence_id: Option<DateTime<Utc>>,
    /// Ocupa tiempo en la disponibilidad (`TRANSP:OPAQUE`).
    pub busy: bool,
    /// Cancelado.
    pub cancelled: bool,
}

#[derive(Debug, Default)]
struct Rule {
    freq: Option<String>,
    interval: i64,
    count: Option<usize>,
    until: Option<DateTime<Utc>>,
    byday: Vec<Weekday>,
}

fn parse_rule(r: &str) -> Rule {
    let mut rule = Rule { interval: 1, ..Default::default() };
    for part in r.split(';').take(32) {
        let Some((k, v)) = part.split_once('=') else { continue };
        match k.trim().to_ascii_uppercase().as_str() {
            "FREQ" => rule.freq = Some(v.trim().to_ascii_uppercase()),
            "INTERVAL" => rule.interval = v.trim().parse::<i64>().unwrap_or(1).clamp(1, 1000),
            "COUNT" => rule.count = v.trim().parse::<usize>().ok().map(|c| c.min(10_000)),
            "UNTIL" => {
                let v = v.trim();
                let d = v.get(..8).and_then(|d| NaiveDate::parse_from_str(d, "%Y%m%d").ok());
                rule.until = d.and_then(|d| d.and_hms_opt(23, 59, 59)).map(|t| t.and_utc());
            }
            "BYDAY" => {
                rule.byday = v
                    .split(',')
                    .filter_map(|d| {
                        let d = d.trim().trim_start_matches(|c: char| c == '+' || c == '-' || c.is_ascii_digit());
                        match d {
                            "MO" => Some(Weekday::Mon),
                            "TU" => Some(Weekday::Tue),
                            "WE" => Some(Weekday::Wed),
                            "TH" => Some(Weekday::Thu),
                            "FR" => Some(Weekday::Fri),
                            "SA" => Some(Weekday::Sat),
                            "SU" => Some(Weekday::Sun),
                            _ => None,
                        }
                    })
                    .collect()
            }
            _ => {}
        }
    }
    rule
}

fn occurrence(e: &Event, start: DateTime<Utc>, len: Duration, color: u32) -> Occurrence {
    let all_day = e.start.is_all_day();
    let days = if all_day {
        let d0 = start.date_naive();
        let span = len.num_days().max(1);
        Some((d0, d0 + Duration::days(span)))
    } else {
        None
    };
    Occurrence {
        id: e.id,
        summary: e.summary.clone(),
        location: e.location.clone(),
        start,
        end: start + len,
        all_day,
        days,
        color,
        tentative: e.status == EventStatus::Tentative,
        recurring: e.is_recurring(),
        calendar: e.calendar,
        recurrence_id: e.is_recurring().then_some(start),
        busy: e.transparency == Transparency::Opaque,
        cancelled: e.status == EventStatus::Cancelled,
    }
}

/// Una aparición de `iuredesk-pim-sync` para las vistas (los días completos se fechan en `tz`).
pub fn from_pim(o: &iuredesk_model::EventOccurrence, color: u32, tz: chrono_tz::Tz) -> Occurrence {
    use iuredesk_model::EventTime;
    let at = |t: &EventTime| match t {
        EventTime::Date(d) => crate::time::tz_day_start(tz, *d),
        other => other.to_utc(),
    };
    let (start, end) = (at(&o.start), at(&o.end));
    let days = match (&o.start, &o.end) {
        (EventTime::Date(a), EventTime::Date(b)) => Some((*a, (*b).max(*a + Duration::days(1)))),
        (EventTime::Date(a), _) => Some((*a, *a + Duration::days(1))),
        _ => None,
    };
    Occurrence {
        id: o.event,
        summary: o.summary.clone(),
        location: o.location.clone(),
        start,
        end: end.max(start),
        all_day: o.is_all_day(),
        days,
        color,
        tentative: o.status == EventStatus::Tentative,
        recurring: o.recurring,
        calendar: o.calendar,
        recurrence_id: if o.recurring { o.recurrence_id.or(Some(start)) } else { None },
        busy: o.transparency == Transparency::Opaque,
        cancelled: o.status == EventStatus::Cancelled,
    }
}

/// Apariciones de `events` que se solapan con `[from, to)`, ordenadas por inicio. `color_of`
/// da el color del calendario de cada evento.
pub fn occurrences(
    events: &[Event],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    color_of: impl Fn(&Event) -> u32,
) -> Vec<Occurrence> {
    let mut out = Vec::new();
    for e in events {
        let start = e.start.to_utc();
        let len = (e.end.to_utc() - start).max(Duration::zero());
        let color = color_of(e);
        let overlaps = |s: DateTime<Utc>| s < to && (s + len > from || (len.is_zero() && s >= from));
        let Some(rule) = e.rrule.as_deref().map(parse_rule) else {
            if overlaps(start) {
                out.push(occurrence(e, start, len, color));
            }
            continue;
        };
        let exdates: Vec<String> = e.exdates.iter().map(|x| x.trim().to_string()).collect();
        let excluded = |s: DateTime<Utc>| {
            let stamp = s.format("%Y%m%dT%H%M%SZ").to_string();
            let day = s.format("%Y%m%d").to_string();
            exdates.iter().any(|x| *x == stamp || *x == day)
        };
        let mut emitted = 0usize;
        let mut produced = 0usize;
        let mut push = |s: DateTime<Utc>, out: &mut Vec<Occurrence>| -> bool {
            produced = produced.saturating_add(1);
            if rule.count.is_some_and(|c| produced > c) || rule.until.is_some_and(|u| s > u) || s >= to {
                return false;
            }
            if overlaps(s) && !excluded(s) {
                out.push(occurrence(e, s, len, color));
                emitted = emitted.saturating_add(1);
            }
            emitted < MAX_PER_SERIES
        };
        match rule.freq.as_deref() {
            Some("DAILY") => {
                let mut s = start;
                for _ in 0..20_000 {
                    if !push(s, &mut out) {
                        break;
                    }
                    s += Duration::days(rule.interval);
                }
            }
            Some("WEEKLY") => {
                let days: Vec<Weekday> = if rule.byday.is_empty() { vec![start.weekday()] } else { rule.byday.clone() };
                // Lunes de la semana del inicio, a la hora del inicio.
                let week0 = start - Duration::days(i64::from(start.weekday().num_days_from_monday()));
                let mut w = 0i64;
                'weeks: for _ in 0..5_000 {
                    let base = week0 + Duration::weeks(w.saturating_mul(rule.interval));
                    if base >= to + Duration::days(7) {
                        break;
                    }
                    let mut wd: Vec<Weekday> = days.clone();
                    wd.sort_by_key(|d| d.num_days_from_monday());
                    for d in wd {
                        let s = base + Duration::days(i64::from(d.num_days_from_monday()));
                        if s < start {
                            continue;
                        }
                        if !push(s, &mut out) {
                            break 'weeks;
                        }
                    }
                    w += 1;
                }
            }
            Some("MONTHLY") | Some("YEARLY") => {
                let step = if rule.freq.as_deref() == Some("YEARLY") { 12 } else { 1 };
                for k in 0..2_000u32 {
                    let months = u32::try_from(rule.interval).unwrap_or(1).saturating_mul(step).saturating_mul(k);
                    let Some(s) = start.checked_add_months(Months::new(months)) else { break };
                    if !push(s, &mut out) {
                        break;
                    }
                }
            }
            // Regla desconocida: el evento maestro solo.
            _ => {
                if overlaps(start) {
                    out.push(occurrence(e, start, len, color));
                }
            }
        }
    }
    out.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)).then(a.id.cmp(&b.id)));
    out
}

/// Columnas para eventos que se solapan en un día: (índice de columna, columnas del grupo) por
/// cada elemento de `spans` (inicio, fin), que deben venir ordenados por inicio.
pub fn layout_columns(spans: &[(i64, i64)]) -> Vec<(usize, usize)> {
    let mut result = vec![(0usize, 1usize); spans.len()];
    let mut group: Vec<usize> = Vec::new();
    let mut cols_end: Vec<i64> = Vec::new();
    let mut group_end = i64::MIN;
    let flush = |group: &mut Vec<usize>, cols: usize, result: &mut Vec<(usize, usize)>| {
        for &i in group.iter() {
            if let Some(r) = result.get_mut(i) {
                r.1 = cols.max(1);
            }
        }
        group.clear();
    };
    for (i, &(s, e)) in spans.iter().enumerate() {
        if s >= group_end && !group.is_empty() {
            let n = cols_end.len();
            flush(&mut group, n, &mut result);
            cols_end.clear();
        }
        let col = match cols_end.iter().position(|&end| end <= s) {
            Some(c) => c,
            None => {
                cols_end.push(i64::MIN);
                cols_end.len() - 1
            }
        };
        if let Some(c) = cols_end.get_mut(col) {
            *c = e.max(s + 1);
        }
        if let Some(r) = result.get_mut(i) {
            r.0 = col;
        }
        group.push(i);
        group_end = group_end.max(e.max(s + 1));
        if group.len() == 1 {
            group_end = e.max(s + 1);
        }
    }
    let n = cols_end.len();
    flush(&mut group, n, &mut result);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use iuredesk_model::EventTime;

    fn ev(start: DateTime<Utc>, mins: i64, rrule: Option<&str>) -> Event {
        Event {
            summary: "x".into(),
            start: EventTime::DateTime { utc: start, tzid: None },
            end: EventTime::DateTime { utc: start + Duration::minutes(mins), tzid: None },
            rrule: rrule.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn weekly_and_daily() {
        let mon = Utc.with_ymd_and_hms(2026, 10, 5, 8, 30, 0).unwrap();
        let from = Utc.with_ymd_and_hms(2026, 10, 19, 0, 0, 0).unwrap();
        let to = from + Duration::days(7);
        let e = ev(mon, 30, Some("FREQ=WEEKLY;BYDAY=MO"));
        let o = occurrences(std::slice::from_ref(&e), from, to, |_| 0);
        assert_eq!(o.len(), 1);
        assert_eq!(o[0].start, Utc.with_ymd_and_hms(2026, 10, 19, 8, 30, 0).unwrap());
        let d = ev(mon, 30, Some("FREQ=DAILY;COUNT=17"));
        assert_eq!(occurrences(std::slice::from_ref(&d), from, to, |_| 0).len(), 3, "días 14, 15 y 16 de la cuenta");
        let mwf = ev(mon, 30, Some("FREQ=WEEKLY;BYDAY=MO,WE,FR;UNTIL=20261021"));
        assert_eq!(occurrences(std::slice::from_ref(&mwf), from, to, |_| 0).len(), 2);
        // Regla rara: no se cuelga ni falla.
        let bad = ev(mon, 30, Some("FREQ=SECONDLY;INTERVAL=0"));
        assert!(occurrences(std::slice::from_ref(&bad), from, to, |_| 0).is_empty());
    }

    #[test]
    fn columns_for_overlaps() {
        // 9-10, 9:30-11, 10-10:30, 12-13
        let spans = [(540, 600), (570, 660), (600, 630), (720, 780)];
        let l = layout_columns(&spans);
        assert_eq!(l[0], (0, 2));
        assert_eq!(l[1], (1, 2));
        assert_eq!(l[2], (0, 2), "reutiliza la primera columna");
        assert_eq!(l[3], (0, 1));
    }
}
