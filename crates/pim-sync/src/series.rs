//! Editar una serie repetida desde una de sus apariciones (como Outlook y Apple Calendar):
//!
//! - **Solo este evento** ([`override_occurrence`], R4.5): la aparición queda como excepción
//!   (`RECURRENCE-ID`) dentro de la misma serie, con su título, lugar, notas, horario y estado
//!   propios; sigue siendo parte de la serie para el servidor y los invitados. Si se cambió algo
//!   que una excepción no guarda (invitados, recordatorios, calendario…) o el servidor no las admite
//!   (Graph), se usa [`detach_occurrence`]: la serie excluye esa fecha (`EXDATE`) y la aparición
//!   pasa a ser un evento propio.
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
use iuredesk_model::{Event, EventId, EventOverride, EventTime};

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
    // Su excepción, si la había, se va con ella (R4.5).
    let instant = instant(at);
    m.overrides.retain(|o| !o.is_instance(instant));
    let v = exdate_value(at);
    if !m.exdates.contains(&v) {
        m.exdates.push(v);
    }
    m.exdates.sort();
    m.exdates.dedup();
    m
}

/// «Solo este evento» como excepción de la serie (R4.5): la serie con la aparición que empezaba en
/// `at` cambiada por lo editado. `None` si `edited` cambia algo que una excepción no lleva
/// (invitados, organizador, recordatorios, disponibilidad, videollamada o calendario): entonces se
/// separa con [`detach_occurrence`].
pub fn override_occurrence(master: &Event, at: &EventTime, edited: &Event) -> Option<Event> {
    let fits = edited.attendees == master.attendees
        && edited.organizer == master.organizer
        && edited.reminders == master.reminders
        && edited.transparency == master.transparency
        && edited.online_meeting == master.online_meeting
        && edited.calendar == master.calendar;
    if !fits {
        return None;
    }
    let instant = instant(at);
    let mut m = master.clone();
    let o = EventOverride {
        recurrence_id: at.clone(),
        start: edited.start.clone(),
        end: edited.end.clone(),
        summary: edited.summary.clone(),
        location: edited.location.clone(),
        description: edited.description.clone(),
        status: edited.status.clone(),
    };
    match m.overrides.iter_mut().find(|x| x.is_instance(instant)) {
        Some(x) => *x = o,
        None => m.overrides.push(o),
    }
    // Una aparición excluida que se vuelve a editar deja de estarlo.
    let ex = exdate_value(at);
    m.exdates.retain(|e| *e != ex);
    Some(m)
}

/// R4.5: cambios a toda una serie de Microsoft 365 hechos desde una de sus apariciones (Graph no
/// trae el maestro: se le aplican al servidor con un `PATCH` y en local a las demás apariciones).
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SeriesPatch {
    pub summary: Option<String>,
    pub location: Option<String>,
    pub description: Option<String>,
    /// Corrimiento del inicio, en segundos.
    pub shift_secs: i64,
    /// Duración nueva, en segundos, si cambió.
    pub duration_secs: Option<i64>,
}

impl SeriesPatch {
    /// Lo que cambió de la aparición `old` a `edited` (y el parche que lo deshace).
    pub fn between(old: &Event, edited: &Event) -> (SeriesPatch, SeriesPatch) {
        let changed = |a: &String, b: &String| (a != b).then(|| b.clone());
        let dur = |e: &Event| (instant(&e.end) - instant(&e.start)).num_seconds();
        let shift = (instant(&edited.start) - instant(&old.start)).num_seconds();
        let new_dur = (dur(old) != dur(edited)).then(|| dur(edited));
        let fwd = SeriesPatch {
            summary: changed(&old.summary, &edited.summary),
            location: changed(&old.location, &edited.location),
            description: changed(&old.description, &edited.description),
            shift_secs: shift,
            duration_secs: new_dur,
        };
        let back = SeriesPatch {
            summary: fwd.summary.as_ref().map(|_| old.summary.clone()),
            location: fwd.location.as_ref().map(|_| old.location.clone()),
            description: fwd.description.as_ref().map(|_| old.description.clone()),
            shift_secs: shift.saturating_neg(),
            duration_secs: new_dur.map(|_| dur(old)),
        };
        (fwd, back)
    }

    pub fn is_empty(&self) -> bool {
        self.summary.is_none()
            && self.location.is_none()
            && self.description.is_none()
            && self.shift_secs == 0
            && self.duration_secs.is_none()
    }

    /// Inicio y fin de una aparición (o del maestro) con el parche aplicado.
    pub fn times(&self, start: &EventTime, end: &EventTime) -> (EventTime, EventTime) {
        let shift = Duration::seconds(self.shift_secs.clamp(-3_650 * 86_400, 3_650 * 86_400));
        match (start, end) {
            (EventTime::Date(s), EventTime::Date(e)) => {
                let days = Duration::days(shift.num_days());
                let ns = *s + days;
                let ne = match self.duration_secs {
                    Some(d) => ns + Duration::days((d / 86_400).max(1)),
                    None => *e + days,
                };
                (EventTime::Date(ns), EventTime::Date(ne))
            }
            _ => {
                let tz = |t: &EventTime| match t {
                    EventTime::DateTime { tzid, .. } => tzid.clone(),
                    EventTime::Date(_) => None,
                };
                let ns = instant(start) + shift;
                let ne = match self.duration_secs {
                    Some(d) => ns + Duration::seconds(d.clamp(0, 3_650 * 86_400)),
                    None => instant(end) + shift,
                };
                (EventTime::DateTime { utc: ns, tzid: tz(start) }, EventTime::DateTime { utc: ne, tzid: tz(end) })
            }
        }
    }

    /// Aplica el parche a una aparición: los textos solo si no se habían cambiado aparte (iguales
    /// a los de `like`, la aparición editada antes del cambio).
    pub fn apply(&self, e: &mut Event, like: &Event) {
        if let Some(x) = &self.summary
            && e.summary == like.summary
        {
            e.summary = x.clone();
        }
        if let Some(x) = &self.location
            && e.location == like.location
        {
            e.location = x.clone();
        }
        if let Some(x) = &self.description
            && e.description == like.description
        {
            e.description = x.clone();
        }
        let (s, en) = self.times(&e.start, &e.end);
        e.start = s;
        e.end = en;
    }
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
        // Las excepciones son de la serie original (otro UID): no se copian.
        overrides: Vec::new(),
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
    // Las apariciones cambiadas que ya no están en la serie se van (R4.5).
    m.overrides.retain(|o| o.recurrence_id.to_utc() < cut);
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

    /// R4.5: «solo este» como excepción: se agrega, se reemplaza, saca la fecha de los `EXDATE`, y
    /// no aplica si cambian los invitados; borrar o recortar la serie se lleva sus excepciones.
    #[test]
    fn this_only_as_exception() {
        let m = weekly(None);
        let mut edited = m.clone();
        edited.summary = "Movida".into();
        edited.start = EventTime::DateTime { utc: Utc.with_ymd_and_hms(2026, 3, 16, 11, 0, 0).unwrap(), tzid: None };
        edited.end = EventTime::DateTime { utc: Utc.with_ymd_and_hms(2026, 3, 16, 12, 0, 0).unwrap(), tzid: None };
        let s = override_occurrence(&m, &at(16), &edited).unwrap();
        assert_eq!((s.id, s.uid.as_str(), s.rrule.as_deref()), (m.id, m.uid.as_str(), m.rrule.as_deref()));
        assert_eq!(s.summary, "Semanal", "la serie no cambia");
        assert_eq!(s.overrides.len(), 1);
        assert_eq!(s.overrides[0].recurrence_id, at(16));
        assert_eq!(s.overrides[0].summary, "Movida");
        // Otra vez la misma aparición: se reemplaza.
        edited.summary = "Movida otra vez".into();
        let s = override_occurrence(&s, &at(16), &edited).unwrap();
        assert_eq!(s.overrides.len(), 1);
        assert_eq!(s.overrides[0].summary, "Movida otra vez");
        // Una fecha excluida que se edita vuelve.
        let s2 = override_occurrence(&s, &at(9), &edited).unwrap();
        assert!(!s2.exdates.contains(&"20260309T090000Z".to_string()));
        assert_eq!(s2.overrides.len(), 2);
        // Con otros invitados no cabe en una excepción.
        let mut other = edited.clone();
        other.attendees.push(iuredesk_model::Attendee {
            mailbox: iuredesk_model::Mailbox::new("Luis", "luis@x.com"),
            ..Default::default()
        });
        assert!(override_occurrence(&m, &at(16), &other).is_none());
        // Borrar esa aparición o recortar la serie antes de ella se lleva la excepción.
        assert!(exclude_occurrence(&s2, &at(16)).overrides.iter().all(|o| o.recurrence_id != at(16)));
        let head = truncate_before(&s2, &at(16)).unwrap();
        assert_eq!(head.overrides.len(), 1, "solo la del 9");
        // La serie nueva de «este y los siguientes» no hereda excepciones de otro UID.
        let (_, tail) = split_series(&s2, &at(23), &s2, chrono_tz::UTC).unwrap();
        assert!(tail.overrides.is_empty());
    }

    /// R4.5: parche de toda una serie de Microsoft 365: qué cambió, su inverso, horario corrido
    /// (y días completos) y textos que solo cambian donde no se habían tocado aparte.
    #[test]
    fn series_patch_between_times_and_apply() {
        let m = weekly(None);
        let mut edited = m.clone();
        edited.summary = "Junta nueva".into();
        edited.start = at(2);
        edited.start = EventTime::DateTime { utc: Utc.with_ymd_and_hms(2026, 3, 2, 10, 30, 0).unwrap(), tzid: None };
        edited.end = EventTime::DateTime { utc: Utc.with_ymd_and_hms(2026, 3, 2, 12, 0, 0).unwrap(), tzid: None };
        let (p, back) = SeriesPatch::between(&m, &edited);
        assert_eq!(p.summary.as_deref(), Some("Junta nueva"));
        assert!(p.location.is_none() && p.description.is_none());
        assert_eq!((p.shift_secs, p.duration_secs), (5_400, Some(5_400)));
        assert_eq!(
            (back.summary.as_deref(), back.shift_secs, back.duration_secs),
            (Some("Semanal"), -5_400, Some(3_600))
        );
        assert!(!p.is_empty() && SeriesPatch::between(&m, &m).0.is_empty());
        // Otra aparición (la del 9): se corre igual; su título solo si seguía igual al de la serie.
        let mut other = m.clone();
        other.start = at(9);
        other.end = EventTime::DateTime { utc: Utc.with_ymd_and_hms(2026, 3, 9, 10, 0, 0).unwrap(), tzid: None };
        let mut renamed = other.clone();
        renamed.summary = "Cambiada aparte".into();
        p.apply(&mut other, &m);
        p.apply(&mut renamed, &m);
        assert_eq!(other.summary, "Junta nueva");
        assert_eq!(renamed.summary, "Cambiada aparte");
        assert_eq!(other.start.to_utc(), Utc.with_ymd_and_hms(2026, 3, 9, 10, 30, 0).unwrap());
        assert_eq!(other.end.to_utc(), Utc.with_ymd_and_hms(2026, 3, 9, 12, 0, 0).unwrap());
        // Ida y vuelta.
        back.apply(&mut other, &edited);
        assert_eq!(other.start, at(9));
        assert_eq!(other.summary, "Semanal");
        // Días completos: se corren días enteros.
        let d = |n| EventTime::Date(chrono::NaiveDate::from_ymd_opt(2026, 3, n).unwrap());
        let day = SeriesPatch { shift_secs: 2 * 86_400, ..Default::default() };
        assert_eq!(day.times(&d(2), &d(3)), (d(4), d(5)));
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
