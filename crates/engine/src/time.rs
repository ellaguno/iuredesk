//! Fechas para mostrar: zona de la vista y textos relativos («10:42», «Ayer», «lun 19», «03/08/25»).

use chrono::{DateTime, Datelike, Duration, FixedOffset, Local, NaiveDate, NaiveDateTime, Offset, TimeZone, Utc};

use crate::lang::pick;

/// Zona en la que se muestran las horas.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Zone {
    /// La del sistema.
    #[default]
    Local,
    /// Desfase fijo (capturas deterministas, buzón de ejemplo).
    Fixed(FixedOffset),
}

impl Zone {
    /// UTC.
    pub fn utc() -> Zone {
        Zone::Fixed(Utc.fix())
    }

    /// Fecha y hora locales de un instante.
    pub fn local(&self, t: DateTime<Utc>) -> NaiveDateTime {
        match self {
            Zone::Local => t.with_timezone(&Local).naive_local(),
            Zone::Fixed(o) => t.with_timezone(o).naive_local(),
        }
    }

    /// Instante UTC de una fecha y hora locales (la primera si es ambigua; si no existe por el
    /// cambio de horario, se interpreta como UTC).
    pub fn to_utc(&self, t: NaiveDateTime) -> DateTime<Utc> {
        match self {
            Zone::Local => {
                Local.from_local_datetime(&t).earliest().map_or_else(|| t.and_utc(), |d| d.with_timezone(&Utc))
            }
            Zone::Fixed(o) => {
                o.from_local_datetime(&t).earliest().map_or_else(|| t.and_utc(), |d| d.with_timezone(&Utc))
            }
        }
    }

    /// Medianoche local del día `d`, en UTC.
    pub fn day_start(&self, d: NaiveDate) -> DateTime<Utc> {
        self.to_utc(d.and_hms_opt(0, 0, 0).unwrap_or_default())
    }
}

/// Nombre corto del día de la semana.
pub fn weekday_short(d: NaiveDate) -> &'static str {
    let i = d.weekday().num_days_from_monday() as usize;
    let en = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    let es = ["lun", "mar", "mié", "jue", "vie", "sáb", "dom"];
    pick(en.get(i).copied().unwrap_or(""), es.get(i).copied().unwrap_or(""))
}

/// Nombre del mes.
pub fn month_name(m: u32) -> &'static str {
    let i = m.saturating_sub(1) as usize;
    let en = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    let es = [
        "enero",
        "febrero",
        "marzo",
        "abril",
        "mayo",
        "junio",
        "julio",
        "agosto",
        "septiembre",
        "octubre",
        "noviembre",
        "diciembre",
    ];
    pick(en.get(i).copied().unwrap_or(""), es.get(i).copied().unwrap_or(""))
}

/// Fecha de la lista de mensajes: hora si es de hoy, «Ayer», día de la semana en los últimos 6
/// días, día y mes este año, y fecha corta en años anteriores.
pub fn list_date(t: DateTime<Utc>, now: DateTime<Utc>, zone: Zone) -> String {
    let lt = zone.local(t);
    let ln = zone.local(now);
    let (d, today) = (lt.date(), ln.date());
    if d == today {
        return lt.format("%H:%M").to_string();
    }
    if Some(d) == today.pred_opt() {
        return pick("Yesterday", "Ayer").to_string();
    }
    if d < today && today.signed_duration_since(d) < Duration::days(7) {
        return format!("{} {}", weekday_short(d), d.day());
    }
    if d.year() == today.year() {
        let mon = month_name(d.month());
        let short: String = mon.chars().take(3).collect();
        return if crate::lang::spanish() { format!("{} {short}", d.day()) } else { format!("{short} {}", d.day()) };
    }
    if crate::lang::spanish() { lt.format("%d/%m/%y").to_string() } else { lt.format("%m/%d/%y").to_string() }
}

/// Fecha larga para la cabecera del mensaje: «lun 19 oct 2026, 10:42».
pub fn long_date(t: DateTime<Utc>, zone: Zone) -> String {
    let lt = zone.local(t);
    let d = lt.date();
    let mon: String = month_name(d.month()).chars().take(3).collect();
    if crate::lang::spanish() {
        format!("{} {} {mon} {}, {}", weekday_short(d), d.day(), d.year(), lt.format("%H:%M"))
    } else {
        format!("{} {mon} {}, {}, {}", weekday_short(d), d.day(), d.year(), lt.format("%H:%M"))
    }
}

/// Lunes de la semana de `d`.
pub fn monday_of(d: NaiveDate) -> NaiveDate {
    d - Duration::days(i64::from(d.weekday().num_days_from_monday()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_dates() {
        crate::lang::set_lang(crate::lang::Lang::Es);
        let z = Zone::utc();
        let now = Utc.with_ymd_and_hms(2026, 10, 21, 15, 0, 0).unwrap();
        assert_eq!(list_date(Utc.with_ymd_and_hms(2026, 10, 21, 9, 5, 0).unwrap(), now, z), "09:05");
        assert_eq!(list_date(Utc.with_ymd_and_hms(2026, 10, 20, 9, 5, 0).unwrap(), now, z), "Ayer");
        assert_eq!(list_date(Utc.with_ymd_and_hms(2026, 10, 17, 9, 5, 0).unwrap(), now, z), "sáb 17");
        assert_eq!(list_date(Utc.with_ymd_and_hms(2026, 3, 2, 9, 5, 0).unwrap(), now, z), "2 mar");
        assert_eq!(list_date(Utc.with_ymd_and_hms(2025, 3, 2, 9, 5, 0).unwrap(), now, z), "02/03/25");
        assert_eq!(
            monday_of(NaiveDate::from_ymd_opt(2026, 10, 21).unwrap()),
            NaiveDate::from_ymd_opt(2026, 10, 19).unwrap()
        );
    }
}
