//! Conjuntos de secuencia / UID (`1:5,7,9:*`).

use std::fmt;

/// Valor que representa `*` (el mayor número en uso).
pub const STAR: u32 = u32::MAX;
/// Rangos máximos que se aceptan al interpretar un conjunto del servidor.
const MAX_RANGES: usize = 1_000_000;

/// Conjunto de números como lista de rangos cerrados `(inicio, fin)`; `fin == STAR` es `*`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SeqSet(Vec<(u32, u32)>);

impl SeqSet {
    /// Conjunto vacío.
    pub fn new() -> Self {
        Self(Vec::new())
    }

    /// Un solo número.
    pub fn single(n: u32) -> Self {
        Self(vec![(n, n)])
    }

    /// Rango `a:b` (se ordena).
    pub fn range(a: u32, b: u32) -> Self {
        Self(vec![(a.min(b), a.max(b))])
    }

    /// `a:*`.
    pub fn from_to_star(a: u32) -> Self {
        Self(vec![(a, STAR)])
    }

    /// Conjunto compacto a partir de números sueltos (ordena, quita duplicados y une rangos).
    pub fn from_numbers(nums: impl IntoIterator<Item = u32>) -> Self {
        let mut v: Vec<u32> = nums.into_iter().filter(|n| *n > 0).collect();
        v.sort_unstable();
        v.dedup();
        let mut out: Vec<(u32, u32)> = Vec::new();
        for n in v {
            match out.last_mut() {
                Some((_, end)) if end.checked_add(1) == Some(n) => *end = n,
                _ => out.push((n, n)),
            }
        }
        Self(out)
    }

    /// Rangos.
    pub fn ranges(&self) -> &[(u32, u32)] {
        &self.0
    }

    /// `true` si está vacío.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// ¿Contiene `n`? (`*` se trata como infinito).
    pub fn contains(&self, n: u32) -> bool {
        self.0.iter().any(|&(a, b)| a.min(b) <= n && n <= a.max(b))
    }

    /// Números del conjunto, como mucho `max`; `*` se sustituye por `star`.
    pub fn to_vec(&self, star: u32, max: usize) -> Vec<u32> {
        let mut out = Vec::new();
        for &(a, b) in &self.0 {
            let a = if a == STAR { star } else { a };
            let b = if b == STAR { star } else { b };
            let (lo, hi) = (a.min(b), a.max(b));
            let mut n = lo;
            loop {
                if out.len() >= max {
                    return out;
                }
                if n > 0 {
                    out.push(n);
                }
                if n >= hi {
                    break;
                }
                n = n.saturating_add(1);
            }
        }
        out
    }

    /// Cuántos números contiene (sin expandir; `*` cuenta como uno).
    pub fn len(&self) -> u64 {
        self.0
            .iter()
            .map(|&(a, b)| {
                if a == STAR || b == STAR {
                    1
                } else {
                    u64::from(a.max(b)).saturating_sub(u64::from(a.min(b))).saturating_add(1)
                }
            })
            .fold(0u64, u64::saturating_add)
    }

    /// Interpreta `1:5,7,9:*`. Devuelve `None` si no es válido.
    pub fn parse(s: &str) -> Option<Self> {
        if s.is_empty() || s.len() > 64 * 1024 * 1024 {
            return None;
        }
        let mut out = Vec::new();
        for part in s.split(',') {
            if out.len() >= MAX_RANGES {
                return None;
            }
            let num = |t: &str| -> Option<u32> {
                if t == "*" {
                    Some(STAR)
                } else if !t.is_empty() && t.len() <= 10 && t.bytes().all(|b| b.is_ascii_digit()) {
                    t.parse::<u32>().ok().filter(|n| *n > 0)
                } else {
                    None
                }
            };
            match part.split_once(':') {
                Some((a, b)) => {
                    let (a, b) = (num(a)?, num(b)?);
                    out.push((a.min(b), a.max(b)));
                }
                None => {
                    let n = num(part)?;
                    out.push((n, n));
                }
            }
        }
        Some(Self(out))
    }

    /// Divide en trozos de como mucho `n` números (para pedir por lotes). `*` no se divide.
    pub fn chunks(nums: &[u32], n: usize) -> Vec<SeqSet> {
        nums.chunks(n.max(1)).map(|c| SeqSet::from_numbers(c.iter().copied())).collect()
    }
}

impl fmt::Display for SeqSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let item = |n: u32| if n == STAR { "*".to_string() } else { n.to_string() };
        for (i, &(a, b)) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(",")?;
            }
            if a == b {
                f.write_str(&item(a))?;
            } else {
                write!(f, "{}:{}", item(a), item(b))?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compacts_and_formats() {
        let s = SeqSet::from_numbers([5, 1, 2, 3, 9, 3, 10]);
        assert_eq!(s.to_string(), "1:3,5,9:10");
        assert_eq!(s.len(), 6);
        assert!(s.contains(2) && !s.contains(4));
        assert_eq!(SeqSet::from_to_star(7).to_string(), "7:*");
    }

    #[test]
    fn parses() {
        let s = SeqSet::parse("1:3,7,10:*").unwrap();
        assert_eq!(s.to_vec(12, 100), vec![1, 2, 3, 7, 10, 11, 12]);
        assert_eq!(SeqSet::parse("5:2").unwrap().to_vec(0, 10), vec![2, 3, 4, 5]);
        assert!(SeqSet::parse("").is_none());
        assert!(SeqSet::parse("a").is_none());
        assert!(SeqSet::parse("0").is_none());
        assert!(SeqSet::parse("1:99999999999").is_none());
        assert_eq!(SeqSet::parse("1:4294967294").unwrap().to_vec(0, 3), vec![1, 2, 3]);
    }

    proptest::proptest! {
        #[test]
        fn roundtrip(v in proptest::collection::vec(1u32..100_000, 0..200)) {
            let s = SeqSet::from_numbers(v.iter().copied());
            let mut sorted = v.clone();
            sorted.sort_unstable();
            sorted.dedup();
            if !s.is_empty() {
                let p = SeqSet::parse(&s.to_string()).unwrap();
                proptest::prop_assert_eq!(p.to_vec(0, 1_000), sorted);
            }
        }

        #[test]
        fn parse_never_panics(s in ".{0,60}") {
            let _ = SeqSet::parse(&s).map(|x| x.to_vec(10, 100));
        }
    }
}
