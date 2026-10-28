//! Cabecera `Autocrypt` (Autocrypt 1.1, nivel 1): enviar la clave propia en cada mensaje y
//! aprender las claves de los demás sin servidores.
//!
//! `Autocrypt: addr=ana@x.com; prefer-encrypt=mutual; keydata=<base64 de la clave binaria>`

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

/// Tamaño máximo de `keydata` (decodificado) que se acepta.
const MAX_KEYDATA: usize = 64 * 1024;

/// Preferencia de cifrado del remitente.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
pub enum PreferEncrypt {
    Mutual,
    #[default]
    NoPreference,
}

/// Una cabecera `Autocrypt` interpretada.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutocryptHeader {
    pub addr: String,
    pub prefer_encrypt: PreferEncrypt,
    /// Clave pública en binario.
    pub keydata: Vec<u8>,
}

impl AutocryptHeader {
    /// Interpreta el valor de la cabecera (desplegado o no). `None` si no es válida: falta `addr`
    /// o `keydata`, o trae un atributo crítico desconocido (los que no empiezan con `_`).
    pub fn parse(value: &str) -> Option<Self> {
        if value.len() > MAX_KEYDATA * 2 {
            return None;
        }
        let mut addr = None;
        let mut prefer = PreferEncrypt::NoPreference;
        let mut keydata = None;
        for attr in value.split(';').take(32) {
            let (k, v) = attr.split_once('=')?;
            let k = k.trim().to_ascii_lowercase();
            match k.as_str() {
                "addr" => addr = Some(v.trim().to_ascii_lowercase()),
                "prefer-encrypt" => {
                    if v.trim().eq_ignore_ascii_case("mutual") {
                        prefer = PreferEncrypt::Mutual;
                    }
                }
                "keydata" => {
                    let clean: String = v.chars().filter(|c| !c.is_ascii_whitespace()).collect();
                    let bytes = STANDARD.decode(clean.as_bytes()).ok()?;
                    if bytes.is_empty() || bytes.len() > MAX_KEYDATA {
                        return None;
                    }
                    keydata = Some(bytes);
                }
                k if k.starts_with('_') => {}
                _ => return None,
            }
        }
        let addr = addr.filter(|a| a.contains('@'))?;
        Some(AutocryptHeader { addr, prefer_encrypt: prefer, keydata: keydata? })
    }

    /// Valor de la cabecera, plegado en líneas de 78 columnas (incluye el nombre del campo para
    /// calcular el primer pliegue, pero no lo devuelve).
    pub fn to_value(&self) -> String {
        let mut v = format!("addr={}; ", self.addr);
        if self.prefer_encrypt == PreferEncrypt::Mutual {
            v.push_str("prefer-encrypt=mutual; ");
        }
        v.push_str("keydata=");
        let b64 = STANDARD.encode(&self.keydata);
        let mut out = v;
        for chunk in b64.as_bytes().chunks(72) {
            out.push_str("\r\n ");
            out.push_str(&String::from_utf8_lossy(chunk));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_rules() {
        let h =
            AutocryptHeader { addr: "ana@x.com".into(), prefer_encrypt: PreferEncrypt::Mutual, keydata: vec![7; 300] };
        let v = h.to_value();
        assert!(v.lines().all(|l| l.len() <= 78));
        // Desplegado como lo entrega el lector de cabeceras.
        let unfolded = v.replace("\r\n ", " ");
        assert_eq!(AutocryptHeader::parse(&unfolded), Some(h.clone()));
        assert_eq!(AutocryptHeader::parse(&v), Some(h));
        // Atributo no crítico: se ignora; crítico desconocido: la cabecera no vale.
        assert!(AutocryptHeader::parse("addr=a@b.c; _x=1; keydata=AAAA").is_some());
        assert!(AutocryptHeader::parse("addr=a@b.c; type=2; keydata=AAAA").is_none());
        assert!(AutocryptHeader::parse("addr=a@b.c").is_none());
        assert!(AutocryptHeader::parse("keydata=AAAA").is_none());
        assert!(AutocryptHeader::parse("addr=a@b.c; keydata=%%%").is_none());
        assert_eq!(
            AutocryptHeader::parse("addr=A@B.C; keydata=AAAA").map(|h| (h.addr, h.prefer_encrypt)),
            Some(("a@b.c".into(), PreferEncrypt::NoPreference))
        );
    }

    proptest::proptest! {
        #[test]
        fn never_panics(s in ".{0,400}") {
            let _ = AutocryptHeader::parse(&s);
        }
    }
}
