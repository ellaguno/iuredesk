//! UX-15: el clima de hoy en «Mi día» (lo pidió el dueño 2026-11-09: «solo datos de hoy y, si
//! se pica, ir a la fuente»).
//!
//! - Pronóstico: **MET Norway** (`api.met.no`, `locationforecast/2.0/compact`; gratis, sin clave,
//!   datos CC BY 4.0). Pide un User-Agent que diga quién llama. El clic abre la misma previsión en
//!   **yr.no** (el sitio público de esos datos) por coordenadas.
//! - Ciudad: la elige el usuario; se busca con el geocodificador de **Open-Meteo** (sin clave). La
//!   primera sugerencia sale de la zona horaria del sistema («America/Mexico_City» → «Mexico City»),
//!   sin preguntarle nada a nadie por la IP.
//!
//! Solo se pide si el usuario lo activó. Todo acotado: 10 s por petición, [`MAX_JSON`] de
//! respuesta, coordenadas comprobadas, textos recortados.

use std::time::Duration;

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::lang::{Text, pick};

/// Pronóstico de MET Norway.
pub const MET_API: &str = "https://api.met.no/weatherapi/locationforecast/2.0/compact";
/// Geocodificador de Open-Meteo.
pub const GEO_API: &str = "https://geocoding-api.open-meteo.com/v1/search";
/// Respuesta JSON como mucho.
pub const MAX_JSON: usize = 2 * 1024 * 1024;
/// Resultados de búsqueda de ciudades.
pub const MAX_PLACES: usize = 8;
/// Cada cuánto se vuelve a pedir el pronóstico.
pub const REFRESH: Duration = Duration::from_secs(30 * 60);
/// Tras un error, cuándo se reintenta.
pub const RETRY: Duration = Duration::from_secs(10 * 60);
const TIMEOUT: Duration = Duration::from_secs(10);
const USER_AGENT: &str = concat!("IureDesk/", env!("CARGO_PKG_VERSION"), " (https://iurefficient.com; desktop PIM)");

/// Una ciudad con sus coordenadas.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Place {
    pub name: String,
    /// Estado o región («Ciudad de México», «Jalisco»).
    #[serde(default)]
    pub admin: String,
    #[serde(default)]
    pub country: String,
    pub lat: f64,
    pub lon: f64,
}

// Las preferencias son `Eq`; las coordenadas solo entran comprobadas ([`Place::valid`]: finitas).
impl Eq for Place {}
impl Eq for WeatherPref {}

impl Place {
    /// «Guadalajara, Jalisco, México».
    pub fn full_name(&self) -> String {
        let mut parts = vec![self.name.as_str()];
        if !self.admin.is_empty() && self.admin != self.name {
            parts.push(&self.admin);
        }
        if !self.country.is_empty() {
            parts.push(&self.country);
        }
        parts.join(", ")
    }

    /// ¿Coordenadas válidas?
    pub fn valid(&self) -> bool {
        self.lat.is_finite()
            && self.lon.is_finite()
            && (-90.0..=90.0).contains(&self.lat)
            && (-180.0..=180.0).contains(&self.lon)
    }

    /// La previsión de este lugar en yr.no (la fuente pública de los datos de MET Norway).
    pub fn source_url(&self) -> String {
        format!("https://www.yr.no/en/forecast/daily-table/{:.4},{:.4}", self.lat, self.lon)
    }
}

/// Preferencia del clima (Ajustes y la cabecera de «Mi día»).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WeatherPref {
    /// Mostrar el clima (pide la red).
    #[serde(default)]
    pub enabled: bool,
    /// El usuario dijo «no, gracias»: ya no se ofrece.
    #[serde(default)]
    pub dismissed: bool,
    #[serde(default)]
    pub place: Option<Place>,
    /// Grados Fahrenheit.
    #[serde(default)]
    pub fahrenheit: bool,
}

/// Cielo, para el icono.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sky {
    Clear,
    PartlyCloudy,
    Cloudy,
    Fog,
    Rain,
    Showers,
    Sleet,
    Snow,
    Thunder,
}

impl Sky {
    /// De un `symbol_code` de MET Norway («partlycloudy_day», «heavyrainshowersandthunder»…).
    pub fn from_symbol(code: &str) -> Sky {
        let c = code.split('_').next().unwrap_or(code);
        if c.contains("thunder") {
            Sky::Thunder
        } else if c.contains("sleet") {
            Sky::Sleet
        } else if c.contains("snow") {
            Sky::Snow
        } else if c.contains("showers") {
            Sky::Showers
        } else if c.contains("rain") {
            Sky::Rain
        } else if c.contains("fog") {
            Sky::Fog
        } else if c.starts_with("partlycloudy") || c.starts_with("fair") {
            Sky::PartlyCloudy
        } else if c.starts_with("cloudy") {
            Sky::Cloudy
        } else {
            Sky::Clear
        }
    }

    /// Descripción corta.
    pub fn label(self) -> &'static str {
        match self {
            Sky::Clear => pick("Clear", "Despejado"),
            Sky::PartlyCloudy => pick("Partly cloudy", "Parcialmente nublado"),
            Sky::Cloudy => pick("Cloudy", "Nublado"),
            Sky::Fog => pick("Fog", "Niebla"),
            Sky::Rain => pick("Rain", "Lluvia"),
            Sky::Showers => pick("Showers", "Chubascos"),
            Sky::Sleet => pick("Sleet", "Aguanieve"),
            Sky::Snow => pick("Snow", "Nieve"),
            Sky::Thunder => pick("Thunderstorms", "Tormentas"),
        }
    }
}

/// El clima de hoy.
#[derive(Clone, Debug, PartialEq)]
pub struct DayWeather {
    pub date: NaiveDate,
    /// Temperatura ahora (°C).
    pub now_c: f32,
    /// Mínima y máxima de lo que queda del día (°C).
    pub min_c: f32,
    pub max_c: f32,
    pub sky: Sky,
    /// ¿Es de noche ahora? (icono de luna en vez de sol).
    pub night: bool,
    /// Lluvia esperada en las próximas horas (mm).
    pub precip_mm: f32,
}

/// Resultado de [`crate::Session::request_weather`] (llega por `Event::Custom`).
#[derive(Debug)]
pub struct WeatherResult {
    pub for_place: Place,
    pub result: Result<DayWeather, Text>,
}

/// Resultado de [`crate::Session::search_places`] (llega por `Event::Custom`).
#[derive(Debug)]
pub struct PlaceResults {
    pub query: String,
    pub result: Result<Vec<Place>, Text>,
}

/// °C → la unidad elegida, redondeada.
pub fn degrees(c: f32, fahrenheit: bool) -> i32 {
    let v = if fahrenheit { c * 9.0 / 5.0 + 32.0 } else { c };
    if v.is_finite() { v.round().clamp(-200.0, 200.0) as i32 } else { 0 }
}

fn num(v: &Value) -> Option<f32> {
    v.as_f64().filter(|x| x.is_finite() && x.abs() < 1e4).map(|x| x as f32)
}

/// El clima de `today` (en `tz`) de una respuesta `compact` de MET Norway.
pub fn parse_met(json: &Value, tz: chrono_tz::Tz, today: NaiveDate) -> Option<DayWeather> {
    let series = json.get("properties")?.get("timeseries")?.as_array()?;
    let mut first: Option<(f32, &Value)> = None;
    let (mut min, mut max) = (f32::INFINITY, f32::NEG_INFINITY);
    for entry in series.iter().take(200) {
        let Some(t) = entry.get("time").and_then(Value::as_str) else { continue };
        let Ok(t) = DateTime::parse_from_rfc3339(t) else { continue };
        if t.with_timezone(&tz).date_naive() != today {
            continue;
        }
        let data = entry.get("data");
        let Some(temp) = data
            .and_then(|d| d.get("instant"))
            .and_then(|i| i.get("details"))
            .and_then(|d| d.get("air_temperature"))
            .and_then(num)
        else {
            continue;
        };
        min = min.min(temp);
        max = max.max(temp);
        if first.is_none()
            && let Some(d) = data
        {
            first = Some((temp, d));
        }
    }
    let (now_c, data) = first?;
    let summary =
        |k: &str| data.get(k).and_then(|n| n.get("summary")).and_then(|s| s.get("symbol_code")).and_then(Value::as_str);
    let code = summary("next_12_hours").or_else(|| summary("next_6_hours")).or_else(|| summary("next_1_hours"))?;
    let code: String = code.chars().take(64).collect();
    let precip = ["next_6_hours", "next_1_hours"]
        .iter()
        .find_map(|k| data.get(*k)?.get("details")?.get("precipitation_amount").and_then(num))
        .unwrap_or(0.0)
        .max(0.0);
    let now_code = summary("next_1_hours").unwrap_or(&code);
    Some(DayWeather {
        date: today,
        now_c,
        min_c: min,
        max_c: max,
        sky: Sky::from_symbol(&code),
        night: now_code.ends_with("_night"),
        precip_mm: precip,
    })
}

fn text(v: Option<&Value>, max: usize) -> String {
    v.and_then(Value::as_str).unwrap_or("").chars().filter(|c| !c.is_control()).take(max).collect()
}

/// Ciudades de una respuesta del geocodificador de Open-Meteo.
pub fn parse_places(json: &Value) -> Vec<Place> {
    let Some(rows) = json.get("results").and_then(Value::as_array) else { return Vec::new() };
    rows.iter()
        .filter_map(|r| {
            let p = Place {
                name: text(r.get("name"), 80),
                admin: text(r.get("admin1"), 80),
                country: text(r.get("country"), 60),
                lat: r.get("latitude")?.as_f64()?,
                lon: r.get("longitude")?.as_f64()?,
            };
            (p.valid() && !p.name.trim().is_empty()).then_some(p)
        })
        .take(MAX_PLACES)
        .collect()
}

/// Ciudad que sugiere la zona horaria («America/Argentina/Buenos_Aires» → «Buenos Aires»).
pub fn guess_city(tz: &str) -> Option<String> {
    if tz.starts_with("Etc/") || !tz.contains('/') {
        return None;
    }
    let city = tz.rsplit('/').next()?.replace('_', " ");
    (!city.trim().is_empty() && city.len() <= 60).then_some(city)
}

/// URL del geocodificador.
pub fn geo_url(base: &str, query: &str, lang: &str) -> String {
    let q: String = query.trim().chars().take(80).collect();
    let mut enc = String::new();
    for b in q.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            enc.push(char::from(b));
        } else {
            enc.push_str(&format!("%{b:02X}"));
        }
    }
    format!("{base}?name={enc}&count={MAX_PLACES}&language={lang}&format=json")
}

/// URL del pronóstico (MET pide como mucho 4 decimales).
pub fn met_url(base: &str, p: &Place) -> String {
    format!("{base}?lat={:.4}&lon={:.4}", p.lat, p.lon)
}

fn client() -> Result<reqwest::Client, Text> {
    reqwest::Client::builder()
        .timeout(TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(3))
        .user_agent(USER_AGENT)
        .build()
        .map_err(|_| unavailable())
}

async fn get_json(client: &reqwest::Client, url: &str) -> Result<Value, Text> {
    let resp = client.get(url).send().await.map_err(|_| unavailable())?;
    if !resp.status().is_success() {
        return Err(unavailable());
    }
    let body = crate::today_bg::body_limited(resp, MAX_JSON).await.ok_or_else(unavailable)?;
    serde_json::from_slice(&body).map_err(|_| unavailable())
}

/// El clima de hoy en `place`.
pub async fn fetch_today(base: &str, place: &Place, tz: chrono_tz::Tz, now: DateTime<Utc>) -> Result<DayWeather, Text> {
    if !place.valid() {
        return Err(unavailable());
    }
    let json = get_json(&client()?, &met_url(base, place)).await?;
    parse_met(&json, tz, now.with_timezone(&tz).date_naive()).ok_or_else(unavailable)
}

/// Busca ciudades por nombre.
pub async fn search_places(base: &str, query: &str, lang: &str) -> Result<Vec<Place>, Text> {
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let json = get_json(&client()?, &geo_url(base, query, lang)).await?;
    Ok(parse_places(&json))
}

fn unavailable() -> Text {
    Text::new("Couldn't get the weather (no connection?).", "No se pudo obtener el clima (¿sin conexión?).")
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn met_sample() -> Value {
        serde_json::json!({"properties": {"timeseries": [
            {"time": "2026-11-09T15:00:00Z", "data": {
                "instant": {"details": {"air_temperature": 16.1}},
                "next_12_hours": {"summary": {"symbol_code": "lightrainshowers_day"}},
                "next_6_hours": {"summary": {"symbol_code": "fair_day"}, "details": {"precipitation_amount": 1.5}},
                "next_1_hours": {"summary": {"symbol_code": "partlycloudy_day"}, "details": {"precipitation_amount": 0.0}}
            }},
            {"time": "2026-11-09T21:00:00Z", "data": {"instant": {"details": {"air_temperature": 22.4}}}},
            {"time": "2026-11-10T04:00:00Z", "data": {"instant": {"details": {"air_temperature": 11.0}}}},
            {"time": "2026-11-10T09:00:00Z", "data": {"instant": {"details": {"air_temperature": 9.0}}}}
        ]}})
    }

    #[test]
    fn today_only_in_the_users_zone() {
        let tz: chrono_tz::Tz = "America/Mexico_City".parse().unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 11, 9).unwrap();
        let w = parse_met(&met_sample(), tz, d).unwrap();
        assert_eq!(w.now_c, 16.1);
        assert_eq!(w.max_c, 22.4);
        // 04:00Z es el 9 a las 22:00 en México; 09:00Z ya es el 10.
        assert_eq!(w.min_c, 11.0);
        assert_eq!(w.sky, Sky::Showers);
        assert!(!w.night);
        assert_eq!(w.precip_mm, 1.5);
        // Del día siguiente solo hay temperaturas sin cielo: sin dato.
        assert!(parse_met(&met_sample(), tz, d.succ_opt().unwrap()).is_none());
        assert!(parse_met(&serde_json::json!({"properties": {}}), tz, d).is_none());
    }

    #[test]
    fn symbols_map_to_skies() {
        assert_eq!(Sky::from_symbol("clearsky_night"), Sky::Clear);
        assert_eq!(Sky::from_symbol("fair_day"), Sky::PartlyCloudy);
        assert_eq!(Sky::from_symbol("cloudy"), Sky::Cloudy);
        assert_eq!(Sky::from_symbol("heavyrainandthunder"), Sky::Thunder);
        assert_eq!(Sky::from_symbol("lightsleetshowers_day"), Sky::Sleet);
        assert_eq!(Sky::from_symbol("snow"), Sky::Snow);
        assert_eq!(Sky::from_symbol("fog"), Sky::Fog);
        assert_eq!(Sky::from_symbol("rain"), Sky::Rain);
    }

    #[test]
    fn places_are_validated_and_capped() {
        let json = serde_json::json!({"results": [
            {"name": "Ciudad de México", "admin1": "Ciudad de México", "country": "México", "latitude": 19.43, "longitude": -99.13},
            {"name": "Nowhere", "latitude": 123.0, "longitude": 0.0},
            {"name": "", "latitude": 1.0, "longitude": 1.0},
            {"name": "Sin coordenadas"}
        ]});
        let p = parse_places(&json);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].full_name(), "Ciudad de México, México");
        assert_eq!(p[0].source_url(), "https://www.yr.no/en/forecast/daily-table/19.4300,-99.1300");
    }

    #[test]
    fn city_from_time_zone_and_encoded_query() {
        assert_eq!(guess_city("America/Mexico_City").as_deref(), Some("Mexico City"));
        assert_eq!(guess_city("America/Argentina/Buenos_Aires").as_deref(), Some("Buenos Aires"));
        assert_eq!(guess_city("Etc/GMT+6"), None);
        assert_eq!(guess_city("UTC"), None);
        assert_eq!(
            geo_url("https://g", "São Paulo&x=1", "es"),
            "https://g?name=S%C3%A3o%20Paulo%26x%3D1&count=8&language=es&format=json"
        );
        assert_eq!(degrees(16.4, false), 16);
        assert_eq!(degrees(20.0, true), 68);
        assert_eq!(degrees(f32::NAN, false), 0);
    }

    proptest! {
        /// Datos hostiles: nunca falla y lo que devuelve es coherente.
        #[test]
        fn hostile_met_json_never_panics(temps in prop::collection::vec(any::<f64>(), 0..30), code in ".{0,80}") {
            let series: Vec<Value> = temps.iter().enumerate().map(|(i, t)| serde_json::json!({
                "time": format!("2026-11-09T{:02}:00:00Z", i % 24),
                "data": {"instant": {"details": {"air_temperature": t}}, "next_1_hours": {"summary": {"symbol_code": code}}}
            })).collect();
            let json = serde_json::json!({"properties": {"timeseries": series}});
            let d = NaiveDate::from_ymd_opt(2026, 11, 9).unwrap();
            if let Some(w) = parse_met(&json, chrono_tz::UTC, d) {
                prop_assert!(w.min_c <= w.now_c && w.now_c <= w.max_c);
            }
            let _ = parse_places(&serde_json::json!({"results": [{"name": code, "latitude": temps.first(), "longitude": temps.last()}]}));
        }
    }
}
