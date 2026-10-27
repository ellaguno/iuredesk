//! Expansión de recurrencias para las vistas de calendario.
//!
//! **API para la interfaz** (sustituye la expansión ingenua de `engine::calendar::occurrences`):
//!
//! ```ignore
//! let occ: Vec<EventOccurrence> =
//!     iuredesk_pim_sync::occurrences_between(&store, &calendars, from, to, user_tz)?;
//! ```
//!
//! - Usa el motor de `iuredesk-dav` ([`iuredesk_dav::recur::expand`]): la serie se expande en su
//!   zona (`TZID`), así «lunes a las 9:00» sigue a las 9:00 tras el cambio de horario; `EXDATE`
//!   (también de día completo sobre series con hora), `RDATE` y excepciones `RECURRENCE-ID` con
//!   sus propios datos (movidas, retituladas, canceladas).
//! - Lee el objeto crudo del servidor si lo hay (ahí viven las excepciones) y le aplica en memoria
//!   las ediciones locales aún no subidas; si no hay (eventos locales, muestra), lo arma desde el
//!   [`Event`].
//! - Topes: ventana ≤ 3 660 días (la de [`Store::events_between`]), ≤ 2 000 repeticiones por
//!   serie y ≤ [`MAX_OCCURRENCES`] en total. Una serie rota no tumba las demás: se muestra su
//!   primera repetición.
//! - Coste: un análisis del `.ics` por serie y consulta (las vistas piden semanas o meses; con
//!   cientos de series es del orden de milisegundos). Si hiciera falta, una caché por
//!   `(evento, ETag)` cabe aquí sin cambiar la firma.

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use iuredesk_dav::calendar::{EventView, ICalendar};
use iuredesk_dav::recur::{Limits, expand};
use iuredesk_model::{CalendarId, Event, EventOccurrence, EventStatus, EventTime, Transparency};
use iuredesk_store::{PimRef, Store};

use crate::error::Result;
use crate::map::ical::{events_from_ics, new_event_ics, patch_event};

/// Apariciones máximas devueltas por consulta.
pub const MAX_OCCURRENCES: usize = 20_000;

/// Apariciones de los eventos de `cals` (todos si está vacío) que se solapan con `[from, to)`,
/// ordenadas por inicio. `user_tz` ubica las horas flotantes y los días completos.
pub fn occurrences_between(
    store: &Store,
    cals: &[CalendarId],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    user_tz: Tz,
) -> Result<Vec<EventOccurrence>> {
    let mut out = Vec::new();
    for e in store.events_between(cals, from, to)? {
        if out.len() >= MAX_OCCURRENCES {
            break;
        }
        if !e.is_recurring() {
            out.push(single(&e));
            continue;
        }
        let cal = series_calendar(store, &e, user_tz);
        match expand(&cal, from, to, user_tz, Limits::default()) {
            Ok(exp) => {
                for inst in exp.instances.iter().filter(|i| i.uid == e.uid) {
                    let comp = EventView::new(inst.component);
                    let (start, end) = match (inst.start_date, inst.end_date) {
                        (Some(s), Some(en)) if inst.all_day => (EventTime::Date(s), EventTime::Date(en)),
                        _ => {
                            let tzid = match &e.start {
                                EventTime::DateTime { tzid, .. } => tzid.clone(),
                                EventTime::Date(_) => None,
                            };
                            (
                                EventTime::DateTime { utc: inst.start, tzid: tzid.clone() },
                                EventTime::DateTime { utc: inst.end, tzid },
                            )
                        }
                    };
                    let status = if inst.overridden {
                        match comp.status().as_deref() {
                            Some("CANCELLED") => EventStatus::Cancelled,
                            Some("TENTATIVE") => EventStatus::Tentative,
                            Some(_) => EventStatus::Confirmed,
                            None => e.status.clone(),
                        }
                    } else {
                        e.status.clone()
                    };
                    let transparency = match inst.component.text("TRANSP").map(|t| t.trim().to_ascii_uppercase()) {
                        Some(t) if inst.overridden => {
                            if t == "TRANSPARENT" {
                                Transparency::Transparent
                            } else {
                                Transparency::Opaque
                            }
                        }
                        _ => e.transparency.clone(),
                    };
                    out.push(EventOccurrence {
                        event: e.id,
                        calendar: e.calendar,
                        uid: e.uid.clone(),
                        start,
                        end,
                        recurrence_id: inst.recurrence_id,
                        recurring: true,
                        overridden: inst.overridden,
                        summary: if inst.overridden { comp.summary().unwrap_or_default() } else { e.summary.clone() },
                        location: if inst.overridden {
                            comp.location().unwrap_or_default()
                        } else {
                            e.location.clone()
                        },
                        status,
                        transparency,
                    });
                    if out.len() >= MAX_OCCURRENCES {
                        break;
                    }
                }
            }
            // Ventana fuera de tope o serie imposible: al menos el evento tal cual.
            Err(_) => out.push(single(&e)),
        }
    }
    out.sort_by(|a, b| a.start.to_utc().cmp(&b.start.to_utc()).then(a.event.cmp(&b.event)));
    Ok(out)
}

fn single(e: &Event) -> EventOccurrence {
    EventOccurrence {
        event: e.id,
        calendar: e.calendar,
        uid: e.uid.clone(),
        start: e.start.clone(),
        end: e.end.clone(),
        recurrence_id: None,
        recurring: false,
        overridden: false,
        summary: e.summary.clone(),
        location: e.location.clone(),
        status: e.status.clone(),
        transparency: e.transparency.clone(),
    }
}

/// El `VCALENDAR` de una serie: el crudo del servidor con las ediciones locales aplicadas, o uno
/// armado desde el evento.
fn series_calendar(store: &Store, e: &Event, user_tz: Tz) -> ICalendar {
    let epoch = DateTime::<Utc>::UNIX_EPOCH;
    if let Ok(Some((_, data))) = store.raw_object_data(PimRef::Event(e.id))
        && let Ok(mut cal) = ICalendar::parse_bytes(&data)
        && let Some(base) = events_from_ics(&cal, user_tz).into_iter().find(|b| b.uid == e.uid)
    {
        let mut local = e.clone();
        // Los campos que no salen del .ics no cuentan como edición.
        local.updated = base.updated;
        patch_event(&mut cal, &base, &local, user_tz, epoch);
        return cal;
    }
    new_event_ics(e, epoch)
}

#[cfg(test)]
mod tests {
    use chrono::{NaiveDate, TimeZone};
    use iuredesk_model::{AccountId, Calendar};

    use super::*;

    #[test]
    fn local_series_without_raw() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let cal = s.put_calendar(&Calendar { account: AccountId(1), ..Default::default() }).unwrap();
        let start = Utc.with_ymd_and_hms(2026, 3, 2, 9, 0, 0).unwrap();
        let id = s
            .put_event(&Event {
                calendar: cal,
                uid: "w".into(),
                summary: "Semanal".into(),
                start: EventTime::DateTime { utc: start, tzid: None },
                end: EventTime::DateTime { utc: start + chrono::Duration::hours(1), tzid: None },
                rrule: Some("FREQ=WEEKLY;COUNT=4".into()),
                exdates: vec!["20260309T090000Z".into()],
                ..Default::default()
            })
            .unwrap();
        let birthday = s
            .put_event(&Event {
                calendar: cal,
                uid: "b".into(),
                summary: "Cumple".into(),
                start: EventTime::Date(NaiveDate::from_ymd_opt(2020, 3, 5).unwrap()),
                end: EventTime::Date(NaiveDate::from_ymd_opt(2020, 3, 6).unwrap()),
                rrule: Some("FREQ=YEARLY".into()),
                ..Default::default()
            })
            .unwrap();
        let from = Utc.with_ymd_and_hms(2026, 3, 1, 0, 0, 0).unwrap();
        let to = Utc.with_ymd_and_hms(2026, 4, 1, 0, 0, 0).unwrap();
        let occ = occurrences_between(&s, &[], from, to, chrono_tz::UTC).unwrap();
        let weekly: Vec<_> = occ.iter().filter(|o| o.event == id).collect();
        assert_eq!(weekly.len(), 3, "COUNT=4 menos un EXDATE");
        assert!(weekly.iter().all(|o| o.recurring && !o.overridden));
        let b: Vec<_> = occ.iter().filter(|o| o.event == birthday).collect();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].start, EventTime::Date(NaiveDate::from_ymd_opt(2026, 3, 5).unwrap()));
        assert!(b[0].is_all_day());
    }
}
