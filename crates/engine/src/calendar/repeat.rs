//! Reglas de repetición para el editor de eventos: la `RRULE` de RFC 5545 ⇄ un modelo de
//! formulario (ninguna, diaria, semanal con días, mensual por fecha o «el 2.º martes», anual,
//! personalizada) con fin por número de veces o fecha, y su descripción en lenguaje humano.
//!
//! Una regla que el formulario no sabe representar (`BYSETPOS`, `BYHOUR`, varias `BYMONTH`…) se
//! conserva tal cual (`raw`) mientras el usuario no cambie la repetición.

use chrono::{Datelike, Duration, NaiveDate, Weekday};

use crate::lang::{pick, spanish};
use crate::time::{Zone, month_name};

/// Frecuencia.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Hash)]
pub enum Freq {
    #[default]
    None,
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

/// Mensual: el mismo número de día, o «el n-ésimo día de la semana» (el último = -1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Hash)]
pub enum MonthlyBy {
    #[default]
    Date,
    NthWeekday,
}

/// Cuándo termina.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Hash)]
pub enum RepeatEnd {
    #[default]
    Never,
    Count(u32),
    /// Último día (incluido), en la zona del evento.
    Until(NaiveDate),
}

/// Repetición en forma de formulario.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Repeat {
    pub freq: Freq,
    /// Cada cuántos (1 = cada día/semana/mes/año).
    pub interval: u32,
    /// Días de la semana (semanal). Vacío = el día del inicio.
    pub weekdays: Vec<Weekday>,
    pub monthly: MonthlyBy,
    pub end: RepeatEnd,
    /// Regla original que el formulario no representa; se conserva si no se toca.
    pub raw: Option<String>,
}

/// Preajustes del menú «Repetir».
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Preset {
    None,
    Daily,
    Weekdays,
    Weekly,
    Monthly,
    Yearly,
    Custom,
}

impl Preset {
    pub const ALL: [Preset; 7] = [
        Preset::None,
        Preset::Daily,
        Preset::Weekdays,
        Preset::Weekly,
        Preset::Monthly,
        Preset::Yearly,
        Preset::Custom,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Preset::None => pick("Does not repeat", "No se repite"),
            Preset::Daily => pick("Every day", "Cada día"),
            Preset::Weekdays => pick("Every weekday (Mon–Fri)", "Cada día laborable (lun–vie)"),
            Preset::Weekly => pick("Every week", "Cada semana"),
            Preset::Monthly => pick("Every month", "Cada mes"),
            Preset::Yearly => pick("Every year", "Cada año"),
            Preset::Custom => pick("Custom…", "Personalizado…"),
        }
    }
}

const WORKDAYS: [Weekday; 5] = [Weekday::Mon, Weekday::Tue, Weekday::Wed, Weekday::Thu, Weekday::Fri];

fn wd_code(w: Weekday) -> &'static str {
    match w {
        Weekday::Mon => "MO",
        Weekday::Tue => "TU",
        Weekday::Wed => "WE",
        Weekday::Thu => "TH",
        Weekday::Fri => "FR",
        Weekday::Sat => "SA",
        Weekday::Sun => "SU",
    }
}

fn wd_parse(s: &str) -> Option<Weekday> {
    Some(match s {
        "MO" => Weekday::Mon,
        "TU" => Weekday::Tue,
        "WE" => Weekday::Wed,
        "TH" => Weekday::Thu,
        "FR" => Weekday::Fri,
        "SA" => Weekday::Sat,
        "SU" => Weekday::Sun,
        _ => return None,
    })
}

/// Nombre del día de la semana (en plural para «los lunes» no hace falta: es igual en español).
pub fn weekday_name(w: Weekday) -> &'static str {
    let i = w.num_days_from_monday() as usize;
    let en = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
    let es = ["lunes", "martes", "miércoles", "jueves", "viernes", "sábado", "domingo"];
    pick(en.get(i).copied().unwrap_or(""), es.get(i).copied().unwrap_or(""))
}

/// Letra corta del día para los botones del editor.
pub fn weekday_letter(w: Weekday) -> &'static str {
    let i = w.num_days_from_monday() as usize;
    let en = ["M", "T", "W", "T", "F", "S", "S"];
    let es = ["L", "M", "X", "J", "V", "S", "D"];
    pick(en.get(i).copied().unwrap_or(""), es.get(i).copied().unwrap_or(""))
}

/// Qué número de ese día de la semana es `d` en su mes (1–5) y si es el último.
pub fn nth_of_month(d: NaiveDate) -> (u32, bool) {
    let n = (d.day() - 1) / 7 + 1;
    let last = (d + Duration::days(7)).month() != d.month();
    (n, last)
}

fn ordinal(n: i32) -> String {
    if n == -1 {
        return pick("last", "último").into();
    }
    if spanish() {
        format!("{n}.º")
    } else {
        let suf = match n {
            1 => "st",
            2 => "nd",
            3 => "rd",
            _ => "th",
        };
        format!("{n}{suf}")
    }
}

impl Repeat {
    /// Sin repetición.
    pub fn none() -> Repeat {
        Repeat { interval: 1, ..Default::default() }
    }

    /// El preajuste que corresponde a esta regla (para el menú).
    pub fn preset(&self, start: NaiveDate) -> Preset {
        if self.raw.is_some() {
            return Preset::Custom;
        }
        let plain = self.interval <= 1 && self.end == RepeatEnd::Never;
        match self.freq {
            Freq::None => Preset::None,
            Freq::Daily if plain => Preset::Daily,
            Freq::Weekly if plain && sorted(&self.weekdays) == WORKDAYS.to_vec() => Preset::Weekdays,
            Freq::Weekly if plain && (self.weekdays.is_empty() || self.weekdays == vec![start.weekday()]) => {
                Preset::Weekly
            }
            Freq::Monthly if plain && self.monthly == MonthlyBy::Date => Preset::Monthly,
            Freq::Yearly if plain => Preset::Yearly,
            _ => Preset::Custom,
        }
    }

    /// Regla de un preajuste (`Custom` deja la actual o, si no hay, una semanal).
    pub fn from_preset(&self, p: Preset, start: NaiveDate) -> Repeat {
        let base = Repeat::none();
        match p {
            Preset::None => base,
            Preset::Daily => Repeat { freq: Freq::Daily, ..base },
            Preset::Weekdays => Repeat { freq: Freq::Weekly, weekdays: WORKDAYS.to_vec(), ..base },
            Preset::Weekly => Repeat { freq: Freq::Weekly, weekdays: vec![start.weekday()], ..base },
            Preset::Monthly => Repeat { freq: Freq::Monthly, ..base },
            Preset::Yearly => Repeat { freq: Freq::Yearly, ..base },
            Preset::Custom => {
                if self.freq == Freq::None {
                    Repeat { freq: Freq::Weekly, weekdays: vec![start.weekday()], ..base }
                } else {
                    Repeat { raw: None, ..self.clone() }
                }
            }
        }
    }

    /// Lee una `RRULE` (sin el prefijo `RRULE:`). `start` es el primer día (para saber qué
    /// representa «mensual el 2.º martes»).
    pub fn from_rrule(rrule: Option<&str>, start: NaiveDate) -> Repeat {
        let Some(r) = rrule.map(str::trim).filter(|r| !r.is_empty()) else { return Repeat::none() };
        let r = r.strip_prefix("RRULE:").unwrap_or(r);
        let mut rep = Repeat::none();
        let mut understood = true;
        let mut byday_raw: Vec<String> = Vec::new();
        for part in r.split(';').take(32) {
            let Some((k, v)) = part.split_once('=') else {
                understood = false;
                continue;
            };
            let v = v.trim().to_ascii_uppercase();
            match k.trim().to_ascii_uppercase().as_str() {
                "FREQ" => {
                    rep.freq = match v.as_str() {
                        "DAILY" => Freq::Daily,
                        "WEEKLY" => Freq::Weekly,
                        "MONTHLY" => Freq::Monthly,
                        "YEARLY" => Freq::Yearly,
                        _ => {
                            understood = false;
                            Freq::None
                        }
                    }
                }
                "INTERVAL" => match v.parse::<u32>() {
                    Ok(n) if (1..=999).contains(&n) => rep.interval = n,
                    _ => understood = false,
                },
                "COUNT" => match v.parse::<u32>() {
                    Ok(n) if (1..=9_999).contains(&n) => rep.end = RepeatEnd::Count(n),
                    _ => understood = false,
                },
                "UNTIL" => match v.get(..8).and_then(|d| NaiveDate::parse_from_str(d, "%Y%m%d").ok()) {
                    // Un `UNTIL` en UTC puede caer el día anterior en la zona del evento; el
                    // formulario muestra la fecha; al guardar se vuelve a poner al final del día.
                    Some(d) => rep.end = RepeatEnd::Until(d),
                    None => understood = false,
                },
                "BYDAY" => byday_raw = v.split(',').map(|s| s.trim().to_string()).collect(),
                "WKST" => {}
                "BYMONTHDAY" => {
                    if v.parse::<u32>().ok() != Some(start.day()) {
                        understood = false;
                    }
                }
                "BYMONTH" => {
                    if v.parse::<u32>().ok() != Some(start.month()) {
                        understood = false;
                    }
                }
                _ => understood = false,
            }
        }
        match rep.freq {
            Freq::Weekly => {
                for d in &byday_raw {
                    match wd_parse(d) {
                        Some(w) if !rep.weekdays.contains(&w) => rep.weekdays.push(w),
                        Some(_) => {}
                        None => understood = false,
                    }
                }
                rep.weekdays = sorted(&rep.weekdays);
            }
            Freq::Monthly if !byday_raw.is_empty() => {
                // Solo «el n-ésimo <día del inicio>».
                let ok = byday_raw.len() == 1
                    && byday_raw.first().is_some_and(|d| {
                        let (n, last) = nth_of_month(start);
                        let code = wd_code(start.weekday());
                        *d == format!("{n}{code}") || *d == format!("+{n}{code}") || (last && *d == format!("-1{code}"))
                    });
                if ok {
                    rep.monthly = MonthlyBy::NthWeekday;
                } else {
                    understood = false;
                }
            }
            _ if !byday_raw.is_empty() => understood = false,
            _ => {}
        }
        if rep.freq == Freq::None || !understood {
            rep.raw = Some(r.to_string());
            if rep.freq == Freq::None {
                rep.freq = Freq::Weekly;
            }
        }
        rep
    }

    /// `RRULE` para guardar (`None` = no se repite). `all_day` decide el formato de `UNTIL`; para
    /// eventos con hora, `UNTIL` es el final del último día en `zone`, en UTC (RFC 5545).
    pub fn to_rrule(&self, start: NaiveDate, all_day: bool, zone: Zone) -> Option<String> {
        if let Some(raw) = &self.raw {
            return Some(raw.clone());
        }
        let freq = match self.freq {
            Freq::None => return None,
            Freq::Daily => "DAILY",
            Freq::Weekly => "WEEKLY",
            Freq::Monthly => "MONTHLY",
            Freq::Yearly => "YEARLY",
        };
        let mut parts = vec![format!("FREQ={freq}")];
        if self.interval > 1 {
            parts.push(format!("INTERVAL={}", self.interval.min(999)));
        }
        match self.freq {
            Freq::Weekly => {
                let days = if self.weekdays.is_empty() { vec![start.weekday()] } else { sorted(&self.weekdays) };
                parts.push(format!("BYDAY={}", days.iter().map(|d| wd_code(*d)).collect::<Vec<_>>().join(",")));
            }
            Freq::Monthly if self.monthly == MonthlyBy::NthWeekday => {
                let (n, last) = nth_of_month(start);
                let n = if last && n >= 5 { -1 } else { n as i32 };
                parts.push(format!("BYDAY={n}{}", wd_code(start.weekday())));
            }
            _ => {}
        }
        match self.end {
            RepeatEnd::Never => {}
            RepeatEnd::Count(n) => parts.push(format!("COUNT={}", n.clamp(1, 9_999))),
            RepeatEnd::Until(d) => {
                let d = d.max(start);
                if all_day {
                    parts.push(format!("UNTIL={}", d.format("%Y%m%d")));
                } else {
                    let end = zone.day_start(d + Duration::days(1)) - Duration::seconds(1);
                    parts.push(format!("UNTIL={}", end.format("%Y%m%dT%H%M%SZ")));
                }
            }
        }
        Some(parts.join(";"))
    }

    /// Descripción para mostrar («Cada semana el lunes y el miércoles, 10 veces»).
    pub fn describe(&self, start: NaiveDate) -> String {
        if self.raw.is_some() {
            return pick("Custom repeat (kept as is)", "Repetición personalizada (se conserva)").into();
        }
        let n = self.interval.max(1);
        let every = |one_en: &str, one_es: &str, many_en: &str, many_es: &str| -> String {
            if n == 1 {
                pick(one_en, one_es).to_string()
            } else if spanish() {
                format!("Cada {n} {many_es}")
            } else {
                format!("Every {n} {many_en}")
            }
        };
        let mut s = match self.freq {
            Freq::None => return pick("Does not repeat", "No se repite").into(),
            Freq::Daily => every("Every day", "Cada día", "days", "días"),
            Freq::Weekly => {
                let days = if self.weekdays.is_empty() { vec![start.weekday()] } else { sorted(&self.weekdays) };
                let head = every("Every week", "Cada semana", "weeks", "semanas");
                if days == WORKDAYS.to_vec() {
                    format!("{head} {}", pick("on weekdays", "de lunes a viernes"))
                } else {
                    let names: Vec<&str> = days.iter().map(|d| weekday_name(*d)).collect();
                    format!("{head} {} {}", pick("on", "el"), join_and(&names))
                }
            }
            Freq::Monthly => {
                let head = every("Every month", "Cada mes", "months", "meses");
                match self.monthly {
                    MonthlyBy::Date => format!("{head} {} {}", pick("on day", "el día"), start.day()),
                    MonthlyBy::NthWeekday => {
                        let (k, last) = nth_of_month(start);
                        let k = if last && k >= 5 { -1 } else { k as i32 };
                        format!("{head} {} {} {}", pick("on the", "el"), ordinal(k), weekday_name(start.weekday()))
                    }
                }
            }
            Freq::Yearly => {
                let head = every("Every year", "Cada año", "years", "años");
                if spanish() {
                    format!("{head} el {} de {}", start.day(), month_name(start.month()))
                } else {
                    format!("{head} on {} {}", month_name(start.month()), start.day())
                }
            }
        };
        match self.end {
            RepeatEnd::Never => {}
            RepeatEnd::Count(c) => {
                s.push_str(&if spanish() { format!(", {c} veces") } else { format!(", {c} times") });
            }
            RepeatEnd::Until(d) => {
                s.push_str(&if spanish() {
                    format!(", hasta el {} de {} de {}", d.day(), month_name(d.month()), d.year())
                } else {
                    format!(", until {} {}, {}", month_name(d.month()), d.day(), d.year())
                });
            }
        }
        s
    }
}

fn sorted(v: &[Weekday]) -> Vec<Weekday> {
    let mut v = v.to_vec();
    v.sort_by_key(|d| d.num_days_from_monday());
    v.dedup();
    v
}

fn join_and(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [a] => (*a).to_string(),
        [rest @ .., last] => format!("{} {} {last}", rest.join(", "), pick("and", "y")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn round_trips() {
        let start = d(2026, 10, 13); // martes, 2.º del mes
        for r in [
            "FREQ=DAILY",
            "FREQ=WEEKLY;BYDAY=MO,WE,FR",
            "FREQ=WEEKLY;INTERVAL=2;BYDAY=TU;COUNT=10",
            "FREQ=MONTHLY",
            "FREQ=MONTHLY;BYDAY=2TU",
            "FREQ=YEARLY;UNTIL=20301013",
        ] {
            let rep = Repeat::from_rrule(Some(r), start);
            assert!(rep.raw.is_none(), "{r}");
            assert_eq!(rep.to_rrule(start, true, Zone::utc()).as_deref(), Some(r), "{r}");
        }
        assert_eq!(Repeat::from_rrule(None, start).to_rrule(start, false, Zone::utc()), None);
    }

    #[test]
    fn unknown_rules_are_kept() {
        let start = d(2026, 10, 13);
        let r = "FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1";
        let rep = Repeat::from_rrule(Some(r), start);
        assert!(rep.raw.is_some());
        assert_eq!(rep.preset(start), Preset::Custom);
        assert_eq!(rep.to_rrule(start, false, Zone::utc()).as_deref(), Some(r));
        // Elegir otro preajuste la reemplaza.
        let weekly = rep.from_preset(Preset::Weekly, start);
        assert_eq!(weekly.to_rrule(start, false, Zone::utc()).as_deref(), Some("FREQ=WEEKLY;BYDAY=TU"));
    }

    #[test]
    fn until_for_timed_events_is_end_of_day_utc() {
        let start = d(2026, 10, 13);
        let rep = Repeat { freq: Freq::Daily, end: RepeatEnd::Until(d(2026, 10, 20)), ..Repeat::none() };
        let off = chrono::FixedOffset::west_opt(6 * 3600).unwrap();
        assert_eq!(rep.to_rrule(start, false, Zone::Fixed(off)).as_deref(), Some("FREQ=DAILY;UNTIL=20261021T055959Z"));
    }

    #[test]
    fn presets_and_descriptions() {
        crate::lang::set_lang(crate::lang::Lang::Es);
        let start = d(2026, 10, 13);
        let wk = Repeat::none().from_preset(Preset::Weekdays, start);
        assert_eq!(wk.preset(start), Preset::Weekdays);
        assert_eq!(wk.describe(start), "Cada semana de lunes a viernes");
        let m = Repeat::from_rrule(Some("FREQ=MONTHLY;BYDAY=2TU;COUNT=5"), start);
        assert_eq!(m.describe(start), "Cada mes el 2.º martes, 5 veces");
        let w = Repeat::from_rrule(Some("FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,WE"), start);
        assert_eq!(w.describe(start), "Cada 2 semanas el lunes y miércoles");
        assert_eq!(w.preset(start), Preset::Custom);
        // Último viernes del mes.
        let last_fri = d(2026, 10, 30);
        let r = Repeat { freq: Freq::Monthly, monthly: MonthlyBy::NthWeekday, ..Repeat::none() };
        assert_eq!(r.to_rrule(last_fri, true, Zone::utc()).as_deref(), Some("FREQ=MONTHLY;BYDAY=-1FR"));
        assert!(Repeat::from_rrule(Some("FREQ=MONTHLY;BYDAY=-1FR"), last_fri).raw.is_none());
    }
}
