//! UX-12: mapas mentales de IureEditor en la vista previa de las notas.
//!
//! IureEditor guarda un mapa mental como una cita-aviso con lista:
//!
//! ```text
//! > [!MINDMAP|map] Idea central
//! > - Análisis
//! >   - Requisitos
//! > - Diseño
//! ```
//!
//! El metadato tras `|` son palabras en cualquier orden: la vista (`map`: solo el mapa; `right` o
//! nada: la lista y el mapa), la forma de los nodos ([`Shape`], las mismas de IureEditor), `links`
//! (mapa de enlaces generado) y, S1.19, la distribución ([`Layout`]: `layout-right`,
//! `layout-radial`, `layout-both`, `layout-down`; sin ella, la de Preferencias). Lo que no se
//! entiende se conserva al reescribir el metadato ([`set_options`]). El centro es el texto de la línea del
//! marcador (o el primer párrafo; si no hay, queda vacío y quien dibuja pone el nombre por omisión,
//! en el idioma de la interfaz); las listas son las ramas con su
//! sangría y un párrafo suelto cuelga del centro.
//!
//! [`find`] localiza los bloques (fuera de los bloques de código) y [`draw`] dibuja el árbol con un
//! color por rama, con los colores y el tamaño de letra de la nota: radial con `mermaid-svg` (como
//! IureEditor) y las demás distribuciones con dibujo propio. Todo acotado: nodos, profundidad,
//! largo de etiquetas y tamaño del dibujo.

use std::fmt::Write as _;
use std::ops::Range;

use crate::markdown::NoteStyle;

/// Nodos por mapa, como mucho (el resto se ignora).
pub const MAX_NODES: usize = 200;
/// Niveles bajo el centro.
pub const MAX_DEPTH: usize = 6;
/// Mapas por nota.
pub const MAX_MAPS: usize = 16;
/// Caracteres por etiqueta.
const MAX_LABEL: usize = 60;

/// Cómo pide IureEditor que se vea.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    /// La lista y el mapa (`below`, `right` o sin metadato).
    ListAndMap,
    /// Solo el mapa (`map`).
    MapOnly,
}

/// S1.19: cómo se reparten las ramas alrededor del centro.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Layout {
    /// Centro a la izquierda, ramas hacia la derecha.
    #[default]
    Right,
    /// Ramas en abanico alrededor del centro (el dibujo de Mermaid, como IureEditor).
    Radial,
    /// Ramas repartidas a ambos lados del centro.
    Balanced,
    /// Centro arriba y ramas hacia abajo (organigrama).
    Down,
}

impl Layout {
    pub const ALL: [Layout; 4] = [Layout::Right, Layout::Radial, Layout::Balanced, Layout::Down];

    /// Palabra del metadato.
    pub fn token(self) -> &'static str {
        match self {
            Layout::Right => "layout-right",
            Layout::Radial => "layout-radial",
            Layout::Balanced => "layout-both",
            Layout::Down => "layout-down",
        }
    }

    pub fn from_token(t: &str) -> Option<Layout> {
        Layout::ALL.into_iter().find(|l| t.eq_ignore_ascii_case(l.token()))
    }
}

/// Forma de los nodos (las de IureEditor y Mermaid).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Shape {
    /// Sin forma explícita (aquí, píldora; en el dibujo radial, la de Mermaid).
    #[default]
    Default,
    Square,
    Rounded,
    Circle,
    Cloud,
    Bang,
    Hexagon,
}

impl Shape {
    pub const ALL: [Shape; 7] =
        [Shape::Default, Shape::Square, Shape::Rounded, Shape::Circle, Shape::Cloud, Shape::Bang, Shape::Hexagon];

    /// Palabra del metadato (la de IureEditor).
    pub fn token(self) -> &'static str {
        match self {
            Shape::Default => "default",
            Shape::Square => "square",
            Shape::Rounded => "rounded",
            Shape::Circle => "circle",
            Shape::Cloud => "cloud",
            Shape::Bang => "bang",
            Shape::Hexagon => "hexagon",
        }
    }

    pub fn from_token(t: &str) -> Option<Shape> {
        Shape::ALL.into_iter().find(|s| t.eq_ignore_ascii_case(s.token()))
    }

    /// Delimitadores de Mermaid (`None`: sin forma).
    pub fn delims(self) -> Option<(&'static str, &'static str)> {
        match self {
            Shape::Default => None,
            Shape::Square => Some(("[", "]")),
            Shape::Rounded => Some(("(", ")")),
            Shape::Circle => Some(("((", "))")),
            Shape::Cloud => Some((")", "(")),
            Shape::Bang => Some(("))", "((")),
            Shape::Hexagon => Some(("{{", "}}")),
        }
    }
}

/// Tamaño del nodo central. Un centro con mucho texto se parte en varias líneas en lugar de
/// volverse una píldora (o un disco) enorme.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum RootSize {
    /// Una línea si cabe; si es largo, partido en varias.
    #[default]
    Auto,
    Small,
    Medium,
    Large,
}

impl RootSize {
    pub const ALL: [RootSize; 4] = [RootSize::Auto, RootSize::Small, RootSize::Medium, RootSize::Large];

    /// Palabra del metadato (`None`: automático, sin palabra).
    pub fn token(self) -> Option<&'static str> {
        match self {
            RootSize::Auto => None,
            RootSize::Small => Some("root-s"),
            RootSize::Medium => Some("root-m"),
            RootSize::Large => Some("root-l"),
        }
    }

    pub fn from_token(t: &str) -> Option<RootSize> {
        RootSize::ALL.into_iter().find(|r| r.token().is_some_and(|k| t.eq_ignore_ascii_case(k)))
    }

    /// Escala de la letra del centro, caracteres por línea y líneas como mucho (el chico es
    /// angosto pero alto: no corta lo que cabe en una etiqueta de [`MAX_LABEL`]).
    fn metrics(self, chars: usize) -> (f32, usize, usize) {
        match self {
            RootSize::Auto if chars <= 30 => (1.0, 30, 1),
            RootSize::Auto => (1.0, 22, 4),
            RootSize::Small => (0.95, 16, 6),
            RootSize::Medium => (1.12, 20, 4),
            RootSize::Large => (1.3, 28, 3),
        }
    }
}

/// Colores de las ramas.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Palette {
    /// Los de la app: intensos, texto blanco con contraste AA.
    #[default]
    Iure,
    /// Suaves, texto oscuro (líneas en un tono más oscuro para que se vean).
    Pastel,
    Warm,
    Cool,
    /// Grises.
    Mono,
}

impl Palette {
    pub const ALL: [Palette; 5] = [Palette::Iure, Palette::Pastel, Palette::Warm, Palette::Cool, Palette::Mono];

    /// Palabra del metadato (`None`: la de la app, sin palabra).
    pub fn token(self) -> Option<&'static str> {
        match self {
            Palette::Iure => None,
            Palette::Pastel => Some("palette-pastel"),
            Palette::Warm => Some("palette-warm"),
            Palette::Cool => Some("palette-cool"),
            Palette::Mono => Some("palette-mono"),
        }
    }

    pub fn from_token(t: &str) -> Option<Palette> {
        Palette::ALL.into_iter().find(|p| p.token().is_some_and(|k| t.eq_ignore_ascii_case(k)))
    }

    /// Valor en `iure.palette` de los bloques Mermaid.
    pub fn value(self) -> &'static str {
        match self {
            Palette::Iure => "app",
            Palette::Pastel => "pastel",
            Palette::Warm => "warm",
            Palette::Cool => "cool",
            Palette::Mono => "mono",
        }
    }

    pub fn from_value(v: &str) -> Option<Palette> {
        Palette::ALL.into_iter().find(|p| v.eq_ignore_ascii_case(p.value()))
    }

    /// Los seis colores, en orden de rama.
    pub fn colors(self) -> &'static [[u8; 3]; 6] {
        match self {
            Palette::Iure => &BRANCH,
            Palette::Pastel => &[
                [0xbf, 0xdb, 0xfe],
                [0xbb, 0xf7, 0xd0],
                [0xfe, 0xd7, 0xaa],
                [0xe9, 0xd5, 0xff],
                [0xfe, 0xca, 0xca],
                [0xa5, 0xf3, 0xfc],
            ],
            Palette::Warm => &[
                [0xb4, 0x53, 0x09],
                [0xb9, 0x1c, 0x1c],
                [0xbe, 0x18, 0x5d],
                [0xc2, 0x41, 0x0c],
                [0x92, 0x40, 0x0e],
                [0x9d, 0x17, 0x4d],
            ],
            Palette::Cool => &[
                [0x1d, 0x4e, 0xd8],
                [0x0f, 0x76, 0x6e],
                [0x43, 0x38, 0xca],
                [0x03, 0x69, 0xa1],
                [0x15, 0x80, 0x3d],
                [0x6d, 0x28, 0xd9],
            ],
            Palette::Mono => &[
                [0x1f, 0x29, 0x37],
                [0x47, 0x55, 0x69],
                [0x37, 0x41, 0x51],
                [0x52, 0x52, 0x5b],
                [0x33, 0x41, 0x55],
                [0x4b, 0x55, 0x63],
            ],
        }
    }
}

/// Opciones de dibujo que un mapa guarda en su metadato.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct MapOptions {
    /// `None`: la de Preferencias.
    pub layout: Option<Layout>,
    pub shape: Shape,
    pub root: RootSize,
    pub palette: Palette,
}

impl MapOptions {
    /// Cómo se dibuja, con `default_layout` si el mapa no pide una.
    pub fn look(&self, default_layout: Layout) -> Look {
        Look {
            layout: self.layout.unwrap_or(default_layout),
            shape: self.shape,
            root: self.root,
            palette: self.palette,
        }
    }
}

/// Todo lo que decide el dibujo de un mapa.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Look {
    pub layout: Layout,
    pub shape: Shape,
    pub root: RootSize,
    pub palette: Palette,
}

/// Un nodo del árbol.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Node {
    pub label: String,
    pub children: Vec<Node>,
    /// Línea del bloque de donde sale (0: la del marcador), para editarlo desde el dibujo.
    pub line: usize,
}

/// Dónde quedó un nodo en el dibujo (px del SVG = puntos), y su línea del bloque.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NodeBox {
    pub line: usize,
    pub depth: usize,
    /// `[x0, y0, x1, y1]`.
    pub rect: [f32; 4],
}

/// Un mapa mental de la nota.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    /// Bytes de la nota que ocupa (todas sus líneas, con el salto final).
    pub range: Range<usize>,
    pub view: View,
    /// Opciones de dibujo del metadato.
    pub options: MapOptions,
    /// El centro y sus ramas.
    pub root: Node,
    /// Líneas del cuerpo sin el `>` (la lista, para la vista con lista).
    pub body: Vec<String>,
}

/// Quita el `>` de una línea de cita (con su espacio opcional). `None` si no es de cita.
fn unquote(line: &str) -> Option<&str> {
    let t = line.trim_start_matches([' ', '\t']);
    let indent = line.len() - t.len();
    if indent > 3 {
        return None;
    }
    let rest = t.strip_prefix('>')?;
    Some(rest.strip_prefix(' ').unwrap_or(rest))
}

/// `[!MINDMAP|meta] título` → (meta, título).
fn marker(s: &str) -> Option<(String, String)> {
    let rest = s.trim_start().strip_prefix("[!")?;
    let close = rest.find(']')?;
    let inner = rest.get(..close)?;
    let (kind, meta) = inner.split_once('|').unwrap_or((inner, ""));
    if !kind.trim().eq_ignore_ascii_case("mindmap") {
        return None;
    }
    let title = rest.get(close + 1..).unwrap_or("").trim().to_string();
    Some((meta.trim().to_ascii_lowercase(), title))
}

/// Texto visible de una etiqueta Markdown sencilla: sin `**`, `*`, `_`, `` ` ``, `~~`; enlaces
/// `[t](u)` → `t`; wikilinks `[[a|b]]` → `b`; casilla `[ ]`/`[x]` fuera. Acotado.
pub fn plain_label(s: &str) -> String {
    let mut t = s.trim();
    for p in ["[ ] ", "[x] ", "[X] "] {
        if let Some(r) = t.strip_prefix(p) {
            t = r;
        }
    }
    let mut out = String::new();
    let chars: Vec<char> = t.chars().take(MAX_LABEL * 8).collect();
    let mut i = 0;
    while let Some(&c) = chars.get(i) {
        match c {
            '[' if chars.get(i + 1) == Some(&'[') => {
                // [[destino|alias]] o [[destino]]
                let end = (i + 2..chars.len()).find(|&k| chars.get(k) == Some(&']') && chars.get(k + 1) == Some(&']'));
                match end {
                    Some(e) => {
                        let inner: String = chars.get(i + 2..e).unwrap_or_default().iter().collect();
                        out.push_str(inner.rsplit('|').next().unwrap_or(""));
                        i = e + 2;
                    }
                    None => {
                        out.push(c);
                        i += 1;
                    }
                }
            }
            '[' => {
                // [texto](url) → texto
                let close = (i + 1..chars.len()).find(|&k| chars.get(k) == Some(&']'));
                match close {
                    Some(cl) if chars.get(cl + 1) == Some(&'(') => {
                        let text: String = chars.get(i + 1..cl).unwrap_or_default().iter().collect();
                        out.push_str(&text);
                        let paren = (cl + 2..chars.len()).find(|&k| chars.get(k) == Some(&')'));
                        i = paren.map_or(cl + 1, |p| p + 1);
                    }
                    _ => {
                        out.push(c);
                        i += 1;
                    }
                }
            }
            '*' | '_' | '`' | '~' => i += 1,
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    let out = out.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut cut: String = out.chars().take(MAX_LABEL).collect();
    if out.chars().count() > MAX_LABEL {
        cut.push('…');
    }
    cut
}

/// Elemento de lista: (sangría en columnas, texto).
fn list_item(line: &str) -> Option<(usize, &str)> {
    let t = line.trim_start_matches([' ', '\t']);
    let lead = line.get(..line.len() - t.len()).unwrap_or("");
    let indent: usize = lead.chars().map(|c| if c == '\t' { 4 } else { 1 }).sum();
    let rest = if let Some(r) = t.strip_prefix(['-', '*', '+']) {
        r
    } else {
        let digits = t.chars().take_while(char::is_ascii_digit).count();
        if digits == 0 || digits > 9 {
            return None;
        }
        t.get(digits..)?.strip_prefix(['.', ')'])?
    };
    let text = rest.strip_prefix([' ', '\t'])?;
    Some((indent, text))
}

/// Nodo de la lista plana mientras se lee: etiqueta, padre (0 = centro) y nivel.
struct Flat {
    label: String,
    parent: usize,
    depth: usize,
    line: usize,
}

/// El subárbol de `idx` desde la lista plana (≤ [`MAX_NODES`] nodos).
fn build(flat: &[Flat], idx: usize, label: String) -> Node {
    let children = flat
        .iter()
        .enumerate()
        .filter(|(k, f)| f.parent == idx && *k != idx)
        .map(|(k, f)| build(flat, k, f.label.clone()))
        .collect();
    Node { label, children, line: flat.get(idx).map_or(0, |f| f.line) }
}

/// Árbol desde las líneas del cuerpo.
fn tree(title: &str, body: &[String]) -> Node {
    let mut root_label = plain_label(title);
    // El 0 es el centro.
    let mut flat = vec![Flat { label: String::new(), parent: 0, depth: 0, line: 0 }];
    // Pila de (sangría, índice en `flat`).
    let mut stack: Vec<(usize, usize)> = Vec::new();
    for (k, line) in body.iter().enumerate() {
        // Línea del bloque (la 0 es la del marcador).
        let at = k + 1;
        if flat.len() >= MAX_NODES {
            break;
        }
        if line.trim().is_empty() {
            continue;
        }
        let (indent, text, is_item) = match list_item(line) {
            Some((i, t)) => (i, t, true),
            None => (0, line.as_str(), false),
        };
        let label = plain_label(text);
        if label.is_empty() {
            continue;
        }
        if root_label.is_empty() && !is_item {
            root_label = label;
            if let Some(r) = flat.first_mut() {
                r.line = at;
            }
            continue;
        }
        if !is_item {
            stack.clear();
        }
        while stack.last().is_some_and(|(i, _)| *i >= indent) {
            stack.pop();
        }
        let parent = stack.last().map_or(0, |(_, p)| *p);
        let depth = flat.get(parent).map_or(0, |f| f.depth) + 1;
        if depth > MAX_DEPTH {
            continue;
        }
        flat.push(Flat { label, parent, depth, line: at });
        if is_item {
            stack.push((indent, flat.len() - 1));
        }
    }
    build(&flat, 0, root_label)
}

/// Mapas mentales de la nota, fuera de los bloques de código. Como mucho [`MAX_MAPS`].
pub fn find(md: &str) -> Vec<Block> {
    let mut out = Vec::new();
    let mut fence: Option<String> = None;
    let mut lines = Vec::new();
    let mut pos = 0usize;
    for l in md.split_inclusive('\n') {
        lines.push((pos, l));
        pos += l.len();
    }
    let mut i = 0usize;
    while let Some(&(start, line)) = lines.get(i) {
        let text = line.trim_end_matches(['\n', '\r']);
        let t = text.trim_start();
        if let Some(f) = &fence {
            if t.starts_with(f.as_str()) {
                fence = None;
            }
            i += 1;
            continue;
        }
        if t.starts_with("```") || t.starts_with("~~~") {
            fence = Some(t.chars().take(3).collect());
            i += 1;
            continue;
        }
        let Some((meta, title)) = unquote(text).and_then(marker) else {
            i += 1;
            continue;
        };
        let mut body = Vec::new();
        let mut j = i + 1;
        while let Some(&(_, l)) = lines.get(j) {
            match unquote(l.trim_end_matches(['\n', '\r'])) {
                Some(b) => body.push(b.to_string()),
                None => break,
            }
            j += 1;
        }
        let end = lines.get(j).map_or(md.len(), |(p, _)| *p);
        let words: Vec<&str> = meta.split_whitespace().collect();
        let view = if words.contains(&"map") { View::MapOnly } else { View::ListAndMap };
        let options = MapOptions {
            layout: words.iter().find_map(|w| Layout::from_token(w)),
            shape: words.iter().find_map(|w| Shape::from_token(w)).unwrap_or_default(),
            root: words.iter().find_map(|w| RootSize::from_token(w)).unwrap_or_default(),
            palette: words.iter().find_map(|w| Palette::from_token(w)).unwrap_or_default(),
        };
        let root = tree(&title, &body);
        out.push(Block { range: start..end, view, options, root, body });
        if out.len() >= MAX_MAPS {
            break;
        }
        i = j;
    }
    out
}

/// S1.19–S1.20: cambia las opciones de dibujo del mapa `n` (en el orden de [`find`]) y devuelve
/// el Markdown nuevo. Solo se reescribe el metadato del marcador: la forma va primero (IureEditor
/// toma como forma la primera palabra que no sea vista ni `links`), luego lo que ya había (vista,
/// `links`, palabras desconocidas, tal cual) y al final distribución, tamaño del centro y paleta.
/// Lo que está en su valor de omisión no se escribe. `None` si no hay mapa `n` o nada cambia.
pub fn set_options(md: &str, n: usize, opts: &MapOptions) -> Option<String> {
    let block = find(md).into_iter().nth(n)?;
    let line_end = md.get(block.range.clone())?.find('\n').map_or(block.range.end, |e| block.range.start + e);
    let line = md.get(block.range.start..line_end)?;
    let open = line.find("[!")? + 2;
    let close = open + line.get(open..)?.find(']')?;
    let inner = line.get(open..close)?;
    let (kind, meta) = inner.split_once('|').unwrap_or((inner, ""));
    let ours = |w: &str| {
        Layout::from_token(w).is_some()
            || Shape::from_token(w).is_some()
            || RootSize::from_token(w).is_some()
            || Palette::from_token(w).is_some()
    };
    let mut words: Vec<&str> = Vec::new();
    if opts.shape != Shape::Default {
        words.push(opts.shape.token());
    }
    words.extend(meta.split_whitespace().filter(|w| !ours(w)));
    words.extend(opts.layout.map(Layout::token));
    words.extend(opts.root.token());
    words.extend(opts.palette.token());
    let new_inner = if words.is_empty() { kind.to_string() } else { format!("{kind}|{}", words.join(" ")) };
    if new_inner == inner {
        return None;
    }
    let at = block.range.start + open;
    let mut out = String::with_capacity(md.len() + 24);
    out.push_str(md.get(..at)?);
    out.push_str(&new_inner);
    out.push_str(md.get(block.range.start + close..)?);
    Some(out)
}

/// Colores de las ramas (fondo; el texto va en blanco). Contraste AA con blanco.
pub(crate) const BRANCH: [[u8; 3]; 6] = [
    [0x1d, 0x4e, 0xd8],
    [0x47, 0x55, 0x69],
    [0x99, 0x1b, 0x1b],
    [0x15, 0x80, 0x3d],
    [0x6d, 0x28, 0xd9],
    [0x0f, 0x76, 0x6e],
];

fn hex(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Un nodo ya colocado: centro (`x`, `y`), ancho y alto, líneas de texto y su letra.
struct Placed {
    depth: usize,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    lines: Vec<String>,
    font: f32,
    color: [u8; 3],
    parent: Option<usize>,
    /// -1: rama a la izquierda del centro; 1: a la derecha o abajo.
    side: f32,
    /// Línea del bloque del nodo.
    line: usize,
}

/// Medidas del dibujo, según la letra de la nota, la forma y el centro.
struct Metrics {
    font: f32,
    char_w: f32,
    /// Distancia entre filas (horizontal) o entre niveles (vertical).
    row: f32,
    node_h: f32,
    gap: f32,
    pad: f32,
    shape: Shape,
    palette: Palette,
    /// El centro: líneas, letra, ancho y alto.
    root_lines: Vec<String>,
    root_font: f32,
    root_w: f32,
    root_h: f32,
}

/// Parte `text` en líneas de `max` caracteres como mucho (por palabras; una palabra más larga se
/// corta), con `lines` líneas como mucho (la última termina en «…» si sobra texto).
fn wrap(text: &str, max: usize, lines: usize) -> Vec<String> {
    let max = max.max(4);
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cut = false;
    for word in text.split_whitespace() {
        let mut word: Vec<char> = word.chars().collect();
        while !word.is_empty() {
            let len = cur.chars().count();
            let room = if len == 0 { max } else { max.saturating_sub(len + 1) };
            if word.len() <= room {
                if len > 0 {
                    cur.push(' ');
                }
                cur.extend(word.drain(..));
            } else if len == 0 {
                cur.extend(word.drain(..max.min(word.len())));
            } else {
                out.push(std::mem::take(&mut cur));
            }
            if out.len() >= lines {
                cut = true;
                break;
            }
        }
        if cut {
            break;
        }
    }
    if !cur.is_empty() && out.len() < lines {
        out.push(cur);
    } else if !cur.is_empty() {
        cut = true;
    }
    if cut && let Some(last) = out.last_mut() {
        let mut l: String = last.chars().take(max.saturating_sub(1)).collect();
        l.push('…');
        *last = l;
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

impl Metrics {
    fn new(style: &NoteStyle, look: &Look, root_label: &str) -> Self {
        let font = style.font_px.clamp(10.0, 24.0);
        let node_h = font * 1.9;
        // Las formas que sobresalen del rectángulo (nube, estrella, elipse) piden más aire entre filas.
        let row = font
            * match look.shape {
                Shape::Cloud | Shape::Bang => 3.4,
                Shape::Circle => 3.0,
                _ => 2.6,
            };
        let (scale, chars, max_lines) = look.root.metrics(root_label.chars().count());
        let root_lines = wrap(root_label, chars, max_lines);
        let root_font = font * scale;
        let longest = root_lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
        let mut m = Self {
            font,
            char_w: font * 0.58,
            row,
            node_h,
            gap: 44.0,
            pad: 12.0,
            shape: look.shape,
            palette: look.palette,
            root_lines,
            root_font,
            root_w: 0.0,
            root_h: 0.0,
        };
        let line_h = root_font * 1.25;
        m.root_w = (longest as f32 * root_font * 0.6 + 32.0 + m.extra()).clamp(56.0, 520.0);
        m.root_h = node_h.max(m.root_lines.len() as f32 * line_h + root_font * 0.9);
        m
    }

    /// Relleno horizontal que pide la forma.
    fn extra(&self) -> f32 {
        match self.shape {
            Shape::Default | Shape::Square | Shape::Rounded => 0.0,
            Shape::Hexagon => self.node_h * 0.6,
            Shape::Circle | Shape::Cloud => self.node_h * 0.7,
            Shape::Bang => self.node_h * 1.1,
        }
    }

    /// Ancho de un nodo (no el centro): el texto más el relleno que pide la forma.
    fn width(&self, label: &str) -> f32 {
        (label.chars().count() as f32 * self.char_w + 28.0 + self.extra()).clamp(48.0, 460.0)
    }

    /// Ancho del nodo a esa profundidad.
    fn node_w(&self, depth: usize, label: &str) -> f32 {
        if depth == 0 { self.root_w } else { self.width(label) }
    }

    /// Color de la rama `k` del centro.
    fn branch(&self, k: usize) -> [u8; 3] {
        let c = self.palette.colors();
        c.get(k % c.len()).copied().unwrap_or([0x47, 0x55, 0x69])
    }

    /// Un nodo (no el centro) listo para colocar.
    fn node(&self, n: &Node, depth: usize, color: [u8; 3], parent: Option<usize>, side: f32) -> Placed {
        if depth == 0 {
            return self.root(n, color);
        }
        Placed {
            depth,
            x: 0.0,
            y: 0.0,
            w: self.width(&n.label),
            h: self.node_h,
            lines: vec![n.label.clone()],
            font: self.font,
            color,
            parent,
            side,
            line: n.line,
        }
    }

    /// El centro listo para colocar en (0, 0).
    fn root(&self, n: &Node, color: [u8; 3]) -> Placed {
        Placed {
            depth: 0,
            x: 0.0,
            y: 0.0,
            w: self.root_w,
            h: self.root_h,
            lines: self.root_lines.clone(),
            font: self.root_font,
            color,
            parent: None,
            side: 1.0,
            line: n.line,
        }
    }
}

/// Color del centro.
const CENTER: [u8; 3] = [0x64, 0x74, 0x8b];

/// Texto oscuro o blanco según lo claro del fondo.
fn ink(c: [u8; 3]) -> [u8; 3] {
    let l = 0.2126 * f32::from(c[0]) + 0.7152 * f32::from(c[1]) + 0.0722 * f32::from(c[2]);
    if l > 150.0 { [0x1f, 0x23, 0x28] } else { [0xff, 0xff, 0xff] }
}

/// Color de la línea de una rama: el mismo; si el color es claro, más oscuro (que se vea).
fn edge_color(c: [u8; 3]) -> [u8; 3] {
    if ink(c) == [0xff, 0xff, 0xff] { c } else { c.map(|v| (f32::from(v) * 0.55) as u8) }
}

/// Horizontal: hojas una debajo de otra, cada padre a la altura media de sus hijos. Deja la `y`.
#[allow(clippy::too_many_arguments)]
fn place_rows(
    n: &Node,
    depth: usize,
    color: [u8; 3],
    parent: Option<usize>,
    side: f32,
    m: &Metrics,
    next_y: &mut f32,
    out: &mut Vec<Placed>,
) -> f32 {
    let idx = out.len();
    out.push(m.node(n, depth, color, parent, side));
    let y = if n.children.is_empty() {
        let y = *next_y;
        *next_y += m.row;
        y
    } else {
        let mut first = None;
        let mut last = 0.0;
        for c in &n.children {
            let y = place_rows(c, depth + 1, color, Some(idx), side, m, next_y, out);
            first.get_or_insert(y);
            last = y;
        }
        (first.unwrap_or(last) + last) / 2.0
    };
    if let Some(p) = out.get_mut(idx) {
        p.y = y;
    }
    y
}

/// Hojas del subárbol (para repartir las ramas entre los dos lados o el círculo).
fn leaves(n: &Node) -> usize {
    if n.children.is_empty() { 1 } else { n.children.iter().map(leaves).sum::<usize>().max(1) }
}

/// Columnas horizontales: la `x` de cada nodo según su nivel y su lado. El centro en `x = 0`.
fn columns(placed: &mut [Placed], m: &Metrics) {
    let depth = placed.iter().map(|p| p.depth).max().unwrap_or(0);
    for side in [1.0_f32, -1.0] {
        // Borde interior de cada columna (distancia desde el centro).
        let mut edge = m.root_w / 2.0 + m.gap;
        for d in 1..=depth {
            let widest = placed.iter().filter(|p| p.depth == d && p.side == side).map(|p| p.w).fold(0.0_f32, f32::max);
            for p in placed.iter_mut().filter(|p| p.depth == d && p.side == side) {
                p.x = side * (edge + p.w / 2.0);
            }
            edge += widest + m.gap;
        }
    }
}

/// Coloca el árbol con la distribución horizontal (`Right` o `Balanced`).
fn layout_rows(root: &Node, balanced: bool, m: &Metrics) -> Vec<Placed> {
    let mut placed = vec![m.root(root, CENTER)];
    // Lado de cada rama: en equilibrado, las primeras a la derecha hasta la mitad de las hojas.
    let total: usize = root.children.iter().map(leaves).sum();
    let mut acc = 0usize;
    let sides: Vec<f32> = root
        .children
        .iter()
        .map(|c| {
            let side = if balanced && acc * 2 >= total && acc > 0 { -1.0 } else { 1.0 };
            acc += leaves(c);
            side
        })
        .collect();
    let mut spans = [(f32::MAX, f32::MIN); 2];
    for side in [1.0_f32, -1.0] {
        let mut next_y = 0.0_f32;
        let from = placed.len();
        for (k, c) in root.children.iter().enumerate() {
            if sides.get(k).copied() == Some(side) {
                place_rows(c, 1, m.branch(k), Some(0), side, m, &mut next_y, &mut placed);
            }
        }
        let Some(span) = spans.get_mut(usize::from(side < 0.0)) else { continue };
        for p in placed.get(from..).unwrap_or_default().iter().filter(|p| p.depth == 1) {
            span.0 = span.0.min(p.y);
            span.1 = span.1.max(p.y);
        }
    }
    // Cada lado, centrado en el centro (y = 0).
    for (i, side) in [1.0_f32, -1.0].into_iter().enumerate() {
        let Some(&(lo, hi)) = spans.get(i) else { continue };
        if lo > hi {
            continue;
        }
        let shift = (lo + hi) / 2.0;
        for p in placed.iter_mut().filter(|p| p.depth > 0 && p.side == side) {
            p.y -= shift;
        }
    }
    columns(&mut placed, m);
    placed
}

/// Ancho del subárbol en la distribución vertical.
fn extent(n: &Node, m: &Metrics, depth: usize) -> f32 {
    let own = m.node_w(depth, &n.label);
    if n.children.is_empty() || depth > MAX_DEPTH + 1 {
        return own;
    }
    let kids: f32 = n.children.iter().map(|c| extent(c, m, depth + 1)).sum::<f32>()
        + m.gap / 2.0 * n.children.len().saturating_sub(1) as f32;
    own.max(kids)
}

/// `y` del nivel `depth` en la distribución vertical (el centro puede ser más alto).
fn level_y(depth: usize, m: &Metrics) -> f32 {
    if depth == 0 { 0.0 } else { m.root_h / 2.0 + m.gap + m.node_h / 2.0 + (depth - 1) as f32 * (m.node_h + m.gap) }
}

/// Vertical: cada subárbol ocupa su ancho; el padre, centrado sobre sus hijos.
#[allow(clippy::too_many_arguments)]
fn place_down(
    n: &Node,
    depth: usize,
    color: [u8; 3],
    parent: Option<usize>,
    left: f32,
    m: &Metrics,
    out: &mut Vec<Placed>,
) -> f32 {
    let idx = out.len();
    let mut node = m.node(n, depth, color, parent, 1.0);
    node.y = level_y(depth, m);
    out.push(node);
    let ext = extent(n, m, depth);
    let kids: f32 = n.children.iter().map(|c| extent(c, m, depth + 1)).sum::<f32>()
        + m.gap / 2.0 * n.children.len().saturating_sub(1) as f32;
    let mut x = left + (ext - kids).max(0.0) / 2.0;
    let mut first = None;
    let mut last = 0.0;
    for (k, c) in n.children.iter().enumerate() {
        let col = if depth == 0 { m.branch(k) } else { color };
        let cx = place_down(c, depth + 1, col, Some(idx), x, m, out);
        first.get_or_insert(cx);
        last = cx;
        x += extent(c, m, depth + 1) + m.gap / 2.0;
    }
    let cx = match first {
        Some(f) => (f + last) / 2.0,
        None => left + ext / 2.0,
    };
    if let Some(p) = out.get_mut(idx) {
        p.x = cx;
    }
    cx
}

/// Radial: cada rama recibe un sector del círculo proporcional a sus hojas; cada nivel es un
/// anillo. Los anillos se alejan hasta que ningún nodo se encima con otro (ni con el centro).
fn layout_radial(root: &Node, m: &Metrics) -> Vec<Placed> {
    use std::f32::consts::{FRAC_PI_2, TAU};
    let mut placed = vec![m.root(root, CENTER)];
    let mut angles = vec![0.0_f32];
    #[allow(clippy::too_many_arguments)]
    fn assign(
        n: &Node,
        depth: usize,
        a0: f32,
        a1: f32,
        color: [u8; 3],
        parent: usize,
        m: &Metrics,
        placed: &mut Vec<Placed>,
        angles: &mut Vec<f32>,
    ) {
        let idx = placed.len();
        placed.push(m.node(n, depth, color, Some(parent), 1.0));
        angles.push((a0 + a1) / 2.0);
        let total = leaves(n) as f32;
        let mut a = a0;
        for c in &n.children {
            let span = (a1 - a0) * leaves(c) as f32 / total;
            assign(c, depth + 1, a, a + span, color, idx, m, placed, angles);
            a += span;
        }
    }
    let total = leaves(root) as f32;
    let mut a = -FRAC_PI_2;
    for (k, c) in root.children.iter().enumerate() {
        let span = TAU * leaves(c) as f32 / total;
        assign(c, 1, a, a + span, m.branch(k), 0, m, &mut placed, &mut angles);
        a += span;
    }
    let max_depth = placed.iter().map(|p| p.depth).max().unwrap_or(0);
    let widest = |d: usize| placed.iter().filter(|p| p.depth == d).map(|p| p.w).fold(0.0_f32, f32::max);
    let mut radius = vec![0.0_f32; max_depth + 1];
    for d in 1..=max_depth {
        let prev = radius.get(d - 1).copied().unwrap_or(0.0);
        let inner = if d == 1 { m.root_w.max(m.root_h) / 2.0 } else { widest(d - 1) * 0.35 };
        if let Some(r) = radius.get_mut(d) {
            *r = prev + inner + widest(d) * 0.35 + m.gap;
        }
    }
    let margin = 8.0;
    for _ in 0..48 {
        for (p, ang) in placed.iter_mut().zip(&angles) {
            let r = radius.get(p.depth).copied().unwrap_or(0.0);
            p.x = r * ang.cos();
            p.y = r * ang.sin();
        }
        // El primer par que se encima: se alejan los anillos desde el más externo de los dos.
        let mut hit = None;
        'outer: for (i, a) in placed.iter().enumerate() {
            for b in placed.iter().skip(i + 1) {
                if (a.x - b.x).abs() * 2.0 < a.w + b.w + margin * 2.0 && (a.y - b.y).abs() * 2.0 < a.h + b.h + margin {
                    hit = Some(a.depth.max(b.depth).max(1));
                    break 'outer;
                }
            }
        }
        let Some(d) = hit else { break };
        let step = (radius.get(d).copied().unwrap_or(0.0) * 0.12).max(m.node_h * 0.6);
        for r in radius.iter_mut().skip(d) {
            *r += step;
        }
    }
    placed
}

/// Figura de un nodo de ancho `w` centrado en (`cx`, `cy`).
#[allow(clippy::too_many_arguments)]
fn shape_svg(s: &mut String, shape: Shape, cx: f32, cy: f32, w: f32, h: f32, fill: &str, stroke: &str) {
    let (x, y) = (cx - w / 2.0, cy - h / 2.0);
    let paint = format!("fill=\"{fill}\" stroke=\"{stroke}\" stroke-width=\"1.5\"");
    let _ = match shape {
        Shape::Default | Shape::Square | Shape::Rounded => {
            let rx = match shape {
                Shape::Square => 2.0,
                Shape::Rounded => h.min(40.0) * 0.28,
                _ => h.min(w) / 2.0,
            };
            write!(s, "<rect x=\"{x:.1}\" y=\"{y:.1}\" width=\"{w:.1}\" height=\"{h:.1}\" rx=\"{rx:.1}\" {paint}/>")
        }
        Shape::Circle => {
            write!(s, "<ellipse cx=\"{cx:.1}\" cy=\"{cy:.1}\" rx=\"{:.1}\" ry=\"{:.1}\" {paint}/>", w / 2.0, h * 0.62)
        }
        Shape::Hexagon => {
            let i = h.min(w / 2.0) / 2.0;
            write!(
                s,
                "<polygon points=\"{:.1},{y:.1} {:.1},{y:.1} {:.1},{cy:.1} {:.1},{:.1} {:.1},{:.1} {x:.1},{cy:.1}\" {paint}/>",
                x + i,
                x + w - i,
                x + w,
                x + w - i,
                y + h,
                x + i,
                y + h
            )
        }
        Shape::Cloud => {
            // Puntos repartidos por igual sobre el contorno de una píldora, unidos por arcos hacia
            // fuera: bultos del mismo tamaño en todo el borde.
            let a = (h * 0.55).min(w / 2.0);
            let straight = (w - 2.0 * a).max(0.0);
            let per = 2.0 * straight + std::f32::consts::TAU * a;
            let n = ((per / (h.min(48.0) * 0.75)).round() as usize).clamp(8, 40);
            let (left, right) = (cx - straight / 2.0, cx + straight / 2.0);
            let pt = |k: usize| {
                let mut d = per * (k % n) as f32 / n as f32;
                if d < straight {
                    return (left + d, cy - a);
                }
                d -= straight;
                let half = std::f32::consts::PI * a;
                if d < half {
                    let t = -std::f32::consts::FRAC_PI_2 + d / a;
                    return (right + a * t.cos(), cy + a * t.sin());
                }
                d -= half;
                if d < straight {
                    return (right - d, cy + a);
                }
                d -= straight;
                let t = std::f32::consts::FRAC_PI_2 + d / a;
                (left + a * t.cos(), cy + a * t.sin())
            };
            let (x0, y0) = pt(0);
            let mut d = format!("M{x0:.1} {y0:.1}");
            for k in 1..=n {
                let (px, py) = pt(k - 1);
                let (qx, qy) = pt(k);
                let r = ((qx - px).hypot(qy - py) * 0.6).max(2.0);
                let _ = write!(d, " A{r:.1} {r:.1} 0 0 1 {qx:.1} {qy:.1}");
            }
            write!(s, "<path d=\"{d} Z\" {paint}/>")
        }
        Shape::Bang => {
            // Estrella: picos sobre una elipse exterior y valles sobre una interior.
            let n = ((w / (h.min(48.0) * 0.45)).round() as usize).clamp(8, 32);
            let mut pts = String::new();
            for k in 0..n * 2 {
                let a = std::f32::consts::PI * k as f32 / n as f32;
                let dip = h.min(48.0) * 0.3;
                let (rx, ry) = if k % 2 == 0 { (w / 2.0, h * 0.75) } else { (w / 2.0 - dip, h * 0.75 - dip) };
                let _ = write!(pts, "{:.1},{:.1} ", cx + rx * a.cos(), cy + ry * a.sin());
            }
            write!(s, "<polygon points=\"{}\" {paint}/>", pts.trim_end())
        }
    };
}

/// Cómo se dibujan las líneas.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Edges {
    /// Curvas de lado a lado (derecha, ambos lados).
    Horizontal,
    /// Curvas de abajo hacia arriba (organigrama).
    Vertical,
    /// Rectas de borde a borde (radial).
    Straight,
}

/// Punto del borde de la caja (`w` × `h`) en dirección (`dx`, `dy`) desde su centro.
fn border(w: f32, h: f32, dx: f32, dy: f32) -> (f32, f32) {
    let len = dx.hypot(dy);
    if len < f32::EPSILON {
        return (0.0, 0.0);
    }
    let (ux, uy) = (dx / len, dy / len);
    let tx = if ux.abs() > f32::EPSILON { (w / 2.0) / ux.abs() } else { f32::MAX };
    let ty = if uy.abs() > f32::EPSILON { (h / 2.0) / uy.abs() } else { f32::MAX };
    let t = tx.min(ty);
    (ux * t, uy * t)
}

/// SVG de un árbol ya colocado (coordenadas relativas; aquí se encuadran).
/// Encuadre del dibujo: desplazamiento de los nodos y tamaño del SVG.
fn frame(placed: &[Placed], m: &Metrics) -> (f32, f32, f32, f32) {
    // Margen extra para las formas que sobresalen (nube, estrella, elipse).
    let bleed = match m.shape {
        Shape::Bang => m.node_h * 0.3,
        Shape::Cloud | Shape::Circle => m.node_h * 0.25,
        _ => 0.0,
    };
    let min_x = placed.iter().map(|p| p.x - p.w / 2.0).fold(f32::MAX, f32::min);
    let max_x = placed.iter().map(|p| p.x + p.w / 2.0).fold(f32::MIN, f32::max);
    let min_y = placed.iter().map(|p| p.y - p.h / 2.0).fold(f32::MAX, f32::min);
    let max_y = placed.iter().map(|p| p.y + p.h / 2.0).fold(f32::MIN, f32::max);
    let (ox, oy) = (m.pad + bleed - min_x, m.pad + bleed - min_y);
    let width = (max_x - min_x + 2.0 * (m.pad + bleed)).clamp(1.0, 4_000.0);
    let height = (max_y - min_y + 2.0 * (m.pad + bleed)).clamp(1.0, 8_000.0);
    (ox, oy, width, height)
}

fn paint(placed: &[Placed], m: &Metrics, style: &NoteStyle, edges: Edges) -> String {
    let (ox, oy, width, height) = frame(placed, m);
    let font = m.font;
    let mut s = String::with_capacity(placed.len() * 360 + 512);
    let _ = write!(
        s,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width:.0}\" height=\"{height:.0}\" viewBox=\"0 0 {width:.0} {height:.0}\" font-family=\"sans-serif\" font-size=\"{font:.1}\">"
    );
    // Conectores primero (debajo de los nodos).
    for p in placed {
        let Some(par) = p.parent.and_then(|i| placed.get(i)) else { continue };
        let stroke = format!(
            "fill=\"none\" stroke=\"{}\" stroke-width=\"{:.1}\" stroke-opacity=\"0.85\" stroke-linecap=\"round\"",
            hex(edge_color(p.color)),
            if p.depth == 1 { 3.0 } else { 2.0 }
        );
        let _ = match edges {
            Edges::Vertical => {
                let (x1, y1) = (par.x + ox, par.y + oy + par.h / 2.0);
                let (x2, y2) = (p.x + ox, p.y + oy - p.h / 2.0);
                let my = (y1 + y2) / 2.0;
                write!(s, "<path d=\"M{x1:.1} {y1:.1} C{x1:.1} {my:.1} {x2:.1} {my:.1} {x2:.1} {y2:.1}\" {stroke}/>")
            }
            Edges::Horizontal => {
                let x1 = par.x + ox + p.side * par.w / 2.0;
                let x2 = p.x + ox - p.side * p.w / 2.0;
                let (y1, y2) = (par.y + oy, p.y + oy);
                let mx = (x1 + x2) / 2.0;
                write!(s, "<path d=\"M{x1:.1} {y1:.1} C{mx:.1} {y1:.1} {mx:.1} {y2:.1} {x2:.1} {y2:.1}\" {stroke}/>")
            }
            Edges::Straight => {
                let (dx, dy) = (p.x - par.x, p.y - par.y);
                let (bx, by) = border(par.w, par.h, dx, dy);
                let (cx, cy) = border(p.w, p.h, -dx, -dy);
                let (x1, y1) = (par.x + ox + bx, par.y + oy + by);
                let (x2, y2) = (p.x + ox + cx, p.y + oy + cy);
                write!(s, "<path d=\"M{x1:.1} {y1:.1} L{x2:.1} {y2:.1}\" {stroke}/>")
            }
        };
    }
    for p in placed {
        let (cx, cy) = (p.x + ox, p.y + oy);
        let (fill, text) = if p.depth == 0 { (style.code_bg, style.text) } else { (p.color, ink(p.color)) };
        let stroke = if p.depth == 0 { hex(style.border) } else { hex(edge_color(p.color)) };
        shape_svg(&mut s, m.shape, cx, cy, p.w, p.h, &hex(fill), &stroke);
        let weight = if p.depth <= 1 { " font-weight=\"600\"" } else { "" };
        let size = if (p.font - font).abs() > 0.05 { format!(" font-size=\"{:.1}\"", p.font) } else { String::new() };
        if let [line] = p.lines.as_slice() {
            let _ = write!(
                s,
                "<text x=\"{cx:.1}\" y=\"{cy:.1}\" fill=\"{}\" text-anchor=\"middle\" dominant-baseline=\"central\"{weight}{size}>{}</text>",
                hex(text),
                esc(line)
            );
        } else {
            let line_h = p.font * 1.25;
            let top = cy - (p.lines.len().saturating_sub(1)) as f32 * line_h / 2.0;
            let _ = write!(
                s,
                "<text fill=\"{}\" text-anchor=\"middle\" dominant-baseline=\"central\"{weight}{size}>",
                hex(text)
            );
            for (i, line) in p.lines.iter().enumerate() {
                let _ = write!(s, "<tspan x=\"{cx:.1}\" y=\"{:.1}\">{}</tspan>", top + i as f32 * line_h, esc(line));
            }
            s.push_str("</text>");
        }
    }
    s.push_str("</svg>");
    s
}

/// SVG del mapa con los colores y la letra de la nota y el aspecto pedido. Todo dibujo propio
/// (el radial también: el de `mermaid-svg` tapaba las líneas con un disco central enorme). Ancho
/// y alto acotados.
pub fn draw(root: &Node, style: &NoteStyle, look: &Look) -> String {
    draw_boxed(root, style, look).0
}

/// Como [`draw`], con dónde quedó cada nodo (para pulsarlos y editar el mapa desde el dibujo).
pub fn draw_boxed(root: &Node, style: &NoteStyle, look: &Look) -> (String, Vec<NodeBox>) {
    let m = Metrics::new(style, look, &root.label);
    let (placed, edges) = match look.layout {
        Layout::Down => {
            let mut placed = Vec::new();
            place_down(root, 0, CENTER, None, 0.0, &m, &mut placed);
            (placed, Edges::Vertical)
        }
        Layout::Balanced => (layout_rows(root, true, &m), Edges::Horizontal),
        Layout::Right => (layout_rows(root, false, &m), Edges::Horizontal),
        Layout::Radial => (layout_radial(root, &m), Edges::Straight),
    };
    let (ox, oy, _, _) = frame(&placed, &m);
    let boxes = placed
        .iter()
        .map(|p| NodeBox {
            line: p.line,
            depth: p.depth,
            rect: [p.x + ox - p.w / 2.0, p.y + oy - p.h / 2.0, p.x + ox + p.w / 2.0, p.y + oy + p.h / 2.0],
        })
        .collect();
    (paint(&placed, &m, style, edges), boxes)
}

/// Como [`draw`] a la derecha, con píldoras y lo de omisión (lo de antes de S1.19).
pub fn svg(root: &Node, style: &NoteStyle) -> String {
    draw(root, style, &Look::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTE: &str = "# titulo\n\n> Veamos diagrama\n\n> [!MINDMAP|map] Idea central\n> - Analisis\n> - Diseño\n>   - **Bocetos**\n> - [Pruebas](https://x)\n>\n> - Pre-análisis\n\nY ahora\n";

    #[test]
    fn nodes_know_their_line_and_where_they_are_drawn() {
        let md = "> [!MINDMAP] Centro\n> - Uno\n>   - Uno.a\n> - Dos\n";
        let b = find(md);
        let root = &b[0].root;
        assert_eq!(root.line, 0);
        assert_eq!(root.children.iter().map(|c| c.line).collect::<Vec<_>>(), [1, 3]);
        assert_eq!(root.children[0].children[0].line, 2);
        let style = crate::markdown::NoteStyle::default();
        for layout in Layout::ALL {
            let look = Look { layout, ..Look::default() };
            let (svg, boxes) = draw_boxed(root, &style, &look);
            let (w, h) = crate::diagram::svg_size(&svg).unwrap();
            let mut lines: Vec<usize> = boxes.iter().map(|b| b.line).collect();
            lines.sort();
            assert_eq!(lines, [0, 1, 2, 3], "{layout:?}");
            for bx in &boxes {
                assert!(
                    bx.rect[0] >= 0.0 && bx.rect[1] >= 0.0 && bx.rect[2] <= w + 0.5 && bx.rect[3] <= h + 0.5,
                    "{layout:?} {bx:?}"
                );
            }
        }
    }

    #[test]
    fn finds_the_owner_note_map() {
        let b = find(NOTE);
        assert_eq!(b.len(), 1);
        let m = &b[0];
        assert_eq!(m.view, View::MapOnly);
        assert_eq!(m.root.label, "Idea central");
        let kids: Vec<&str> = m.root.children.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(kids, vec!["Analisis", "Diseño", "Pruebas", "Pre-análisis"]);
        assert_eq!(m.root.children[1].children[0].label, "Bocetos");
        let span = NOTE.get(m.range.clone()).unwrap();
        assert!(span.starts_with("> [!MINDMAP") && span.ends_with("> - Pre-análisis\n"), "{span:?}");
        // La cita de antes no es un mapa.
        assert!(!span.contains("Veamos"));
    }

    #[test]
    fn root_fallbacks_and_views() {
        let b = find("> [!mindmap]\n> Centro\n> - a\n");
        assert_eq!((b[0].root.label.as_str(), b[0].view), ("Centro", View::ListAndMap));
        assert_eq!(b[0].root.children.len(), 1);
        let b = find("> [!MINDMAP|right bang]\n> - a\n");
        assert_eq!(b[0].root.label, "", "lo pone quien dibuja");
        assert!(find("> [!NOTE] Ojo\n> - a\n").is_empty());
    }

    #[test]
    fn code_blocks_are_not_maps() {
        assert!(find("```\n> [!MINDMAP] x\n> - a\n```\n").is_empty());
        assert_eq!(find("~~~\nx\n~~~\n> [!MINDMAP] x\n").len(), 1);
    }

    #[test]
    fn labels_are_plain() {
        assert_eq!(plain_label("**negrita** y `código`"), "negrita y código");
        assert_eq!(plain_label("[[Nota|Alias]] y [[Otra]]"), "Alias y Otra");
        assert_eq!(plain_label("[ ] pendiente"), "pendiente");
        assert_eq!(plain_label(&"x".repeat(500)).chars().count(), MAX_LABEL + 1);
    }

    #[test]
    fn svg_is_safe_and_escaped() {
        let root = Node {
            label: "<script>&".into(),
            children: vec![Node { label: "a\"b".into(), children: vec![], line: 1 }],
            line: 0,
        };
        let s = svg(&root, &NoteStyle::default());
        assert!(s.starts_with("<svg") && s.ends_with("</svg>"));
        assert!(!s.contains("<script>") && s.contains("&lt;script&gt;&amp;") && s.contains("a&quot;b"));
        assert!(crate::net::svg_is_safe(s.as_bytes()));
    }

    #[test]
    fn hostile_sizes_are_bounded() {
        let deep: String = (0..300).map(|i| format!("> {}- n{i}\n", "  ".repeat(i))).collect();
        let b = find(&format!("> [!MINDMAP] r\n{deep}"));
        fn depth(n: &Node) -> usize {
            1 + n.children.iter().map(depth).max().unwrap_or(0)
        }
        assert!(depth(&b[0].root) <= MAX_DEPTH + 1);
        let wide: String = (0..5_000).map(|i| format!("> - n{i}\n")).collect();
        let b = find(&format!("> [!MINDMAP] r\n{wide}"));
        assert_eq!(b[0].root.children.len(), MAX_NODES - 1);
        let s = svg(&b[0].root, &NoteStyle::default());
        let h: f32 = s.split("height=\"").nth(1).and_then(|r| r.split('"').next()).unwrap().parse().unwrap();
        assert!(h > 1_000.0 && h <= 8_000.0, "alto acotado: {h}");
        let many: String = (0..100).map(|_| "> [!MINDMAP] r\n> - a\n\n").collect();
        assert_eq!(find(&many).len(), MAX_MAPS);
    }

    #[test]
    fn meta_words_in_any_order() {
        // S1.19–S1.20: forma, vista, `links`, distribución, centro y paleta, en cualquier orden.
        let b = find("> [!MINDMAP|Layout-Radial map CLOUD links root-s palette-warm] X\n> - a\n");
        let o = b[0].options;
        assert_eq!(
            (o.layout, o.shape, o.root, o.palette),
            (Some(Layout::Radial), Shape::Cloud, RootSize::Small, Palette::Warm)
        );
        assert_eq!(b[0].view, View::MapOnly);
        let b = find("> [!MINDMAP|right] X\n");
        assert_eq!((b[0].options, b[0].view), (MapOptions::default(), View::ListAndMap));
        let b = find("> [!MINDMAP|rara layout-both hexagon] X\n");
        assert_eq!((b[0].options.layout, b[0].options.shape), (Some(Layout::Balanced), Shape::Hexagon));
    }

    fn opts(layout: Option<Layout>, shape: Shape) -> MapOptions {
        MapOptions { layout, shape, ..Default::default() }
    }

    #[test]
    fn set_options_rewrites_only_the_meta() {
        let md = "Antes\n\n> [!MINDMAP|map links rara] Centro\n> - a\n\n> [!mindmap] Otro\n> - b\n";
        // La forma va primero (IureEditor la toma de ahí), lo demás se conserva y lo nuestro al final.
        let out = set_options(md, 0, &opts(Some(Layout::Down), Shape::Bang)).unwrap();
        assert_eq!(out, md.replace("[!MINDMAP|map links rara]", "[!MINDMAP|bang map links rara layout-down]"));
        // Cambiar otra vez reemplaza, no acumula; centro y paleta van al final.
        let all = MapOptions {
            layout: Some(Layout::Radial),
            shape: Shape::Square,
            root: RootSize::Large,
            palette: Palette::Mono,
        };
        let again = set_options(&out, 0, &all).unwrap();
        assert!(again.contains("[!MINDMAP|square map links rara layout-radial root-l palette-mono] Centro"), "{again}");
        // Volver a lo de omisión quita las palabras propias.
        let back = set_options(&again, 0, &MapOptions::default()).unwrap();
        assert_eq!(back, md);
        // El segundo mapa (sin metadato, en minúsculas) gana uno sin tocar el resto.
        let second = set_options(md, 1, &opts(Some(Layout::Balanced), Shape::Default)).unwrap();
        assert_eq!(second, md.replace("[!mindmap] Otro", "[!mindmap|layout-both] Otro"));
        // Nada que cambiar o mapa inexistente.
        assert_eq!(set_options(md, 1, &MapOptions::default()), None);
        assert_eq!(set_options(md, 2, &opts(Some(Layout::Down), Shape::Default)), None);
        // Lo que se escribe se vuelve a leer igual.
        assert_eq!(find(&again)[0].options, all);
        assert_eq!(find(&out)[0].root, find(md)[0].root);
    }

    /// Centros de los `<text>` de un SVG propio.
    fn text_centers(svg: &str) -> Vec<(f32, f32)> {
        svg.split("<text x=\"")
            .skip(1)
            .filter_map(|r| {
                let x: f32 = r.split('"').next()?.parse().ok()?;
                let y: f32 = r.split("y=\"").nth(1)?.split('"').next()?.parse().ok()?;
                Some((x, y))
            })
            .collect()
    }

    fn sample() -> Node {
        let leaf = |l: &str| Node { label: l.into(), children: vec![], line: 0 };
        Node {
            label: "Centro".into(),
            children: vec![
                Node { label: "Uno".into(), children: vec![leaf("a"), leaf("b")], line: 0 },
                Node { label: "Dos".into(), children: vec![leaf("c")], line: 0 },
                Node { label: "Tres".into(), children: vec![leaf("d"), leaf("e")], line: 0 },
                leaf("Cuatro"),
            ],
            line: 0,
        }
    }

    fn look(layout: Layout, shape: Shape) -> Look {
        Look { layout, shape, ..Default::default() }
    }

    #[test]
    fn layouts_place_branches_where_asked() {
        let st = NoteStyle::default();
        let root = sample();
        let centers = |l| text_centers(&draw(&root, &st, &look(l, Shape::Default)));
        // A la derecha: todo a la derecha del centro.
        let r = centers(Layout::Right);
        assert_eq!(r.len(), 10);
        assert!(r.iter().skip(1).all(|(x, _)| *x > r[0].0));
        // Equilibrado: ramas a ambos lados.
        let b = centers(Layout::Balanced);
        assert!(b.iter().any(|(x, _)| *x < b[0].0) && b.iter().any(|(x, _)| *x > b[0].0), "{b:?}");
        // Abajo: el centro arriba de todo, los hijos debajo y sin encimarse en su fila.
        let d = centers(Layout::Down);
        assert!(d.iter().skip(1).all(|(_, y)| *y > d[0].1));
        let mut leaves: Vec<f32> = d.iter().filter(|(_, y)| *y > d[1].1 + 1.0).map(|(x, _)| *x).collect();
        leaves.sort_by(f32::total_cmp);
        assert!(leaves.windows(2).all(|w| w[1] - w[0] > 20.0), "{leaves:?}");
        // Radial: ramas alrededor del centro (arriba, abajo, izquierda y derecha).
        let rad = centers(Layout::Radial);
        let (cx, cy) = rad[0];
        assert!(rad.iter().any(|(x, _)| *x < cx - 20.0) && rad.iter().any(|(x, _)| *x > cx + 20.0), "{rad:?}");
        assert!(rad.iter().any(|(_, y)| *y < cy - 20.0) && rad.iter().any(|(_, y)| *y > cy + 20.0), "{rad:?}");
    }

    /// Cajas (x0, y0, x1, y1) de los `<rect>` de un SVG propio con píldoras.
    fn rects(svg: &str) -> Vec<(f32, f32, f32, f32)> {
        svg.split("<rect ")
            .skip(1)
            .filter_map(|r| {
                let a =
                    |n: &str| -> Option<f32> { r.split(&format!("{n}=\"")).nth(1)?.split('"').next()?.parse().ok() };
                let (x, y, w, h) = (a("x")?, a("y")?, a("width")?, a("height")?);
                Some((x, y, x + w, y + h))
            })
            .collect()
    }

    #[test]
    fn radial_nodes_never_overlap_even_with_a_long_center() {
        // S1.21: el disco central de `mermaid-svg` tapaba las líneas; aquí nada se encima.
        let mut root = sample();
        root.label = "Lanzamiento de IureDesk para despachos jurídicos en México y España".into();
        for c in &mut root.children {
            c.children.extend((0..4).map(|i| Node { label: format!("Detalle número {i}"), children: vec![], line: 0 }));
        }
        let st = NoteStyle::default();
        for size in RootSize::ALL {
            let s = draw(&root, &st, &Look { layout: Layout::Radial, root: size, ..Default::default() });
            let r = rects(&s);
            assert_eq!(r.len(), 1 + 4 + 6 + 5 + 6 + 4, "{size:?}");
            for (i, a) in r.iter().enumerate() {
                for b in r.iter().skip(i + 1) {
                    let overlap = a.0 < b.2 && b.0 < a.2 && a.1 < b.3 && b.1 < a.3;
                    assert!(!overlap, "{size:?}: {a:?} {b:?}");
                }
            }
        }
    }

    #[test]
    fn root_size_wraps_a_long_center() {
        let mut root = sample();
        root.label = "Lanzamiento de IureDesk para despachos jurídicos".into();
        let st = NoteStyle::default();
        let one = |size| {
            let s = draw(&root, &st, &Look { root: size, ..Default::default() });
            let first = rects(&s)[0];
            (first.2 - first.0, first.3 - first.1, s.matches("<tspan").count())
        };
        let (auto_w, auto_h, auto_lines) = one(RootSize::Auto);
        let (small_w, small_h, small_lines) = one(RootSize::Small);
        let (large_w, _, large_lines) = one(RootSize::Large);
        assert_eq!(auto_lines, 3, "automático: partido en líneas de 22");
        assert!(small_lines >= 4 && small_w < auto_w && small_h > auto_h, "chico: angosto y alto");
        // El chico no corta una etiqueta del largo máximo.
        let max: String = "palabra ".repeat(MAX_LABEL / 8);
        let s = draw(
            &Node { label: max.trim().into(), children: vec![], line: 0 },
            &st,
            &Look { root: RootSize::Small, ..Default::default() },
        );
        assert!(!s.contains('…'), "{s}");
        assert!(large_lines == 2 && large_w > auto_w, "grande: líneas largas y letra mayor");
        // Un centro corto sigue en una línea.
        let short = draw(&sample(), &st, &Look::default());
        assert_eq!(short.matches("<tspan").count(), 0);
        assert_eq!(wrap("palabra", 4, 4), vec!["pala", "bra"]);
        assert_eq!(wrap("a b c d e f g h", 1, 3).len(), 3);
        assert!(wrap("uno dos tres cuatro cinco", 8, 2).last().unwrap().ends_with('…'));
        assert_eq!(wrap("", 10, 2), vec![String::new()]);
    }

    #[test]
    fn palettes_color_branches_and_keep_text_readable() {
        let st = NoteStyle::default();
        let root = sample();
        let pastel = draw(&root, &st, &Look { palette: Palette::Pastel, ..Default::default() });
        assert!(pastel.contains("fill=\"#bfdbfe\""), "primer color pastel");
        assert!(pastel.contains("fill=\"#1f2328\""), "texto oscuro sobre pastel");
        let warm = draw(&root, &st, &Look { palette: Palette::Warm, ..Default::default() });
        assert!(warm.contains("fill=\"#b45309\"") && warm.contains("fill=\"#ffffff\""));
        for p in Palette::ALL {
            for c in p.colors() {
                let text = ink(*c);
                // Contraste aproximado (luminancia relativa WCAG) ≥ 4.5 entre fondo y texto.
                let lum = |c: [u8; 3]| {
                    let f = |v: u8| {
                        let v = f32::from(v) / 255.0;
                        if v <= 0.039_28 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
                    };
                    0.2126 * f(c[0]) + 0.7152 * f(c[1]) + 0.0722 * f(c[2])
                };
                let (a, b) = (lum(*c), lum(text));
                let ratio = (a.max(b) + 0.05) / (a.min(b) + 0.05);
                assert!(ratio >= 4.5, "{p:?} {c:?}: {ratio}");
            }
        }
    }

    #[test]
    fn every_shape_and_layout_is_safe() {
        let st = NoteStyle::default();
        let mut root = sample();
        root.label = "<b>&\"x\" con un centro largo que se parte".into();
        for l in Layout::ALL {
            for sh in Shape::ALL {
                for size in RootSize::ALL {
                    let s = draw(&root, &st, &Look { layout: l, shape: sh, root: size, palette: Palette::Pastel });
                    assert!(s.starts_with("<svg") && s.ends_with("</svg>"), "{l:?} {sh:?}");
                    assert!(!s.contains("<b>") && !s.contains("NaN") && !s.contains("inf"), "{l:?} {sh:?}");
                    assert!(crate::net::svg_is_safe(s.as_bytes()), "{l:?} {sh:?}");
                }
            }
        }
        // Las formas propias sí cambian el dibujo.
        let pill = draw(&root, &st, &Look::default());
        for sh in [Shape::Square, Shape::Circle, Shape::Cloud, Shape::Bang, Shape::Hexagon] {
            assert_ne!(draw(&root, &st, &look(Layout::Right, sh)), pill, "{sh:?}");
        }
    }

    proptest::proptest! {
        #[test]
        fn never_panics(s in "(\\PC|> |\\[!MINDMAP\\]|\\[!MINDMAP\\|layout-down cloud root-l\\]|\\[!MINDMAP\\|layout-radial palette-pastel\\]|- |  |\n|```){0,200}",
                        n in 0usize..4, l in proptest::sample::select(Layout::ALL.to_vec()),
                        sh in proptest::sample::select(Shape::ALL.to_vec()),
                        rs in proptest::sample::select(RootSize::ALL.to_vec()),
                        pal in proptest::sample::select(Palette::ALL.to_vec())) {
            for b in find(&s) {
                let _ = draw(&b.root, &NoteStyle::default(), &b.options.look(l));
                proptest::prop_assert!(s.get(b.range.clone()).is_some());
            }
            let want = MapOptions { layout: Some(l), shape: sh, root: rs, palette: pal };
            if let Some(out) = set_options(&s, n, &want) {
                let b = find(&out);
                proptest::prop_assert_eq!(b.get(n).map(|b| b.options), Some(want));
                proptest::prop_assert_eq!(b.len(), find(&s).len());
            }
        }
    }
}
