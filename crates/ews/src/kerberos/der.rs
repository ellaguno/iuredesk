//! DER mínimo para Kerberos (RFC 4120 usa ASN.1 con etiquetas explícitas): lo justo para armar
//! peticiones y leer respuestas hostiles. Etiquetas de un byte (Kerberos no pasa de la 30),
//! longitudes de hasta 4 bytes acotadas por la entrada y listas acotadas.

use super::KerberosError;

/// Tope de elementos de una secuencia.
const MAX_CHILDREN: usize = 256;

// ------------------------------------------------------------------ escritura

/// TLV con la etiqueta y el contenido dados.
pub fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    let n = content.len();
    if n < 0x80 {
        out.push(n as u8);
    } else {
        let bytes: Vec<u8> = (n as u64).to_be_bytes().into_iter().skip_while(|b| *b == 0).collect();
        out.push(0x80 | bytes.len() as u8);
        out.extend_from_slice(&bytes);
    }
    out.extend_from_slice(content);
    out
}

/// `SEQUENCE`.
pub fn seq(items: &[Vec<u8>]) -> Vec<u8> {
    tlv(0x30, &items.concat())
}

/// `[n]` explícita (de contexto, construida).
pub fn ctx(n: u8, inner: Vec<u8>) -> Vec<u8> {
    tlv(0xA0 | (n & 0x1F), &inner)
}

/// `[APPLICATION n]` (construida).
pub fn app(n: u8, inner: Vec<u8>) -> Vec<u8> {
    tlv(0x60 | (n & 0x1F), &inner)
}

/// `INTEGER` en complemento a dos mínimo.
pub fn int(v: i64) -> Vec<u8> {
    let b = v.to_be_bytes();
    let mut start = 0;
    while start < 7 {
        let (cur, next) = (b[start], b[start + 1]);
        if (cur == 0 && next & 0x80 == 0) || (cur == 0xFF && next & 0x80 != 0) {
            start += 1;
        } else {
            break;
        }
    }
    tlv(0x02, &b[start..])
}

/// `OCTET STRING`.
pub fn octets(b: &[u8]) -> Vec<u8> {
    tlv(0x04, b)
}

/// `GeneralString` (KerberosString).
pub fn gstr(s: &str) -> Vec<u8> {
    tlv(0x1B, s.as_bytes())
}

/// `GeneralizedTime` (KerberosTime, `AAAAMMDDHHMMSSZ`).
pub fn gtime(s: &str) -> Vec<u8> {
    tlv(0x18, s.as_bytes())
}

/// `BIT STRING` de 32 bits (KDCOptions, APOptions).
pub fn bits32(v: u32) -> Vec<u8> {
    let mut c = vec![0u8];
    c.extend_from_slice(&v.to_be_bytes());
    tlv(0x03, &c)
}

/// `BOOLEAN`.
pub fn boolean(v: bool) -> Vec<u8> {
    tlv(0x01, &[if v { 0xFF } else { 0 }])
}

// ------------------------------------------------------------------ lectura

/// Un elemento leído.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tlv<'a> {
    pub tag: u8,
    pub body: &'a [u8],
    /// El elemento completo (etiqueta, longitud y contenido).
    pub raw: &'a [u8],
}

fn bad(what: &str) -> KerberosError {
    KerberosError::Protocol(format!("DER: {what}"))
}

/// Lee un elemento del principio de `b`; devuelve el elemento y lo que sigue.
pub fn read(b: &[u8]) -> Result<(Tlv<'_>, &[u8]), KerberosError> {
    let tag = *b.first().ok_or_else(|| bad("vacío"))?;
    if tag & 0x1F == 0x1F {
        return Err(bad("etiqueta larga"));
    }
    let first = *b.get(1).ok_or_else(|| bad("sin longitud"))?;
    let (len, head) = if first < 0x80 {
        (usize::from(first), 2usize)
    } else {
        let n = usize::from(first & 0x7F);
        if n == 0 || n > 4 {
            return Err(bad("longitud no válida"));
        }
        let bytes = b.get(2..2 + n).ok_or_else(|| bad("longitud cortada"))?;
        let len = bytes.iter().fold(0usize, |acc, x| (acc << 8) | usize::from(*x));
        (len, 2 + n)
    };
    let end = head.checked_add(len).ok_or_else(|| bad("longitud enorme"))?;
    let body = b.get(head..end).ok_or_else(|| bad("contenido cortado"))?;
    let raw = b.get(..end).ok_or_else(|| bad("contenido cortado"))?;
    let rest = b.get(end..).unwrap_or_default();
    Ok((Tlv { tag, body, raw }, rest))
}

/// Lee un elemento que debe ocupar todo `b` y tener la etiqueta `tag`.
pub fn expect(b: &[u8], tag: u8) -> Result<Tlv<'_>, KerberosError> {
    let (t, _) = read(b)?;
    if t.tag != tag {
        return Err(bad(&format!("se esperaba la etiqueta {tag:#x} y llegó {:#x}", t.tag)));
    }
    Ok(t)
}

/// Los elementos dentro de un contenido construido.
pub fn children(mut body: &[u8]) -> Result<Vec<Tlv<'_>>, KerberosError> {
    let mut out = Vec::new();
    while !body.is_empty() {
        if out.len() >= MAX_CHILDREN {
            return Err(bad("demasiados elementos"));
        }
        let (t, rest) = read(body)?;
        out.push(t);
        body = rest;
    }
    Ok(out)
}

/// Los campos de una `SEQUENCE` (o de una etiqueta de aplicación que la envuelve).
pub fn fields(t: Tlv<'_>) -> Result<Vec<Tlv<'_>>, KerberosError> {
    let inner = if t.tag & 0x60 == 0x60 && t.tag != 0x30 {
        // [APPLICATION n] SEQUENCE {…}
        let (s, _) = read(t.body)?;
        if s.tag != 0x30 {
            return Err(bad("se esperaba SEQUENCE"));
        }
        s
    } else if t.tag == 0x30 {
        t
    } else {
        return Err(bad("se esperaba SEQUENCE"));
    };
    children(inner.body)
}

/// El contenido del campo `[n]` (explícito) de una secuencia ya leída.
pub fn field<'a>(fields: &[Tlv<'a>], n: u8) -> Option<Tlv<'a>> {
    let f = fields.iter().find(|f| f.tag == 0xA0 | n)?;
    read(f.body).ok().map(|(t, _)| t)
}

/// Como [`field`], pero obligatorio.
pub fn need<'a>(fields: &[Tlv<'a>], n: u8, what: &str) -> Result<Tlv<'a>, KerberosError> {
    field(fields, n).ok_or_else(|| bad(&format!("falta {what}")))
}

impl<'a> Tlv<'a> {
    /// `INTEGER` que cabe en `i64`.
    pub fn int(&self) -> Result<i64, KerberosError> {
        if self.tag != 0x02 || self.body.is_empty() || self.body.len() > 8 {
            return Err(bad("INTEGER no válido"));
        }
        let neg = self.body.first().is_some_and(|b| b & 0x80 != 0);
        let mut v: i64 = if neg { -1 } else { 0 };
        for b in self.body {
            v = (v << 8) | i64::from(*b);
        }
        Ok(v)
    }

    /// `OCTET STRING`.
    pub fn octets(&self) -> Result<&'a [u8], KerberosError> {
        if self.tag != 0x04 {
            return Err(bad("se esperaba OCTET STRING"));
        }
        Ok(self.body)
    }

    /// Cadena (`GeneralString`, `UTF8String`, `IA5String`, `GeneralizedTime`), acotada.
    pub fn string(&self) -> Result<String, KerberosError> {
        if !matches!(self.tag, 0x1B | 0x0C | 0x16 | 0x18) || self.body.len() > 1024 {
            return Err(bad("cadena no válida"));
        }
        Ok(String::from_utf8_lossy(self.body).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integers_are_minimal() {
        assert_eq!(int(5), [2, 1, 5]);
        assert_eq!(int(128), [2, 2, 0, 128]);
        assert_eq!(int(-1), [2, 1, 0xFF]);
        assert_eq!(int(-129), [2, 2, 0xFF, 0x7F]);
        assert_eq!(int(0x8003), [2, 3, 0, 0x80, 0x03]);
        for v in [0i64, 1, -1, 127, 128, -128, -129, 0x7FFF_FFFF, -0x8000_0000, i64::MAX, i64::MIN] {
            let e = int(v);
            assert_eq!(expect(&e, 2).unwrap().int().unwrap(), v);
        }
    }

    #[test]
    fn long_lengths_round_trip() {
        let big = vec![7u8; 300];
        let e = octets(&big);
        assert_eq!(&e[..4], &[0x04, 0x82, 0x01, 0x2C]);
        assert_eq!(expect(&e, 0x04).unwrap().octets().unwrap(), &big[..]);
    }

    #[test]
    fn sequences_and_fields() {
        let s = app(10, seq(&[ctx(1, int(5)), ctx(2, int(10)), ctx(4, gstr("CONTOSO.COM"))]));
        let (t, rest) = read(&s).unwrap();
        assert!(rest.is_empty());
        let f = fields(t).unwrap();
        assert_eq!(need(&f, 1, "pvno").unwrap().int().unwrap(), 5);
        assert_eq!(field(&f, 4).unwrap().string().unwrap(), "CONTOSO.COM");
        assert!(field(&f, 3).is_none());
    }

    #[test]
    fn hostile_input_is_an_error() {
        for b in [
            &[][..],
            &[0x30],
            &[0x30, 0x85, 1, 1, 1, 1, 1],
            &[0x30, 0x05, 1],
            &[0x1F, 1, 0],
            &[0x30, 0x84, 0xFF, 0xFF, 0xFF, 0xFF],
        ] {
            assert!(read(b).is_err(), "{b:?}");
        }
        assert!(expect(&int(1), 0x04).is_err());
        assert!(tlv(0x02, &[0; 9]).len() == 11 && expect(&tlv(0x02, &[0; 9]), 2).unwrap().int().is_err());
    }
}
