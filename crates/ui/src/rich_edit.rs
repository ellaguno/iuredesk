//! Editor de texto enriquecido sencillo sobre `egui::TextEdit`.
//!
//! El documento es texto plano con un estilo por carácter ([`RichDoc`]) y marcas de bloque al
//! principio de cada línea, visibles y fáciles de entender: `• ` (viñeta), `1. ` (lista numerada),
//! `> ` (cita) y la línea `-- ` que abre la firma. Así el `TextEdit` de egui edita el texto (con
//! deshacer, selección, IME y accesibilidad) y un *layouter* pinta los estilos.
//!
//! [`RichDoc::to_rich`] y [`RichDoc::from_rich`] convierten a y desde el [`RichBody`] del
//! redactor (que luego se serializa a HTML apto para correo y a texto `format=flowed`).

use std::sync::Arc;

use egui::text::{CCursor, CCursorRange, LayoutJob};
use egui::{Color32, FontId, Key, Modifiers, Stroke, TextFormat, Ui};
use iuredesk_engine::iuredesk_compose::{Block, Inline, RichBody, Span, Style};

/// Profundidad máxima de citas que se reconocen al convertir.
const MAX_DEPTH: usize = 12;
/// Caracteres máximos del documento (los correos más largos se recortan al importar).
const MAX_CHARS: usize = 400_000;
/// Viñeta.
pub const BULLET: &str = "• ";
/// Separador de firma.
pub const SIG: &str = "-- ";

/// Estilo de un carácter.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CharStyle {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub code: bool,
    pub link: Option<Arc<str>>,
}

impl CharStyle {
    fn from_span(s: &Span) -> Self {
        CharStyle {
            bold: s.style.bold,
            italic: s.style.italic,
            underline: s.style.underline,
            strike: s.style.strike,
            code: s.style.code,
            link: s.link.as_deref().map(Arc::from),
        }
    }

    fn style(&self) -> Style {
        Style { bold: self.bold, italic: self.italic, underline: self.underline, strike: self.strike, code: self.code }
    }
}

/// Estilo que se puede alternar desde la barra.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Toggle {
    Bold,
    Italic,
    Underline,
}

impl Toggle {
    fn get(self, s: &CharStyle) -> bool {
        match self {
            Toggle::Bold => s.bold,
            Toggle::Italic => s.italic,
            Toggle::Underline => s.underline,
        }
    }
    fn set(self, s: &mut CharStyle, v: bool) {
        match self {
            Toggle::Bold => s.bold = v,
            Toggle::Italic => s.italic = v,
            Toggle::Underline => s.underline = v,
        }
    }
}

/// Marca de bloque de una línea.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Bullet,
    Numbered,
    Quote,
}

/// El documento del editor.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RichDoc {
    pub text: String,
    /// Un estilo por carácter de `text`.
    pub styles: Vec<CharStyle>,
    /// Estilo de lo que se escriba (lo cambian los botones sin selección).
    pub typing: Option<CharStyle>,
}

fn byte_at(s: &str, ch: usize) -> usize {
    s.char_indices().nth(ch).map_or(s.len(), |(b, _)| b)
}

/// Longitud de la marca de bloque al principio de `line` (en caracteres) y su tipo.
pub fn line_marker(line: &str) -> Option<(LineKind, usize)> {
    if line.starts_with(BULLET) {
        return Some((LineKind::Bullet, BULLET.chars().count()));
    }
    if let Some(rest) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
        let _ = rest;
        return Some((LineKind::Bullet, 2));
    }
    if line.starts_with('>') {
        return Some((LineKind::Quote, if line.starts_with("> ") { 2 } else { 1 }));
    }
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    if (1..=4).contains(&digits) && line.get(digits..).is_some_and(|r| r.starts_with(". ")) {
        return Some((LineKind::Numbered, digits + 2));
    }
    None
}

impl RichDoc {
    /// Documento de texto sin estilo.
    #[cfg(test)]
    pub fn plain(text: &str) -> Self {
        let text: String = text.chars().take(MAX_CHARS).collect();
        let n = text.chars().count();
        RichDoc { text, styles: vec![CharStyle::default(); n], typing: None }
    }

    fn len(&self) -> usize {
        self.styles.len()
    }

    /// Ajusta los estilos tras una edición del `TextEdit` (`old` = texto anterior): lo que no
    /// cambió conserva su estilo; lo insertado toma el estilo de escritura o el del carácter
    /// anterior (sin enlace: seguir escribiendo tras un enlace no lo alarga).
    pub fn sync_after_edit(&mut self, old: &str) {
        if old == self.text {
            return;
        }
        let a: Vec<char> = old.chars().collect();
        let b: Vec<char> = self.text.chars().collect();
        let mut p = 0usize;
        while p < a.len() && p < b.len() && a.get(p) == b.get(p) {
            p += 1;
        }
        let mut q = 0usize;
        while q < a.len().saturating_sub(p)
            && q < b.len().saturating_sub(p)
            && a.get(a.len() - 1 - q) == b.get(b.len() - 1 - q)
        {
            q += 1;
        }
        let removed_end = a.len().saturating_sub(q);
        let inserted = b.len().saturating_sub(q).saturating_sub(p);
        let base = self.typing.clone().unwrap_or_else(|| {
            let mut s = p.checked_sub(1).and_then(|i| self.styles.get(i)).cloned().unwrap_or_default();
            s.link = None;
            s
        });
        let mut styles: Vec<CharStyle> = Vec::with_capacity(b.len());
        styles.extend(self.styles.iter().take(p).cloned());
        styles.extend(std::iter::repeat_n(base, inserted));
        styles.extend(self.styles.iter().skip(removed_end).cloned());
        styles.resize(b.len(), CharStyle::default());
        self.styles = styles;
    }

    /// Inserta `s` en la posición `at` (caracteres) con `style`.
    pub fn insert(&mut self, at: usize, s: &str, style: CharStyle) {
        let at = at.min(self.len());
        let b = byte_at(&self.text, at);
        self.text.insert_str(b, s);
        let n = s.chars().count();
        let tail = self.styles.split_off(at.min(self.styles.len()));
        self.styles.extend(std::iter::repeat_n(style, n));
        self.styles.extend(tail);
    }

    /// Borra `[from, to)` (caracteres).
    pub fn remove(&mut self, from: usize, to: usize) {
        let (from, to) = (from.min(self.len()), to.min(self.len()));
        if from >= to {
            return;
        }
        let (bf, bt) = (byte_at(&self.text, from), byte_at(&self.text, to));
        self.text.replace_range(bf..bt, "");
        self.styles.drain(from..to);
    }

    /// Alterna un estilo en `[from, to)`; sin selección, cambia el estilo de escritura.
    pub fn toggle(&mut self, from: usize, to: usize, t: Toggle) {
        let (from, to) = (from.min(to), to.max(from).min(self.len()));
        if from >= to {
            let mut s = self
                .typing
                .clone()
                .unwrap_or_else(|| from.checked_sub(1).and_then(|i| self.styles.get(i)).cloned().unwrap_or_default());
            let v = !t.get(&s);
            t.set(&mut s, v);
            s.link = None;
            self.typing = Some(s);
            return;
        }
        let all = self.styles.get(from..to).is_some_and(|r| r.iter().all(|s| t.get(s)));
        if let Some(r) = self.styles.get_mut(from..to) {
            for s in r {
                t.set(s, !all);
            }
        }
    }

    /// ¿Todo `[from, to)` (o el estilo de escritura) tiene el estilo?
    pub fn has(&self, from: usize, to: usize, t: Toggle) -> bool {
        let (from, to) = (from.min(to), to.max(from).min(self.len()));
        if from >= to {
            return self
                .typing
                .as_ref()
                .or_else(|| from.checked_sub(1).and_then(|i| self.styles.get(i)))
                .is_some_and(|s| t.get(s));
        }
        self.styles.get(from..to).is_some_and(|r| r.iter().all(|s| t.get(s)))
    }

    /// Pone (o quita, con `None`) un enlace en `[from, to)`.
    pub fn set_link(&mut self, from: usize, to: usize, href: Option<&str>) {
        let (from, to) = (from.min(to), to.max(from).min(self.len()));
        let link: Option<Arc<str>> = href.map(Arc::from);
        if let Some(r) = self.styles.get_mut(from..to) {
            for s in r {
                s.link = link.clone();
            }
        }
    }

    /// Inicio (en caracteres) de cada línea.
    fn line_starts(&self) -> Vec<usize> {
        let mut v = vec![0usize];
        for (i, c) in self.text.chars().enumerate() {
            if c == '\n' {
                v.push(i + 1);
            }
        }
        v
    }

    /// Alterna la marca de bloque en las líneas que toca `[from, to]`. Devuelve cuántos caracteres
    /// se movió el principio de la selección.
    pub fn toggle_lines(&mut self, from: usize, to: usize, kind: LineKind) -> isize {
        let (from, to) = (from.min(to), to.max(from));
        let starts = self.line_starts();
        let lines: Vec<usize> = starts
            .iter()
            .enumerate()
            .filter(|(i, s)| {
                let end = starts.get(i + 1).map_or(usize::MAX, |n| n.saturating_sub(1));
                **s <= to && end >= from
            })
            .map(|(_, s)| *s)
            .collect();
        let texts: Vec<String> = self.text.split('\n').map(str::to_string).collect();
        let line_text = |start: usize| -> String {
            let idx = starts.iter().position(|s| *s == start).unwrap_or(0);
            texts.get(idx).cloned().unwrap_or_default()
        };
        let all = lines.iter().all(|s| line_marker(&line_text(*s)).is_some_and(|(k, _)| k == kind));
        let mut shift = 0isize;
        let mut n = 1usize;
        // De abajo arriba para no mover los inicios que faltan.
        for start in lines.iter().rev() {
            let t = line_text(*start);
            if let Some((_, len)) = line_marker(&t) {
                self.remove(*start, start + len);
                if *start <= from {
                    shift -= len as isize;
                }
            }
            if !all {
                let mark = match kind {
                    LineKind::Bullet => BULLET.to_string(),
                    LineKind::Quote => "> ".to_string(),
                    LineKind::Numbered => format!("{}. ", lines.len().saturating_sub(n).saturating_add(1)),
                };
                n += 1;
                let len = mark.chars().count();
                self.insert(*start, &mark, CharStyle::default());
                if *start <= from {
                    shift += len as isize;
                }
            }
        }
        shift
    }

    /// Tras pulsar Intro en una lista: continúa la lista en la línea nueva o, si el elemento quedó
    /// vacío, la termina. `cursor` = posición tras el salto. Devuelve la nueva posición del cursor.
    pub fn continue_list(&mut self, cursor: usize) -> Option<usize> {
        let starts = self.line_starts();
        let cur_line = starts.iter().rposition(|s| *s <= cursor)?;
        if cur_line == 0 || starts.get(cur_line).copied() != Some(cursor) {
            return None;
        }
        let prev_start = starts.get(cur_line - 1).copied()?;
        let texts: Vec<&str> = self.text.split('\n').collect();
        let prev = texts.get(cur_line - 1).copied().unwrap_or("");
        let (kind, len) = line_marker(prev)?;
        if kind == LineKind::Quote {
            return None;
        }
        if prev.chars().count() == len {
            // Elemento vacío: se quita la viñeta y el salto (fin de la lista).
            self.remove(prev_start, cursor);
            return Some(prev_start);
        }
        let mark = match kind {
            LineKind::Bullet => BULLET.to_string(),
            _ => {
                let n: u32 = prev.chars().take_while(char::is_ascii_digit).collect::<String>().parse().unwrap_or(0);
                format!("{}. ", n.saturating_add(1))
            }
        };
        let l = mark.chars().count();
        self.insert(cursor, &mark, CharStyle::default());
        Some(cursor + l)
    }

    // ------------------------------------------------------------------ conversión

    /// Documento a partir del cuerpo del redactor.
    pub fn from_rich(body: &RichBody) -> Self {
        let mut w = Writer::default();
        w.blocks(&body.blocks, 0);
        while w.doc.text.ends_with('\n') {
            w.doc.text.pop();
            w.doc.styles.pop();
        }
        w.doc
    }

    /// Cuerpo del redactor a partir del documento.
    pub fn to_rich(&self) -> RichBody {
        let chars: Vec<char> = self.text.chars().collect();
        let mut lines: Vec<Line> = Vec::new();
        let mut start = 0usize;
        for (i, c) in chars.iter().enumerate() {
            if *c == '\n' {
                lines.push(Line { start, len: i - start });
                start = i + 1;
            }
        }
        lines.push(Line { start, len: chars.len() - start });
        let p = Parser { chars: &chars, styles: &self.styles };
        RichBody { blocks: p.blocks(&lines, 0, true) }
    }
}

#[derive(Default)]
struct Writer {
    doc: RichDoc,
    /// Prefijo de cita de cada línea (`> `, `> > `…).
    prefix: String,
}

impl Writer {
    fn push(&mut self, s: &str, style: &CharStyle) {
        if self.doc.styles.len() >= MAX_CHARS {
            return;
        }
        for c in s.chars() {
            if c == '\n' {
                self.newline();
            } else {
                self.doc.text.push(c);
                self.doc.styles.push(style.clone());
            }
        }
    }

    fn newline(&mut self) {
        self.doc.text.push('\n');
        self.doc.styles.push(CharStyle::default());
        let p = self.prefix.clone();
        self.push(&p, &CharStyle::default());
    }

    fn inlines(&mut self, content: &[Inline], extra: &CharStyle) {
        for i in content {
            match i {
                Inline::Text(span) => {
                    let mut st = CharStyle::from_span(span);
                    st.bold |= extra.bold;
                    st.code |= extra.code;
                    self.push(&span.text, &st);
                }
                Inline::Image(img) => {
                    let alt = if img.alt.trim().is_empty() { "imagen" } else { img.alt.as_str() };
                    self.push(&format!("[{alt}]"), &CharStyle::default());
                }
                Inline::LineBreak => self.newline(),
            }
        }
    }

    fn blocks(&mut self, blocks: &[Block], depth: usize) {
        let n = blocks.len();
        for (k, b) in blocks.iter().enumerate() {
            match b {
                Block::Paragraph { content } => self.inlines(content, &CharStyle::default()),
                Block::Heading { content, .. } => {
                    self.inlines(content, &CharStyle { bold: true, ..Default::default() });
                }
                Block::List { ordered, start, items } => {
                    for (j, item) in items.iter().enumerate() {
                        if j > 0 {
                            self.newline();
                        }
                        let mark = if *ordered {
                            format!("{}. ", start.saturating_add(u32::try_from(j).unwrap_or(0)))
                        } else {
                            BULLET.to_string()
                        };
                        self.push(&mark, &CharStyle::default());
                        // Dentro del elemento, todo en la misma línea lógica.
                        for (bi, ib) in item.iter().enumerate() {
                            if bi > 0 {
                                self.push(" ", &CharStyle::default());
                            }
                            match ib {
                                Block::Paragraph { content } | Block::Heading { content, .. } => {
                                    self.inlines(content, &CharStyle::default())
                                }
                                other => {
                                    let mut inner = Writer::default();
                                    inner.blocks(std::slice::from_ref(other), depth + 1);
                                    let flat = inner.doc.text.replace('\n', " ");
                                    self.push(&flat, &CharStyle::default());
                                }
                            }
                        }
                    }
                }
                Block::Quote { blocks } => {
                    if depth >= MAX_DEPTH {
                        self.blocks(blocks, depth + 1);
                    } else {
                        let old = self.prefix.clone();
                        self.prefix.push_str("> ");
                        self.push("> ", &CharStyle::default());
                        self.blocks(blocks, depth + 1);
                        self.prefix = old;
                    }
                }
                Block::Code { text } => self.push(text, &CharStyle { code: true, ..Default::default() }),
                Block::Rule => self.push("———", &CharStyle::default()),
                Block::Signature { blocks } => {
                    self.push(SIG, &CharStyle::default());
                    for (si, sb) in blocks.iter().enumerate() {
                        self.newline();
                        let _ = si;
                        let mut inner = Writer::default();
                        inner.blocks(std::slice::from_ref(sb), depth + 1);
                        // La firma va en un solo bloque (sin líneas en blanco).
                        let mut first = true;
                        for line in inner.doc.text.split('\n').filter(|l| !l.trim().is_empty()) {
                            if !first {
                                self.newline();
                            }
                            first = false;
                            self.push(line, &CharStyle::default());
                        }
                    }
                }
            }
            if k + 1 < n {
                self.newline();
                self.newline();
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Line {
    start: usize,
    len: usize,
}

struct Parser<'a> {
    chars: &'a [char],
    styles: &'a [CharStyle],
}

impl Parser<'_> {
    fn text(&self, l: Line) -> String {
        self.chars.iter().skip(l.start).take(l.len).collect()
    }

    fn skip(l: Line, n: usize) -> Line {
        let n = n.min(l.len);
        Line { start: l.start + n, len: l.len - n }
    }

    /// Tramos con estilo de una línea.
    fn inlines(&self, l: Line, out: &mut Vec<Inline>) {
        let mut cur: Option<(String, CharStyle)> = None;
        for i in l.start..l.start + l.len {
            let c = self.chars.get(i).copied().unwrap_or(' ');
            let st = self.styles.get(i).cloned().unwrap_or_default();
            match &mut cur {
                Some((s, cs)) if *cs == st => s.push(c),
                _ => {
                    if let Some((s, cs)) = cur.take() {
                        out.push(span(s, &cs));
                    }
                    cur = Some((c.to_string(), st));
                }
            }
        }
        if let Some((s, cs)) = cur {
            out.push(span(s, &cs));
        }
    }

    fn blocks(&self, lines: &[Line], depth: usize, top: bool) -> Vec<Block> {
        let mut out = Vec::new();
        let mut i = 0usize;
        let mut para: Vec<Inline> = Vec::new();
        let flush = |para: &mut Vec<Inline>, out: &mut Vec<Block>| {
            if !para.is_empty() {
                out.push(Block::Paragraph { content: std::mem::take(para) });
            }
        };
        while let Some(&l) = lines.get(i) {
            let t = self.text(l);
            if t.trim().is_empty() {
                flush(&mut para, &mut out);
                i += 1;
                continue;
            }
            if top && t == SIG {
                // Firma: hasta la próxima línea en blanco.
                flush(&mut para, &mut out);
                let mut content = Vec::new();
                let mut j = i + 1;
                while let Some(&sl) = lines.get(j) {
                    if self.text(sl).trim().is_empty() {
                        break;
                    }
                    if !content.is_empty() {
                        content.push(Inline::LineBreak);
                    }
                    self.inlines(sl, &mut content);
                    j += 1;
                }
                let blocks = if content.is_empty() { Vec::new() } else { vec![Block::Paragraph { content }] };
                out.push(Block::Signature { blocks });
                i = j;
                continue;
            }
            match line_marker(&t) {
                Some((LineKind::Quote, _)) if depth < MAX_DEPTH => {
                    flush(&mut para, &mut out);
                    let mut inner = Vec::new();
                    while let Some(&ql) = lines.get(i) {
                        let qt = self.text(ql);
                        match line_marker(&qt) {
                            Some((LineKind::Quote, n)) => inner.push(Self::skip(ql, n)),
                            _ => break,
                        }
                        i += 1;
                    }
                    out.push(Block::Quote { blocks: self.blocks(&inner, depth + 1, false) });
                }
                Some((kind @ (LineKind::Bullet | LineKind::Numbered), _)) => {
                    flush(&mut para, &mut out);
                    let start: u32 =
                        t.chars().take_while(char::is_ascii_digit).collect::<String>().parse().unwrap_or(1);
                    let mut items = Vec::new();
                    while let Some(&il) = lines.get(i) {
                        let it = self.text(il);
                        match line_marker(&it) {
                            Some((k, n)) if k == kind => {
                                let mut content = Vec::new();
                                self.inlines(Self::skip(il, n), &mut content);
                                items.push(vec![Block::Paragraph { content }]);
                            }
                            _ => break,
                        }
                        i += 1;
                    }
                    out.push(Block::List { ordered: kind == LineKind::Numbered, start: start.max(1), items });
                }
                _ if t == "———" => {
                    flush(&mut para, &mut out);
                    out.push(Block::Rule);
                    i += 1;
                }
                _ => {
                    if !para.is_empty() {
                        para.push(Inline::LineBreak);
                    }
                    self.inlines(l, &mut para);
                    i += 1;
                }
            }
        }
        flush(&mut para, &mut out);
        out
    }
}

fn span(text: String, cs: &CharStyle) -> Inline {
    Inline::Text(Span { text, style: cs.style(), link: cs.link.as_deref().map(str::to_string) })
}

// ---------------------------------------------------------------------------------------------
// Widget

/// Estado de un editor en pantalla.
#[derive(Clone, Debug, Default)]
pub struct EditorState {
    pub doc: RichDoc,
    /// Texto del cuadro anterior (para detectar la edición).
    prev: String,
    /// Última selección (inicio, fin) en caracteres.
    pub sel: (usize, usize),
    /// Hubo cambios desde la última vez que se preguntó.
    pub changed: bool,
}

impl EditorState {
    pub fn new(doc: RichDoc) -> Self {
        let prev = doc.text.clone();
        EditorState { doc, prev, sel: (0, 0), changed: false }
    }

    /// Reemplaza el documento (p. ej. al insertar la firma).
    pub fn set_doc(&mut self, doc: RichDoc) {
        self.prev = doc.text.clone();
        self.doc = doc;
        self.changed = true;
    }

    /// Alterna un estilo sobre la selección (o el de escritura).
    pub fn toggle(&mut self, t: Toggle) {
        let (a, b) = self.sel;
        self.doc.toggle(a, b, t);
        self.changed = true;
    }

    /// Alterna una marca de bloque en las líneas seleccionadas.
    pub fn toggle_lines(&mut self, kind: LineKind) {
        let (a, b) = self.sel;
        let shift = self.doc.toggle_lines(a, b, kind);
        self.prev = self.doc.text.clone();
        let fix = |x: usize| x.saturating_add_signed(shift);
        self.sel = (fix(a), fix(b));
        self.changed = true;
    }

    /// Pone un enlace en la selección.
    pub fn link(&mut self, href: Option<&str>) {
        let (a, b) = self.sel;
        if a == b {
            if let Some(h) = href {
                // Sin selección: se inserta la propia dirección como enlace.
                let st = CharStyle { link: Some(Arc::from(h)), ..Default::default() };
                self.doc.insert(a, h, st);
                self.prev = self.doc.text.clone();
            }
        } else {
            self.doc.set_link(a, b, href);
        }
        self.changed = true;
    }
}

/// Fuentes y colores del editor.
pub struct Look {
    pub size: f32,
    pub bold: FontId,
    pub text: Color32,
    pub muted: Color32,
    pub accent: Color32,
}

fn layout_doc(doc: &RichDoc, text: &str, look: &Look, wrap: f32) -> LayoutJob {
    let mut job = LayoutJob::default();
    let mut quote_line = false;
    let mut at_line_start = true;
    let mut marker_left = 0usize;
    let mut run = String::new();
    let mut run_fmt: Option<TextFormat> = None;
    let flush = |job: &mut LayoutJob, run: &mut String, fmt: &Option<TextFormat>| {
        if let Some(f) = fmt
            && !run.is_empty()
        {
            job.append(run, 0.0, f.clone());
            run.clear();
        }
    };
    let lines: Vec<&str> = text.split('\n').collect();
    let mut line_idx = 0usize;
    for (i, c) in text.chars().enumerate() {
        if at_line_start {
            let l = lines.get(line_idx).copied().unwrap_or("");
            quote_line = l.starts_with('>');
            marker_left = match line_marker(l) {
                Some((LineKind::Quote, _)) => l.chars().take_while(|c| *c == '>' || *c == ' ').count(),
                Some((_, n)) => n,
                None => usize::from(l == SIG) * 3,
            };
            at_line_start = false;
        }
        let st = doc.styles.get(i).cloned().unwrap_or_default();
        let mut f = TextFormat {
            font_id: if st.bold && !st.code {
                FontId::new(look.size, look.bold.family.clone())
            } else if st.code {
                FontId::monospace(look.size - 1.0)
            } else {
                FontId::proportional(look.size)
            },
            color: if quote_line || marker_left > 0 { look.muted } else { look.text },
            italics: st.italic,
            line_height: Some(look.size * 1.45),
            ..Default::default()
        };
        if st.underline || st.link.is_some() {
            f.underline = Stroke::new(1.0, if st.link.is_some() { look.accent } else { f.color });
        }
        if st.link.is_some() {
            f.color = look.accent;
        }
        if st.strike {
            f.strikethrough = Stroke::new(1.0, f.color);
        }
        if marker_left > 0 && !quote_line {
            f.color = look.accent;
        }
        marker_left = marker_left.saturating_sub(1);
        if c == '\n' {
            at_line_start = true;
            line_idx += 1;
        }
        if run_fmt.as_ref() != Some(&f) {
            flush(&mut job, &mut run, &run_fmt);
            run_fmt = Some(f);
        }
        run.push(c);
    }
    flush(&mut job, &mut run, &run_fmt);
    if job.sections.is_empty() {
        job.append(
            "",
            0.0,
            TextFormat { font_id: FontId::proportional(look.size), color: look.text, ..Default::default() },
        );
    }
    job.wrap.max_width = wrap;
    job
}

/// Dibuja el editor. `id` estable por ventana. Devuelve la respuesta del `TextEdit`.
pub fn editor(
    ui: &mut Ui,
    id: egui::Id,
    st: &mut EditorState,
    look: &Look,
    hint: &str,
    min_rows: usize,
) -> egui::Response {
    // Atajos de formato mientras el editor tiene el foco.
    let focused = ui.memory(|m| m.has_focus(id));
    if focused {
        let (b, i, u) = ui.input_mut(|inp| {
            (
                inp.consume_key(Modifiers::COMMAND, Key::B),
                inp.consume_key(Modifiers::COMMAND, Key::I),
                inp.consume_key(Modifiers::COMMAND, Key::U),
            )
        });
        if b {
            st.toggle(Toggle::Bold);
        }
        if i {
            st.toggle(Toggle::Italic);
        }
        if u {
            st.toggle(Toggle::Underline);
        }
    }
    let EditorState { doc, prev, sel, changed } = st;
    let styles_doc = doc.clone_styles();
    let mut layouter = |ui: &Ui, buf: &dyn egui::TextBuffer, wrap: f32| {
        ui.fonts_mut(|f| f.layout_job(layout_doc(&styles_doc, buf.as_str(), look, wrap)))
    };
    let out = egui::TextEdit::multiline(&mut doc.text)
        .id(id)
        .hint_text(hint)
        .desired_width(f32::INFINITY)
        .desired_rows(min_rows)
        .frame(egui::Frame::NONE)
        .lock_focus(true)
        .layouter(&mut layouter)
        .show(ui);
    if let Some(r) = out.cursor_range {
        let (a, b) = (r.primary.index.0, r.secondary.index.0);
        *sel = (a.min(b), a.max(b));
    }
    if doc.text != *prev {
        let old = std::mem::take(prev);
        let inserted_newline = doc.text.chars().count() == old.chars().count() + 1
            && doc.text.matches('\n').count() > old.matches('\n').count();
        doc.sync_after_edit(&old);
        if inserted_newline && let Some(pos) = doc.continue_list(sel.0) {
            if let Some(mut s) = egui::text_edit::TextEditState::load(ui.ctx(), id) {
                s.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(pos))));
                s.store(ui.ctx(), id);
            }
            *sel = (pos, pos);
        }
        // Con selección vacía, el estilo de escritura vale hasta el siguiente movimiento.
        *prev = doc.text.clone();
        *changed = true;
    } else if out.response.changed() {
        *changed = true;
    }
    if out.response.clicked() {
        doc.typing = None;
    }
    out.response.response
}

impl RichDoc {
    fn clone_styles(&self) -> RichDoc {
        RichDoc { text: String::new(), styles: self.styles.clone(), typing: None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bold() -> CharStyle {
        CharStyle { bold: true, ..Default::default() }
    }

    #[test]
    fn edits_keep_styles() {
        let mut d = RichDoc::plain("hola mundo");
        d.toggle(0, 4, Toggle::Bold);
        assert!(d.has(0, 4, Toggle::Bold) && !d.has(4, 10, Toggle::Bold));
        let old = d.text.clone();
        d.text = "hola!! mundo".into();
        d.sync_after_edit(&old);
        assert_eq!(d.styles.len(), 12);
        assert!(d.styles[4].bold, "lo escrito tras negrita sigue en negrita");
        assert!(!d.styles[7].bold);
        let old = d.text.clone();
        d.text = "ho mundo".into();
        d.sync_after_edit(&old);
        assert_eq!(d.styles.len(), 8);
        assert!(d.styles[0].bold && !d.styles[3].bold);
        // Estilo de escritura sin selección.
        d.toggle(8, 8, Toggle::Italic);
        let old = d.text.clone();
        d.text.push('x');
        d.sync_after_edit(&old);
        assert!(d.styles[8].italic);
    }

    #[test]
    fn lists_toggle_and_continue() {
        let mut d = RichDoc::plain("uno\ndos");
        d.toggle_lines(0, 5, LineKind::Bullet);
        assert_eq!(d.text, "• uno\n• dos");
        d.toggle_lines(0, 9, LineKind::Bullet);
        assert_eq!(d.text, "uno\ndos");
        d.toggle_lines(0, 5, LineKind::Numbered);
        assert_eq!(d.text, "1. uno\n2. dos");
        // Intro al final de un elemento: el siguiente número; en uno vacío: fin de la lista.
        let old = d.text.clone();
        d.text.push('\n');
        d.sync_after_edit(&old);
        let pos = d.continue_list(d.text.chars().count()).unwrap();
        assert_eq!(d.text, "1. uno\n2. dos\n3. ");
        assert_eq!(pos, d.text.chars().count());
        let old = d.text.clone();
        d.text.push('\n');
        d.sync_after_edit(&old);
        d.continue_list(d.text.chars().count());
        assert_eq!(d.text, "1. uno\n2. dos\n");
        assert_eq!(d.styles.len(), d.text.chars().count());
    }

    #[test]
    fn rich_round_trip() {
        let mut d =
            RichDoc::plain("Hola equipo,\nmañana firmamos.\n\n• uno\n• dos\n\n> cita\n> > vieja\n\n-- \nAna\nDespacho");
        d.toggle(0, 4, Toggle::Bold);
        d.set_link(5, 11, Some("https://example.com"));
        let body = d.to_rich();
        let html = body.to_html_fragment();
        assert!(html.contains("<strong>Hola</strong>") || html.contains("<b>Hola</b>"), "{html}");
        assert!(html.contains("https://example.com"));
        assert!(body.blocks.iter().any(|b| matches!(b, Block::List { ordered: false, items, .. } if items.len() == 2)));
        let quote = body.blocks.iter().find_map(|b| match b {
            Block::Quote { blocks } => Some(blocks),
            _ => None,
        });
        assert!(quote.is_some_and(|q| q.iter().any(|b| matches!(b, Block::Quote { .. }))), "cita anidada");
        assert!(body.signature().is_some());
        let plain = body.to_plain();
        assert!(plain.contains("-- \nAna"), "{plain}");
        // Y de vuelta.
        let back = RichDoc::from_rich(&body);
        assert!(back.text.starts_with("Hola equipo,\nmañana firmamos."), "{}", back.text);
        assert!(back.text.contains("• uno\n• dos"));
        assert!(back.text.contains("> cita\n> \n> > vieja"), "{}", back.text);
        assert!(back.text.contains("-- \nAna\nDespacho"));
        assert!(back.styles[0].bold);
        assert_eq!(back.styles.len(), back.text.chars().count());
        let _ = bold();
    }

    #[test]
    fn reply_body_from_compose_is_editable() {
        let body = RichBody {
            blocks: vec![
                Block::Paragraph { content: vec![] },
                Block::Signature { blocks: vec![Block::paragraph("Laura")] },
                Block::paragraph("El lun, Ana escribió:"),
                Block::Quote { blocks: vec![Block::paragraph("hola"), Block::paragraph("adiós")] },
            ],
        };
        let d = RichDoc::from_rich(&body);
        assert!(d.text.contains("-- \nLaura\n\nEl lun, Ana escribió:\n\n> hola\n> \n> adiós"), "{:?}", d.text);
        let back = d.to_rich();
        assert!(back.signature().is_some());
        assert!(back.blocks.iter().any(|b| matches!(b, Block::Quote { .. })));
        assert!(back.text_content().contains("Ana escribió"));
    }

    #[test]
    fn hostile_input_never_panics() {
        for t in ["", "\n\n\n", ">>>>>>>>>>>>>>>>>>>>>>>>>>>>>>> x", "1. \n2.", "-- ", "• ", "ñ\u{301}é"] {
            let mut d = RichDoc::plain(t);
            let n = d.text.chars().count();
            d.toggle(0, n + 5, Toggle::Bold);
            d.toggle_lines(0, n + 5, LineKind::Quote);
            d.set_link(n, n + 10, Some("x"));
            let _ = d.to_rich().to_html();
            let _ = RichDoc::from_rich(&d.to_rich());
            d.remove(3, 1);
            d.insert(999, "z", CharStyle::default());
            assert_eq!(d.styles.len(), d.text.chars().count());
        }
    }
}
