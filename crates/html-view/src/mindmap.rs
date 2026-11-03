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
//! El metadato tras `|` trae la vista (`map`: solo el mapa; `right` o nada: la lista y el mapa) y
//! la forma de los nodos (aquí todos son píldoras). El centro es el texto de la línea del
//! marcador (o el primer párrafo; si no hay, queda vacío y quien dibuja pone el nombre por omisión,
//! en el idioma de la interfaz); las listas son las ramas con su
//! sangría y un párrafo suelto cuelga del centro.
//!
//! [`find`] localiza los bloques (fuera de los bloques de código) y [`svg`] dibuja el árbol en
//! horizontal con un color por rama, con los colores y el tamaño de letra de la nota. Todo
//! acotado: nodos, profundidad, largo de etiquetas y tamaño del dibujo.

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
        let view = if meta.split_whitespace().any(|w| w == "map") { View::MapOnly } else { View::ListAndMap };
        let root = tree(&title, &body);
        out.push(Block { range: start..end, view, root, body });
        if out.len() >= MAX_MAPS {
            break;
        }
        i = j;
    }
    out
}

/// Colores de las ramas (fondo; el texto va en blanco). Contraste AA con blanco.
const BRANCH: [[u8; 3]; 6] = [
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

struct Placed {
    depth: usize,
    y: f32,
    w: f32,
    label: String,
    color: [u8; 3],
    parent: Option<usize>,
}

/// Coloca los nodos: hojas una debajo de otra, cada padre a la altura media de sus hijos.
fn place(
    n: &Node,
    depth: usize,
    color: [u8; 3],
    parent: Option<usize>,
    char_w: f32,
    row: f32,
    next_y: &mut f32,
    out: &mut Vec<Placed>,
) -> f32 {
    let idx = out.len();
    let w = (n.label.chars().count() as f32 * char_w + 28.0).clamp(48.0, 420.0);
    out.push(Placed { depth, y: 0.0, w, label: n.label.clone(), color, parent });
    let y = if n.children.is_empty() {
        let y = *next_y;
        *next_y += row;
        y
    } else {
        let mut ys = Vec::with_capacity(n.children.len());
        for (k, c) in n.children.iter().enumerate() {
            let col = if depth == 0 { BRANCH.get(k % BRANCH.len()).copied().unwrap_or(color) } else { color };
            ys.push(place(c, depth + 1, col, Some(idx), char_w, row, next_y, out));
        }
        let first = ys.first().copied().unwrap_or(0.0);
        let last = ys.last().copied().unwrap_or(first);
        (first + last) / 2.0
    };
    if let Some(p) = out.get_mut(idx) {
        p.y = y;
    }
    y
}

/// SVG del mapa con los colores y la letra de la nota. Ancho y alto acotados.
pub fn svg(root: &Node, style: &NoteStyle) -> String {
    let font = style.font_px.clamp(10.0, 24.0);
    let char_w = font * 0.58;
    let row = font * 2.6;
    let node_h = font * 1.9;
    let gap = 44.0;
    let pad = 12.0;
    let mut placed = Vec::new();
    let mut next_y = pad + node_h / 2.0;
    let center = [0x64, 0x74, 0x8b];
    place(root, 0, center, None, char_w, row, &mut next_y, &mut placed);
    // x de cada columna: el ancho mayor de la anterior más el hueco.
    let depth = placed.iter().map(|p| p.depth).max().unwrap_or(0);
    let mut col_x = vec![pad; depth + 1];
    for d in 1..=depth {
        let prev = placed.iter().filter(|p| p.depth == d - 1).map(|p| p.w).fold(0.0_f32, f32::max);
        let base = col_x.get(d - 1).copied().unwrap_or(pad);
        if let Some(x) = col_x.get_mut(d) {
            *x = base + prev + gap;
        }
    }
    let width = placed.iter().map(|p| col_x.get(p.depth).copied().unwrap_or(pad) + p.w).fold(0.0_f32, f32::max) + pad;
    let height = (next_y - row + node_h / 2.0 + pad).max(node_h + 2.0 * pad);
    let (width, height) = (width.min(4_000.0), height.min(8_000.0));
    let mut s = String::with_capacity(placed.len() * 320 + 512);
    let _ = write!(
        s,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width:.0}\" height=\"{height:.0}\" viewBox=\"0 0 {width:.0} {height:.0}\" font-family=\"sans-serif\" font-size=\"{font:.1}\">"
    );
    // Conectores primero (debajo de los nodos).
    for p in &placed {
        let Some(par) = p.parent.and_then(|i| placed.get(i)) else { continue };
        let x1 = col_x.get(par.depth).copied().unwrap_or(pad) + par.w;
        let x2 = col_x.get(p.depth).copied().unwrap_or(pad);
        let mx = (x1 + x2) / 2.0;
        let _ = write!(
            s,
            "<path d=\"M{x1:.1} {:.1} C{mx:.1} {:.1} {mx:.1} {:.1} {x2:.1} {:.1}\" fill=\"none\" stroke=\"{}\" stroke-width=\"{:.1}\" stroke-opacity=\"0.85\"/>",
            par.y,
            par.y,
            p.y,
            p.y,
            hex(p.color),
            if p.depth == 1 { 3.0 } else { 2.0 }
        );
    }
    for p in &placed {
        let x = col_x.get(p.depth).copied().unwrap_or(pad);
        let y = p.y - node_h / 2.0;
        let (fill, ink) = if p.depth == 0 { (style.code_bg, style.text) } else { (p.color, [0xff, 0xff, 0xff]) };
        let stroke = if p.depth == 0 { hex(style.border) } else { hex(p.color) };
        let _ = write!(
            s,
            "<rect x=\"{x:.1}\" y=\"{y:.1}\" width=\"{:.1}\" height=\"{node_h:.1}\" rx=\"{:.1}\" fill=\"{}\" stroke=\"{stroke}\"/>\
<text x=\"{:.1}\" y=\"{:.1}\" fill=\"{}\" text-anchor=\"middle\" dominant-baseline=\"central\"{}>{}</text>",
            p.w,
            node_h / 2.0,
            hex(fill),
            x + p.w / 2.0,
            p.y,
            hex(ink),
            if p.depth <= 1 { " font-weight=\"600\"" } else { "" },
            esc(&p.label)
        );
    }
    s.push_str("</svg>");
    s
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

    proptest::proptest! {
        #[test]
        fn never_panics(s in "(\\PC|> |\\[!MINDMAP\\]|- |  |\n|```){0,200}") {
            for b in find(&s) {
                let _ = svg(&b.root, &NoteStyle::default());
                proptest::prop_assert!(s.get(b.range.clone()).is_some());
            }
        }
    }
}
