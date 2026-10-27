//! Editar una serie repetida desde una de sus apariciones (como Outlook y Apple Calendar):
//!
//! - **Solo este evento** ([`detach_occurrence`]): la serie excluye esa fecha (`EXDATE`) y la
//!   aparición editada pasa a ser un evento propio. (Una excepción `RECURRENCE-ID` dentro del
//!   mismo objeto conservaría el vínculo con la serie; queda pendiente — ver `docs/gaps.md`.)
//! - **Este y los siguientes** ([`split_series`]): la serie original termina justo antes de la
//!   aparición (`UNTIL`) y nace una serie nueva (otro `UID`) desde ella con los cambios; `COUNT`
//!   se reparte entre las dos y los `EXDATE` van a la mitad que les toca. Es la forma que entienden
//!   todos los servidores (el `RANGE=THISANDFUTURE` de RFC 5545 casi ninguno).
//! - **Toda la serie**: es editar el evento maestro sin más (no hace falta nada de aquí).
//!
//! Son funciones puras sobre [`Event`]: quien llama guarda los resultados con
//! [`crate::ops::save_event`] / [`crate::PimSyncHandle::save_event`] (y así se suben y se pueden
//! deshacer como cualquier edición).

use chrono::{DateTime, Duration, NaiveDate, Utc};
use chrono_tz::Tz;
use iuredesk_dav::recur::{Limits, expand};
use iuredesk_model::{Event, EventId, EventTime};

use crate::error::{PimError, Result};
use crate::map::ical::new_event_ics;
use crate::ops::new_uid;

/// Alcance de una edición sobre una aparición de una serie.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SeriesScope {
    /// Solo esta aparición.
    This,
    /// Esta y las siguientes.
    ThisAndFollowing,
    /// Toda la serie.
    All,
}

/// Valor de `EXDATE` normalizado de una aparición (`AAAAMMDD` o `AAAAMMDDTHHMMSSZ`, como los que
/// guarda el mapeo de `pim-sync`).
pub fn exdate_value(start: &EventTime) -> String {
    match start {
        EventTime::Date(d) => d.format("%Y%m%d").to_string(),
        EventTime::DateTime { utc, .. } => utc.format("%Y%m%dT%H%M%SZ").to_string(),
    }
}

/// Instante de un `EXDATE` normalizado (los de día completo, a medianoche UTC).
fn exdate_instant(v: &str) -> Option<DateTime<Utc>> {
    let v = v.trim();
    if v.len() == 8 {
        return NaiveDate::parse_from_str(v, "%Y%m%d").ok().map(|d| d.and_time(chrono::NaiveTime::MIN).and_utc());
    }
    chrono::NaiveDateTime::parse_from_str(v.trim_end_matches('Z'), "%Y%m%dT%H%M%S").ok().map(|t| t.and_utc())
}

/// Instante de comparación de una aparición.
fn instant(t: &EventTime) -> DateTime<Utc> {
    t.to_utc()
}

/// La serie sin la aparición que empieza en `at`.
pub fn exclude_occurrence(master: &Event, at: &EventTime) -> Event {
    let mut m = master.clone();
    let v = exdate_value(at);
    if !m.exdates.contains(&v) {
        m.exdates.push(v);
    }
    m.exdates.sort();
    m.exdates.dedup();
    m
}

/// Copia «nueva» de un evento: sin id, sin vínculo con el servidor y con otro `UID`.
fn fresh_copy(e: &Event) -> Event {
    Event {
        id: EventId::default(),
        uid: new_uid("iuredesk"),
        remote_id: None,
        etag: None,
        sequence: 0,
        updated: None,
        ..e.clone()
    }
}

/// «Solo este evento»: devuelve `(serie sin esa fecha, evento suelto con los cambios)`. `edited`
/// trae los datos editados de la aparición (fechas incluidas); el resultado no se repite.
pub fn detach_occurrence(master: &Event, at: &EventTime, edited: &Event) -> (Event, Event) {
    let series = exclude_occurrence(master, at);
    let mut single = fresh_copy(edited);
    single.calendar = edited.calendar;
    single.rrule = None;
    single.exdates.clear();
    (series, single)
}

/// Partes de una `RRULE` (`CLAVE=valor`), sin vacías.
fn rule_parts(rrule: &str) -> Vec<(String, String)> {
    rrule
        .split(';')
        .take(64)
        .filter_map(|p| {
            let (k, v) = p.split_once('=')?;
            let k = k.trim().to_ascii_uppercase();
            (!k.is_empty()).then(|| (k, v.trim().to_string()))
        })
        .collect()
}

fn join_parts(parts: &[(String, String)]) -> String {
    parts.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(";")
}

/// Repeticiones de `master` (sin contar `EXDATE`, como hace `COUNT`) que empiezan antes de `at`.
fn occurrences_before(master: &Event, at: DateTime<Utc>, user_tz: Tz) -> usize {
    let start = instant(&master.start);
    if at <= start {
        return 0;
    }
    let mut plain = master.clone();
    plain.exdates.clear();
    let cal = new_event_ics(&plain, DateTime::<Utc>::UNIX_EPOCH);
    let limits = Limits::default();
    // La ventana de la expansión está acotada: con series muy largas se cuenta solo lo último.
    let from = start.max(at - limits.max_window + Duration::days(1));
    match expand(&cal, from, at, user_tz, limits) {
        Ok(exp) => exp.instances.iter().filter(|i| i.start < at).count(),
        Err(_) => 0,
    }
}

/// La serie terminada justo antes de la aparición `at` (`UNTIL`; `COUNT` se cambia por `UNTIL`).
/// `None` si `at` es la primera aparición (no queda nada antes).
pub fn truncate_before(master: &Event, at: &EventTime) -> Option<Event> {
    let rrule = master.rrule.as_deref()?;
    if instant(at) <= instant(&master.start) {
        return None;
    }
    let until = match at {
        EventTime::Date(d) => (*d - Duration::days(1)).format("%Y%m%d").to_string(),
        EventTime::DateTime { utc, .. } => (*utc - Duration::seconds(1)).format("%Y%m%dT%H%M%SZ").to_string(),
    };
    let mut parts: Vec<(String, String)> =
        rule_parts(rrule).into_iter().filter(|(k, _)| k != "COUNT" && k != "UNTIL").collect();
    parts.push(("UNTIL".into(), until));
    let mut m = master.clone();
    m.rrule = Some(join_parts(&parts));
    let cut = instant(at);
    m.exdates.retain(|x| exdate_instant(x).is_none_or(|t| t < cut));
    Some(m)
}

/// «Este y los siguientes»: devuelve `(serie original recortada, serie nueva)`. La serie nueva
/// sale de `edited` (fechas incluidas: la aparición editada es su primera repetición), con la
/// regla de `edited` (o la original si no la cambió), otro `UID` y la parte de `COUNT` que
/// quedaba. Si `at` es la primera aparición, la serie recortada es `None`: hay que editar la
/// serie entera en su lugar.
pub fn split_series(master: &Event, at: &EventTime, edited: &Event, user_tz: Tz) -> Result<(Option<Event>, Event)> {
    let rrule = master
        .rrule
        .as_deref()
        .filter(|r| !r.trim().is_empty())
        .ok_or_else(|| PimError::Invalid("el evento no se repite".into()))?;
    let head = truncate_before(master, at);
    let mut tail = fresh_copy(edited);
    let cut = instant(at);
    let new_rule = edited.rrule.as_deref().filter(|r| !r.trim().is_empty()).unwrap_or(rrule);
    let mut parts = rule_parts(new_rule);
    // Si la regla no cambió y llevaba `COUNT`, la serie nueva se queda con lo que faltaba.
    if new_rule == rrule
        && let Some(pos) = parts.iter().position(|(k, _)| k == "COUNT")
        && let Some(count) = parts.get(pos).and_then(|(_, v)| v.parse::<usize>().ok())
    {
        let before = occurrences_before(master, cut, user_tz);
        let left = count.saturating_sub(before).max(1);
        if let Some(p) = parts.get_mut(pos) {
            p.1 = left.to_string();
        }
    }
    tail.rrule = Some(join_parts(&parts));
    // Los `EXDATE` de la parte que sigue se quedan con la serie nueva, corridos lo que se haya
    // movido la aparición.
    let shift = instant(&edited.start) - cut;
    tail.exdates = master
        .exdates
        .iter()
        .filter_map(|x| {
            let t = exdate_instant(x)?;
            if t < cut {
                return None;
            }
            Some(if x.len() == 8 {
                (t + shift).format("%Y%m%d").to_string()
            } else {
                (t + shift).format("%Y%m%dT%H%M%SZ").to_string()
            })
        })
        .collect();
    Ok((head, tail))
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn weekly(count: Option<u32>) -> Event {
        let start = Utc.with_ymd_and_hms(2026, 3, 2, 9, 0, 0).unwrap();
        Event {
            id: EventId(7),
            uid: "serie@x".into(),
            summary: "Semanal".into(),
            start: EventTime::DateTime { utc: start, tzid: None },
            end: EventTime::DateTime { utc: start + Duration::hours(1), tzid: None },
            rrule: Some(match count {
                Some(c) => format!("FREQ=WEEKLY;COUNT={c}"),
                None => "FREQ=WEEKLY".into(),
            }),
            exdates: vec!["20260309T090000Z".into(), "20260330T090000Z".into()],
            remote_id: Some("/cal/serie.ics".into()),
            ..Default::default()
        }
    }

    fn at(d: u32) -> EventTime {
        EventTime::DateTime { utc: Utc.with_ymd_and_hms(2026, 3, d, 9, 0, 0).unwrap(), tzid: None }
    }

    #[test]
    fn this_only_excludes_and_detaches() {
        let m = weekly(None);
        let mut edited = m.clone();
        edited.summary = "Movida".into();
        let (series, single) = detach_occurrence(&m, &at(16), &edited);
        assert!(series.exdates.contains(&"20260316T090000Z".to_string()));
        assert_eq!(series.id, m.id);
        assert!(single.id.is_unassigned() && single.rrule.is_none() && single.remote_id.is_none());
        assert_ne!(single.uid, m.uid);
        assert_eq!(single.summary, "Movida");
    }

    #[test]
    fn split_moves_count_and_exdates() {
        let m = weekly(Some(10));
        let mut edited = m.clone();
        edited.start = at(23);
        edited.end = EventTime::DateTime { utc: Utc.with_ymd_and_hms(2026, 3, 23, 10, 0, 0).unwrap(), tzid: None };
        let (head, tail) = split_series(&m, &at(23), &edited, chrono_tz::UTC).unwrap();
        let head = head.unwrap();
        assert_eq!(head.rrule.as_deref(), Some("FREQ=WEEKLY;UNTIL=20260323T085959Z"));
        assert_eq!(head.exdates, vec!["20260309T090000Z".to_string()]);
        // Antes del 23: 2, 9 y 16 (el 9 excluido también cuenta para COUNT) → quedan 7.
        assert_eq!(tail.rrule.as_deref(), Some("FREQ=WEEKLY;COUNT=7"));
        assert_eq!(tail.exdates, vec!["20260330T090000Z".to_string()]);
        assert!(tail.id.is_unassigned());
        assert_ne!(tail.uid, m.uid);
    }

    #[test]
    fn split_at_first_is_whole_series() {
        let m = weekly(None);
        let (head, tail) = split_series(&m, &at(2), &m, chrono_tz::UTC).unwrap();
        assert!(head.is_none());
        assert_eq!(tail.rrule.as_deref(), Some("FREQ=WEEKLY"));
    }

    #[test]
    fn split_shifts_exdates_with_the_moved_occurrence() {
        let m = weekly(None);
        let mut edited = m.clone();
        edited.start = EventTime::DateTime { utc: Utc.with_ymd_and_hms(2026, 3, 23, 10, 0, 0).unwrap(), tzid: None };
        let (_, tail) = split_series(&m, &at(23), &edited, chrono_tz::UTC).unwrap();
        assert_eq!(tail.exdates, vec!["20260330T100000Z".to_string()]);
    }

    #[test]
    fn all_day_truncation_uses_dates() {
        let mut m = weekly(None);
        m.start = EventTime::Date(NaiveDate::from_ymd_opt(2026, 3, 2).unwrap());
        m.end = EventTime::Date(NaiveDate::from_ymd_opt(2026, 3, 3).unwrap());
        m.exdates.clear();
        let t = truncate_before(&m, &EventTime::Date(NaiveDate::from_ymd_opt(2026, 3, 16).unwrap())).unwrap();
        assert_eq!(t.rrule.as_deref(), Some("FREQ=WEEKLY;UNTIL=20260315"));
    }

    #[test]
    fn not_recurring_is_an_error() {
        let mut m = weekly(None);
        m.rrule = None;
        assert!(split_series(&m, &at(9), &m, chrono_tz::UTC).is_err());
    }
}
