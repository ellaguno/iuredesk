//! Expansión de recurrencias en una ventana de tiempo (RFC 5545 §3.8.5).
//!
//! - `RRULE` con el crate [`rrule`]; `RDATE` y `EXDATE` (también `EXDATE` de día completo sobre
//!   una serie con hora, que generan Outlook y Google) y excepciones `RECURRENCE-ID`.
//! - La serie se expande en su zona (`TZID`), así «cada lunes a las 9:00» sigue a las 9:00 tras el
//!   cambio de horario. Las horas flotantes y los días completos se ubican en la zona del usuario.
//! - `DTSTART` siempre es la primera repetición aunque no cumpla la regla (como Google y Apple).
//! - Topes: ventana máxima, repeticiones máximas por consulta, y la expansión de una serie rota o
//!   maliciosa no tumba las demás: se anota en [`Expansion::errors`] y se sigue.
//!
//! Limitaciones conocidas: `RANGE=THISANDFUTURE` se trata como una excepción de una sola
//! repetición; `EXRULE` (obsoleta) se ignora; los `RDATE` de tipo periodo usan la duración de la
//! serie.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use chrono::{DateTime, Duration, NaiveDate, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;

use crate::calendar::{EventView, ICalendar};
use crate::ical::Component;
use crate::value::{self, DateTimeValue};
use crate::{Error, Result};

/// Topes de una consulta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Repeticiones máximas devueltas (todas las series juntas).
    pub max_instances: usize,
    /// Repeticiones máximas que se generan por serie antes de filtrar.
    pub max_per_series: u16,
    /// Ancho máximo de la ventana.
    pub max_window: Duration,
    /// `INTERVAL` máximo aceptado en una `RRULE`.
    pub max_interval: u16,
}

impl Default for Limits {
    fn default() -> Self {
        Self { max_instances: 5_000, max_per_series: 2_000, max_window: Duration::days(3_700), max_interval: 1_000 }
    }
}

/// Una repetición concreta.
#[derive(Debug, Clone)]
pub struct Instance<'a> {
    /// `UID` de la serie.
    pub uid: String,
    /// Inicio (UTC). En días completos, la medianoche de la zona del usuario.
    pub start: DateTime<Utc>,
    /// Fin exclusivo (UTC).
    pub end: DateTime<Utc>,
    /// Día completo.
    pub all_day: bool,
    /// Primer día (solo días completos).
    pub start_date: Option<NaiveDate>,
    /// Día siguiente al último (exclusivo; solo días completos).
    pub end_date: Option<NaiveDate>,
    /// Identificador de la repetición (valor de `RECURRENCE-ID`, en UTC); `None` si no es serie.
    pub recurrence_id: Option<DateTime<Utc>>,
    /// La repetición viene de una excepción (`RECURRENCE-ID`) con datos propios.
    pub overridden: bool,
    /// Componente del que salen título, lugar, etc. (la serie o la excepción).
    pub component: &'a Component,
}

/// Resultado de [`expand`].
#[derive(Debug, Clone, Default)]
pub struct Expansion<'a> {
    /// Repeticiones en la ventana, ordenadas por inicio.
    pub instances: Vec<Instance<'a>>,
    /// Se alcanzó un tope: puede haber más repeticiones.
    pub truncated: bool,
    /// Series que no se pudieron expandir (UID y motivo); se muestran como evento único.
    pub errors: Vec<(String, String)>,
}

/// Expande `VEVENT` y `VTODO` de `cal` en `[window_start, window_end)`. `user_tz` ubica las horas
/// flotantes, los días completos y las zonas que no se reconocen.
pub fn expand<'a>(
    cal: &'a ICalendar,
    window_start: DateTime<Utc>,
    window_end: DateTime<Utc>,
    user_tz: Tz,
    limits: Limits,
) -> Result<Expansion<'a>> {
    if window_end <= window_start {
        return Err(Error::Recurrence("la ventana termina antes de empezar".into()));
    }
    if window_end.signed_duration_since(window_start) > limits.max_window {
        return Err(Error::Limit(format!("ventana de más de {} días", limits.max_window.num_days())));
    }
    let ctx = Ctx { cal, user_tz, window_start, window_end, limits };

    // Agrupa por UID: la serie (sin RECURRENCE-ID) y sus excepciones.
    let mut groups: BTreeMap<String, (Vec<&'a Component>, Vec<&'a Component>)> = BTreeMap::new();
    for (i, c) in cal.root.components.iter().enumerate() {
        if c.name != "VEVENT" && c.name != "VTODO" {
            continue;
        }
        let view = EventView::new(c);
        // Sin UID: cada componente es su propia serie.
        let uid = view.uid().unwrap_or_else(|| format!("\u{0}sin-uid-{i}"));
        let entry = groups.entry(uid).or_default();
        if c.prop("RECURRENCE-ID").is_some() {
            entry.1.push(c);
        } else {
            entry.0.push(c);
        }
    }

    let mut out = Expansion::default();
    for (uid, (masters, overrides)) in groups {
        let public_uid = if uid.starts_with('\u{0}') { String::new() } else { uid.clone() };
        match masters.first() {
            Some(master) => {
                if let Err(e) = ctx.expand_series(&public_uid, master, &overrides, &mut out) {
                    out.errors.push((public_uid.clone(), e.to_string()));
                    // Se muestra al menos la primera repetición.
                    ctx.push_single(&public_uid, master, None, false, &mut out);
                }
            }
            // Excepciones sin serie (invitación a una sola repetición): cada una por su cuenta.
            None => {
                for o in overrides {
                    let rid = EventView::new(o).recurrence_id().map(|r| cal.to_utc(&r, user_tz));
                    ctx.push_single(&public_uid, o, rid, true, &mut out);
                }
            }
        }
    }
    out.instances.sort_by(|a, b| a.start.cmp(&b.start).then_with(|| a.uid.cmp(&b.uid)));
    if out.instances.len() > limits.max_instances {
        out.instances.truncate(limits.max_instances);
        out.truncated = true;
    }
    Ok(out)
}

struct Ctx<'a> {
    cal: &'a ICalendar,
    user_tz: Tz,
    window_start: DateTime<Utc>,
    window_end: DateTime<Utc>,
    limits: Limits,
}

/// Duración de una repetición: días nominales (días completos) o segundos exactos.
#[derive(Debug, Clone, Copy)]
enum Span {
    Days(i64),
    Exact(Duration),
}

/// Clave de una repetición para casar `EXDATE` y `RECURRENCE-ID`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Occ {
    utc: DateTime<Utc>,
    /// Fecha local en la zona de la serie.
    date: NaiveDate,
}

impl<'a> Ctx<'a> {
    fn span(&self, ev: &EventView<'_>, start: &DateTimeValue) -> Span {
        if let Some(end) = ev.dtend() {
            if let (DateTimeValue::Date(s), DateTimeValue::Date(e)) = (start, &end) {
                return Span::Days(e.signed_duration_since(*s).num_days().max(0));
            }
            let d = self.cal.to_utc(&end, self.user_tz).signed_duration_since(self.cal.to_utc(start, self.user_tz));
            return Span::Exact(d.max(Duration::zero()));
        }
        match (ev.duration(), start.is_date()) {
            (Some(d), true) if d.num_seconds() % 86_400 == 0 => Span::Days((d.num_days()).max(0)),
            (Some(d), _) => Span::Exact(d.max(Duration::zero())),
            (None, true) => Span::Days(1),
            (None, false) => Span::Exact(Duration::zero()),
        }
    }

    /// Fecha → medianoche en la zona del usuario.
    fn midnight(&self, d: NaiveDate) -> DateTime<Utc> {
        crate::tz::local_to_utc(d.and_time(NaiveTime::MIN), self.user_tz)
    }

    /// Agrega una repetición si toca la ventana.
    fn push(
        &self,
        uid: &str,
        comp: &'a Component,
        start: &DateTimeValue,
        span: Span,
        rid: Option<DateTime<Utc>>,
        overridden: bool,
        out: &mut Expansion<'a>,
    ) {
        let (s, e, sd, ed) = match (start, span) {
            (DateTimeValue::Date(d), Span::Days(n)) => {
                let end_date = d.checked_add_signed(Duration::days(n)).unwrap_or(*d);
                (self.midnight(*d), self.midnight(end_date), Some(*d), Some(end_date))
            }
            (_, Span::Days(n)) => {
                let s = self.cal.to_utc(start, self.user_tz);
                (s, s.checked_add_signed(Duration::days(n)).unwrap_or(s), None, None)
            }
            (_, Span::Exact(d)) => {
                let s = self.cal.to_utc(start, self.user_tz);
                (s, s.checked_add_signed(d).unwrap_or(s), None, None)
            }
        };
        let overlaps = if s == e {
            s >= self.window_start && s < self.window_end
        } else {
            s < self.window_end && e > self.window_start
        };
        if overlaps {
            out.instances.push(Instance {
                uid: uid.to_string(),
                start: s,
                end: e,
                all_day: start.is_date(),
                start_date: sd,
                end_date: ed,
                recurrence_id: rid,
                overridden,
                component: comp,
            });
        }
    }

    fn push_single(
        &self,
        uid: &str,
        comp: &'a Component,
        rid: Option<DateTime<Utc>>,
        overridden: bool,
        out: &mut Expansion<'a>,
    ) {
        let ev = EventView::new(comp);
        let Some(start) = ev.dtstart().or_else(|| ev.dtend()) else { return };
        let span = self.span(&ev, &start);
        self.push(uid, comp, &start, span, rid, overridden, out);
    }

    fn expand_series(
        &self,
        uid: &str,
        master: &'a Component,
        overrides: &[&'a Component],
        out: &mut Expansion<'a>,
    ) -> Result<()> {
        let ev = EventView::new(master);
        if !ev.is_recurring() {
            self.push_single(uid, master, None, false, out);
            // Excepciones de algo que no es serie: se muestran tal cual.
            for o in overrides {
                self.push_single(uid, o, None, true, out);
            }
            return Ok(());
        }
        let Some(start) = ev.dtstart() else { return Err(Error::Missing("DTSTART")) };
        let span = self.span(&ev, &start);

        // Zona en la que se expande la serie.
        let (series_tz, all_day) = match &start {
            DateTimeValue::Date(_) => (chrono_tz::UTC, true),
            DateTimeValue::Floating(_) => (self.user_tz, false),
            DateTimeValue::Utc(_) => (chrono_tz::UTC, false),
            DateTimeValue::Zoned { tzid, .. } => (self.cal.resolve_tz(tzid).unwrap_or(self.user_tz), false),
        };
        let rtz = rrule::Tz::Tz(series_tz);
        let start_utc = crate::tz::local_to_utc(start.naive(), series_tz);
        let dt_start = start_utc.with_timezone(&rtz);
        let occ_of = |t: DateTime<rrule::Tz>| Occ { utc: t.with_timezone(&Utc), date: t.date_naive() };

        // Margen: lo que dura una repetición + un día, para no perder las que empiezan antes.
        let margin = match span {
            Span::Days(n) => Duration::days(n.saturating_add(1)),
            Span::Exact(d) => d.checked_add(&Duration::days(1)).unwrap_or(Duration::days(1)),
        };
        let after_utc = self.window_start.checked_sub_signed(margin).unwrap_or(self.window_start);
        let before_utc = self.window_end.checked_add_signed(Duration::days(1)).unwrap_or(self.window_end);

        let mut occs: BTreeSet<Occ> = BTreeSet::new();
        if start_utc >= after_utc && start_utc < before_utc {
            occs.insert(occ_of(dt_start));
        }
        let mut limited = false;
        for raw in ev.rrules() {
            let rule = self.parse_rule(&raw, &start, series_tz, dt_start, before_utc)?;
            let Some(rule) = rule else { continue };
            let set = rule
                .build(dt_start)
                .map_err(|e| Error::Recurrence(e.to_string()))?
                .after(after_utc.with_timezone(&rtz))
                .before(before_utc.with_timezone(&rtz));
            let max = self.limits.max_per_series;
            // Defensa en profundidad: un fallo interno del crate no debe tumbar la aplicación.
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| set.all(max)))
                .map_err(|_| Error::Recurrence("la regla no se pudo expandir".into()))?;
            limited |= result.limited && result.dates.len() >= usize::from(max);
            occs.extend(result.dates.into_iter().map(occ_of));
        }
        for rdate in ev.rdates()? {
            let t = self.value_in(&rdate, series_tz, all_day);
            if t.utc >= after_utc && t.utc < before_utc {
                occs.insert(t);
            }
        }
        // EXDATE: por instante exacto o, si es de día completo, por fecha local.
        let exdates = ev.exdates()?;
        let (ex_dates, ex_instants): (Vec<_>, Vec<_>) = exdates.iter().partition(|v| v.is_date());
        let ex_dates: BTreeSet<NaiveDate> = ex_dates.iter().map(|v| v.naive().date()).collect();
        let ex_instants: BTreeSet<DateTime<Utc>> =
            ex_instants.iter().map(|v| self.value_in(v, series_tz, all_day).utc).collect();
        occs.retain(|o| !ex_dates.contains(&o.date) && !ex_instants.contains(&o.utc));

        // Excepciones por RECURRENCE-ID.
        let mut by_rid: Vec<(Occ, bool, &'a Component)> = overrides
            .iter()
            .filter_map(|o| {
                let rid = EventView::new(o).recurrence_id()?;
                Some((self.value_in(&rid, series_tz, all_day), rid.is_date(), *o))
            })
            .collect();

        for occ in &occs {
            let found = by_rid.iter().position(
                |(k, is_date, _)| {
                    if *is_date || all_day { k.date == occ.date } else { k.utc == occ.utc }
                },
            );
            let rid = if all_day { self.midnight(occ.date) } else { occ.utc };
            match found {
                Some(pos) => {
                    let (_, _, comp) = by_rid.swap_remove(pos);
                    self.push_single(uid, comp, Some(rid), true, out);
                }
                None => {
                    let value = if all_day { DateTimeValue::Date(occ.date) } else { DateTimeValue::Utc(occ.utc) };
                    self.push(uid, master, &value, span, Some(rid), false, out);
                }
            }
        }
        // Excepciones cuya repetición original cae fuera de la ventana pero que se movieron
        // dentro (y que no se excluyeron con EXDATE).
        for (k, _, comp) in by_rid {
            if ex_instants.contains(&k.utc) || (all_day && ex_dates.contains(&k.date)) {
                continue;
            }
            if k.utc >= after_utc && k.utc < before_utc {
                continue; // su repetición estaba en el rango pero ya no la genera la regla
            }
            let rid = if all_day { self.midnight(k.date) } else { k.utc };
            self.push_single(uid, comp, Some(rid), true, out);
        }
        if limited {
            out.truncated = true;
        }
        Ok(())
    }

    /// Un valor de fecha como clave en la zona de la serie.
    fn value_in(&self, v: &DateTimeValue, series_tz: Tz, all_day: bool) -> Occ {
        match v {
            DateTimeValue::Date(d) if all_day => {
                Occ { utc: Utc.from_utc_datetime(&d.and_time(NaiveTime::MIN)), date: *d }
            }
            DateTimeValue::Date(d) => {
                let utc = crate::tz::local_to_utc(d.and_time(NaiveTime::MIN), series_tz);
                Occ { utc, date: *d }
            }
            other => {
                let utc = match other {
                    DateTimeValue::Floating(n) => crate::tz::local_to_utc(*n, series_tz),
                    _ => self.cal.to_utc(other, self.user_tz),
                };
                let date = if all_day { utc.date_naive() } else { utc.with_timezone(&series_tz).date_naive() };
                Occ { utc, date }
            }
        }
    }

    /// Prepara una `RRULE`: normaliza `UNTIL` (UTC, como pide el crate; un `UNTIL` de solo fecha
    /// sobre una serie con hora cubre todo ese día) y, sin `COUNT` ni `UNTIL`, la acota al final
    /// de la ventana. `Ok(None)` si la regla termina antes de empezar.
    fn parse_rule(
        &self,
        raw: &str,
        start: &DateTimeValue,
        series_tz: Tz,
        dt_start: DateTime<rrule::Tz>,
        before_utc: DateTime<Utc>,
    ) -> Result<Option<rrule::RRule<rrule::Unvalidated>>> {
        if raw.len() > 1_024 || !raw.is_ascii() {
            return Err(Error::Recurrence("regla demasiado larga o con caracteres no válidos".into()));
        }
        let mut until: Option<DateTime<Utc>> = None;
        let mut has_count = false;
        let mut parts = Vec::new();
        for part in raw.split(';').map(str::trim).filter(|p| !p.is_empty()) {
            let Some((k, v)) = part.split_once('=') else {
                return Err(Error::Recurrence(format!("parte no válida «{part}»")));
            };
            let key = k.trim().to_ascii_uppercase();
            match key.as_str() {
                "UNTIL" => {
                    let u = value::parse_date_time(v, None, None)?;
                    until = Some(match (&u, start) {
                        (DateTimeValue::Date(d), DateTimeValue::Date(_)) => {
                            Utc.from_utc_datetime(&d.and_time(NaiveTime::MIN))
                        }
                        (DateTimeValue::Date(d), _) => {
                            let end_of_day = d.and_time(NaiveTime::from_hms_opt(23, 59, 59).unwrap_or(NaiveTime::MIN));
                            crate::tz::local_to_utc(end_of_day, series_tz)
                        }
                        (DateTimeValue::Utc(t), _) => *t,
                        (other, _) => crate::tz::local_to_utc(other.naive(), series_tz),
                    });
                }
                "INTERVAL" => {
                    let n: u32 = v.trim().parse().map_err(|_| Error::Recurrence(format!("INTERVAL «{v}»")))?;
                    if n == 0 || n > u32::from(self.limits.max_interval) {
                        return Err(Error::Limit(format!("INTERVAL={n}")));
                    }
                    parts.push(format!("INTERVAL={n}"));
                }
                "COUNT" => {
                    has_count = true;
                    parts.push(format!("COUNT={}", v.trim()));
                }
                // Partes con valores sin «:» (la biblioteca usa «:» como separador de propiedad).
                _ => parts.push(format!("{key}={}", v.trim().replace(':', ""))),
            }
        }
        if has_count && until.is_some() {
            return Err(Error::Recurrence("COUNT y UNTIL a la vez".into()));
        }
        let mut rule = rrule::RRule::from_str(&parts.join(";")).map_err(|e| Error::Recurrence(e.to_string()))?;
        let start_utc = dt_start.with_timezone(&Utc);
        // Con COUNT la regla ya está acotada; sin COUNT se corta al final de la ventana.
        let effective_until = match until {
            Some(u) => Some(u.min(before_utc)),
            None if has_count => None,
            None => Some(before_utc),
        };
        if let Some(u) = effective_until {
            if u < start_utc {
                return Ok(None); // termina antes de empezar: solo queda DTSTART
            }
            rule = rule.until(u.with_timezone(&rrule::Tz::UTC));
        }
        Ok(Some(rule))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, Timelike, Weekday};

    fn cal(body: &str) -> ICalendar {
        ICalendar::parse(&format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n{body}END:VCALENDAR\r\n")).unwrap()
    }

    fn utc(y: i32, m: u32, d: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, 0, 0, 0).unwrap()
    }

    fn run(c: &ICalendar, from: DateTime<Utc>, to: DateTime<Utc>) -> Expansion<'_> {
        expand(c, from, to, chrono_tz::Europe::Madrid, Limits::default()).unwrap()
    }

    #[test]
    fn weekly_across_dst_keeps_wall_clock() {
        let c = cal("BEGIN:VEVENT\r\nUID:w\r\nDTSTART;TZID=America/New_York:20260302T090000\r\n\
DTEND;TZID=America/New_York:20260302T100000\r\nRRULE:FREQ=WEEKLY;COUNT=4\r\nEND:VEVENT\r\n");
        let e = run(&c, utc(2026, 1, 1), utc(2026, 12, 31));
        let hours: Vec<u32> = e.instances.iter().map(|i| i.start.hour()).collect();
        // 2 de marzo EST (UTC-5) → 14:00 UTC; desde el 8 de marzo EDT (UTC-4) → 13:00 UTC.
        assert_eq!(hours, vec![14, 13, 13, 13]);
        assert!(e.instances.iter().all(|i| i.end - i.start == Duration::hours(1)));
        assert!(!e.truncated);
    }

    #[test]
    fn europe_dst_fall_back() {
        let c = cal("BEGIN:VEVENT\r\nUID:d\r\nDTSTART;TZID=Europe/Madrid:20261020T100000\r\n\
DURATION:PT30M\r\nRRULE:FREQ=DAILY;UNTIL=20261030T080000Z\r\nEND:VEVENT\r\n");
        let e = run(&c, utc(2026, 10, 1), utc(2026, 11, 30));
        assert_eq!(e.instances.len(), 10); // del 20 al 29
        // 24 oct CEST (UTC+2) → 08:00 UTC; 26 oct CET (UTC+1) → 09:00 UTC.
        let by_day: BTreeMap<u32, u32> = e.instances.iter().map(|i| (i.start.day(), i.start.hour())).collect();
        assert_eq!(by_day[&24], 8);
        assert_eq!(by_day[&26], 9);
        // UNTIL 30 oct 08:00Z: la del 30 (09:00 UTC) ya no entra.
        assert!(!by_day.contains_key(&30));
    }

    #[test]
    fn monthly_last_friday() {
        let c =
            cal("BEGIN:VEVENT\r\nUID:m\r\nDTSTART:20260130T170000Z\r\nRRULE:FREQ=MONTHLY;BYDAY=-1FR\r\nEND:VEVENT\r\n");
        let e = run(&c, utc(2026, 1, 1), utc(2027, 1, 1));
        assert_eq!(e.instances.len(), 12);
        for i in &e.instances {
            assert_eq!(i.start.weekday(), Weekday::Fri);
            let next_week = i.start + Duration::days(7);
            assert_ne!(next_week.month(), i.start.month(), "{} no es el último viernes", i.start);
        }
        assert_eq!(e.instances[0].start.day(), 30);
        assert_eq!(e.instances[1].start.day(), 27); // febrero 2026
    }

    #[test]
    fn yearly_all_day_birthday() {
        let c = cal("BEGIN:VEVENT\r\nUID:b\r\nDTSTART;VALUE=DATE:19900229\r\nDTEND;VALUE=DATE:19900301\r\n\
RRULE:FREQ=YEARLY\r\nEND:VEVENT\r\n");
        // 1990-02-29 no existe; usamos una fecha válida.
        assert!(c.events().next().unwrap().dtstart().is_none());
        let c = cal("BEGIN:VEVENT\r\nUID:b\r\nDTSTART;VALUE=DATE:19900315\r\nRRULE:FREQ=YEARLY\r\nEND:VEVENT\r\n");
        let e = run(&c, utc(2026, 1, 1), utc(2029, 1, 1));
        assert_eq!(e.instances.len(), 3);
        let i = &e.instances[0];
        assert!(i.all_day);
        assert_eq!(i.start_date, NaiveDate::from_ymd_opt(2026, 3, 15));
        assert_eq!(i.end_date, NaiveDate::from_ymd_opt(2026, 3, 16));
        // Medianoche en Madrid (UTC+1 en marzo) = 23:00 UTC del día anterior.
        assert_eq!(i.start, Utc.with_ymd_and_hms(2026, 3, 14, 23, 0, 0).unwrap());
    }

    #[test]
    fn count_and_until() {
        let c = cal("BEGIN:VEVENT\r\nUID:c\r\nDTSTART:20260105T080000Z\r\nRRULE:FREQ=DAILY;COUNT=3\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:u\r\nDTSTART;VALUE=DATE:20260105\r\nRRULE:FREQ=DAILY;UNTIL=20260107\r\nEND:VEVENT\r\n");
        let e = run(&c, utc(2026, 1, 1), utc(2026, 2, 1));
        assert_eq!(e.instances.iter().filter(|i| i.uid == "c").count(), 3);
        assert_eq!(e.instances.iter().filter(|i| i.uid == "u").count(), 3);
    }

    #[test]
    fn date_until_on_timed_series_covers_that_day() {
        let c = cal("BEGIN:VEVENT\r\nUID:x\r\nDTSTART;TZID=Europe/Madrid:20260105T180000\r\n\
RRULE:FREQ=DAILY;UNTIL=20260107\r\nEND:VEVENT\r\n");
        assert_eq!(run(&c, utc(2026, 1, 1), utc(2026, 2, 1)).instances.len(), 3);
    }

    #[test]
    fn exdate_and_overrides() {
        let c = cal("BEGIN:VEVENT\r\nUID:s\r\nSUMMARY:Serie\r\nDTSTART;TZID=Europe/Madrid:20260105T100000\r\n\
DTEND;TZID=Europe/Madrid:20260105T110000\r\nRRULE:FREQ=DAILY;COUNT=5\r\n\
EXDATE;TZID=Europe/Madrid:20260106T100000\r\nEXDATE;VALUE=DATE:20260107\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:s\r\nSUMMARY:Movida\r\nRECURRENCE-ID;TZID=Europe/Madrid:20260108T100000\r\n\
DTSTART;TZID=Europe/Madrid:20260108T150000\r\nDTEND;TZID=Europe/Madrid:20260108T160000\r\nEND:VEVENT\r\n");
        let e = run(&c, utc(2026, 1, 1), utc(2026, 2, 1));
        let days: Vec<u32> = e.instances.iter().map(|i| i.start.day()).collect();
        assert_eq!(days, vec![5, 8, 9]);
        let moved = &e.instances[1];
        assert!(moved.overridden);
        assert_eq!(moved.start.hour(), 14); // 15:00 CET
        assert_eq!(moved.recurrence_id.unwrap().hour(), 9);
        assert_eq!(EventView::new(moved.component).summary().as_deref(), Some("Movida"));
    }

    #[test]
    fn override_moved_into_window() {
        let c =
            cal("BEGIN:VEVENT\r\nUID:s\r\nDTSTART:20260105T100000Z\r\nRRULE:FREQ=WEEKLY;COUNT=10\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:s\r\nRECURRENCE-ID:20260112T100000Z\r\nDTSTART:20260301T100000Z\r\nEND:VEVENT\r\n");
        let e = run(&c, utc(2026, 2, 28), utc(2026, 3, 2));
        assert_eq!(e.instances.len(), 1);
        assert!(e.instances[0].overridden);
    }

    #[test]
    fn rdate_and_floating() {
        let c = cal("BEGIN:VEVENT\r\nUID:r\r\nDTSTART:20260105T100000\r\nRDATE:20260110T100000,20260120T100000\r\n\
END:VEVENT\r\n");
        let e = run(&c, utc(2026, 1, 1), utc(2026, 2, 1));
        assert_eq!(e.instances.len(), 3);
        assert!(e.instances.iter().all(|i| i.start.hour() == 9)); // flotante en Madrid (UTC+1)
    }

    #[test]
    fn caps_and_bad_rules() {
        let c = cal("BEGIN:VEVENT\r\nUID:m\r\nDTSTART:20260101T000000Z\r\nRRULE:FREQ=MINUTELY\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:bad\r\nDTSTART:20260101T000000Z\r\nRRULE:FREQ=NOPE\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:huge\r\nDTSTART:20260101T000000Z\r\nRRULE:FREQ=YEARLY;INTERVAL=65000;COUNT=5;BYMONTH=2;BYMONTHDAY=30\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:never\r\nDTSTART:20260101T000000Z\r\nRRULE:FREQ=YEARLY;COUNT=5;BYMONTH=2;BYMONTHDAY=30\r\nEND:VEVENT\r\n");
        let e = expand(&c, utc(2026, 1, 1), utc(2026, 1, 31), chrono_tz::UTC, Limits::default()).unwrap();
        assert!(e.truncated);
        assert!(e.instances.len() <= Limits::default().max_instances);
        assert!(e.errors.iter().any(|(u, _)| u == "bad"));
        assert!(e.errors.iter().any(|(u, _)| u == "huge"));
        assert!(expand(&c, utc(2000, 1, 1), utc(2026, 1, 1), chrono_tz::UTC, Limits::default()).is_err());
    }

    #[test]
    fn non_recurring_and_all_day_overlap() {
        let c = cal("BEGIN:VEVENT\r\nUID:a\r\nDTSTART;VALUE=DATE:20260110\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:z\r\nDTSTART:20260110T120000Z\r\nEND:VEVENT\r\n");
        let e = expand(&c, utc(2026, 1, 10), utc(2026, 1, 11), chrono_tz::UTC, Limits::default()).unwrap();
        assert_eq!(e.instances.len(), 2);
        assert_eq!(e.instances[0].recurrence_id, None);
        let e = expand(&c, utc(2026, 1, 11), utc(2026, 1, 12), chrono_tz::UTC, Limits::default()).unwrap();
        assert!(e.instances.is_empty());
    }
}
