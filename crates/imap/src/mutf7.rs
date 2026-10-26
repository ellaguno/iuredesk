//! UTF-7 modificado de los nombres de buzón (RFC 3501 §5.1.3): `Útil` ↔ `&ANo-til`.

use base64::Engine;
use base64::alphabet::Alphabet;
use base64::engine::{GeneralPurpose, GeneralPurposeConfig};

fn engine() -> Option<GeneralPurpose> {
    let alpha = Alphabet::new("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+,").ok()?;
    let cfg = GeneralPurposeConfig::new()
        .with_encode_padding(false)
        .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent)
        .with_decode_allow_trailing_bits(true);
    Some(GeneralPurpose::new(&alpha, cfg))
}

/// Codifica un nombre Unicode en UTF-7 modificado.
pub fn encode(name: &str) -> String {
    let Some(eng) = engine() else { return name.to_string() };
    let mut out = String::with_capacity(name.len());
    let mut pending: Vec<u16> = Vec::new();
    let flush = |pending: &mut Vec<u16>, out: &mut String| {
        if pending.is_empty() {
            return;
        }
        let bytes: Vec<u8> = pending.iter().flat_map(|u| u.to_be_bytes()).collect();
        out.push('&');
        out.push_str(&eng.encode(bytes));
        out.push('-');
        pending.clear();
    };
    for c in name.chars() {
        if (' '..='~').contains(&c) {
            flush(&mut pending, &mut out);
            if c == '&' {
                out.push_str("&-");
            } else {
                out.push(c);
            }
        } else {
            let mut buf = [0u16; 2];
            pending.extend_from_slice(c.encode_utf16(&mut buf));
        }
    }
    flush(&mut pending, &mut out);
    out
}

/// Decodifica UTF-7 modificado; lo que no sea válido se deja tal cual (con pérdida si no es
/// UTF-8). Nunca falla.
pub fn decode(raw: &[u8]) -> String {
    let s = String::from_utf8_lossy(raw);
    if !s.contains('&') {
        return s.into_owned();
    }
    let Some(eng) = engine() else { return s.into_owned() };
    let mut out = String::with_capacity(s.len());
    let mut rest: &str = &s;
    // Cada vuelta consume al menos un carácter: el bucle termina.
    while let Some(amp) = rest.find('&') {
        out.push_str(rest.get(..amp).unwrap_or(""));
        let after = rest.get(amp.saturating_add(1)..).unwrap_or("");
        match after.find('-') {
            Some(0) => {
                out.push('&');
                rest = after.get(1..).unwrap_or("");
            }
            Some(end) => {
                let chunk = after.get(..end).unwrap_or("");
                match eng.decode(chunk) {
                    Ok(bytes) if bytes.len() % 2 == 0 => {
                        let units: Vec<u16> = bytes
                            .chunks_exact(2)
                            .filter_map(|p| match p {
                                [a, b] => Some(u16::from_be_bytes([*a, *b])),
                                _ => None,
                            })
                            .collect();
                        out.push_str(&String::from_utf16_lossy(&units));
                    }
                    _ => {
                        out.push('&');
                        out.push_str(chunk);
                        out.push('-');
                    }
                }
                rest = after.get(end.saturating_add(1)..).unwrap_or("");
            }
            None => {
                out.push('&');
                out.push_str(after);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors() {
        assert_eq!(encode("INBOX"), "INBOX");
        assert_eq!(encode("Útil"), "&ANo-til");
        assert_eq!(encode("A&B"), "A&-B");
        assert_eq!(encode("台北"), "&U,BTFw-");
        assert_eq!(decode(b"&U,BTFw-"), "台北");
        assert_eq!(decode(b"Entw&APw-rfe"), "Entwürfe");
        assert_eq!(decode(b"A&-B"), "A&B");
        assert_eq!(decode(b"roto&"), "roto&");
        assert_eq!(decode(b"&%%%-x"), "&%%%-x");
    }

    proptest::proptest! {
        #[test]
        fn roundtrip(s in "\\PC{0,40}") {
            proptest::prop_assert_eq!(decode(encode(&s).as_bytes()), s);
        }

        #[test]
        fn decode_never_panics(b in proptest::collection::vec(proptest::num::u8::ANY, 0..80)) {
            let _ = decode(&b);
        }
    }
}
