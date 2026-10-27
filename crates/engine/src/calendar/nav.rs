//! Navegación del calendario: vistas (día, semana laboral, semana, mes, agenda), periodo visible,
//! avanzar/retroceder y el título del periodo. Sin egui: lo usan la UI y los comandos `cal.*`.

use chrono::{Datelike, Duration, Months, NaiveDate};
use serde::{Deserialize, Serialize};

use crate::lang::{pick, spanish};
use crate::time::{monday_of, month_name};

/// Días que muestra la agenda.
pub const AGENDA_DAYS: i64 = 30;

/// Vista del calendario.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CalView {
    Day,
    #[default]
    WorkWeek,
    Week,
    Month,
    /// Lista de lo que viene, agrupada por día.
    Agenda,
}

impl CalView {
    pub const ALL: [CalView; 5] = [CalView::Day, CalView::WorkWeek, CalView::Week, CalView::Month, CalView::Agenda];

    /// Id estable (comandos `cal.view.<key>`).
    pub fn key(self) -> &'static str {
        match self {
            CalView::Day => "day",
            CalView::WorkWeek => "work_week",
            CalView::Week => "week",
            CalView::Month => "month",
            CalView::Agenda => "agenda",
        }
    }

    pub fn from_key(s: &str) -> Option<CalView> {
        CalView::ALL.into_iter().find(|v| v.key() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            CalView::Day => pick("Day", "Día"),
            CalView::WorkWeek => pick("Work week", "Semana laboral"),
            CalView::Week => pick("Week", "Semana"),
            CalView::Month => pick("Month", "Mes"),
            CalView::Agenda => pick("Agenda", "Agenda"),
        }
    }

    /// ¿Es una rejilla de horas (día o semanas)?
    pub fn is_grid(self) -> bool {
        matches!(self, CalView::Day | CalView::WorkWeek | CalView::Week)
    }
}

/// Primer día y número de días visibles de una vista alrededor de `anchor`.
pub fn visible_range(view: CalView, anchor: NaiveDate) -> (NaiveDate, i64) {
    match view {
        CalView::Day => (anchor, 1),
        CalView::WorkWeek => (monday_of(anchor), 5),
        CalView::Week => (monday_of(anchor), 7),
        CalView::Month => {
            let first = NaiveDate::from_ymd_opt(anchor.year(), anchor.month(), 1).unwrap_or(anchor);
            (monday_of(first), 42)
        }
        CalView::Agenda => (anchor, AGENDA_DAYS),
    }
}

/// El día de referencia un periodo antes o después.
pub fn shift(view: CalView, anchor: NaiveDate, forward: bool) -> NaiveDate {
    let days = |n: i64| if forward { anchor + Duration::days(n) } else { anchor - Duration::days(n) };
    match view {
        CalView::Day => days(1),
        CalView::WorkWeek | CalView::Week => days(7),
        CalView::Agenda => days(AGENDA_DAYS),
        CalView::Month => {
            let m = if forward {
                anchor.checked_add_months(Months::new(1))
            } else {
                anchor.checked_sub_months(Months::new(1))
            };
            m.unwrap_or(anchor)
        }
    }
}

fn capitalize(s: &str) -> String {
    let mut ch = s.chars();
    match ch.next() {
        Some(f) => f.to_uppercase().collect::<String>() + ch.as_str(),
        None => String::new(),
    }
}

fn short_month(m: u32) -> String {
    month_name(m).chars().take(3).collect()
}

/// Nombre largo del día de la semana.
pub fn weekday_long(d: NaiveDate) -> &'static str {
    let i = d.weekday().num_days_from_monday() as usize;
    let en = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
    let es = ["lunes", "martes", "miércoles", "jueves", "viernes", "sábado", "domingo"];
    pick(en.get(i).copied().unwrap_or(""), es.get(i).copied().unwrap_or(""))
}

/// «lunes 21 de octubre» / «Monday, October 21».
pub fn day_title(d: NaiveDate) -> String {
    if spanish() {
        format!("{} {} de {}", weekday_long(d), d.day(), month_name(d.month()))
    } else {
        format!("{}, {} {}", weekday_long(d), capitalize(month_name(d.month())), d.day())
    }
}

/// Título del periodo visible («19–23 de octubre de 2026», «Octubre 2026»…).
pub fn title(view: CalView, anchor: NaiveDate) -> String {
    let (start, n) = visible_range(view, anchor);
    match view {
        CalView::Month => format!("{} {}", capitalize(month_name(anchor.month())), anchor.year()),
        CalView::Day => {
            let t = day_title(anchor);
            format!("{} {}", capitalize(&t), anchor.year())
        }
        _ => {
            let end = start + Duration::days(n - 1);
            if start.month() == end.month() && start.year() == end.year() {
                if spanish() {
                    format!("{}–{} de {} de {}", start.day(), end.day(), month_name(start.month()), start.year())
                } else {
                    format!("{} {}–{}, {}", capitalize(month_name(start.month())), start.day(), end.day(), start.year())
                }
            } else if start.year() == end.year() {
                format!(
                    "{} {} – {} {} {}",
                    start.day(),
                    short_month(start.month()),
                    end.day(),
                    short_month(end.month()),
                    end.year()
                )
            } else {
                format!(
                    "{} {} {} – {} {} {}",
                    start.day(),
                    short_month(start.month()),
                    start.year(),
                    end.day(),
                    short_month(end.month()),
                    end.year()
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn ranges_and_shifts() {
        let wed = d(2026, 10, 21);
        assert_eq!(visible_range(CalView::Day, wed), (wed, 1));
        assert_eq!(visible_range(CalView::WorkWeek, wed), (d(2026, 10, 19), 5));
        assert_eq!(visible_range(CalView::Week, wed), (d(2026, 10, 19), 7));
        assert_eq!(visible_range(CalView::Month, wed), (d(2026, 9, 28), 42));
        assert_eq!(visible_range(CalView::Agenda, wed), (wed, AGENDA_DAYS));
        assert_eq!(shift(CalView::Day, wed, true), d(2026, 10, 22));
        assert_eq!(shift(CalView::Week, wed, false), d(2026, 10, 14));
        assert_eq!(shift(CalView::Month, d(2026, 1, 31), true), d(2026, 2, 28));
        for v in CalView::ALL {
            assert_eq!(CalView::from_key(v.key()), Some(v));
        }
    }

    #[test]
    fn titles() {
        crate::lang::set_lang(crate::lang::Lang::Es);
        assert_eq!(title(CalView::WorkWeek, d(2026, 10, 21)), "19–23 de octubre de 2026");
        assert_eq!(title(CalView::Week, d(2026, 10, 28)), "26 oct – 1 nov 2026");
        assert_eq!(title(CalView::Day, d(2026, 10, 21)), "Miércoles 21 de octubre 2026");
    }
}
