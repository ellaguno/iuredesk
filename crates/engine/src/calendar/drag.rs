//! Cuentas de arrastrar en la rejilla del calendario (crear, mover, estirar), con pasos de 15
//! minutos. Puras y sin egui: la UI pasa píxeles convertidos a minutos y recibe horas locales.

use chrono::{Duration, NaiveDate, NaiveDateTime, NaiveTime, Timelike};

/// Paso de ajuste, en minutos.
pub const SNAP_MIN: i64 = 15;
/// Minutos de un día.
pub const DAY_MIN: i64 = 24 * 60;
/// Duración de un evento creado con un clic (sin arrastrar).
pub const CLICK_MIN: i64 = 30;

/// Minutos del día bajo `y` píxeles desde el borde superior de la rejilla (`hh` px por hora),
/// acotados al día.
pub fn y_to_minutes(y: f32, hh: f32) -> f32 {
    if hh.is_nan() || hh <= 0.0 || !y.is_finite() {
        return 0.0;
    }
    (y / hh * 60.0).clamp(0.0, DAY_MIN as f32)
}

/// Redondea al paso más cercano.
pub fn snap(min: f32) -> i64 {
    if !min.is_finite() {
        return 0;
    }
    let step = SNAP_MIN as f32;
    ((min / step).round() as i64).saturating_mul(SNAP_MIN)
}

fn snap_floor(min: f32) -> i64 {
    if !min.is_finite() {
        return 0;
    }
    ((min / SNAP_MIN as f32).floor() as i64).saturating_mul(SNAP_MIN)
}

fn snap_ceil(min: f32) -> i64 {
    if !min.is_finite() {
        return 0;
    }
    ((min / SNAP_MIN as f32).ceil() as i64).saturating_mul(SNAP_MIN)
}

/// Rango (inicio, fin) en minutos del día al arrastrar para crear desde `press` hasta `current`
/// (en cualquier sentido). Un clic sin arrastre da [`CLICK_MIN`] minutos desde el cuarto de hora
/// pulsado. Nunca sale del día ni queda vacío.
pub fn create_span(press: f32, current: f32) -> (i64, i64) {
    let (a, b) = if current < press { (current, press) } else { (press, current) };
    let mut start = snap_floor(a).clamp(0, DAY_MIN - SNAP_MIN);
    let mut end = snap_ceil(b).clamp(0, DAY_MIN);
    if end - start < SNAP_MIN || (b - a).abs() < 4.0 {
        end = (start + CLICK_MIN).min(DAY_MIN);
    }
    if end <= start {
        start = (end - SNAP_MIN).max(0);
    }
    (start, end)
}

/// Hora local de `day` a `min` minutos (24:00 = medianoche del día siguiente).
pub fn at_minutes(day: NaiveDate, min: i64) -> NaiveDateTime {
    let min = min.clamp(0, DAY_MIN);
    day.and_time(NaiveTime::MIN) + Duration::minutes(min)
}

/// Minutos desde la medianoche de una hora local.
pub fn minutes_of(t: NaiveDateTime) -> i64 {
    i64::from(t.hour()) * 60 + i64::from(t.minute())
}

/// Mover un evento `delta_days` días y `delta_min` minutos: el inicio cae en un cuarto de hora y
/// la duración se conserva.
pub fn move_times(
    start: NaiveDateTime,
    end: NaiveDateTime,
    delta_days: i64,
    delta_min: f32,
) -> (NaiveDateTime, NaiveDateTime) {
    let len = (end - start).max(Duration::zero());
    let day = start.date() + Duration::days(delta_days.clamp(-3660, 3660));
    let target = minutes_of(start) as f32 + if delta_min.is_finite() { delta_min } else { 0.0 };
    let new_start = day.and_time(NaiveTime::MIN) + Duration::minutes(snap(target));
    (new_start, new_start + len)
}

/// Estirar el borde de abajo: el fin nuevo cae en un cuarto de hora y dura al menos uno.
pub fn resize_end(start: NaiveDateTime, end: NaiveDateTime, delta_min: f32) -> NaiveDateTime {
    let target = (end - start).num_minutes() as f32 + if delta_min.is_finite() { delta_min } else { 0.0 };
    let len = snap(target).clamp(SNAP_MIN, 14 * DAY_MIN);
    start + Duration::minutes(len)
}

/// Mover un evento de día completo `[d0, d1)` `delta` días.
pub fn move_days(d0: NaiveDate, d1: NaiveDate, delta: i64) -> (NaiveDate, NaiveDate) {
    let delta = delta.clamp(-3660, 3660);
    let len = (d1 - d0).num_days().max(1);
    let s = d0 + Duration::days(delta);
    (s, s + Duration::days(len))
}

/// Días `[inicio, fin)` al arrastrar en la fila de días completos de `a` a `b`.
pub fn day_span(a: NaiveDate, b: NaiveDate) -> (NaiveDate, NaiveDate) {
    let (s, e) = if b < a { (b, a) } else { (a, b) };
    (s, e + Duration::days(1))
}

/// Columna de día bajo `x` (px desde el borde izquierdo de las columnas, `col_w` px cada una).
pub fn column_at(x: f32, col_w: f32, days: i64) -> i64 {
    if col_w.is_nan() || col_w <= 0.0 || !x.is_finite() || days <= 0 {
        return 0;
    }
    ((x / col_w).floor() as i64).clamp(0, days - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(d: u32, h: u32, m: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 10, d).unwrap().and_hms_opt(h, m, 0).unwrap()
    }

    #[test]
    fn y_and_snap() {
        assert_eq!(y_to_minutes(88.0, 44.0), 120.0);
        assert_eq!(y_to_minutes(-5.0, 44.0), 0.0);
        assert_eq!(y_to_minutes(1e9, 44.0), 1440.0);
        assert_eq!(y_to_minutes(10.0, 0.0), 0.0);
        assert_eq!(snap(7.0), 0);
        assert_eq!(snap(8.0), 15);
        assert_eq!(snap(52.0), 45);
        assert_eq!(snap(f32::NAN), 0);
    }

    #[test]
    fn create_both_directions_and_click() {
        assert_eq!(create_span(9.0 * 60.0 + 5.0, 10.0 * 60.0 + 20.0), (540, 630));
        assert_eq!(create_span(10.0 * 60.0 + 20.0, 9.0 * 60.0 + 5.0), (540, 630), "hacia arriba");
        assert_eq!(create_span(600.0, 601.0), (600, 630), "clic = 30 min");
        assert_eq!(create_span(1439.0, 1440.0), (1425, 1440), "no se sale del día");
        let (s, e) = create_span(0.0, 0.0);
        assert!(s == 0 && e == 30);
    }

    #[test]
    fn move_keeps_length_and_snaps() {
        let (s, e) = move_times(t(21, 9, 0), t(21, 10, 30), 1, 37.0);
        assert_eq!((s, e), (t(22, 9, 30), t(22, 11, 0)));
        let (s, e) = move_times(t(21, 9, 10), t(21, 9, 40), 0, 0.0);
        assert_eq!((s, e), (t(21, 9, 15), t(21, 9, 45)), "se ajusta al cuarto");
        let (s, _) = move_times(t(21, 0, 30), t(21, 1, 0), 0, -120.0);
        assert_eq!(s, t(20, 22, 30), "cruza la medianoche hacia atrás");
    }

    #[test]
    fn resize_snaps_and_never_collapses() {
        assert_eq!(resize_end(t(21, 9, 0), t(21, 10, 0), 22.0), t(21, 10, 15));
        assert_eq!(resize_end(t(21, 9, 0), t(21, 10, 0), -300.0), t(21, 9, 15));
    }

    #[test]
    fn all_day_moves() {
        let d = |x| NaiveDate::from_ymd_opt(2026, 10, x).unwrap();
        assert_eq!(move_days(d(21), d(23), 2), (d(23), d(25)));
        assert_eq!(day_span(d(23), d(21)), (d(21), d(24)));
        assert_eq!(column_at(250.0, 100.0, 5), 2);
        assert_eq!(column_at(-3.0, 100.0, 5), 0);
        assert_eq!(column_at(9e9, 100.0, 5), 4);
    }
}
