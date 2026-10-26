//! Codificación de registros y claves.
//!
//! Los registros se guardan con postcard (binario compacto, Rust puro) precedidos de un byte de
//! formato. Si un día cambia la forma de un tipo, el byte permite a la migración distinguir los
//! registros viejos de los nuevos.

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::{Result, StoreError};

/// Formato actual de los registros.
const RECORD_FORMAT: u8 = 1;

/// Serializa un registro.
pub(crate) fn encode<T: Serialize>(v: &T) -> Result<Vec<u8>> {
    let mut out = vec![RECORD_FORMAT];
    let body = postcard::to_allocvec(v)?;
    out.extend_from_slice(&body);
    Ok(out)
}

/// Deserializa un registro.
pub(crate) fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    match bytes.split_first() {
        Some((&RECORD_FORMAT, body)) => Ok(postcard::from_bytes(body)?),
        Some((f, _)) => Err(StoreError::Codec(format!("formato de registro desconocido {f}"))),
        None => Err(StoreError::Codec("registro vacío".into())),
    }
}

/// Fecha → `u64` que conserva el orden (milisegundos con el bit de signo invertido).
pub(crate) fn date_key(d: DateTime<Utc>) -> u64 {
    (d.timestamp_millis() as u64) ^ (1 << 63)
}

/// Clave de fecha invertida: el recorrido ascendente da el orden de fecha descendente.
pub(crate) fn inv_date_key(d: DateTime<Utc>) -> u64 {
    !date_key(d)
}

/// Día (desde la época Unix) que contiene un instante.
pub(crate) fn day_of(d: DateTime<Utc>) -> i64 {
    d.timestamp().div_euclid(86_400)
}

/// Recorta una cadena a `max` bytes sin partir un carácter.
pub(crate) fn clip(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    s.get(..end).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    #[test]
    fn date_keys_preserve_order() {
        let a = Utc.with_ymd_and_hms(1960, 1, 1, 0, 0, 0).unwrap();
        let b = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        assert!(date_key(a) < date_key(b));
        assert!(inv_date_key(a) > inv_date_key(b));
    }

    #[test]
    fn clip_respects_chars() {
        assert_eq!(clip("añb", 2), "a");
        assert_eq!(clip("abc", 10), "abc");
    }

    #[test]
    fn decode_rejects_garbage() {
        assert!(decode::<String>(&[]).is_err());
        assert!(decode::<String>(&[9, 1, 2]).is_err());
        assert!(decode::<String>(&[1, 200]).is_err());
        let v = encode(&"hola".to_string()).unwrap();
        assert_eq!(decode::<String>(&v).unwrap(), "hola");
    }
}
