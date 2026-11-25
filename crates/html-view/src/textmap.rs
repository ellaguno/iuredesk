//! UX-22: el texto pintado, carácter a carácter con su caja, para seleccionar y copiar sobre la
//! imagen del correo (lo pidió el dueño 2026-11-25: «debe permitir seleccionar texto»). Se arma
//! una vez al pintar (de los layouts de `parley` que deja Blitz); la UI solo pregunta qué carácter
//! hay bajo el puntero, qué texto hay entre dos posiciones y qué rectángulos resaltar.
//!
//! Las posiciones son *cursores*: `0..=len()`, entre caracteres.

use crate::Rect;

/// Un grupo de glifos (un carácter visible, una ligadura) en px físicos.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextGlyph {
    pub rect: Rect,
    /// Su texto en [`TextMap::text`] (bytes).
    pub at: u32,
    pub len: u16,
    /// Línea (índice en [`TextMap::lines`]).
    pub line: u32,
}

/// Una línea pintada: su caja y sus glifos (`start..end`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextLine {
    pub rect: Rect,
    pub start: u32,
    pub end: u32,
}

/// El texto pintado de un documento, en orden de lectura.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TextMap {
    pub text: String,
    pub glyphs: Vec<TextGlyph>,
    pub lines: Vec<TextLine>,
}

/// Glifos como mucho (un boletín enorme no llena la memoria).
pub const MAX_GLYPHS: usize = 400_000;

impl TextMap {
    /// Número de glifos (los cursores van de 0 a este valor).
    pub fn len(&self) -> usize {
        self.glyphs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.glyphs.is_empty()
    }

    /// Empieza una línea nueva (los glifos que se agreguen hasta [`Self::end_line`] van en ella).
    pub(crate) fn push_glyph(&mut self, rect: Rect, text: &str) -> bool {
        if self.glyphs.len() >= MAX_GLYPHS || text.len() > usize::from(u16::MAX) {
            return false;
        }
        let Ok(at) = u32::try_from(self.text.len()) else { return false };
        let Ok(line) = u32::try_from(self.lines.len()) else { return false };
        self.text.push_str(text);
        self.glyphs.push(TextGlyph { rect, at, len: text.len() as u16, line });
        true
    }

    /// Cierra la línea de los glifos agregados desde `start`.
    pub(crate) fn end_line(&mut self, start: usize) {
        let end = self.glyphs.len();
        let Some(first) = self.glyphs.get(start) else { return };
        let mut r = first.rect;
        for g in self.glyphs.get(start..end).unwrap_or(&[]) {
            let right = (r.x + r.width).max(g.rect.x + g.rect.width);
            let bottom = (r.y + r.height).max(g.rect.y + g.rect.height);
            r.x = r.x.min(g.rect.x);
            r.y = r.y.min(g.rect.y);
            r.width = right - r.x;
            r.height = bottom - r.y;
        }
        let (Ok(s), Ok(e)) = (u32::try_from(start), u32::try_from(end)) else { return };
        self.lines.push(TextLine { rect: r, start: s, end: e });
    }

    fn glyph_text(&self, g: &TextGlyph) -> &str {
        let a = g.at as usize;
        self.text.get(a..a + usize::from(g.len)).unwrap_or("")
    }

    fn line_range(&self, l: &TextLine) -> (usize, usize) {
        let n = self.glyphs.len();
        ((l.start as usize).min(n), (l.end as usize).min(n))
    }

    /// El cursor más cercano al punto (px físicos): en la línea bajo él (o la más cercana; en
    /// tablas, la de la columna), antes o después del carácter según la mitad en que caiga.
    pub fn caret_at(&self, x: f32, y: f32) -> Option<usize> {
        if !x.is_finite() || !y.is_finite() {
            return None;
        }
        let dist = |lo: f32, len: f32, v: f32| {
            if v < lo {
                lo - v
            } else if v > lo + len {
                v - (lo + len)
            } else {
                0.0
            }
        };
        let line = self.lines.iter().min_by(|a, b| {
            let ka = (dist(a.rect.y, a.rect.height, y), dist(a.rect.x, a.rect.width, x));
            let kb = (dist(b.rect.y, b.rect.height, y), dist(b.rect.x, b.rect.width, x));
            ka.0.total_cmp(&kb.0).then(ka.1.total_cmp(&kb.1))
        })?;
        let (start, end) = self.line_range(line);
        for i in start..end {
            let g = self.glyphs.get(i)?;
            if x < g.rect.x + g.rect.width / 2.0 {
                return Some(i);
            }
        }
        Some(end)
    }

    /// El texto entre dos cursores (en cualquier orden); un salto de línea entre líneas.
    pub fn text_between(&self, a: usize, b: usize) -> String {
        let (a, b) = (a.min(b).min(self.len()), a.max(b).min(self.len()));
        let mut out = String::new();
        let mut last_line: Option<u32> = None;
        for g in self.glyphs.get(a..b).unwrap_or(&[]) {
            if last_line.is_some_and(|l| l != g.line) {
                // Sin espacios colgando al final de la línea.
                let trimmed = out.trim_end_matches([' ', '\u{a0}']).len();
                out.truncate(trimmed);
                out.push('\n');
            }
            last_line = Some(g.line);
            let t = self.glyph_text(g);
            out.push_str(if t == "\u{a0}" { " " } else { t });
        }
        out
    }

    /// Rectángulos que resaltar entre dos cursores: uno por línea.
    pub fn selection_rects(&self, a: usize, b: usize) -> Vec<Rect> {
        let (a, b) = (a.min(b), a.max(b));
        let mut out = Vec::new();
        if a == b {
            return out;
        }
        for l in &self.lines {
            let (start, end) = self.line_range(l);
            let (s, e) = (a.max(start), b.min(end));
            if s >= e {
                continue;
            }
            let (Some(first), Some(last)) = (self.glyphs.get(s), self.glyphs.get(e - 1)) else { continue };
            let x0 = first.rect.x.min(last.rect.x);
            let x1 = (last.rect.x + last.rect.width).max(first.rect.x + first.rect.width);
            out.push(Rect { x: x0, y: l.rect.y, width: (x1 - x0).max(1.0), height: l.rect.height });
        }
        out
    }

    /// La palabra alrededor del cursor (doble clic): letras, números y `_-'.@` seguidos, en la
    /// misma línea. Sobre un espacio, solo ese carácter.
    pub fn word_at(&self, caret: usize) -> (usize, usize) {
        let word = |i: usize| {
            self.glyphs.get(i).is_some_and(|g| {
                let t = self.glyph_text(g);
                !t.is_empty() && t.chars().all(|c| c.is_alphanumeric() || "_-'’.@".contains(c))
            })
        };
        let line_of = |i: usize| self.glyphs.get(i).map(|g| g.line);
        let i = caret.min(self.len().saturating_sub(1));
        if !word(i) {
            return (i, (i + 1).min(self.len()));
        }
        let mut s = i;
        while s > 0 && word(s - 1) && line_of(s - 1) == line_of(i) {
            s -= 1;
        }
        let mut e = i + 1;
        while e < self.len() && word(e) && line_of(e) == line_of(i) {
            e += 1;
        }
        (s, e)
    }

    /// La línea del cursor (triple clic).
    pub fn line_at(&self, caret: usize) -> (usize, usize) {
        let i = caret.min(self.len().saturating_sub(1));
        self.glyphs.get(i).and_then(|g| self.lines.get(g.line as usize)).map_or((i, i), |l| self.line_range(l))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Dos líneas: «Hola mundo» (y=0) y «adiós» (y=20); 10 px por carácter.
    fn sample() -> TextMap {
        let mut m = TextMap::default();
        for (row, text) in ["Hola mundo", "adiós"].iter().enumerate() {
            let start = m.len();
            for (k, ch) in text.chars().enumerate() {
                let r = Rect { x: k as f32 * 10.0, y: row as f32 * 20.0, width: 10.0, height: 18.0 };
                m.push_glyph(r, &ch.to_string());
            }
            m.end_line(start);
        }
        m
    }

    #[test]
    fn caret_text_and_rects() {
        let m = sample();
        assert_eq!(m.len(), 15);
        assert_eq!(m.caret_at(2.0, 5.0), Some(0), "mitad izquierda: antes");
        assert_eq!(m.caret_at(8.0, 5.0), Some(1), "mitad derecha: después");
        assert_eq!(m.caret_at(500.0, 5.0), Some(10), "más allá del final: al final de la línea");
        assert_eq!(m.caret_at(15.0, 100.0), Some(12), "debajo de todo: la última línea");
        assert_eq!(m.text_between(5, 13), "mundo\nadi");
        assert_eq!(m.text_between(13, 5), "mundo\nadi", "en cualquier orden");
        assert_eq!(m.text_between(0, 99), "Hola mundo\nadiós");
        let r = m.selection_rects(5, 13);
        assert_eq!(r.len(), 2);
        assert_eq!((r[0].x, r[0].width), (50.0, 50.0));
        assert_eq!((r[1].x, r[1].width, r[1].y), (0.0, 30.0, 20.0));
        assert!(m.selection_rects(4, 4).is_empty());
    }

    #[test]
    fn word_and_line() {
        let m = sample();
        assert_eq!(m.word_at(6), (5, 10), "«mundo»");
        assert_eq!(m.word_at(4), (4, 5), "el espacio solo");
        assert_eq!(m.word_at(12), (10, 15), "«adiós», sin cruzar a la línea de arriba");
        assert_eq!(m.line_at(12), (10, 15));
        assert_eq!(TextMap::default().caret_at(1.0, 1.0), None);
    }

    proptest! {
        /// Cualquier punto y cualquier par de cursores: sin pánico y dentro de rango.
        #[test]
        fn any_point_and_range(x in -1e6f32..1e6, y in -1e6f32..1e6, a in 0usize..40, b in 0usize..40) {
            let m = sample();
            if let Some(c) = m.caret_at(x, y) {
                prop_assert!(c <= m.len());
            }
            let t = m.text_between(a, b);
            prop_assert!(t.chars().count() <= m.len() + 2);
            prop_assert!(m.selection_rects(a, b).len() <= 2);
            let (s, e) = m.word_at(a);
            prop_assert!(s <= e && e <= m.len());
            let _ = m.line_at(b);
            prop_assert!(m.caret_at(f32::NAN, y).is_none());
        }
    }
}
