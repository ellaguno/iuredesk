//! Asistente de programación: con los intervalos ocupados de cada persona (los míos salen del
//! calendario local; los de los demás, del servidor si lo dice), propone huecos libres para todos
//! dentro del horario laboral.

use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};

use super::drag::{SNAP_MIN, at_minutes};
use crate::time::Zone;

/// Horario laboral por omisión (minutos del día).
pub const WORK_START_MIN: i64 = 9 * 60;
pub const WORK_END_MIN: i64 = 18 * 60;
/// Paso entre propuestas.
const STEP_MIN: i64 = 30;

/// Disponibilidad de una persona para el asistente.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Availability {
    /// Todavía se está preguntando.
    Loading,
    /// El servidor no lo sabe o no lo comparte.
    Unknown,
    /// Intervalos ocupados (inicio, fin, provisional).
    Busy(Vec<(DateTime<Utc>, DateTime<Utc>, bool)>),
}

fn overlaps(a0: DateTime<Utc>, a1: DateTime<Utc>, b0: DateTime<Utc>, b1: DateTime<Utc>) -> bool {
    a0 < b1 && b0 < a1
}

/// ¿Está libre `[from, to)` para todos los que tienen disponibilidad conocida?
pub fn all_free(people: &[Availability], from: DateTime<Utc>, to: DateTime<Utc>) -> bool {
    people.iter().all(|p| match p {
        Availability::Busy(b) => !b.iter().any(|(s, e, _)| overlaps(*s, *e, from, to)),
        _ => true,
    })
}

/// Hasta `max` inicios libres para todos, de `len_min` minutos, en días laborables desde `from_day`
/// (máx. 14 días), entre `WORK_START_MIN` y `WORK_END_MIN` en `zone` y no antes de `now`.
pub fn suggest(
    people: &[Availability],
    from_day: NaiveDate,
    len_min: i64,
    zone: Zone,
    now: DateTime<Utc>,
    max: usize,
) -> Vec<DateTime<Utc>> {
    let len = len_min.clamp(SNAP_MIN, 8 * 60);
    let mut out = Vec::new();
    for k in 0..14i64 {
        let day = from_day + Duration::days(k);
        if day.weekday().num_days_from_monday() >= 5 {
            continue;
        }
        let mut m = WORK_START_MIN;
        while m + len <= WORK_END_MIN {
            let s = zone.to_utc(at_minutes(day, m));
            let e = s + Duration::minutes(len);
            if s >= now && all_free(people, s, e) {
                out.push(s);
                if out.len() >= max {
                    return out;
                }
                // Una propuesta por franja: la siguiente, después de esta.
                m += len.max(STEP_MIN);
                continue;
            }
            m += STEP_MIN;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn at(d: u32, h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, d, h, m, 0).unwrap()
    }

    #[test]
    fn suggests_common_gaps_and_skips_weekends() {
        let me = Availability::Busy(vec![(at(21, 9, 0), at(21, 11, 0), false)]);
        let ana = Availability::Busy(vec![(at(21, 11, 0), at(21, 12, 0), true)]);
        let unknown = Availability::Unknown;
        let s = suggest(&[me, ana, unknown], NaiveDate::from_ymd_opt(2026, 10, 21).unwrap(), 60, Zone::utc(), at(21, 8, 0), 3);
        assert_eq!(s, vec![at(21, 12, 0), at(21, 13, 0), at(21, 14, 0)]);
        // Sábado 24 → el lunes 26.
        let s = suggest(&[], NaiveDate::from_ymd_opt(2026, 10, 24).unwrap(), 30, Zone::utc(), at(21, 8, 0), 1);
        assert_eq!(s, vec![at(26, 9, 0)]);
        assert!(all_free(&[Availability::Loading], at(21, 9, 0), at(21, 10, 0)));
    }
}
