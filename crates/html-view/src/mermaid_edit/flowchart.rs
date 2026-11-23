//! Diagramas de flujo (`flowchart` / `graph`): agregar un nodo después de otro, conectar dos nodos,
//! cambiar el texto o la forma de un nodo y borrarlo (con sus flechas). Cada cambio toca solo las
//! líneas de ese nodo; lo que no se entiende se deja igual o la acción no se hace (`None`).

use super::MAX_LINES;

/// Texto de un nodo más largo que se escribe.
const MAX_TEXT: usize = 120;
/// Nodos que se revisan como mucho.
const MAX_NODES: usize = 2_000;

/// Las formas que se ofrecen al agregar un nodo o cambiar la suya.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    Rect,
    Round,
    Stadium,
    Rhombus,
    Circle,
    Cylinder,
    Subroutine,
    Hexagon,
    Parallelogram,
}

impl Shape {
    pub const ALL: [Shape; 9] = [
        Shape::Rect,
        Shape::Round,
        Shape::Stadium,
        Shape::Rhombus,
        Shape::Circle,
        Shape::Cylinder,
        Shape::Subroutine,
        Shape::Hexagon,
        Shape::Parallelogram,
    ];

    /// Sus delimitadores en Mermaid.
    pub fn delims(self) -> (&'static str, &'static str) {
        match self {
            Shape::Rect => ("[", "]"),
            Shape::Round => ("(", ")"),
            Shape::Stadium => ("([", "])"),
            Shape::Rhombus => ("{", "}"),
            Shape::Circle => ("((", "))"),
            Shape::Cylinder => ("[(", ")]"),
            Shape::Subroutine => ("[[", "]]"),
            Shape::Hexagon => ("{{", "}}"),
            Shape::Parallelogram => ("[/", "/]"),
        }
    }

    /// Nombre en inglés y español.
    pub fn label(self) -> (&'static str, &'static str) {
        match self {
            Shape::Rect => ("Rectangle", "Rectángulo"),
            Shape::Round => ("Rounded", "Redondeado"),
            Shape::Stadium => ("Start / end", "Inicio / fin"),
            Shape::Rhombus => ("Decision", "Decisión"),
            Shape::Circle => ("Circle", "Círculo"),
            Shape::Cylinder => ("Database", "Base de datos"),
            Shape::Subroutine => ("Subroutine", "Subproceso"),
            Shape::Hexagon => ("Preparation", "Preparación"),
            Shape::Parallelogram => ("Input / output", "Entrada / salida"),
        }
    }
}

/// Aperturas de forma que se reconocen al leer (las largas primero) y sus cierres posibles.
const OPENERS: [(&str, &[&str]); 13] = [
    ("(((", &[")))"]),
    ("([", &["])"]),
    ("[[", &["]]"]),
    ("[(", &[")]"]),
    ("((", &["))"]),
    ("{{", &["}}"]),
    ("[/", &["/]", "\\]"]),
    ("[\\", &["\\]", "/]"]),
    ("[", &["]"]),
    ("(", &[")"]),
    ("{", &["}"]),
    (">", &["]"]),
    ("@{", &["}"]),
];

/// Un nodo en una línea: dónde está su id y, si la tiene, su forma (apertura, texto y cierre).
#[derive(Clone, Debug, PartialEq)]
struct NodeRef {
    id: String,
    /// Bytes de todo el nodo (id, forma y `:::clase`) en la línea.
    span: (usize, usize),
    /// Fin del id.
    id_end: usize,
    /// La forma: inicio del texto, fin del texto y fin del cierre.
    shape: Option<(usize, usize, usize)>,
}

/// Lo que hay en una sentencia: nodos (agrupados con `&`) separados por flechas.
#[derive(Clone, Debug, PartialEq)]
enum Item {
    Node(NodeRef),
    Amp,
    Edge((usize, usize)),
}

/// Primera palabra de las líneas que no son sentencias de nodos y flechas.
const KEYWORDS: [&str; 13] = [
    "flowchart",
    "graph",
    "subgraph",
    "end",
    "direction",
    "classDef",
    "class",
    "style",
    "linkStyle",
    "click",
    "accTitle",
    "accDescr",
    "title",
];

fn is_id_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// ¿Es una línea que no se lee como sentencia (cerca, comentario, palabra clave, vacía)?
fn skip_line(line: &str) -> bool {
    let t = line.trim();
    if t.is_empty() || t.starts_with("```") || t.starts_with("~~~") || t.starts_with("%%") || t.starts_with("---") {
        return true;
    }
    let word: String = t.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
    KEYWORDS.contains(&word.as_str()) && t.get(word.len()..).is_none_or(|r| !r.starts_with(['[', '(', '{', '>']))
}

/// Dónde cierra `close` desde `from`, sin contar lo que va entre comillas.
fn find_close(line: &str, from: usize, closes: &[&str]) -> Option<(usize, usize)> {
    let mut in_quotes = false;
    let mut i = from;
    while let Some(rest) = line.get(i..) {
        let c = rest.chars().next()?;
        if c == '"' {
            in_quotes = !in_quotes;
        } else if !in_quotes && let Some(cl) = closes.iter().find(|cl| rest.starts_with(**cl)) {
            return Some((i, i + cl.len()));
        }
        i += c.len_utf8();
    }
    None
}

/// Lee una flecha desde `i`: su fin (con su `|texto|`), o `None` si no hay.
fn read_edge(line: &str, i: usize) -> Option<usize> {
    let rest = line.get(i..)?;
    let arrow_chars = |c: char| matches!(c, '-' | '=' | '.' | '~' | '<' | '>');
    if !rest.starts_with(['-', '=', '.', '~', '<']) {
        // `o--o`, `x--x`: la punta del principio.
        let mut cs = rest.chars();
        let first = cs.next()?;
        if !(matches!(first, 'o' | 'x') && cs.next().is_some_and(|c| matches!(c, '-' | '=' | '.'))) {
            return None;
        }
    }
    let mut end =
        i + rest.chars().take_while(|c| arrow_chars(*c) || matches!(c, 'o' | 'x')).map(char::len_utf8).sum::<usize>();
    // La punta del final (`--o`, `--x`) solo si va seguida de un espacio.
    let tok = line.get(i..end)?;
    let tok = tok.trim_end_matches(['o', 'x']);
    let head_end = i + tok.len();
    end = match line.get(head_end..end) {
        Some(h) if !h.is_empty() && line.get(end..).is_some_and(|r| r.starts_with(char::is_whitespace)) => end,
        _ => head_end,
    };
    let tok = line.get(i..end)?;
    if tok.chars().filter(|c| matches!(c, '-' | '=' | '.' | '~')).count() < 2 {
        return None;
    }
    // Texto dentro de la flecha: `A -- texto --> B`.
    if matches!(tok, "--" | "==" | "-.") && line.get(end..).is_some_and(|r| r.starts_with(' ')) {
        let ends = ["-->", "---", "==>", "===", ".->", "-.-", "--x", "--o"];
        let after = line.get(end..)?;
        let (p, e) = ends.iter().filter_map(|e| after.find(e).map(|p| (p, e.len()))).min_by_key(|(p, _)| *p)?;
        end = end + p + e;
    }
    // `|texto|` justo después.
    let ws = line.get(end..)?.chars().take_while(|c| *c == ' ').count();
    if line.get(end + ws..).is_some_and(|r| r.starts_with('|')) {
        let close = line.get(end + ws + 1..)?.find('|')?;
        end = end + ws + 1 + close + 1;
    }
    Some(end)
}

/// Lee un nodo desde `i`.
fn read_node(line: &str, i: usize) -> Option<NodeRef> {
    let rest = line.get(i..)?;
    let id_len: usize = rest.chars().take_while(|c| is_id_char(*c)).map(char::len_utf8).sum();
    if id_len == 0 {
        return None;
    }
    let id_end = i + id_len;
    let id = line.get(i..id_end)?.to_string();
    let mut end = id_end;
    let mut shape = None;
    let after = line.get(id_end..)?;
    if let Some((open, closes)) = OPENERS.iter().find(|(o, _)| after.starts_with(o)) {
        let text_start = id_end + open.len();
        let (text_end, close_end) = find_close(line, text_start, closes)?;
        shape = Some((text_start, text_end, close_end));
        end = close_end;
    }
    if line.get(end..).is_some_and(|r| r.starts_with(":::")) {
        let cls: usize =
            line.get(end + 3..)?.chars().take_while(|c| is_id_char(*c) || *c == '-').map(char::len_utf8).sum();
        end += 3 + cls;
    }
    Some(NodeRef { id, span: (i, end), id_end, shape })
}

/// Las partes de una sentencia, o `None` si la línea no se entiende (o no es sentencia).
fn items(line: &str) -> Option<Vec<Item>> {
    if skip_line(line) {
        return None;
    }
    let mut out = Vec::new();
    let mut i = line.len() - line.trim_start().len();
    for _ in 0..200 {
        let rest = line.get(i..)?;
        let ws: usize = rest.chars().take_while(|c| c.is_whitespace()).map(char::len_utf8).sum();
        i += ws;
        let rest = line.get(i..)?;
        if rest.is_empty() || rest.starts_with(';') || rest.starts_with("%%") {
            return (!out.is_empty()).then_some(out);
        }
        let want_node = !matches!(out.last(), Some(Item::Node(_)));
        if rest.starts_with('&') {
            out.push(Item::Amp);
            i += 1;
        } else if !want_node && let Some(end) = read_edge(line, i) {
            out.push(Item::Edge((i, end)));
            i = end;
        } else if want_node && let Some(n) = read_node(line, i) {
            i = n.span.1;
            out.push(Item::Node(n));
        } else {
            return None;
        }
    }
    None
}

/// Las sentencias del bloque: (línea, partes).
fn statements(block: &str) -> Vec<(usize, Vec<Item>)> {
    block.lines().take(MAX_LINES).enumerate().filter_map(|(n, l)| items(l).map(|it| (n, it))).collect()
}

fn nodes_of(items: &[Item]) -> impl Iterator<Item = &NodeRef> {
    items.iter().filter_map(|it| match it {
        Item::Node(n) => Some(n),
        _ => None,
    })
}

/// ¿Es un diagrama de flujo?
pub fn is_flowchart(block: &str) -> bool {
    block
        .lines()
        .take(40)
        .map(str::trim)
        .find(|l| !(l.is_empty() || l.starts_with("```") || l.starts_with("~~~") || l.starts_with("%%")))
        .is_some_and(|l| l.starts_with("flowchart") || l.starts_with("graph"))
}

/// Los nodos (id, texto y clases) en el orden en que `mermaid-svg` los dibuja.
pub fn nodes(code: &str) -> Vec<(String, String, Vec<String>)> {
    match mermaid_svg::parse(code) {
        Ok(mermaid_svg::Diagram::Flowchart(f)) => {
            f.nodes.into_iter().take(MAX_NODES).map(|n| (n.id, n.text, n.classes)).collect()
        }
        _ => Vec::new(),
    }
}

/// Un texto de nodo que no rompe la sintaxis: una línea, sin comillas dobles; entre comillas si
/// lleva signos de Mermaid.
pub fn node_text(s: &str) -> Option<String> {
    let one: String = s
        .chars()
        .map(|c| match c {
            '"' => '\'',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    let t: String = one.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(MAX_TEXT).collect();
    if t.is_empty() {
        return None;
    }
    let special = t.chars().any(|c| "[](){}<>|;#&%:/\\@".contains(c));
    Some(if special { format!("\"{t}\"") } else { t })
}

/// Cambia las líneas `n` del bloque por las de `new` (vacío: se quita), conservando el resto.
fn replace_line(block: &str, n: usize, new: &[String]) -> Option<String> {
    let mut out = String::with_capacity(block.len() + 64);
    let mut found = false;
    for (i, l) in block.split_inclusive('\n').enumerate() {
        if i == n {
            found = true;
            let nl = if l.ends_with("\r\n") {
                "\r\n"
            } else if l.ends_with('\n') {
                "\n"
            } else {
                ""
            };
            for (k, s) in new.iter().enumerate() {
                out.push_str(s);
                if k + 1 < new.len() || !nl.is_empty() {
                    out.push_str(if nl.is_empty() { "\n" } else { nl });
                }
            }
        } else {
            out.push_str(l);
        }
    }
    found.then_some(out)
}

/// Inserta `new` como línea después de la línea `after` (o antes de la cerca de cierre).
fn insert_after(block: &str, after: Option<usize>, new: &str) -> String {
    let lines: Vec<&str> = block.split_inclusive('\n').collect();
    let at = match after {
        Some(a) => a + 1,
        None => lines
            .iter()
            .rposition(|l| l.trim_start().starts_with("```") || l.trim_start().starts_with("~~~"))
            .filter(|&i| i > 0)
            .unwrap_or(lines.len()),
    };
    let mut out = String::with_capacity(block.len() + new.len() + 2);
    for (i, l) in lines.iter().enumerate() {
        if i == at {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(new);
            out.push('\n');
        }
        out.push_str(l);
    }
    if at >= lines.len() {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(new);
        out.push('\n');
    }
    out
}

fn indent_of(line: &str) -> &str {
    line.get(..line.len() - line.trim_start().len()).unwrap_or("")
}

/// La sangría de las sentencias (la de la primera; si no hay, cuatro espacios).
fn body_indent(block: &str) -> String {
    let mut lines = block.lines();
    let first = statements(block).first().and_then(|(n, _)| block.lines().nth(*n)).map(|l| indent_of(l).to_string());
    first.unwrap_or_else(|| {
        lines
            .find(|l| {
                let t = l.trim_start();
                t.starts_with("flowchart") || t.starts_with("graph")
            })
            .map(|l| format!("{}    ", indent_of(l)))
            .unwrap_or_else(|| "    ".into())
    })
}

/// La última línea donde aparece el nodo `id`.
fn last_line_of(block: &str, id: &str) -> Option<usize> {
    statements(block).into_iter().filter(|(_, it)| nodes_of(it).any(|n| n.id == id)).map(|(n, _)| n).next_back()
}

/// Un id libre (`N1`, `N2`…).
fn free_id(block: &str, code: &str) -> String {
    let used: Vec<String> = nodes(code).into_iter().map(|(id, _, _)| id).collect();
    (1..10_000)
        .map(|k| format!("N{k}"))
        .find(|c| !used.contains(c) && !block.contains(c.as_str()))
        .unwrap_or_else(|| "Nuevo".into())
}

/// Agrega un nodo nuevo (forma y texto) unido con una flecha desde `from`, después de la última
/// línea de `from`; hereda su primera clase (`:::clase`).
pub fn add_after(block: &str, code: &str, from: &str, shape: Shape, text: &str) -> Option<String> {
    let text = node_text(text)?;
    let (_, _, classes) = nodes(code).into_iter().find(|(id, _, _)| id == from)?;
    let at = last_line_of(block, from)?;
    let indent = indent_of(block.lines().nth(at)?).to_string();
    let new = free_id(block, code);
    let (o, c) = shape.delims();
    let class = classes.first().map(|c| format!(":::{c}")).unwrap_or_default();
    Some(insert_after(block, Some(at), &format!("{indent}{from} --> {new}{o}{text}{c}{class}")))
}

/// Agrega un nodo suelto al final del diagrama.
pub fn add_loose(block: &str, code: &str, shape: Shape, text: &str) -> Option<String> {
    if !is_flowchart(code) {
        return None;
    }
    let text = node_text(text)?;
    let new = free_id(block, code);
    let (o, c) = shape.delims();
    Some(insert_after(block, None, &format!("{}{new}{o}{text}{c}", body_indent(block))))
}

/// Une `a` con `b` (`a --> b`), después de la última línea de `a`.
pub fn connect(block: &str, code: &str, a: &str, b: &str) -> Option<String> {
    let ids: Vec<String> = nodes(code).into_iter().map(|(id, _, _)| id).collect();
    if a == b || !ids.iter().any(|i| i == a) || !ids.iter().any(|i| i == b) {
        return None;
    }
    let at = last_line_of(block, a)?;
    let indent = indent_of(block.lines().nth(at)?).to_string();
    Some(insert_after(block, Some(at), &format!("{indent}{a} --> {b}")))
}

/// Dónde se define el nodo: la primera aparición con forma (o, si no tiene, la primera).
fn definition(block: &str, id: &str) -> Option<(usize, NodeRef)> {
    let all: Vec<(usize, NodeRef)> = statements(block)
        .into_iter()
        .flat_map(|(n, it)| nodes_of(&it).filter(|r| r.id == id).cloned().map(|r| (n, r)).collect::<Vec<_>>())
        .collect();
    all.iter().find(|(_, r)| r.shape.is_some()).or_else(|| all.first()).cloned()
}

/// Cambia una parte `[a, b)` de la línea `n`.
fn splice(block: &str, n: usize, a: usize, b: usize, with: &str) -> Option<String> {
    let line = block.lines().nth(n)?;
    let new = format!("{}{with}{}", line.get(..a)?, line.get(b..)?);
    replace_line(block, n, &[new])
}

/// Cambia el texto del nodo `id` (su forma queda).
pub fn set_text(block: &str, id: &str, text: &str) -> Option<String> {
    let text = node_text(text)?;
    let (n, r) = definition(block, id)?;
    match r.shape {
        Some((a, b, _)) => {
            // `@{ … }` no se toca.
            let line = block.lines().nth(n)?;
            if line.get(r.id_end..)?.starts_with("@{") {
                return None;
            }
            splice(block, n, a, b, &text)
        }
        None => splice(block, n, r.id_end, r.id_end, &format!("[{text}]")),
    }
}

/// Cambia la forma del nodo `id` (su texto queda).
pub fn set_shape(block: &str, id: &str, shape: Shape) -> Option<String> {
    let (n, r) = definition(block, id)?;
    let line = block.lines().nth(n)?;
    let (o, c) = shape.delims();
    match r.shape {
        Some((a, b, close_end)) => {
            if line.get(r.id_end..)?.starts_with("@{") {
                return None;
            }
            let text = line.get(a..b)?.to_string();
            splice(block, n, r.id_end, close_end, &format!("{o}{text}{c}"))
        }
        None => splice(block, n, r.id_end, r.id_end, &format!("{o}{id}{c}")),
    }
}

/// Una sentencia sin el nodo `id`: los trozos de la cadena que quedan, cada uno en su línea (vacío:
/// se quita la línea).
fn without(line: &str, items: &[Item], id: &str) -> Option<Vec<String>> {
    let indent = indent_of(line);
    // Grupos de nodos (unidos con `&`) y las flechas entre ellos.
    let mut groups: Vec<Vec<&NodeRef>> = vec![Vec::new()];
    let mut edges: Vec<(usize, usize)> = Vec::new();
    for it in items {
        match it {
            Item::Node(n) => groups.last_mut()?.push(n),
            Item::Amp => {}
            Item::Edge(e) => {
                edges.push(*e);
                groups.push(Vec::new());
            }
        }
    }
    let text = |n: &NodeRef| line.get(n.span.0..n.span.1).map(str::to_string);
    let mut pieces: Vec<String> = Vec::new();
    let mut cur = String::new();
    for (k, g) in groups.iter().enumerate() {
        let kept: Vec<String> = g.iter().filter(|n| n.id != id).filter_map(|n| text(n)).collect();
        if kept.is_empty() {
            if !cur.is_empty() {
                pieces.push(std::mem::take(&mut cur));
            }
            continue;
        }
        if !cur.is_empty()
            && let Some((a, b)) = k.checked_sub(1).and_then(|p| edges.get(p))
        {
            cur.push(' ');
            cur.push_str(line.get(*a..*b)?);
            cur.push(' ');
        }
        cur.push_str(&kept.join(" & "));
    }
    if !cur.is_empty() {
        pieces.push(cur);
    }
    Some(pieces.into_iter().map(|p| format!("{indent}{p}")).collect())
}

/// ¿La línea `class a,b c` / `style a …` / `click a …` nombra `id`? Devuelve la línea sin él
/// (`None` dentro: se quita la línea).
fn without_in_directive(line: &str, id: &str) -> Option<Option<String>> {
    let t = line.trim_start();
    let mut words = t.split_whitespace();
    let kw = words.next()?;
    let target = words.next()?;
    match kw {
        "style" | "click" if target == id => Some(None),
        "class" => {
            let ids: Vec<&str> = target.split(',').map(str::trim).collect();
            if !ids.contains(&id) {
                return None;
            }
            let kept: Vec<&str> = ids.into_iter().filter(|i| *i != id).collect();
            if kept.is_empty() {
                return Some(None);
            }
            let rest = t.get(kw.len()..)?.trim_start().get(target.len()..)?;
            Some(Some(format!("{}{kw} {}{rest}", indent_of(line), kept.join(","))))
        }
        _ => None,
    }
}

/// Borra el nodo `id` con sus flechas (y sus `class`, `style` y `click`).
pub fn delete(block: &str, id: &str) -> Option<String> {
    definition(block, id)?;
    let mut out = String::with_capacity(block.len());
    for (n, l) in block.split_inclusive('\n').enumerate() {
        let body = l.trim_end_matches(['\n', '\r']);
        let nl = l.get(body.len()..).unwrap_or("");
        let replaced: Option<Vec<String>> = if n >= MAX_LINES {
            None
        } else if let Some(it) = items(body) {
            if nodes_of(&it).any(|r| r.id == id) { Some(without(body, &it, id)?) } else { None }
        } else {
            without_in_directive(body, id).map(|o| o.into_iter().collect())
        };
        match replaced {
            None => out.push_str(l),
            Some(lines) => {
                for s in lines {
                    out.push_str(&s);
                    out.push_str(if nl.is_empty() { "\n" } else { nl });
                }
            }
        }
    }
    if !block.ends_with('\n') && out.ends_with('\n') {
        out.pop();
    }
    Some(out)
}

// --- Flechas ---

/// Los tipos de flecha que se ofrecen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdgeKind {
    Solid,
    Dotted,
    Thick,
    Open,
}

impl EdgeKind {
    pub const ALL: [EdgeKind; 4] = [EdgeKind::Solid, EdgeKind::Dotted, EdgeKind::Thick, EdgeKind::Open];

    fn arrow(self) -> &'static str {
        match self {
            EdgeKind::Solid => "-->",
            EdgeKind::Dotted => "-.->",
            EdgeKind::Thick => "==>",
            EdgeKind::Open => "---",
        }
    }

    /// Nombre en inglés y español.
    pub fn label(self) -> (&'static str, &'static str) {
        match self {
            EdgeKind::Solid => ("Arrow", "Flecha"),
            EdgeKind::Dotted => ("Dotted arrow", "Flecha punteada"),
            EdgeKind::Thick => ("Thick arrow", "Flecha gruesa"),
            EdgeKind::Open => ("Line without arrowhead", "Línea sin punta"),
        }
    }
}

/// Una flecha del código: su línea, el número de su parte en la sentencia y sus dos nodos.
#[derive(Clone, Debug, PartialEq)]
struct EdgeAt {
    line: usize,
    item: usize,
    from: String,
    to: String,
    /// Sus dos lados son un solo nodo (sin `&`).
    single: bool,
}

/// Las flechas del código en el orden de `mermaid-svg` (los `&` se expanden: cada nodo de la
/// izquierda con cada uno de la derecha).
fn source_edges(block: &str) -> Vec<EdgeAt> {
    let mut out = Vec::new();
    for (line, it) in statements(block) {
        // Los grupos entre flechas.
        let mut groups: Vec<Vec<String>> = vec![Vec::new()];
        let mut edge_items: Vec<usize> = Vec::new();
        for (k, x) in it.iter().enumerate() {
            match x {
                Item::Node(n) => {
                    if let Some(g) = groups.last_mut() {
                        g.push(n.id.clone());
                    }
                }
                Item::Amp => {}
                Item::Edge(_) => {
                    edge_items.push(k);
                    groups.push(Vec::new());
                }
            }
        }
        for (e, item) in edge_items.iter().enumerate() {
            let (Some(l), Some(r)) = (groups.get(e), groups.get(e + 1)) else { continue };
            let single = l.len() == 1 && r.len() == 1;
            for f in l {
                for t in r {
                    out.push(EdgeAt { line, item: *item, from: f.clone(), to: t.clone(), single });
                }
            }
        }
        if out.len() > MAX_NODES {
            break;
        }
    }
    out
}

/// Las flechas del árbol (desde, hacia, texto).
pub fn edges(code: &str) -> Vec<(String, String, Option<String>)> {
    match mermaid_svg::parse(code) {
        Ok(mermaid_svg::Diagram::Flowchart(f)) => {
            f.edges.into_iter().take(MAX_NODES).map(|e| (e.from, e.to, e.label)).collect()
        }
        _ => Vec::new(),
    }
}

/// La flecha `n` del árbol en el código, si el código y el árbol coinciden (si no, no se edita).
fn edge_at(block: &str, code: &str, n: usize) -> Option<EdgeAt> {
    let src = source_edges(block);
    let tree = edges(code);
    let same = src.len() == tree.len() && src.iter().zip(&tree).all(|(a, (f, t, _))| a.from == *f && a.to == *t);
    if !same {
        return None;
    }
    src.into_iter().nth(n).filter(|e| e.single)
}

/// La flecha sin su texto (`-->`, `-.->`…) y su texto.
fn split_edge(tok: &str) -> (String, Option<String>) {
    let tok = tok.trim();
    if let Some(p) = tok.find('|') {
        let core = tok.get(..p).unwrap_or("").trim().to_string();
        let label = tok.get(p + 1..).and_then(|r| r.rfind('|').and_then(|q| r.get(..q))).map(|l| l.trim().to_string());
        return (core, label.filter(|l| !l.is_empty()));
    }
    let words: Vec<&str> = tok.split_whitespace().collect();
    match words.as_slice() {
        [open, middle @ .., close] if !middle.is_empty() => {
            let core = if *open == "-." { format!("-{close}") } else { close.to_string() };
            (core, Some(middle.join(" ")))
        }
        _ => (tok.to_string(), None),
    }
}

/// Cambia la parte `item` (una flecha) de la línea por `with`.
fn replace_item(block: &str, e: &EdgeAt, with: &str) -> Option<String> {
    let line = block.lines().nth(e.line)?;
    let it = items(line)?;
    let Item::Edge((a, b)) = it.get(e.item)? else { return None };
    splice(block, e.line, *a, *b, with)
}

fn edge_tok(block: &str, e: &EdgeAt) -> Option<String> {
    let line = block.lines().nth(e.line)?;
    let it = items(line)?;
    let Item::Edge((a, b)) = it.get(e.item)? else { return None };
    line.get(*a..*b).map(str::to_string)
}

fn with_label(core: &str, label: Option<&str>) -> Option<String> {
    Some(match label {
        Some(l) => format!("{core}|{}|", node_text(&l.replace('|', "/"))?),
        None => core.to_string(),
    })
}

/// Cambia el texto de la flecha `n` (vacío: se quita).
pub fn set_edge_text(block: &str, code: &str, n: usize, text: &str) -> Option<String> {
    let e = edge_at(block, code, n)?;
    let (core, _) = split_edge(&edge_tok(block, &e)?);
    let t = text.trim();
    replace_item(block, &e, &with_label(&core, (!t.is_empty()).then_some(t))?)
}

/// Cambia el tipo de la flecha `n` (su texto queda).
pub fn set_edge_kind(block: &str, code: &str, n: usize, kind: EdgeKind) -> Option<String> {
    let e = edge_at(block, code, n)?;
    let (_, label) = split_edge(&edge_tok(block, &e)?);
    replace_item(block, &e, &with_label(kind.arrow(), label.as_deref().map(|l| l.trim_matches('"')))?)
}

/// ¿Se puede invertir la flecha `n`? (Su sentencia es solo `A --> B`.)
pub fn can_reverse(block: &str, code: &str, n: usize) -> bool {
    edge_at(block, code, n)
        .and_then(|e| block.lines().nth(e.line).and_then(items))
        .is_some_and(|it| matches!(it.as_slice(), [Item::Node(_), Item::Edge(_), Item::Node(_)]))
}

/// Invierte la flecha `n`: `A --> B` pasa a `B --> A` (cada nodo con su forma).
pub fn reverse_edge(block: &str, code: &str, n: usize) -> Option<String> {
    let e = edge_at(block, code, n)?;
    let line = block.lines().nth(e.line)?;
    let it = items(line)?;
    let [Item::Node(a), Item::Edge((ea, eb)), Item::Node(b)] = it.as_slice() else { return None };
    let new = format!(
        "{}{} {} {}{}",
        line.get(..a.span.0)?,
        line.get(b.span.0..b.span.1)?,
        line.get(*ea..*eb)?,
        line.get(a.span.0..a.span.1)?,
        line.get(b.span.1..)?
    );
    replace_line(block, e.line, &[new])
}

/// Borra la flecha `n`; los nodos quedan (una cadena se parte en dos).
pub fn delete_edge(block: &str, code: &str, n: usize) -> Option<String> {
    let e = edge_at(block, code, n)?;
    let line = block.lines().nth(e.line)?;
    let it = items(line)?;
    let indent = indent_of(line);
    let piece = |range: &[Item]| -> Option<String> {
        let a = range.iter().find_map(|x| match x {
            Item::Node(n) => Some(n.span.0),
            _ => None,
        })?;
        let b = range.iter().rev().find_map(|x| match x {
            Item::Node(n) => Some(n.span.1),
            _ => None,
        })?;
        line.get(a..b).map(|t| format!("{indent}{t}"))
    };
    let left = piece(it.get(..e.item)?)?;
    let right = piece(it.get(e.item + 1..)?)?;
    replace_line(block, e.line, &[left, right])
}

/// Texto comparable: sin etiquetas, entidades básicas resueltas y sin espacios.
pub(super) fn norm(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    let out = out
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#34;", "\"");
    out.chars().filter(|c| !c.is_whitespace() && *c != '"').collect()
}

/// Cajas de los nodos en el SVG de `mermaid-svg`, en el orden de [`nodes`] (`None` si no se
/// encontró). Cada figura va seguida de su texto; las etiquetas de las flechas son un rectángulo
/// sin borde y las flechas, trazos con punta o sin relleno.
pub fn boxes(svg: &str, nodes: &[(String, String, Vec<String>)]) -> Vec<Option<[f32; 4]>> {
    let mut cands: Vec<(String, [f32; 4])> = Vec::new();
    let mut buf: Option<[f32; 4]> = None;
    let mut edge_label = false;
    let mut i = 0;
    for _ in 0..50_000 {
        let Some(rest) = svg.get(i..) else { break };
        let Some(p) = rest.find('<') else { break };
        i += p;
        let Some(rest) = svg.get(i..) else { break };
        if rest.starts_with("<defs") {
            match rest.find("</defs>") {
                Some(e) => i += e + 7,
                None => break,
            }
            continue;
        }
        if rest.starts_with("<text") {
            let Some(e) = rest.find("</text>") else { break };
            let inner = rest.get(..e).and_then(|t| t.find('>').and_then(|g| t.get(g + 1..))).unwrap_or("");
            if let Some(b) = buf.take()
                && !edge_label
            {
                cands.push((norm(inner), b));
            }
            edge_label = false;
            i += e + 7;
            continue;
        }
        let Some(e) = rest.find('>') else { break };
        let tag = rest.get(..e + 1).unwrap_or("");
        let shape = ["<rect", "<circle", "<ellipse", "<polygon", "<path"].iter().any(|k| tag.starts_with(k));
        let edge = tag.contains("marker-end") || tag.contains("marker-start") || tag.contains("fill=\"none\"");
        if shape && !edge {
            if tag.starts_with("<rect") && tag.contains("stroke=\"none\"") {
                edge_label = true;
            }
            if let Some((x, y, w, h)) = crate::diagram::shape_box(tag) {
                let r = [x, y, x + w, y + h];
                buf = Some(match buf {
                    Some(b) => [b[0].min(r[0]), b[1].min(r[1]), b[2].max(r[2]), b[3].max(r[3])],
                    None => r,
                });
            }
        }
        i += e + 1;
    }
    let mut used = vec![false; cands.len()];
    let mut cursor = 0;
    nodes
        .iter()
        .map(|(_, text, _)| {
            let want = norm(text);
            let fits = |c: &(String, [f32; 4])| !c.0.is_empty() && want.starts_with(&c.0);
            let pick = (cursor..cands.len())
                .find(|&j| !used.get(j).copied().unwrap_or(true) && cands.get(j).is_some_and(fits))
                .or_else(|| {
                    (0..cands.len()).find(|&j| !used.get(j).copied().unwrap_or(true) && cands.get(j).is_some_and(fits))
                })?;
            if let Some(u) = used.get_mut(pick) {
                *u = true;
            }
            cursor = pick + 1;
            cands.get(pick).map(|c| c.1)
        })
        .collect()
}

/// Zonas de las flechas en el SVG, en el orden de [`edges`]: alrededor de la mitad de cada trazo
/// (y de su etiqueta, si la tiene). Vacío si el número de trazos no coincide.
pub fn edge_boxes(svg: &str, count: usize) -> Vec<[f32; 4]> {
    let mut out: Vec<[f32; 4]> = Vec::new();
    let mut i = 0;
    let mut last_was_edge = false;
    for _ in 0..50_000 {
        let Some(rest) = svg.get(i..) else { break };
        let Some(p) = rest.find('<') else { break };
        i += p;
        let Some(rest) = svg.get(i..) else { break };
        if rest.starts_with("<defs") {
            match rest.find("</defs>") {
                Some(e) => i += e + 7,
                None => break,
            }
            continue;
        }
        let Some(e) = rest.find('>') else { break };
        let tag = rest.get(..e + 1).unwrap_or("");
        i += e + 1;
        if tag.starts_with("<path") && tag.contains("fill=\"none\"") {
            let Some(d) = tag.find(" d=\"").and_then(|a| tag.get(a + 4..)).and_then(|r| r.get(..r.find('"')?)) else {
                continue;
            };
            // Los arcos (`A`) son de figuras (el fondo de un cilindro), no flechas.
            if d.contains('A') || d.contains('a') {
                last_was_edge = false;
                continue;
            }
            let nums: Vec<f32> = d
                .split(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
                .filter_map(|t| t.parse().ok())
                .take(2_000)
                .collect();
            let pairs = nums.len() / 2;
            let mid = pairs / 2;
            let (Some(x), Some(y)) = (nums.get(mid * 2), nums.get(mid * 2 + 1)) else { continue };
            out.push([x - 9.0, y - 9.0, x + 9.0, y + 9.0]);
            last_was_edge = true;
        } else if tag.starts_with("<rect") && tag.contains("stroke=\"none\"") && last_was_edge {
            // La etiqueta de la flecha anterior.
            if let (Some((x, y, w, h)), Some(b)) = (crate::diagram::shape_box(tag), out.last_mut()) {
                *b = [b[0].min(x), b[1].min(y), b[2].max(x + w), b[3].max(y + h)];
            }
            last_was_edge = false;
        } else if !tag.starts_with("<text") && !tag.starts_with("</") {
            last_was_edge = false;
        }
    }
    if out.len() == count { out } else { Vec::new() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const F: &str = "```mermaid\nflowchart TD\n    %% comentario\n    A[Inicio] --> B{¿Listo?}\n    B -->|sí| C(Fin):::verde\n    B -- no --> D[(Base)]\n    D --> A\n    classDef verde fill:#9f9\n    class D verde\n```\n";

    fn code(b: &str) -> String {
        crate::diagram::mermaid_code(b).unwrap()
    }

    fn parsed_ids(b: &str) -> Vec<String> {
        nodes(&code(b)).into_iter().map(|(id, _, _)| id).collect()
    }

    #[test]
    fn reads_statements_with_shapes_labels_and_classes() {
        let it = items("    B -->|sí| C(Fin):::verde").unwrap();
        assert_eq!(nodes_of(&it).map(|n| n.id.as_str()).collect::<Vec<_>>(), ["B", "C"]);
        let it = items("    B -- no --> D[(Base)]").unwrap();
        assert_eq!(nodes_of(&it).map(|n| n.id.as_str()).collect::<Vec<_>>(), ["B", "D"]);
        let it = items("  A & B --> C ;").unwrap();
        assert_eq!(nodes_of(&it).count(), 3);
        assert!(items("    classDef verde fill:#9f9").is_none());
        assert!(items("flowchart TD").is_none());
        assert!(items("  A[\"texto con ] adentro\"] --> B").is_some());
    }

    #[test]
    fn adds_after_with_the_inherited_class() {
        let out = add_after(F, &code(F), "C", Shape::Rhombus, "¿Otra vez?").unwrap();
        assert!(out.contains("    B -->|sí| C(Fin):::verde\n    C --> N1{¿Otra vez?}:::verde\n"), "{out}");
        // En el orden del código: justo después de C.
        assert_eq!(parsed_ids(&out), ["A", "B", "C", "N1", "D"]);
        // Sin clase: sin `:::`.
        let out = add_after(F, &code(F), "A", Shape::Rect, "Paso (1)").unwrap();
        assert!(out.contains("    D --> A\n    A --> N1[\"Paso (1)\"]\n"), "{out}");
        assert!(add_after(F, &code(F), "Z", Shape::Rect, "x").is_none());
        assert!(add_after(F, &code(F), "A", Shape::Rect, "   ").is_none());
    }

    #[test]
    fn adds_loose_nodes_and_connects() {
        let out = add_loose(F, &code(F), Shape::Circle, "Solo").unwrap();
        assert!(out.ends_with("    class D verde\n    N1((Solo))\n```\n"), "{out}");
        let out = connect(F, &code(F), "C", "A").unwrap();
        assert!(out.contains("C(Fin):::verde\n    C --> A\n"), "{out}");
        assert!(connect(F, &code(F), "A", "A").is_none());
    }

    #[test]
    fn changes_text_and_shape_in_place() {
        let out = set_text(F, "B", "¿Todo listo?").unwrap();
        assert!(out.contains("    A[Inicio] --> B{¿Todo listo?}\n"), "{out}");
        let out = set_shape(F, "A", Shape::Stadium).unwrap();
        assert!(out.contains("    A([Inicio]) --> B{¿Listo?}\n"), "{out}");
        // Un nodo sin forma propia: se le agrega.
        let g = "```mermaid\ngraph LR\n  x --> y\n```\n";
        assert_eq!(set_text(g, "y", "Ye").unwrap(), "```mermaid\ngraph LR\n  x --> y[Ye]\n```\n");
        assert_eq!(set_shape(g, "x", Shape::Circle).unwrap(), "```mermaid\ngraph LR\n  x((x)) --> y\n```\n");
    }

    #[test]
    fn deletes_with_its_arrows_and_directives() {
        let out = delete(F, "D").unwrap();
        assert_eq!(
            out,
            "```mermaid\nflowchart TD\n    %% comentario\n    A[Inicio] --> B{¿Listo?}\n    B -->|sí| C(Fin):::verde\n    B\n    A\n    classDef verde fill:#9f9\n```\n"
        );
        // A la mitad de una cadena: quedan los dos trozos.
        let g = "```mermaid\ngraph LR\n  a[Uno] --> b --> c[Tres]\n```\n";
        assert_eq!(delete(g, "b").unwrap(), "```mermaid\ngraph LR\n  a[Uno]\n  c[Tres]\n```\n");
        assert_eq!(parsed_ids(&delete(F, "B").unwrap()), ["A", "C", "D"]);
        assert!(delete(F, "Q").is_none());
    }

    #[test]
    fn boxes_match_the_drawing() {
        let c = code(F);
        let svg = mermaid_svg::render(&c).unwrap();
        let ns = nodes(&c);
        let bx = boxes(&svg, &ns);
        assert_eq!(bx.len(), 4);
        assert!(bx.iter().all(Option::is_some), "{bx:?}");
        // Cada caja contiene el texto de su nodo.
        for ((_, text, _), b) in ns.iter().zip(&bx) {
            let [x0, y0, x1, y1] = b.unwrap();
            let at = svg.find(&format!(">{text}</text>")).unwrap();
            let tag = &svg[svg[..at].rfind("<text").unwrap()..at];
            let num = |k: &str| {
                let s = &tag[tag.find(&format!(" {k}=\"")).unwrap() + k.len() + 3..];
                s[..s.find('"').unwrap()].parse::<f32>().unwrap()
            };
            let (tx, ty) = (num("x"), num("y"));
            assert!(tx >= x0 && tx <= x1 && ty >= y0 && ty <= y1 + 6.0, "{text}: {tx},{ty} fuera de {b:?}");
        }
    }

    #[test]
    fn edits_arrows_one_by_one() {
        let c = code(F);
        // 0: A-->B, 1: B-->|sí|C, 2: B -- no --> D, 3: D-->A
        let out = set_edge_text(F, &c, 0, "empieza").unwrap();
        assert!(out.contains("    A[Inicio] -->|empieza| B{¿Listo?}\n"), "{out}");
        let out = set_edge_text(F, &c, 1, "").unwrap();
        assert!(out.contains("    B --> C(Fin):::verde\n"), "{out}");
        let out = set_edge_kind(F, &c, 2, EdgeKind::Dotted).unwrap();
        assert!(out.contains("    B -.->|no| D[(Base)]\n"), "{out}");
        let out = set_edge_kind(F, &c, 3, EdgeKind::Thick).unwrap();
        assert!(out.contains("    D ==> A\n"), "{out}");
        assert!(can_reverse(F, &c, 3));
        let out = reverse_edge(F, &c, 3).unwrap();
        assert!(out.contains("    A --> D\n"), "{out}");
        let out = delete_edge(F, &c, 3).unwrap();
        assert!(out.contains("    D\n    A\n"), "{out}");
        assert_eq!(edges(&code(&out)).len(), 3);
        // Con `&`: no se edita esa flecha (cambiaría las dos).
        let g = "```mermaid\ngraph LR\n  a & b --> c\n```\n";
        assert!(set_edge_kind(g, &code(g), 0, EdgeKind::Thick).is_none());
    }

    #[test]
    fn edge_boxes_match_the_drawing() {
        let c = code(F);
        let svg = mermaid_svg::render(&c).unwrap();
        let eb = edge_boxes(&svg, edges(&c).len());
        assert_eq!(eb.len(), 4);
        // La flecha con etiqueta «sí» contiene su etiqueta.
        let at = svg.find(">sí</text>").unwrap();
        let tag = &svg[svg[..at].rfind("<text").unwrap()..at];
        let x: f32 = tag[tag.find(" x=\"").unwrap() + 4..].split('"').next().unwrap().parse().unwrap();
        assert!(eb[1][0] <= x && x <= eb[1][2], "{:?} {x}", eb[1]);
        assert!(edge_boxes(&svg, 9).is_empty(), "si no coincide, nada");
    }

    proptest! {
        #[test]
        fn never_panics_and_reparses(text in ".{0,30}", junk in ".{0,120}", k in 0usize..6, s in 0usize..9) {
            let shape = Shape::ALL[s];
            let block = format!("```mermaid\nflowchart TD\n    A --> B\n    {junk}\n```\n");
            let _ = statements(&block);
            let ids = ["A", "B", "C", "N1", "", "é"];
            let id = ids[k];
            let _ = set_text(&block, id, &text);
            let _ = set_shape(&block, id, shape);
            let _ = delete(&block, id);
            let _ = boxes(&junk, &[]);
            let _ = edge_boxes(&junk, 2);
            let c = format!("flowchart TD\n    A --> B\n    {junk}");
            let _ = set_edge_text(&block, &c, k, &text);
            let _ = set_edge_kind(&block, &c, k, EdgeKind::Open);
            let _ = reverse_edge(&block, &c, k);
            let _ = delete_edge(&block, &c, k);
            // Sobre un diagrama válido, lo agregado se vuelve a leer.
            if let Some(out) = add_after(F, &code(F), "A", shape, &text) {
                prop_assert_eq!(parsed_ids(&out).len(), 5);
            }
            if let Some(out) = set_text(F, "B", &text) {
                prop_assert_eq!(parsed_ids(&out), parsed_ids(F));
            }
        }
    }
}
