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
    // El almacén indexa los días completos a medianoche UTC; en la zona del usuario el día
    // empieza antes o después (BUG-GCAL: en Ciudad de México el cumpleaños de mañana caía hoy a
    // las 18:00). Se consulta con un día de margen y se filtra con el día local de verdad.
    let day = chrono::Duration::days(1);
    let (q_from, q_to) = (from.checked_sub_signed(day).unwrap_or(from), to.checked_add_signed(day).unwrap_or(to));
    // En el tope de la ventana el margen no cabe: se consulta tal cual.
    let events = match store.events_between(cals, q_from, q_to) {
        Ok(v) => v,
        Err(_) => store.events_between(cals, from, to)?,
    };
    for e in events {
        if out.len() >= MAX_OCCURRENCES {
            break;
        }
        if !e.is_recurring() {
            let s = local_instant(&e.start, user_tz);
            let en = local_instant(&e.end, user_tz).max(s);
            let overlaps = if s == en { s >= from && s < to } else { s < to && en > from };
            if overlaps {
                out.push(single(&e));
            }
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

/// Instante de un [`EventTime`] en la zona del usuario: los días completos empiezan a la
/// medianoche local, no a la de UTC.
fn local_instant(t: &EventTime, user_tz: Tz) -> DateTime<Utc> {
    match t {
        EventTime::Date(d) => iuredesk_dav::tz::local_to_utc(d.and_time(chrono::NaiveTime::MIN), user_tz),
        other => other.to_utc(),
    }
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

    /// BUG-GCAL: días completos de Google (`DTSTART;VALUE=DATE`) en America/Mexico_City. El de
    /// mañana no puede aparecer hoy (a las 18:00, la medianoche UTC) y el de hoy sí, de día
    /// completo; igual con la serie anual de cumpleaños sin `DTEND`.
    #[test]
    fn all_day_events_follow_the_local_day() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let cal = s.put_calendar(&Calendar { account: AccountId(1), ..Default::default() }).unwrap();
        let tz: Tz = "America/Mexico_City".parse().unwrap();
        let d = |m, day| NaiveDate::from_ymd_opt(2026, m, day).unwrap();
        let ics = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Google Inc//Google Calendar 70.9054//EN\r\n\
BEGIN:VEVENT\r\nDTSTART;VALUE=DATE:20261028\r\nDTEND;VALUE=DATE:20261029\r\nUID:manana@google.com\r\n\
SUMMARY:Mañana\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nDTSTART;VALUE=DATE:20261027\r\nDTEND;VALUE=DATE:20261028\r\n\
UID:hoy@google.com\r\nSUMMARY:Hoy\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nDTSTART;VALUE=DATE:20001027\r\n\
RRULE:FREQ=YEARLY\r\nUID:cumple@google.com\r\nSUMMARY:Cumpleaños\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let parsed = ICalendar::parse(ics).unwrap();
        for mut e in events_from_ics(&parsed, tz) {
            assert!(e.start.is_all_day(), "{} debe ser de día completo", e.summary);
            e.calendar = cal;
            s.put_event(&e).unwrap();
        }
        // «Mi día» del martes 27 de octubre en Ciudad de México (UTC-6).
        let from = iuredesk_dav::tz::local_to_utc(d(10, 27).and_time(chrono::NaiveTime::MIN), tz);
        let to = iuredesk_dav::tz::local_to_utc(d(10, 28).and_time(chrono::NaiveTime::MIN), tz);
        let occ = occurrences_between(&s, &[], from, to, tz).unwrap();
        let names: Vec<&str> = occ.iter().map(|o| o.summary.as_str()).collect();
        assert!(!names.contains(&"Mañana"), "el de mañana no cae hoy: {names:?}");
        let hoy = occ.iter().find(|o| o.summary == "Hoy").expect("el de hoy");
        assert_eq!((hoy.start.clone(), hoy.end.clone()), (EventTime::Date(d(10, 27)), EventTime::Date(d(10, 28))));
        let cumple = occ.iter().find(|o| o.summary == "Cumpleaños").expect("el cumpleaños");
        assert_eq!(cumple.start, EventTime::Date(d(10, 27)));
        assert!(cumple.is_all_day());
        assert_eq!(occ.len(), 2, "{names:?}");
    }
}
