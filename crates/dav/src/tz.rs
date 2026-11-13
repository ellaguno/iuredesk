//! Zonas horarias: de un `TZID` cualquiera a una zona IANA de `chrono-tz`.
//!
//! Orden de resolución de [`resolve_tzid`]:
//! 1. Nombre IANA directo (`Europe/Madrid`), sin distinguir mayúsculas.
//! 2. Prefijos de generadores antiguos (`/mozilla.org/20050126_1/America/New_York`,
//!    `/softwarestudio.org/Olson_20011030_5/…`): se prueba cada sufijo tras una `/`.
//! 3. Nombres de Windows que manda Outlook/Exchange (`Romance Standard Time`), con la tabla
//!    `windowsZones.xml` de CLDR (territorio `001`).
//! 4. Nombres de pantalla de Outlook (`(UTC-06:00) Guadalajara, Mexico City, Monterrey`): por
//!    ciudad y, si no, por desfase fijo (`Etc/GMT+6`).
//! 5. El `VTIMEZONE` del propio archivo: `X-LIC-LOCATION`; si no hay y trae reglas anuales de
//!    `STANDARD` y `DAYLIGHT`, la zona IANA con las mismas reglas (mismo desfase cada día de este año
//!    y el siguiente; R4.5); si no, el desfase de su regla `STANDARD` como zona fija `Etc/GMT±N`.

use chrono::{DateTime, Datelike, LocalResult, NaiveDateTime, Offset, TimeZone, Utc};
use chrono_tz::Tz;

use crate::ical::Component;

/// Resuelve un `TZID` a una zona IANA. `vtimezones` son los `VTIMEZONE` del mismo archivo.
pub fn resolve_tzid(tzid: &str, vtimezones: &[&Component]) -> Option<Tz> {
    let t = tzid.trim().trim_matches('"').trim();
    if t.is_empty() {
        return None;
    }
    if let Some(tz) = iana(t) {
        return Some(tz);
    }
    // Prefijos tipo /mozilla.org/…/America/New_York: probar cada sufijo.
    for (i, c) in t.char_indices() {
        if c == '/'
            && let Some(tz) = t.get(i.saturating_add(1)..).and_then(iana)
        {
            return Some(tz);
        }
    }
    if let Some(tz) = windows_to_iana(t).and_then(iana) {
        return Some(tz);
    }
    if let Some(tz) = display_name(t) {
        return Some(tz);
    }
    let vtz = vtimezones.iter().find(|v| v.text("TZID").is_some_and(|id| id.trim() == t))?;
    if let Some(tz) = vtz.text("X-LIC-LOCATION").and_then(|l| iana(l.trim())) {
        return Some(tz);
    }
    // Con horario de verano: la zona IANA que sigue las mismas reglas.
    if let Some(tz) = iana_matching(vtz, Utc::now().year()) {
        return Some(tz);
    }
    // Desfase de la regla estándar (o de la única que haya).
    let rule = vtz.children("STANDARD").next().or_else(|| vtz.children("DAYLIGHT").next())?;
    let offset = parse_utc_offset(&rule.text("TZOFFSETTO")?)?;
    fixed_offset_zone(offset)
}

fn iana(name: &str) -> Option<Tz> {
    name.parse::<Tz>()
        .ok()
        .or_else(|| chrono_tz::TZ_VARIANTS.iter().copied().find(|t| t.name().eq_ignore_ascii_case(name)))
}

/// `+0100` / `-0530` → segundos.
pub fn parse_utc_offset(s: &str) -> Option<i32> {
    let s = s.trim();
    let (sign, digits) = match s.get(..1)? {
        "+" => (1, s.get(1..)?),
        "-" => (-1, s.get(1..)?),
        _ => (1, s),
    };
    if digits.len() < 4 || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let h: i32 = digits.get(0..2)?.parse().ok()?;
    let m: i32 = digits.get(2..4)?.parse().ok()?;
    let sec: i32 = digits.get(4..6).and_then(|x| x.parse().ok()).unwrap_or(0);
    if h > 23 || m > 59 || sec > 59 {
        return None;
    }
    Some(sign * (h * 3600 + m * 60 + sec))
}

/// Zona fija `Etc/GMT±N` para desfases de horas enteras (ojo: el signo de `Etc/` va al revés).
fn fixed_offset_zone(offset_secs: i32) -> Option<Tz> {
    if offset_secs % 3600 != 0 {
        return None;
    }
    let hours = offset_secs / 3600;
    let name = match hours {
        0 => "Etc/UTC".to_string(),
        h if h > 0 => format!("Etc/GMT-{h}"),
        h => format!("Etc/GMT+{}", h.saturating_neg()),
    };
    name.parse().ok()
}

/// Nombres de pantalla de Outlook: «(UTC+01:00) Brussels, Copenhagen, Madrid, Paris».
fn display_name(t: &str) -> Option<Tz> {
    let lower = t.to_ascii_lowercase();
    if !(lower.starts_with("(utc") || lower.starts_with("(gmt")) {
        return None;
    }
    for (needle, zone) in DISPLAY_CITIES {
        if lower.contains(needle) {
            return iana(zone);
        }
    }
    // Sin ciudad conocida: desfase fijo de «(UTC-06:00)».
    let inner = lower.get(4..)?.split(')').next()?.replace(':', "");
    if inner.is_empty() {
        return Some(Tz::UTC);
    }
    fixed_offset_zone(parse_utc_offset(&inner)?)
}

/// Ciudades de los nombres de pantalla de Windows → IANA (en minúsculas).
const DISPLAY_CITIES: &[(&str, &str)] = &[
    ("guadalajara", "America/Mexico_City"),
    ("mexico city", "America/Mexico_City"),
    ("chihuahua", "America/Chihuahua"),
    ("mazatlan", "America/Mazatlan"),
    ("tijuana", "America/Tijuana"),
    ("cancun", "America/Cancun"),
    ("eastern time", "America/New_York"),
    ("central time", "America/Chicago"),
    ("mountain time", "America/Denver"),
    ("pacific time", "America/Los_Angeles"),
    ("arizona", "America/Phoenix"),
    ("alaska", "America/Anchorage"),
    ("hawaii", "Pacific/Honolulu"),
    ("atlantic time", "America/Halifax"),
    ("bogota", "America/Bogota"),
    ("caracas", "America/Caracas"),
    ("santiago", "America/Santiago"),
    ("buenos aires", "America/Argentina/Buenos_Aires"),
    ("brasilia", "America/Sao_Paulo"),
    ("montevideo", "America/Montevideo"),
    ("asuncion", "America/Asuncion"),
    ("dublin, edinburgh, lisbon, london", "Europe/London"),
    ("amsterdam, berlin", "Europe/Berlin"),
    ("brussels, copenhagen, madrid, paris", "Europe/Paris"),
    ("belgrade, bratislava", "Europe/Budapest"),
    ("sarajevo, skopje, warsaw", "Europe/Warsaw"),
    ("athens, bucharest", "Europe/Bucharest"),
    ("helsinki, kyiv", "Europe/Kyiv"),
    ("moscow", "Europe/Moscow"),
    ("istanbul", "Europe/Istanbul"),
    ("jerusalem", "Asia/Jerusalem"),
    ("cairo", "Africa/Cairo"),
    ("dubai", "Asia/Dubai"),
    ("abu dhabi", "Asia/Dubai"),
    ("chennai, kolkata", "Asia/Kolkata"),
    ("beijing", "Asia/Shanghai"),
    ("singapore", "Asia/Singapore"),
    ("osaka, sapporo, tokyo", "Asia/Tokyo"),
    ("seoul", "Asia/Seoul"),
    ("canberra, melbourne, sydney", "Australia/Sydney"),
    ("brisbane", "Australia/Brisbane"),
    ("perth", "Australia/Perth"),
    ("auckland", "Pacific/Auckland"),
    ("coordinated universal time", "Etc/UTC"),
];

/// Nombre de zona de Windows → IANA (CLDR `windowsZones.xml`, territorio `001`).
pub fn windows_to_iana(name: &str) -> Option<&'static str> {
    WINDOWS_ZONES.iter().find(|(w, _)| w.eq_ignore_ascii_case(name.trim())).map(|(_, i)| *i)
}

/// Tabla CLDR `windowsZones.xml` (territorio `001`), más alias heredados de Exchange.
pub const WINDOWS_ZONES: &[(&str, &str)] = &[
    ("Dateline Standard Time", "Etc/GMT+12"),
    ("UTC-11", "Etc/GMT+11"),
    ("Aleutian Standard Time", "America/Adak"),
    ("Hawaiian Standard Time", "Pacific/Honolulu"),
    ("Marquesas Standard Time", "Pacific/Marquesas"),
    ("Alaskan Standard Time", "America/Anchorage"),
    ("UTC-09", "Etc/GMT+9"),
    ("Pacific Standard Time (Mexico)", "America/Tijuana"),
    ("UTC-08", "Etc/GMT+8"),
    ("Pacific Standard Time", "America/Los_Angeles"),
    ("US Mountain Standard Time", "America/Phoenix"),
    ("Mountain Standard Time (Mexico)", "America/Mazatlan"),
    ("Mountain Standard Time", "America/Denver"),
    ("Yukon Standard Time", "America/Whitehorse"),
    ("Central America Standard Time", "America/Guatemala"),
    ("Central Standard Time", "America/Chicago"),
    ("Easter Island Standard Time", "Pacific/Easter"),
    ("Central Standard Time (Mexico)", "America/Mexico_City"),
    ("Mexico Standard Time", "America/Mexico_City"),
    ("Canada Central Standard Time", "America/Regina"),
    ("SA Pacific Standard Time", "America/Bogota"),
    ("Eastern Standard Time (Mexico)", "America/Cancun"),
    ("Eastern Standard Time", "America/New_York"),
    ("Haiti Standard Time", "America/Port-au-Prince"),
    ("Cuba Standard Time", "America/Havana"),
    ("US Eastern Standard Time", "America/Indiana/Indianapolis"),
    ("Turks And Caicos Standard Time", "America/Grand_Turk"),
    ("Paraguay Standard Time", "America/Asuncion"),
    ("Atlantic Standard Time", "America/Halifax"),
    ("Venezuela Standard Time", "America/Caracas"),
    ("Central Brazilian Standard Time", "America/Cuiaba"),
    ("SA Western Standard Time", "America/La_Paz"),
    ("Pacific SA Standard Time", "America/Santiago"),
    ("Newfoundland Standard Time", "America/St_Johns"),
    ("Tocantins Standard Time", "America/Araguaina"),
    ("E. South America Standard Time", "America/Sao_Paulo"),
    ("SA Eastern Standard Time", "America/Cayenne"),
    ("Argentina Standard Time", "America/Argentina/Buenos_Aires"),
    ("Greenland Standard Time", "America/Nuuk"),
    ("Montevideo Standard Time", "America/Montevideo"),
    ("Magallanes Standard Time", "America/Punta_Arenas"),
    ("Saint Pierre Standard Time", "America/Miquelon"),
    ("Bahia Standard Time", "America/Bahia"),
    ("UTC-02", "Etc/GMT+2"),
    ("Mid-Atlantic Standard Time", "Etc/GMT+2"),
    ("Azores Standard Time", "Atlantic/Azores"),
    ("Cape Verde Standard Time", "Atlantic/Cape_Verde"),
    ("UTC", "Etc/UTC"),
    ("Coordinated Universal Time", "Etc/UTC"),
    ("GMT Standard Time", "Europe/London"),
    ("Greenwich Standard Time", "Atlantic/Reykjavik"),
    ("Sao Tome Standard Time", "Africa/Sao_Tome"),
    ("Morocco Standard Time", "Africa/Casablanca"),
    ("W. Europe Standard Time", "Europe/Berlin"),
    ("Central Europe Standard Time", "Europe/Budapest"),
    ("Romance Standard Time", "Europe/Paris"),
    ("Central European Standard Time", "Europe/Warsaw"),
    ("W. Central Africa Standard Time", "Africa/Lagos"),
    ("Jordan Standard Time", "Asia/Amman"),
    ("GTB Standard Time", "Europe/Bucharest"),
    ("Middle East Standard Time", "Asia/Beirut"),
    ("Egypt Standard Time", "Africa/Cairo"),
    ("E. Europe Standard Time", "Europe/Chisinau"),
    ("Syria Standard Time", "Asia/Damascus"),
    ("West Bank Standard Time", "Asia/Hebron"),
    ("South Africa Standard Time", "Africa/Johannesburg"),
    ("FLE Standard Time", "Europe/Kyiv"),
    ("Israel Standard Time", "Asia/Jerusalem"),
    ("South Sudan Standard Time", "Africa/Juba"),
    ("Kaliningrad Standard Time", "Europe/Kaliningrad"),
    ("Sudan Standard Time", "Africa/Khartoum"),
    ("Libya Standard Time", "Africa/Tripoli"),
    ("Namibia Standard Time", "Africa/Windhoek"),
    ("Arabic Standard Time", "Asia/Baghdad"),
    ("Turkey Standard Time", "Europe/Istanbul"),
    ("Arab Standard Time", "Asia/Riyadh"),
    ("Belarus Standard Time", "Europe/Minsk"),
    ("Russian Standard Time", "Europe/Moscow"),
    ("E. Africa Standard Time", "Africa/Nairobi"),
    ("Volgograd Standard Time", "Europe/Volgograd"),
    ("Iran Standard Time", "Asia/Tehran"),
    ("Arabian Standard Time", "Asia/Dubai"),
    ("Astrakhan Standard Time", "Europe/Astrakhan"),
    ("Azerbaijan Standard Time", "Asia/Baku"),
    ("Russia Time Zone 3", "Europe/Samara"),
    ("Mauritius Standard Time", "Indian/Mauritius"),
    ("Saratov Standard Time", "Europe/Saratov"),
    ("Georgian Standard Time", "Asia/Tbilisi"),
    ("Caucasus Standard Time", "Asia/Yerevan"),
    ("Afghanistan Standard Time", "Asia/Kabul"),
    ("West Asia Standard Time", "Asia/Tashkent"),
    ("Ekaterinburg Standard Time", "Asia/Yekaterinburg"),
    ("Pakistan Standard Time", "Asia/Karachi"),
    ("Qyzylorda Standard Time", "Asia/Qyzylorda"),
    ("India Standard Time", "Asia/Kolkata"),
    ("Sri Lanka Standard Time", "Asia/Colombo"),
    ("Nepal Standard Time", "Asia/Kathmandu"),
    ("Central Asia Standard Time", "Asia/Almaty"),
    ("Bangladesh Standard Time", "Asia/Dhaka"),
    ("Omsk Standard Time", "Asia/Omsk"),
    ("Myanmar Standard Time", "Asia/Yangon"),
    ("SE Asia Standard Time", "Asia/Bangkok"),
    ("Altai Standard Time", "Asia/Barnaul"),
    ("W. Mongolia Standard Time", "Asia/Hovd"),
    ("North Asia Standard Time", "Asia/Krasnoyarsk"),
    ("N. Central Asia Standard Time", "Asia/Novosibirsk"),
    ("Tomsk Standard Time", "Asia/Tomsk"),
    ("China Standard Time", "Asia/Shanghai"),
    ("North Asia East Standard Time", "Asia/Irkutsk"),
    ("Singapore Standard Time", "Asia/Singapore"),
    ("Malay Peninsula Standard Time", "Asia/Singapore"),
    ("W. Australia Standard Time", "Australia/Perth"),
    ("Taipei Standard Time", "Asia/Taipei"),
    ("Ulaanbaatar Standard Time", "Asia/Ulaanbaatar"),
    ("Aus Central W. Standard Time", "Australia/Eucla"),
    ("Transbaikal Standard Time", "Asia/Chita"),
    ("Tokyo Standard Time", "Asia/Tokyo"),
    ("North Korea Standard Time", "Asia/Pyongyang"),
    ("Korea Standard Time", "Asia/Seoul"),
    ("Yakutsk Standard Time", "Asia/Yakutsk"),
    ("Cen. Australia Standard Time", "Australia/Adelaide"),
    ("AUS Central Standard Time", "Australia/Darwin"),
    ("E. Australia Standard Time", "Australia/Brisbane"),
    ("AUS Eastern Standard Time", "Australia/Sydney"),
    ("West Pacific Standard Time", "Pacific/Port_Moresby"),
    ("Tasmania Standard Time", "Australia/Hobart"),
    ("Vladivostok Standard Time", "Asia/Vladivostok"),
    ("Lord Howe Standard Time", "Australia/Lord_Howe"),
    ("Bougainville Standard Time", "Pacific/Bougainville"),
    ("Russia Time Zone 10", "Asia/Srednekolymsk"),
    ("Magadan Standard Time", "Asia/Magadan"),
    ("Norfolk Standard Time", "Pacific/Norfolk"),
    ("Sakhalin Standard Time", "Asia/Sakhalin"),
    ("Central Pacific Standard Time", "Pacific/Guadalcanal"),
    ("Russia Time Zone 11", "Asia/Kamchatka"),
    ("Kamchatka Standard Time", "Asia/Kamchatka"),
    ("New Zealand Standard Time", "Pacific/Auckland"),
    ("UTC+12", "Etc/GMT-12"),
    ("Fiji Standard Time", "Pacific/Fiji"),
    ("Chatham Islands Standard Time", "Pacific/Chatham"),
    ("UTC+13", "Etc/GMT-13"),
    ("Tonga Standard Time", "Pacific/Tongatapu"),
    ("Samoa Standard Time", "Pacific/Apia"),
    ("Line Islands Standard Time", "Pacific/Kiritimati"),
];

/// Hora de reloj en una zona → instante UTC, sin fallar en los huecos y solapes del horario de
/// verano: en un solape gana la primera (RFC 5545 §3.3.5); en un hueco, la hora se corre hacia
/// adelante lo que dure el hueco (02:30 inexistente → 03:30).
pub fn local_to_utc(local: NaiveDateTime, tz: Tz) -> DateTime<Utc> {
    match tz.from_local_datetime(&local) {
        LocalResult::Single(t) => t.with_timezone(&Utc),
        LocalResult::Ambiguous(a, b) => a.min(b).with_timezone(&Utc),
        LocalResult::None => {
            // Hueco: se usa el desfase de antes del cambio (02:30 EST → 07:30 UTC = 03:30 EDT).
            // Los huecos reales duran ≤ 2 h; se busca un instante válido unas horas antes.
            let before = (1..=4).find_map(|h| {
                let probe = local.checked_sub_signed(chrono::Duration::hours(h))?;
                tz.from_local_datetime(&probe).earliest()
            });
            match before {
                Some(b) => {
                    let offset = chrono::Duration::seconds(i64::from(b.offset().fix().local_minus_utc()));
                    Utc.from_utc_datetime(&local.checked_sub_signed(offset).unwrap_or(local))
                }
                None => Utc.from_utc_datetime(&local),
            }
        }
    }
}

// ----------------------------------------------------------------- VTIMEZONE con horario de verano

/// Zonas que se prueban primero al buscar la IANA equivalente a un `VTIMEZONE` propio (las más
/// comunes, para no acabar en un nombre raro con las mismas reglas).
const PREFERRED: &[&str] = &[
    "America/New_York",
    "America/Chicago",
    "America/Denver",
    "America/Los_Angeles",
    "America/Anchorage",
    "America/Halifax",
    "America/St_Johns",
    "America/Mexico_City",
    "America/Havana",
    "America/Santiago",
    "America/Asuncion",
    "Europe/London",
    "Europe/Dublin",
    "Europe/Lisbon",
    "Europe/Paris",
    "Europe/Berlin",
    "Europe/Madrid",
    "Europe/Athens",
    "Europe/Helsinki",
    "Asia/Jerusalem",
    "Asia/Beirut",
    "Australia/Sydney",
    "Australia/Adelaide",
    "Australia/Lord_Howe",
    "Pacific/Auckland",
    "Pacific/Chatham",
];

/// Una observancia (`STANDARD`/`DAYLIGHT`) de un `VTIMEZONE`, con su regla anual si la tiene.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Observance {
    from: i32,
    to: i32,
    /// `DTSTART` (hora local con el desfase `from`).
    start: NaiveDateTime,
    /// `RRULE:FREQ=YEARLY;BYMONTH=m;BYDAY=nWD` (n negativo cuenta desde el final del mes).
    rule: Option<(u32, i8, chrono::Weekday)>,
}

/// Lee las observancias; `None` si no hay al menos una `STANDARD` y una `DAYLIGHT` con regla
/// anual que se entienda (lo demás sigue por desfase fijo).
fn observances(vtz: &Component) -> Option<Vec<Observance>> {
    let mut out = Vec::new();
    let mut kinds = (false, false);
    for (kind, c) in vtz.children("STANDARD").map(|c| (0, c)).chain(vtz.children("DAYLIGHT").map(|c| (1, c))).take(16) {
        let from = parse_utc_offset(&c.text("TZOFFSETFROM")?)?;
        let to = parse_utc_offset(&c.text("TZOFFSETTO")?)?;
        let start = NaiveDateTime::parse_from_str(c.text("DTSTART")?.trim(), "%Y%m%dT%H%M%S").ok()?;
        let rule = c.text("RRULE").and_then(|r| yearly_rule(&r));
        if rule.is_some() {
            if kind == 0 {
                kinds.0 = true;
            } else {
                kinds.1 = true;
            }
        }
        out.push(Observance { from, to, start, rule });
    }
    (kinds.0 && kinds.1).then_some(out)
}

/// `FREQ=YEARLY;BYMONTH=3;BYDAY=2SU` (o `-1SU`, `BYDAY=SU` con `BYSETPOS`… no: solo n y día).
fn yearly_rule(r: &str) -> Option<(u32, i8, chrono::Weekday)> {
    let mut freq = false;
    let mut month = None;
    let mut byday = None;
    for part in r.split(';').take(12) {
        let (k, v) = part.split_once('=')?;
        match k.trim().to_ascii_uppercase().as_str() {
            "FREQ" => freq = v.trim().eq_ignore_ascii_case("YEARLY"),
            "BYMONTH" => month = v.trim().parse::<u32>().ok().filter(|m| (1..=12).contains(m)),
            "BYDAY" => {
                let v = v.trim().to_ascii_uppercase();
                let (n, wd) = v.split_at(v.len().saturating_sub(2));
                let n: i8 = if n.is_empty() || n == "+" { 1 } else { n.trim_start_matches('+').parse().ok()? };
                if n == 0 || !(-5..=5).contains(&n) {
                    return None;
                }
                let wd = match wd {
                    "MO" => chrono::Weekday::Mon,
                    "TU" => chrono::Weekday::Tue,
                    "WE" => chrono::Weekday::Wed,
                    "TH" => chrono::Weekday::Thu,
                    "FR" => chrono::Weekday::Fri,
                    "SA" => chrono::Weekday::Sat,
                    "SU" => chrono::Weekday::Sun,
                    _ => return None,
                };
                byday = Some((n, wd));
            }
            // UNTIL, INTERVAL=1, etc.: se ignoran (las reglas vigentes no los llevan).
            _ => {}
        }
    }
    let (n, wd) = byday?;
    freq.then_some((month?, n, wd))
}

/// El n-ésimo `wd` de `month` de `year` (n < 0 desde el final).
fn nth_weekday(year: i32, month: u32, n: i8, wd: chrono::Weekday) -> Option<chrono::NaiveDate> {
    if n > 0 {
        let first = chrono::NaiveDate::from_ymd_opt(year, month, 1)?;
        let shift = (7 + wd.num_days_from_monday() as i64 - first.weekday().num_days_from_monday() as i64) % 7;
        let d = first.checked_add_signed(chrono::Duration::days(shift + 7 * (i64::from(n) - 1)))?;
        (d.month() == month).then_some(d)
    } else {
        let next = if month == 12 {
            chrono::NaiveDate::from_ymd_opt(year.checked_add(1)?, 1, 1)?
        } else {
            chrono::NaiveDate::from_ymd_opt(year, month + 1, 1)?
        };
        let last = next.pred_opt()?;
        let shift = (7 + last.weekday().num_days_from_monday() as i64 - wd.num_days_from_monday() as i64) % 7;
        let d = last.checked_sub_signed(chrono::Duration::days(shift + 7 * (i64::from(n.saturating_neg()) - 1)))?;
        (d.month() == month).then_some(d)
    }
}

/// Desfase del `VTIMEZONE` en el instante `utc`: el de la observancia que empezó más tarde.
fn vtz_offset(obs: &[Observance], utc: NaiveDateTime) -> Option<i32> {
    let mut best: Option<(NaiveDateTime, i32)> = None;
    for o in obs {
        let onsets: Vec<NaiveDateTime> = match o.rule {
            Some((m, n, wd)) => [utc.year().saturating_sub(1), utc.year()]
                .iter()
                .filter_map(|y| nth_weekday(*y, m, n, wd).map(|d| d.and_time(o.start.time())))
                .filter(|l| *l >= o.start)
                .collect(),
            None => vec![o.start],
        };
        for local in onsets {
            let on_utc = local.checked_sub_signed(chrono::Duration::seconds(i64::from(o.from)))?;
            if on_utc <= utc && best.is_none_or(|(b, _)| on_utc > b) {
                best = Some((on_utc, o.to));
            }
        }
    }
    best.map(|(_, off)| off)
}

/// Zona IANA con las mismas reglas que el `VTIMEZONE` (mismo desfase cada día de este año y el
/// siguiente), o `None`.
fn iana_matching(vtz: &Component, year: i32) -> Option<Tz> {
    let obs = observances(vtz)?;
    let samples: Vec<(NaiveDateTime, i32)> = (0..730)
        .filter_map(|d| {
            let t = chrono::NaiveDate::from_ymd_opt(year, 1, 1)?
                .checked_add_signed(chrono::Duration::days(d))?
                .and_hms_opt(12, 0, 0)?;
            Some((t, vtz_offset(&obs, t)?))
        })
        .collect();
    if samples.len() < 700 {
        return None;
    }
    let matches =
        |tz: &Tz| samples.iter().all(|(t, off)| tz.offset_from_utc_datetime(t).fix().local_minus_utc() == *off);
    let preferred = PREFERRED.iter().filter_map(|n| n.parse::<Tz>().ok());
    let rest = chrono_tz::TZ_VARIANTS.iter().copied().filter(|t| {
        let n = t.name();
        n.contains('/') && !n.starts_with("Etc/") && !n.starts_with("SystemV/") && !n.starts_with("US/")
    });
    preferred.chain(rest).find(matches)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, Timelike};

    #[test]
    fn whole_windows_table_resolves() {
        for (w, i) in WINDOWS_ZONES {
            assert!(iana(i).is_some(), "{w} → {i} no existe en chrono-tz");
        }
        for (c, i) in DISPLAY_CITIES {
            assert!(iana(i).is_some(), "{c} → {i} no existe en chrono-tz");
        }
    }

    #[test]
    fn resolves_variants() {
        assert_eq!(resolve_tzid("Europe/Madrid", &[]), Some(chrono_tz::Europe::Madrid));
        assert_eq!(resolve_tzid("europe/madrid", &[]), Some(chrono_tz::Europe::Madrid));
        assert_eq!(resolve_tzid("/mozilla.org/20050126_1/America/New_York", &[]), Some(chrono_tz::America::New_York));
        assert_eq!(resolve_tzid("Romance Standard Time", &[]), Some(chrono_tz::Europe::Paris));
        assert_eq!(resolve_tzid("Central Standard Time (Mexico)", &[]), Some(chrono_tz::America::Mexico_City));
        assert_eq!(
            resolve_tzid("(UTC-06:00) Guadalajara, Mexico City, Monterrey", &[]),
            Some(chrono_tz::America::Mexico_City)
        );
        assert_eq!(resolve_tzid("(UTC+05:00) Nowhere", &[]), Some(chrono_tz::Etc::GMTMinus5));
        assert_eq!(resolve_tzid("", &[]), None);
    }

    #[test]
    fn resolves_from_vtimezone() {
        let src = "BEGIN:VTIMEZONE\r\nTZID:Custom Zone\r\nBEGIN:STANDARD\r\nTZOFFSETTO:-0500\r\n\
END:STANDARD\r\nEND:VTIMEZONE\r\nBEGIN:VTIMEZONE\r\nTZID:Lic\r\nX-LIC-LOCATION:Asia/Tokyo\r\nEND:VTIMEZONE\r\n";
        let comps = crate::ical::parse(src).unwrap();
        let refs: Vec<&Component> = comps.iter().collect();
        assert_eq!(resolve_tzid("Custom Zone", &refs), Some(chrono_tz::Etc::GMTPlus5));
        assert_eq!(resolve_tzid("Lic", &refs), Some(chrono_tz::Asia::Tokyo));
        assert_eq!(resolve_tzid("Unknown", &refs), None);
    }

    /// R4.5: un `VTIMEZONE` propio con horario de verano ya no queda en desfase fijo.
    #[test]
    fn vtimezone_with_dst_rules_maps_to_iana() {
        let eu = "BEGIN:VTIMEZONE\r\nTZID:Mi hora de Europa\r\nBEGIN:STANDARD\r\nDTSTART:19701025T030000\r\n\
TZOFFSETFROM:+0200\r\nTZOFFSETTO:+0100\r\nRRULE:FREQ=YEARLY;BYMONTH=10;BYDAY=-1SU\r\nEND:STANDARD\r\n\
BEGIN:DAYLIGHT\r\nDTSTART:19700329T020000\r\nTZOFFSETFROM:+0100\r\nTZOFFSETTO:+0200\r\n\
RRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=-1SU\r\nEND:DAYLIGHT\r\nEND:VTIMEZONE\r\n";
        let us = "BEGIN:VTIMEZONE\r\nTZID:Oficina NY\r\nBEGIN:STANDARD\r\nDTSTART:20071104T020000\r\n\
TZOFFSETFROM:-0400\r\nTZOFFSETTO:-0500\r\nRRULE:FREQ=YEARLY;BYMONTH=11;BYDAY=1SU\r\nEND:STANDARD\r\n\
BEGIN:DAYLIGHT\r\nDTSTART:20070311T020000\r\nTZOFFSETFROM:-0500\r\nTZOFFSETTO:-0400\r\n\
RRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=2SU\r\nEND:DAYLIGHT\r\nEND:VTIMEZONE\r\n";
        let comps = crate::ical::parse(&format!("{eu}{us}")).unwrap();
        let refs: Vec<&Component> = comps.iter().collect();
        let tz = resolve_tzid("Mi hora de Europa", &refs).unwrap();
        assert_eq!(tz, chrono_tz::Europe::Paris, "{}", tz.name());
        assert_eq!(resolve_tzid("Oficina NY", &refs), Some(chrono_tz::America::New_York));
        // Verano de verdad: 1 de julio a mediodía en «Oficina NY» = 16:00 UTC (EDT, −4).
        let jul = NaiveDate::from_ymd_opt(2026, 7, 1).unwrap().and_hms_opt(12, 0, 0).unwrap();
        assert_eq!(local_to_utc(jul, chrono_tz::America::New_York).hour(), 16);
        // Mismas reglas comprobadas a mano en el propio VTIMEZONE.
        let obs = observances(refs[1]).unwrap();
        let t = |m, d| NaiveDate::from_ymd_opt(2026, m, d).unwrap().and_hms_opt(12, 0, 0).unwrap();
        assert_eq!(vtz_offset(&obs, t(1, 15)), Some(-5 * 3600));
        assert_eq!(vtz_offset(&obs, t(3, 7)), Some(-5 * 3600));
        assert_eq!(vtz_offset(&obs, t(3, 9)), Some(-4 * 3600));
        assert_eq!(vtz_offset(&obs, t(11, 2)), Some(-5 * 3600));
        // Reglas que ninguna zona sigue (verano en febrero): desfase fijo como antes.
        let odd = us.replace("BYMONTH=3", "BYMONTH=2").replace("Oficina NY", "Rara");
        let comps = crate::ical::parse(&odd).unwrap();
        let refs: Vec<&Component> = comps.iter().collect();
        assert_eq!(resolve_tzid("Rara", &refs), Some(chrono_tz::Etc::GMTPlus5));
    }

    #[test]
    fn nth_weekdays() {
        let d = |y, m, n, w| nth_weekday(y, m, n, w).map(|d| d.to_string());
        assert_eq!(d(2026, 3, 2, chrono::Weekday::Sun).as_deref(), Some("2026-03-08"));
        assert_eq!(d(2026, 3, -1, chrono::Weekday::Sun).as_deref(), Some("2026-03-29"));
        assert_eq!(d(2026, 12, -1, chrono::Weekday::Thu).as_deref(), Some("2026-12-31"));
        assert_eq!(d(2026, 2, 5, chrono::Weekday::Mon), None);
        assert_eq!(yearly_rule("FREQ=YEARLY;BYMONTH=10;BYDAY=-1SU"), Some((10, -1, chrono::Weekday::Sun)));
        assert_eq!(yearly_rule("FREQ=YEARLY;BYMONTH=13;BYDAY=1SU"), None);
        assert_eq!(yearly_rule("FREQ=MONTHLY;BYMONTH=3;BYDAY=1SU"), None);
        assert_eq!(yearly_rule("FREQ=YEARLY;BYMONTH=3;BYDAY=9XX"), None);
        assert_eq!(yearly_rule("basura"), None);
    }

    #[test]
    fn dst_gap_and_overlap() {
        let ny = chrono_tz::America::New_York;
        // 2026-03-08 02:30 no existe en Nueva York → 03:30 EDT = 07:30 UTC.
        let gap = NaiveDate::from_ymd_opt(2026, 3, 8).unwrap().and_hms_opt(2, 30, 0).unwrap();
        assert_eq!(local_to_utc(gap, ny).hour(), 7);
        // 2026-11-01 01:30 existe dos veces → la primera (EDT) = 05:30 UTC.
        let overlap = NaiveDate::from_ymd_opt(2026, 11, 1).unwrap().and_hms_opt(1, 30, 0).unwrap();
        assert_eq!(local_to_utc(overlap, ny).hour(), 5);
    }

    #[test]
    fn offsets() {
        assert_eq!(parse_utc_offset("+0530"), Some(19800));
        assert_eq!(parse_utc_offset("-0100"), Some(-3600));
        assert_eq!(parse_utc_offset("x"), None);
        assert_eq!(parse_utc_offset("+9999"), None);
    }
}
