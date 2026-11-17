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

/// Un nodo del árbol.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Node {
    pub label: String,
    pub children: Vec<Node>,
}

/// Un mapa mental de la nota.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    /// Bytes de la nota que ocupa (todas sus líneas, con el salto final).
    pub range: Range<usize>,
    pub view: View,
    /// Distribución pedida en el metadato (`None`: la de Preferencias).
    pub layout: Option<Layout>,
    pub shape: Shape,
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
}

/// El subárbol de `idx` desde la lista plana (≤ [`MAX_NODES`] nodos).
fn build(flat: &[Flat], idx: usize, label: String) -> Node {
    let children = flat
        .iter()
        .enumerate()
        .filter(|(k, f)| f.parent == idx && *k != idx)
        .map(|(k, f)| build(flat, k, f.label.clone()))
        .collect();
    Node { label, children }
}

/// Árbol desde las líneas del cuerpo.
fn tree(title: &str, body: &[String]) -> Node {
    let mut root_label = plain_label(title);
    // El 0 es el centro.
    let mut flat = vec![Flat { label: String::new(), parent: 0, depth: 0 }];
    // Pila de (sangría, índice en `flat`).
    let mut stack: Vec<(usize, usize)> = Vec::new();
    for line in body {
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
        flat.push(Flat { label, parent, depth });
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
        let layout = words.iter().find_map(|w| Layout::from_token(w));
        let shape = words.iter().find_map(|w| Shape::from_token(w)).unwrap_or_default();
        let root = tree(&title, &body);
        out.push(Block { range: start..end, view, layout, shape, root, body });
        if out.len() >= MAX_MAPS {
            break;
        }
        i = j;
    }
    out
}

/// S1.19: cambia la distribución y la forma del mapa `n` (en el orden de [`find`]) y devuelve el
/// Markdown nuevo. Solo se reescribe el metadato del marcador: la forma va primero (IureEditor toma
/// como forma la primera palabra que no sea vista ni `links`), luego lo que ya había (vista,
/// `links`, palabras desconocidas, tal cual) y al final la distribución. `layout: None` quita la
/// distribución (vuelve la de Preferencias) y [`Shape::Default`] quita la forma. `None` si no hay
/// mapa `n` o nada cambia.
pub fn set_options(md: &str, n: usize, layout: Option<Layout>, shape: Shape) -> Option<String> {
    let block = find(md).into_iter().nth(n)?;
    let line_end = md.get(block.range.clone())?.find('\n').map_or(block.range.end, |e| block.range.start + e);
    let line = md.get(block.range.start..line_end)?;
    let open = line.find("[!")? + 2;
    let close = open + line.get(open..)?.find(']')?;
    let inner = line.get(open..close)?;
    let (kind, meta) = inner.split_once('|').unwrap_or((inner, ""));
    let mut words: Vec<&str> = Vec::new();
    if shape != Shape::Default {
        words.push(shape.token());
    }
    words.extend(meta.split_whitespace().filter(|w| Layout::from_token(w).is_none() && Shape::from_token(w).is_none()));
    if let Some(l) = layout {
        words.push(l.token());
    }
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

/// Un nodo ya colocado: centro (`x`, `y`) y ancho.
struct Placed {
    depth: usize,
    x: f32,
    y: f32,
    w: f32,
    label: String,
    color: [u8; 3],
    parent: Option<usize>,
    /// -1: rama a la izquierda del centro; 1: a la derecha o abajo.
    side: f32,
}

/// Medidas del dibujo, según la letra de la nota y la forma.
struct Metrics {
    font: f32,
    char_w: f32,
    /// Distancia entre filas (horizontal) o entre niveles (vertical).
    row: f32,
    node_h: f32,
    gap: f32,
    pad: f32,
    shape: Shape,
}

impl Metrics {
    fn new(style: &NoteStyle, shape: Shape) -> Self {
        let font = style.font_px.clamp(10.0, 24.0);
        let node_h = font * 1.9;
        // Las formas que sobresalen del rectángulo (nube, estrella, elipse) piden más aire entre filas.
        let row = font
            * match shape {
                Shape::Cloud | Shape::Bang => 3.4,
                Shape::Circle => 3.0,
                _ => 2.6,
            };
        Self { font, char_w: font * 0.58, row, node_h, gap: 44.0, pad: 12.0, shape }
    }

    /// Ancho de un nodo: el texto más el relleno que pide la forma.
    fn width(&self, label: &str) -> f32 {
        let extra = match self.shape {
            Shape::Default | Shape::Square | Shape::Rounded => 0.0,
            Shape::Hexagon => self.node_h * 0.6,
            Shape::Circle | Shape::Cloud => self.node_h * 0.7,
            Shape::Bang => self.node_h * 1.1,
        };
        (label.chars().count() as f32 * self.char_w + 28.0 + extra).clamp(48.0, 460.0)
    }
}

/// Color de la rama `k` del centro.
fn branch(k: usize) -> [u8; 3] {
    BRANCH.get(k % BRANCH.len()).copied().unwrap_or([0x47, 0x55, 0x69])
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
    out.push(Placed { depth, x: 0.0, y: 0.0, w: m.width(&n.label), label: n.label.clone(), color, parent, side });
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

/// Hojas del subárbol (para repartir las ramas entre los dos lados).
fn leaves(n: &Node) -> usize {
    if n.children.is_empty() { 1 } else { n.children.iter().map(leaves).sum() }
}

/// Columnas horizontales: la `x` de cada nodo según su nivel y su lado. El centro en `x = 0`.
fn columns(placed: &mut [Placed], m: &Metrics) {
    let depth = placed.iter().map(|p| p.depth).max().unwrap_or(0);
    let root_w = placed.first().map_or(0.0, |p| p.w);
    for side in [1.0_f32, -1.0] {
        // Borde interior de cada columna (distancia desde el centro).
        let mut edge = root_w / 2.0 + m.gap;
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
    let center = [0x64, 0x74, 0x8b];
    let mut placed = vec![Placed {
        depth: 0,
        x: 0.0,
        y: 0.0,
        w: m.width(&root.label),
        label: root.label.clone(),
        color: center,
        parent: None,
        side: 1.0,
    }];
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
                place_rows(c, 1, branch(k), Some(0), side, m, &mut next_y, &mut placed);
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
    let own = m.width(&n.label);
    if n.children.is_empty() || depth > MAX_DEPTH + 1 {
        return own;
    }
    let kids: f32 = n.children.iter().map(|c| extent(c, m, depth + 1)).sum::<f32>()
        + m.gap / 2.0 * n.children.len().saturating_sub(1) as f32;
    own.max(kids)
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
    let w = m.width(&n.label);
    let y = depth as f32 * (m.node_h + m.gap);
    out.push(Placed { depth, x: 0.0, y, w, label: n.label.clone(), color, parent, side: 1.0 });
    let ext = extent(n, m, depth);
    let kids: f32 = n.children.iter().map(|c| extent(c, m, depth + 1)).sum::<f32>()
        + m.gap / 2.0 * n.children.len().saturating_sub(1) as f32;
    let mut x = left + (ext - kids).max(0.0) / 2.0;
    let mut first = None;
    let mut last = 0.0;
    for (k, c) in n.children.iter().enumerate() {
        let col = if depth == 0 { branch(k) } else { color };
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

/// Figura de un nodo de ancho `w` centrado en (`cx`, `cy`).
fn shape_svg(s: &mut String, shape: Shape, cx: f32, cy: f32, w: f32, h: f32, fill: &str, stroke: &str) {
    let (x, y) = (cx - w / 2.0, cy - h / 2.0);
    let paint = format!("fill=\"{fill}\" stroke=\"{stroke}\" stroke-width=\"1.5\"");
    let _ = match shape {
        Shape::Default | Shape::Square | Shape::Rounded => {
            let rx = match shape {
                Shape::Square => 2.0,
                Shape::Rounded => h * 0.28,
                _ => h / 2.0,
            };
            write!(s, "<rect x=\"{x:.1}\" y=\"{y:.1}\" width=\"{w:.1}\" height=\"{h:.1}\" rx=\"{rx:.1}\" {paint}/>")
        }
        Shape::Circle => {
            write!(s, "<ellipse cx=\"{cx:.1}\" cy=\"{cy:.1}\" rx=\"{:.1}\" ry=\"{:.1}\" {paint}/>", w / 2.0, h * 0.62)
        }
        Shape::Hexagon => {
            let i = h / 2.0;
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
            let a = h * 0.55;
            let straight = (w - 2.0 * a).max(0.0);
            let per = 2.0 * straight + std::f32::consts::TAU * a;
            let n = ((per / (h * 0.75)).round() as usize).clamp(8, 32);
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
            let n = ((w / (h * 0.45)).round() as usize).clamp(8, 28);
            let mut pts = String::new();
            for k in 0..n * 2 {
                let a = std::f32::consts::PI * k as f32 / n as f32;
                let (rx, ry) = if k % 2 == 0 { (w / 2.0, h * 0.75) } else { (w / 2.0 - h * 0.3, h * 0.5) };
                let _ = write!(pts, "{:.1},{:.1} ", cx + rx * a.cos(), cy + ry * a.sin());
            }
            write!(s, "<polygon points=\"{}\" {paint}/>", pts.trim_end())
        }
    };
}

/// SVG de un árbol ya colocado (coordenadas relativas; aquí se encuadran).
fn paint(placed: &[Placed], m: &Metrics, style: &NoteStyle, vertical: bool) -> String {
    let h = m.node_h;
    // Margen vertical extra para las formas que sobresalen (nube, estrella, elipse).
    let bleed = match m.shape {
        Shape::Bang => h * 0.3,
        Shape::Cloud | Shape::Circle => h * 0.25,
        _ => 0.0,
    };
    let min_x = placed.iter().map(|p| p.x - p.w / 2.0).fold(f32::MAX, f32::min);
    let max_x = placed.iter().map(|p| p.x + p.w / 2.0).fold(f32::MIN, f32::max);
    let min_y = placed.iter().map(|p| p.y - h / 2.0).fold(f32::MAX, f32::min);
    let max_y = placed.iter().map(|p| p.y + h / 2.0).fold(f32::MIN, f32::max);
    let (ox, oy) = (m.pad + bleed - min_x, m.pad + bleed - min_y);
    let width = (max_x - min_x + 2.0 * (m.pad + bleed)).clamp(1.0, 4_000.0);
    let height = (max_y - min_y + 2.0 * (m.pad + bleed)).clamp(1.0, 8_000.0);
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
            "fill=\"none\" stroke=\"{}\" stroke-width=\"{:.1}\" stroke-opacity=\"0.85\"",
            hex(p.color),
            if p.depth == 1 { 3.0 } else { 2.0 }
        );
        if vertical {
            let (x1, y1) = (par.x + ox, par.y + oy + h / 2.0);
            let (x2, y2) = (p.x + ox, p.y + oy - h / 2.0);
            let my = (y1 + y2) / 2.0;
            let _ =
                write!(s, "<path d=\"M{x1:.1} {y1:.1} C{x1:.1} {my:.1} {x2:.1} {my:.1} {x2:.1} {y2:.1}\" {stroke}/>");
        } else {
            let x1 = par.x + ox + p.side * par.w / 2.0;
            let x2 = p.x + ox - p.side * p.w / 2.0;
            let (y1, y2) = (par.y + oy, p.y + oy);
            let mx = (x1 + x2) / 2.0;
            let _ =
                write!(s, "<path d=\"M{x1:.1} {y1:.1} C{mx:.1} {y1:.1} {mx:.1} {y2:.1} {x2:.1} {y2:.1}\" {stroke}/>");
        }
    }
    for p in placed {
        let (cx, cy) = (p.x + ox, p.y + oy);
        let (fill, ink) = if p.depth == 0 { (style.code_bg, style.text) } else { (p.color, [0xff, 0xff, 0xff]) };
        let stroke = if p.depth == 0 { hex(style.border) } else { hex(p.color) };
        shape_svg(&mut s, m.shape, cx, cy, p.w, h, &hex(fill), &stroke);
        let _ = write!(
            s,
            "<text x=\"{cx:.1}\" y=\"{cy:.1}\" fill=\"{}\" text-anchor=\"middle\" dominant-baseline=\"central\"{}>{}</text>",
            hex(ink),
            if p.depth <= 1 { " font-weight=\"600\"" } else { "" },
            esc(&p.label)
        );
    }
    s.push_str("</svg>");
    s
}

/// SVG del mapa con los colores y la letra de la nota, la distribución y la forma pedidas. El
/// radial lo dibuja `mermaid-svg` (acotado, en su hilo); si no puede, sale a la derecha. Ancho y
/// alto acotados.
pub fn draw(root: &Node, style: &NoteStyle, layout: Layout, shape: Shape) -> String {
    if layout == Layout::Radial
        && let Some(svg) = crate::mermaid::mindmap_svg(root, shape, style)
    {
        return svg;
    }
    let m = Metrics::new(style, shape);
    match layout {
        Layout::Down => {
            let mut placed = Vec::new();
            place_down(root, 0, [0x64, 0x74, 0x8b], None, 0.0, &m, &mut placed);
            paint(&placed, &m, style, true)
        }
        Layout::Balanced => paint(&layout_rows(root, true, &m), &m, style, false),
        Layout::Right | Layout::Radial => paint(&layout_rows(root, false, &m), &m, style, false),
    }
}

/// Como [`draw`] a la derecha y con píldoras (lo de antes de S1.19).
pub fn svg(root: &Node, style: &NoteStyle) -> String {
    draw(root, style, Layout::Right, Shape::Default)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTE: &str = "# titulo\n\n> Veamos diagrama\n\n> [!MINDMAP|map] Idea central\n> - Analisis\n> - Diseño\n>   - **Bocetos**\n> - [Pruebas](https://x)\n>\n> - Pre-análisis\n\nY ahora\n";

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
        let root = Node { label: "<script>&".into(), children: vec![Node { label: "a\"b".into(), children: vec![] }] };
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
        // S1.19: forma, vista, `links` y distribución, en cualquier orden y mayúsculas.
        let b = find("> [!MINDMAP|Layout-Radial map CLOUD links] X\n> - a\n");
        assert_eq!((b[0].layout, b[0].shape, b[0].view), (Some(Layout::Radial), Shape::Cloud, View::MapOnly));
        let b = find("> [!MINDMAP|right] X\n");
        assert_eq!((b[0].layout, b[0].shape, b[0].view), (None, Shape::Default, View::ListAndMap));
        let b = find("> [!MINDMAP|rara layout-both hexagon] X\n");
        assert_eq!((b[0].layout, b[0].shape), (Some(Layout::Balanced), Shape::Hexagon));
    }

    #[test]
    fn set_options_rewrites_only_the_meta() {
        let md = "Antes\n\n> [!MINDMAP|map links rara] Centro\n> - a\n\n> [!mindmap] Otro\n> - b\n";
        // La forma va primero (IureEditor la toma de ahí), lo demás se conserva y la distribución al final.
        let out = set_options(md, 0, Some(Layout::Down), Shape::Bang).unwrap();
        assert_eq!(out, md.replace("[!MINDMAP|map links rara]", "[!MINDMAP|bang map links rara layout-down]"));
        // Cambiar otra vez reemplaza, no acumula.
        let again = set_options(&out, 0, Some(Layout::Radial), Shape::Square).unwrap();
        assert!(again.contains("[!MINDMAP|square map links rara layout-radial] Centro"), "{again}");
        // Volver a lo de omisión quita las palabras propias.
        let back = set_options(&again, 0, None, Shape::Default).unwrap();
        assert_eq!(back, md);
        // El segundo mapa (sin metadato, en minúsculas) gana uno sin tocar el resto.
        let second = set_options(md, 1, Some(Layout::Balanced), Shape::Default).unwrap();
        assert_eq!(second, md.replace("[!mindmap] Otro", "[!mindmap|layout-both] Otro"));
        // Nada que cambiar o mapa inexistente.
        assert_eq!(set_options(md, 1, None, Shape::Default), None);
        assert_eq!(set_options(md, 2, Some(Layout::Down), Shape::Default), None);
        // Lo que se escribe se vuelve a leer igual.
        let b = find(&out);
        assert_eq!((b[0].layout, b[0].shape, b[0].view), (Some(Layout::Down), Shape::Bang, View::MapOnly));
        assert_eq!(b[0].root, find(md)[0].root);
    }

    /// Cajas de los nodos de un SVG propio: (x, y, ancho) de cada `<text>` con su ancho estimado.
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
        let leaf = |l: &str| Node { label: l.into(), children: vec![] };
        Node {
            label: "Centro".into(),
            children: vec![
                Node { label: "Uno".into(), children: vec![leaf("a"), leaf("b")] },
                Node { label: "Dos".into(), children: vec![leaf("c")] },
                Node { label: "Tres".into(), children: vec![leaf("d"), leaf("e")] },
                leaf("Cuatro"),
            ],
        }
    }

    #[test]
    fn layouts_place_branches_where_asked() {
        let st = NoteStyle::default();
        let root = sample();
        let centers = |l| text_centers(&draw(&root, &st, l, Shape::Default));
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
        // Radial: lo dibuja Mermaid (otro SVG, con todas las etiquetas).
        let radial = draw(&root, &st, Layout::Radial, Shape::Default);
        assert_ne!(radial, draw(&root, &st, Layout::Right, Shape::Default));
        assert!(["Centro", "Uno", "Cuatro", "e"].iter().all(|l| radial.contains(l)));
        assert!(crate::net::svg_is_safe(radial.as_bytes()));
    }

    #[test]
    fn every_shape_and_layout_is_safe() {
        let st = NoteStyle::default();
        let mut root = sample();
        root.label = "<b>&\"x\"".into();
        for l in Layout::ALL {
            for sh in Shape::ALL {
                let s = draw(&root, &st, l, sh);
                assert!(s.starts_with("<svg") && s.ends_with("</svg>"), "{l:?} {sh:?}");
                assert!(!s.contains("<b>") && !s.contains("NaN") && !s.contains("inf"), "{l:?} {sh:?}");
                assert!(crate::net::svg_is_safe(s.as_bytes()), "{l:?} {sh:?}");
            }
        }
        // Las formas propias sí cambian el dibujo.
        let pill = draw(&root, &st, Layout::Right, Shape::Default);
        for sh in [Shape::Square, Shape::Circle, Shape::Cloud, Shape::Bang, Shape::Hexagon] {
            assert_ne!(draw(&root, &st, Layout::Right, sh), pill, "{sh:?}");
        }
    }

    proptest::proptest! {
        #[test]
        fn never_panics(s in "(\\PC|> |\\[!MINDMAP\\]|\\[!MINDMAP\\|layout-down cloud\\]|\\[!MINDMAP\\|layout-both\\]|- |  |\n|```){0,200}",
                        n in 0usize..4, l in proptest::sample::select(Layout::ALL.to_vec()),
                        sh in proptest::sample::select(Shape::ALL.to_vec())) {
            for b in find(&s) {
                let _ = draw(&b.root, &NoteStyle::default(), b.layout.unwrap_or(l), b.shape);
                proptest::prop_assert!(s.get(b.range.clone()).is_some());
            }
            if let Some(out) = set_options(&s, n, Some(l), sh) {
                let b = find(&out);
                proptest::prop_assert_eq!(b.get(n).map(|b| (b.layout, b.shape)), Some((Some(l), sh)));
                proptest::prop_assert_eq!(b.len(), find(&s).len());
            }
        }
    }
}
