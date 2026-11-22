//! S1.20: diagramas Mermaid de las notas con opciones de dibujo, errores en lenguaje humano,
//! rasterizado y exportación. Sin egui (capa L5): la UI pide el SVG, los píxeles o el PNG y los
//! muestra; cuando exista `iure-doc` (S1.11) este módulo se muda allá tal cual.
//!
//! **Opciones de un diagrama**, en la cabecera YAML estándar de Mermaid, para que `mermaid.js` y
//! cualquier otro visor las entiendan:
//!
//! ```text
//! ---
//! config:
//!   theme: forest              # tema de Mermaid; sin él, el de la app
//!   themeVariables:            # colores elegidos (claves estándar de Mermaid)
//!     primaryColor: "#dbeafe"  # relleno de los nodos
//!     lineColor: "#1d4ed8"     # líneas
//! iure:
//!   layout: right              # solo `mindmap`: right | radial | both | down
//!   shape: cloud               # solo `mindmap`: forma de los nodos
//!   root: small                # solo `mindmap`: tamaño del centro
//!   palette: warm              # solo `mindmap`: colores de las ramas
//! ---
//! mindmap
//!   …
//! ```
//!
//! Lo propio va bajo `iure:`, que Mermaid ignora. [`set_options`] reescribe solo esas claves (en
//! su sitio si ya estaban) y conserva todo lo demás (título, otras claves, comentarios).
//!
//! **Tema**: [`DiagramTheme::App`] (el de omisión) parte del claro u oscuro de Mermaid según la
//! nota y le pone los colores de la app (texto, acento, bordes; en mapas mentales, los colores de
//! rama con contraste AA). Los temas de Mermaid se aplican aquí y no dentro de `mermaid-svg`: si
//! se lo dejara a él, perdería la letra de la nota y el tamaño fijo en píxeles que pinta Blitz.
//! Los colores elegidos (`themeVariables`) sí los aplica `mermaid-svg` encima del tema.
//!
//! **Enlaces de nodos** (S1.24): `click A "https://…"` sale del SVG como zona ([`DiagramLink`]) y
//! la UI la atiende con confirmación; los `click A callback` (JavaScript) se descartan.

use std::ops::Range;

use crate::markdown::NoteStyle;
use crate::mindmap::{self, Layout, Look, Node, Palette, RootSize, Shape};

/// Tema de un diagrama.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum DiagramTheme {
    /// Los colores de la app (claro u oscuro según la nota).
    #[default]
    App,
    /// Los de Mermaid.
    Default,
    Neutral,
    Forest,
    Dark,
    Base,
}

impl DiagramTheme {
    pub const ALL: [DiagramTheme; 6] = [
        DiagramTheme::App,
        DiagramTheme::Default,
        DiagramTheme::Neutral,
        DiagramTheme::Forest,
        DiagramTheme::Dark,
        DiagramTheme::Base,
    ];

    /// Nombre en `config.theme` (`None`: el de la app, sin clave).
    pub fn key(self) -> Option<&'static str> {
        match self {
            DiagramTheme::App => None,
            DiagramTheme::Default => Some("default"),
            DiagramTheme::Neutral => Some("neutral"),
            DiagramTheme::Forest => Some("forest"),
            DiagramTheme::Dark => Some("dark"),
            DiagramTheme::Base => Some("base"),
        }
    }

    fn from_key(k: &str) -> DiagramTheme {
        DiagramTheme::ALL.into_iter().find(|t| t.key().is_some_and(|n| n.eq_ignore_ascii_case(k))).unwrap_or_default()
    }
}

/// Un color elegible de un diagrama (variable estándar de Mermaid).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ColorRole {
    /// Relleno de los nodos (`primaryColor`).
    NodeFill,
    /// Borde de los nodos (`primaryBorderColor`).
    NodeBorder,
    /// Texto (`primaryTextColor`).
    Text,
    /// Líneas y flechas (`lineColor`).
    Line,
    /// Fondo de grupos y subgrafos (`secondaryColor`).
    GroupFill,
    /// Fondo de las notas (`noteBkgColor`).
    NoteFill,
}

impl ColorRole {
    pub const ALL: [ColorRole; 6] = [
        ColorRole::NodeFill,
        ColorRole::NodeBorder,
        ColorRole::Text,
        ColorRole::Line,
        ColorRole::GroupFill,
        ColorRole::NoteFill,
    ];

    /// Nombre en `config.themeVariables`.
    pub fn key(self) -> &'static str {
        match self {
            ColorRole::NodeFill => "primaryColor",
            ColorRole::NodeBorder => "primaryBorderColor",
            ColorRole::Text => "primaryTextColor",
            ColorRole::Line => "lineColor",
            ColorRole::GroupFill => "secondaryColor",
            ColorRole::NoteFill => "noteBkgColor",
        }
    }
}

/// Colores elegidos (RGB); `None`: el del tema.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct DiagramColors(pub [Option<[u8; 3]>; 6]);

impl DiagramColors {
    pub fn get(&self, role: ColorRole) -> Option<[u8; 3]> {
        self.0.get(role as usize).copied().flatten()
    }

    pub fn set(&mut self, role: ColorRole, c: Option<[u8; 3]>) {
        if let Some(slot) = self.0.get_mut(role as usize) {
            *slot = c;
        }
    }

    pub fn is_empty(&self) -> bool {
        self.0.iter().all(Option::is_none)
    }
}

/// `#rrggbb` (o `#rgb`) → RGB.
pub fn parse_hex(v: &str) -> Option<[u8; 3]> {
    let h = v.trim().strip_prefix('#')?;
    let h: String = match h.len() {
        3 => h.chars().flat_map(|c| [c, c]).collect(),
        6 => h.to_string(),
        _ => return None,
    };
    let n = u32::from_str_radix(&h, 16).ok()?;
    Some([(n >> 16) as u8, (n >> 8) as u8, n as u8])
}

fn hex(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

/// Opciones de dibujo de un diagrama.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct DiagramOptions {
    pub theme: DiagramTheme,
    pub colors: DiagramColors,
    /// Solo mapas mentales: distribución (`None`: la radial).
    pub layout: Option<Layout>,
    /// Solo mapas mentales: forma de los nodos, tamaño del centro y colores de las ramas.
    pub shape: Shape,
    pub root: RootSize,
    pub palette: Palette,
}

impl DiagramOptions {
    /// ¿Pide algo de lo que solo sabe el dibujo propio de mapas mentales?
    fn wants_own_mindmap(&self) -> bool {
        self.layout.is_some()
            || self.shape != Shape::Default
            || self.root != RootSize::Auto
            || self.palette != Palette::Iure
    }
}

/// Valor de distribución en `iure.layout`.
fn layout_value(l: Layout) -> &'static str {
    match l {
        Layout::Right => "right",
        Layout::Radial => "radial",
        Layout::Balanced => "both",
        Layout::Down => "down",
    }
}

fn layout_from_value(v: &str) -> Option<Layout> {
    Layout::ALL.into_iter().find(|l| v.eq_ignore_ascii_case(layout_value(*l)))
}

fn root_value(r: RootSize) -> Option<&'static str> {
    match r {
        RootSize::Auto => None,
        RootSize::Small => Some("small"),
        RootSize::Medium => Some("medium"),
        RootSize::Large => Some("large"),
    }
}

fn root_from_value(v: &str) -> Option<RootSize> {
    RootSize::ALL.into_iter().find(|r| root_value(*r).is_some_and(|k| v.eq_ignore_ascii_case(k)))
}

/// Líneas de la cabecera `--- … ---` del código (sin los separadores) y la línea donde empieza el
/// diagrama. `None` si no hay cabecera cerrada.
fn frontmatter(code: &str) -> Option<(Vec<&str>, usize)> {
    let lines: Vec<&str> = code.lines().collect();
    let first = lines.iter().position(|l| !l.trim().is_empty())?;
    if lines.get(first)?.trim() != "---" {
        return None;
    }
    let close = lines.iter().enumerate().skip(first + 1).find(|(_, l)| l.trim() == "---").map(|(i, _)| i)?;
    Some((lines.get(first + 1..close)?.to_vec(), close + 1))
}

/// Clave de una línea YAML sencilla: (sangría, clave, valor sin comillas).
fn yaml_pair(line: &str) -> Option<(usize, &str, &str)> {
    let indent = line.len() - line.trim_start().len();
    let t = line.trim();
    if t.starts_with('#') || t.starts_with('-') {
        return None;
    }
    let (k, v) = t.split_once(':')?;
    if k.is_empty() || k.contains(' ') {
        return None;
    }
    let v = v.trim();
    let v = if v.starts_with(['"', '\'']) {
        v.trim_matches(['"', '\''])
    } else {
        v.split(" #").next().unwrap_or("").trim()
    };
    Some((indent, k.trim(), v))
}

/// Una línea de la cabecera y las que cuelgan de ella (por sangría).
#[derive(Clone, Debug)]
struct Y {
    /// Clave, si la línea es `clave: valor`; `None` para comentarios, vacías o lo que no se
    /// entiende (se conserva tal cual).
    key: Option<String>,
    value: String,
    indent: usize,
    /// La línea original (se reescribe solo si cambia su valor).
    raw: String,
    kids: Vec<Y>,
}

/// Árbol de la cabecera por sangría: primero una lista plana con el padre de cada línea, luego
/// el árbol. Lo que no es `clave: valor` cuelga del último abierto (conserva su sitio).
fn parse_yaml(lines: &[&str]) -> Vec<Y> {
    let mut flat: Vec<(Y, Option<usize>)> = Vec::new();
    // Índices (en `flat`) de los nodos con clave abiertos, con su sangría.
    let mut stack: Vec<(usize, usize)> = Vec::new();
    for l in lines {
        let pair = yaml_pair(l);
        if let Some((indent, _, _)) = pair {
            while stack.last().is_some_and(|(i, _)| *i >= indent) {
                stack.pop();
            }
        }
        let parent = stack.last().map(|(_, p)| *p);
        flat.push((
            Y {
                key: pair.map(|(_, k, _)| k.to_string()),
                value: pair.map(|(_, _, v)| v.to_string()).unwrap_or_default(),
                indent: pair.map_or(0, |(i, _, _)| i),
                raw: (*l).to_string(),
                kids: Vec::new(),
            },
            parent,
        ));
        if let Some((indent, _, _)) = pair
            && stack.len() < 8
        {
            stack.push((indent, flat.len() - 1));
        }
    }
    fn build(flat: &[(Y, Option<usize>)], parent: Option<usize>) -> Vec<Y> {
        flat.iter()
            .enumerate()
            .filter(|(_, (_, p))| *p == parent)
            .map(|(i, (y, _))| Y { kids: build(flat, Some(i)), ..y.clone() })
            .collect()
    }
    build(&flat, None)
}

/// Valor en `path` (claves anidadas). Con claves repetidas gana la última (como en YAML).
fn yaml_get<'a>(nodes: &'a [Y], path: &[&str]) -> Option<&'a str> {
    let (first, rest) = path.split_first()?;
    let n = nodes.iter().rev().find(|n| n.key.as_deref() == Some(*first))?;
    if rest.is_empty() { Some(n.value.as_str()) } else { yaml_get(&n.kids, rest) }
}

/// Pone (o quita, con `None`) el valor en `path`; lo que falte se crea con la sangría de sus
/// hermanos (o `indent` si no tiene; en el nivel superior, sin sangría). Con claves repetidas se
/// escribe en la última (la que vale) y se quita de las demás. Al quitar, un padre que queda vacío
/// y sin valor se va también.
fn yaml_set(nodes: &mut Vec<Y>, path: &[&str], value: Option<&str>, indent: usize) {
    let Some((first, rest)) = path.split_first() else { return };
    // En el nivel superior, siempre sin sangría (una clave sangrada sin padre no es modelo).
    // Lo nuevo va al final: con la sangría del último hermano (si las del bloque no coinciden, así
    // no queda colgado del anterior).
    let indent =
        if indent == 0 { 0 } else { nodes.iter().rev().find(|n| n.key.is_some()).map_or(indent, |n| n.indent) };
    let matches: Vec<usize> =
        nodes.iter().enumerate().filter(|(_, n)| n.key.as_deref() == Some(*first)).map(|(i, _)| i).collect();
    let target = match (matches.last().copied(), value) {
        (Some(i), _) => Some(i),
        (None, None) => None,
        (None, Some(v)) => {
            let node = |raw: String, value: &str| Y {
                key: Some((*first).to_string()),
                value: value.to_string(),
                indent,
                raw,
                kids: Vec::new(),
            };
            if rest.is_empty() {
                nodes.push(node(format!("{}{first}: {}", " ".repeat(indent), quote(v)), v));
                return;
            }
            // `config` va antes que `iure` (lo de Mermaid primero).
            let at = if *first == "config" {
                nodes.iter().position(|n| n.key.as_deref() == Some("iure")).unwrap_or(nodes.len())
            } else {
                nodes.len()
            };
            nodes.insert(at, node(format!("{}{first}:", " ".repeat(indent)), ""));
            Some(at)
        }
    };
    // De atrás hacia adelante: quitar no mueve los índices que faltan.
    let mut all: Vec<usize> =
        nodes.iter().enumerate().filter(|(_, n)| n.key.as_deref() == Some(*first)).map(|(i, _)| i).collect();
    all.sort_unstable();
    for i in all.into_iter().rev() {
        let mine = Some(i) == target;
        let v = if mine { value } else { None };
        if rest.is_empty() {
            match v {
                None => {
                    nodes.remove(i);
                }
                Some(v) => {
                    if let Some(n) = nodes.get_mut(i)
                        && n.value != v
                    {
                        n.value = v.to_string();
                        n.raw = format!("{}{first}: {}", " ".repeat(n.indent), quote(v));
                    }
                }
            }
            continue;
        }
        let Some(n) = nodes.get_mut(i) else { continue };
        let child_indent = n.indent + 2;
        yaml_set(&mut n.kids, rest, v, child_indent);
        if v.is_none() && n.value.is_empty() && n.kids.iter().all(|k| k.key.is_none() && k.raw.trim().is_empty()) {
            nodes.remove(i);
        }
    }
}

/// Valor YAML: entre comillas si lleva `#` o `:` (un color `#…` sin comillas sería un comentario).
fn quote(v: &str) -> String {
    if v.contains(['#', ':']) || v.starts_with([' ', '\'', '"']) { format!("\"{v}\"") } else { v.to_string() }
}

fn yaml_emit(nodes: &[Y], out: &mut String) {
    for n in nodes {
        out.push_str(&n.raw);
        out.push('\n');
        yaml_emit(&n.kids, out);
    }
}

/// Opciones que trae el código.
pub fn read_options(code: &str) -> DiagramOptions {
    let Some((lines, _)) = frontmatter(code) else { return DiagramOptions::default() };
    let y = parse_yaml(&lines);
    let mut colors = DiagramColors::default();
    for role in ColorRole::ALL {
        colors.set(role, yaml_get(&y, &["config", "themeVariables", role.key()]).and_then(parse_hex));
    }
    DiagramOptions {
        theme: yaml_get(&y, &["config", "theme"]).map(DiagramTheme::from_key).unwrap_or_default(),
        colors,
        layout: yaml_get(&y, &["iure", "layout"]).and_then(layout_from_value),
        shape: yaml_get(&y, &["iure", "shape"]).and_then(Shape::from_token).unwrap_or_default(),
        root: yaml_get(&y, &["iure", "root"]).and_then(root_from_value).unwrap_or_default(),
        palette: yaml_get(&y, &["iure", "palette"]).and_then(Palette::from_value).unwrap_or_default(),
    }
}

/// El código con las opciones nuevas: reescribe `config.theme`, `config.themeVariables.*` (los
/// colores elegibles) e `iure.*` en su sitio, crea lo que falte, quita lo que vuelve a su valor de
/// omisión (y las secciones que quedan vacías) y conserva todo lo demás. Fin de línea `\n`.
pub fn set_options(code: &str, opts: &DiagramOptions) -> String {
    let (lines, body_start) = frontmatter(code).unwrap_or((Vec::new(), 0));
    let mut y = parse_yaml(&lines);
    yaml_set(&mut y, &["config", "theme"], opts.theme.key(), 0);
    for role in ColorRole::ALL {
        let v = opts.colors.get(role).map(hex);
        yaml_set(&mut y, &["config", "themeVariables", role.key()], v.as_deref(), 0);
    }
    yaml_set(&mut y, &["iure", "layout"], opts.layout.map(layout_value), 0);
    yaml_set(&mut y, &["iure", "shape"], (opts.shape != Shape::Default).then(|| opts.shape.token()), 0);
    yaml_set(&mut y, &["iure", "root"], root_value(opts.root), 0);
    yaml_set(&mut y, &["iure", "palette"], (opts.palette != Palette::Iure).then(|| opts.palette.value()), 0);
    let mut out = String::new();
    if y.iter().any(|n| n.key.is_some() || !n.raw.trim().is_empty()) {
        out.push_str("---\n");
        yaml_emit(&y, &mut out);
        out.push_str("---\n");
    }
    for l in code.lines().skip(body_start) {
        out.push_str(l);
        out.push('\n');
    }
    out
}

/// El código que se le pasa a `mermaid-svg`: sin `config.theme` (el tema se aplica aquí), con las
/// mismas líneas.
fn for_renderer(code: &str) -> String {
    let Some((lines, body_start)) = frontmatter(code) else { return code.to_string() };
    let mut out = String::from("---\n");
    let mut section = String::new();
    for l in &lines {
        if let Some((indent, k, _)) = yaml_pair(l) {
            if indent == 0 {
                section = k.to_string();
            } else if section == "config" && k == "theme" && indent <= 2 {
                // En blanco y no fuera: los números de línea de los errores siguen siendo los del
                // código que ve el usuario.
                out.push('\n');
                continue;
            }
        }
        out.push_str(l);
        out.push('\n');
    }
    out.push_str("---\n");
    for l in code.lines().skip(body_start) {
        out.push_str(l);
        out.push('\n');
    }
    out
}

/// Tipo del diagrama: la primera palabra tras la cabecera, directivas y comentarios.
pub fn kind(code: &str) -> String {
    let start = frontmatter(code).map_or(0, |(_, s)| s);
    code.lines()
        .skip(start)
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("%%"))
        .and_then(|l| l.split_whitespace().next())
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// Por qué no se pudo dibujar, en lenguaje humano.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiagramError {
    /// Línea del código (1 = la primera, contando la cabecera), si se sabe.
    pub line: Option<usize>,
    pub en: String,
    pub es: String,
}

impl DiagramError {
    fn new(line: Option<usize>, en: impl Into<String>, es: impl Into<String>) -> Self {
        Self { line, en: en.into(), es: es.into() }
    }
}

/// Error de sintaxis en palabras sencillas: qué pasa, en qué línea (citada) y, aparte, el detalle
/// técnico de `mermaid-svg` (en inglés).
fn syntax_error(code: &str, kind: mermaid_svg::SyntaxKind, message: &str, line: usize) -> DiagramError {
    use mermaid_svg::SyntaxKind as K;
    let text: String = code.lines().nth(line.saturating_sub(1)).unwrap_or("").trim().chars().take(60).collect();
    let quoted_en = if text.is_empty() { String::new() } else { format!(" (“{text}”)") };
    let quoted_es = if text.is_empty() { String::new() } else { format!(" («{text}»)") };
    let (en, es) = match kind {
        K::Unclosed => (
            format!("On line {line}{quoted_en} a bracket, brace or quote is left open."),
            format!("En la línea {line}{quoted_es} falta cerrar un corchete, una llave o unas comillas."),
        ),
        K::InvalidNumber => (
            format!("On line {line}{quoted_en} there is a number that can't be read."),
            format!("En la línea {line}{quoted_es} hay un número que no se puede leer."),
        ),
        K::UnknownStatement => (
            format!("Line {line}{quoted_en} isn't something this kind of diagram understands."),
            format!("La línea {line}{quoted_es} no es algo que este tipo de diagrama entienda."),
        ),
        K::MissingHeader => (
            "The first line must say the diagram type (flowchart, sequenceDiagram, gantt, mindmap…).".to_string(),
            "La primera línea debe decir el tipo de diagrama (flowchart, sequenceDiagram, gantt, mindmap…)."
                .to_string(),
        ),
        _ => (
            format!("Line {line}{quoted_en} is incomplete or has something out of place."),
            format!("La línea {line}{quoted_es} está incompleta o tiene algo fuera de lugar."),
        ),
    };
    let detail = message.trim().trim_end_matches('.');
    DiagramError::new(Some(line), format!("{en} (Detail: {detail}.)"), format!("{es} (Detalle: {detail}.)"))
}

/// Comprueba la sintaxis sin dibujar. Acotado como el dibujo.
pub fn check(code: &str) -> Result<(), DiagramError> {
    let code = code.trim();
    if code.is_empty() {
        return Err(DiagramError::new(None, "The diagram is empty.", "El diagrama está vacío."));
    }
    if code.len() > crate::mermaid::MAX_SOURCE || code.lines().count() > crate::mermaid::MAX_LINES {
        return Err(DiagramError::new(
            None,
            "The diagram is too large to draw here.",
            "El diagrama es demasiado grande para dibujarlo aquí.",
        ));
    }
    let src = for_renderer(code);
    let r = std::panic::catch_unwind(|| mermaid_svg::parse(&src).map(|_| ()));
    match r {
        Ok(Ok(())) => Ok(()),
        Ok(Err(mermaid_svg::ParseError::UnknownDiagramType(t))) => Err(DiagramError::new(
            Some(frontmatter(code).map_or(1, |(_, s)| s + 1)),
            format!("“{t}” is not a diagram type this view knows (flowchart, sequenceDiagram, gantt, mindmap…)."),
            format!(
                "«{t}» no es un tipo de diagrama que esta vista conozca (flowchart, sequenceDiagram, gantt, mindmap…)."
            ),
        )),
        Ok(Err(mermaid_svg::ParseError::Empty)) => {
            Err(DiagramError::new(None, "The diagram is empty.", "El diagrama está vacío."))
        }
        Ok(Err(mermaid_svg::ParseError::Syntax { kind, message, line })) => {
            Err(syntax_error(code, kind, &message, line))
        }
        Ok(Err(e)) => {
            Err(DiagramError::new(None, format!("It can't be drawn: {e}."), format!("No se puede dibujar: {e}.")))
        }
        Err(_) => Err(DiagramError::new(None, "It can't be drawn here.", "No se puede dibujar aquí.")),
    }
}

/// Tema de la app sobre la base clara u oscura de Mermaid.
fn app_theme(theme: &mut mermaid_svg::Theme, style: &NoteStyle) {
    let hex = |c: [u8; 3]| format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]);
    theme.fg = hex(style.text).into();
    theme.fg_muted = hex(style.muted).into();
    theme.title_color = Some(hex(style.text).into());
    theme.flow_node_fill = hex(style.code_bg).into();
    theme.flow_node_stroke = hex(style.accent).into();
    theme.flow_edge_stroke = hex(style.muted).into();
    theme.flow_cluster_stroke = hex(style.border).into();
    theme.actor_fill = hex(style.code_bg).into();
    theme.actor_stroke = hex(style.accent).into();
    theme.note_stroke = hex(style.border).into();
}

/// Un mapa mental de Mermaid como árbol propio (para dibujarlo con otra distribución). Etiquetas
/// planas y tamaños acotados como los de los mapas de IureEditor.
fn mindmap_tree(code: &str) -> Option<Node> {
    let src = for_renderer(code);
    let d = std::panic::catch_unwind(|| mermaid_svg::parse(&src)).ok()?.ok()?;
    let mermaid_svg::Diagram::Mindmap(m) = d else { return None };
    let mut count = 0usize;
    fn conv(n: &mermaid_svg::ast::MindmapNode, depth: usize, count: &mut usize) -> Node {
        *count += 1;
        let mut children = Vec::new();
        if depth < mindmap::MAX_DEPTH {
            for c in &n.children {
                if *count >= mindmap::MAX_NODES {
                    break;
                }
                children.push(conv(c, depth + 1, count));
            }
        }
        Node { label: mindmap::plain_label(&n.text), children, line: 0 }
    }
    m.root.as_ref().map(|r| conv(r, 0, &mut count))
}

/// S1.24: zona de un nodo con enlace, en px del SVG.
#[derive(Clone, Debug, PartialEq)]
pub struct DiagramLink {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// `http`, `https` o `mailto` (lo demás no se acepta).
    pub href: String,
}

impl DiagramLink {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x <= self.x + self.w && y <= self.y + self.h
    }
}

/// Un diagrama dibujado: el SVG (seguro, sin enlaces), su tamaño y las zonas con enlace.
#[derive(Clone, Debug, PartialEq)]
pub struct Drawn {
    pub svg: String,
    pub width: f32,
    pub height: f32,
    pub links: Vec<DiagramLink>,
}

/// Enlaces de un diagrama como máximo.
const MAX_LINKS: usize = 200;

fn unescape(s: &str) -> String {
    s.replace("&quot;", "\"").replace("&#39;", "'").replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

/// Atributo numérico `name` de una etiqueta.
fn num_attr(tag: &str, name: &str) -> Option<f32> {
    let at = tag.find(&format!(" {name}=\""))? + name.len() + 3;
    let rest = tag.get(at..)?;
    rest.get(..rest.find('"')?)?.trim().parse().ok()
}

/// Caja de la primera figura de `group` (rect, circle, ellipse, polygon o path).
fn shape_box(group: &str) -> Option<(f32, f32, f32, f32)> {
    let starts = ["<rect", "<circle", "<ellipse", "<polygon", "<path"];
    let (pos, kind) = starts.iter().filter_map(|k| group.find(k).map(|p| (p, *k))).min_by_key(|(p, _)| *p)?;
    let tag = group.get(pos..pos + group.get(pos..)?.find('>')?)?;
    match kind {
        "<rect" => Some((num_attr(tag, "x")?, num_attr(tag, "y")?, num_attr(tag, "width")?, num_attr(tag, "height")?)),
        "<circle" => {
            let (cx, cy, r) = (num_attr(tag, "cx")?, num_attr(tag, "cy")?, num_attr(tag, "r")?);
            Some((cx - r, cy - r, 2.0 * r, 2.0 * r))
        }
        "<ellipse" => {
            let (cx, cy) = (num_attr(tag, "cx")?, num_attr(tag, "cy")?);
            let (rx, ry) = (num_attr(tag, "rx")?, num_attr(tag, "ry")?);
            Some((cx - rx, cy - ry, 2.0 * rx, 2.0 * ry))
        }
        _ => {
            // Puntos de `points` o de `d`: la caja de todos los números en pares.
            let attr = if kind == "<polygon" { "points" } else { "d" };
            let at = tag.find(&format!(" {attr}=\""))? + attr.len() + 3;
            let rest = tag.get(at..)?;
            let val = rest.get(..rest.find('"')?)?;
            let nums: Vec<f32> = val
                .split(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
                .filter_map(|t| t.parse().ok())
                .take(400)
                .collect();
            let xs = nums.iter().step_by(2);
            let ys = nums.iter().skip(1).step_by(2);
            let (x0, x1) = xs.fold((f32::MAX, f32::MIN), |(a, b), v| (a.min(*v), b.max(*v)));
            let (y0, y1) = ys.fold((f32::MAX, f32::MIN), |(a, b), v| (a.min(*v), b.max(*v)));
            (x0 <= x1 && y0 <= y1).then_some((x0, y0, x1 - x0, y1 - y0))
        }
    }
}

/// Saca los enlaces del SVG de `mermaid-svg`: cada `<a href>` pasa a ser un `<g>` (el SVG queda
/// sin referencias externas y pasa el filtro) y su zona se devuelve aparte; los `onclick`
/// (llamadas a JavaScript) se quitan. Solo `http`, `https` y `mailto`.
pub(crate) fn extract_links(svg: &str) -> (String, Vec<DiagramLink>) {
    let mut out = String::with_capacity(svg.len());
    let mut links = Vec::new();
    let mut rest = svg;
    // `</a>` que corresponden a un `<a>` cambiado.
    let mut open_anchors = 0usize;
    while let Some(i) = rest.find('<') {
        out.push_str(rest.get(..i).unwrap_or(""));
        rest = rest.get(i..).unwrap_or("");
        let Some(end) = rest.find('>') else { break };
        let tag = rest.get(..=end).unwrap_or("");
        if tag.starts_with("<a ") || tag == "<a>" {
            let href = tag
                .find("href=\"")
                .and_then(|p| tag.get(p + 6..))
                .and_then(|r| r.get(..r.find('"')?))
                .map(unescape)
                .unwrap_or_default();
            let lower = href.trim().to_ascii_lowercase();
            let ok = lower.starts_with("https://") || lower.starts_with("http://") || lower.starts_with("mailto:");
            let close = rest.find("</a>").unwrap_or(rest.len());
            if ok
                && links.len() < MAX_LINKS
                && let Some((x, y, w, h)) = rest.get(end + 1..close).and_then(shape_box)
            {
                links.push(DiagramLink { x, y, w, h, href: href.trim().to_string() });
            }
            out.push_str("<g>");
            open_anchors += 1;
        } else if tag == "</a>" && open_anchors > 0 {
            out.push_str("</g>");
            open_anchors -= 1;
        } else if tag.contains(" onclick=") {
            // `<g class="clickable" … onclick="…">` → `<g>`.
            out.push_str("<g>");
        } else {
            out.push_str(tag);
        }
        rest = rest.get(end + 1..).unwrap_or("");
    }
    out.push_str(rest);
    (out, links)
}

/// Dibuja el diagrama con sus opciones y el estilo de la nota. El SVG es seguro y de tamaño fijo.
pub fn render(code: &str, style: &NoteStyle) -> Result<String, DiagramError> {
    render_drawn(code, style).map(|d| d.svg)
}

fn too_slow() -> DiagramError {
    DiagramError::new(
        None,
        "It took too long or came out too large to show here.",
        "Tardó demasiado o salió demasiado grande para mostrarlo aquí.",
    )
}

/// Como [`render`], con el tamaño y las zonas de los nodos con enlace.
pub fn render_drawn(code: &str, style: &NoteStyle) -> Result<Drawn, DiagramError> {
    check(code)?;
    let opts = read_options(code);
    let is_mindmap = kind(code) == "mindmap";
    let svg = if is_mindmap
        && opts.wants_own_mindmap()
        && let Some(tree) = mindmap_tree(code)
    {
        let look = Look {
            layout: opts.layout.unwrap_or(Layout::Radial),
            shape: opts.shape,
            root: opts.root,
            palette: opts.palette,
        };
        mindmap::draw(&tree, style, &look)
    } else {
        let mut theme = match opts.theme.key() {
            Some(name) => mermaid_svg::Theme::by_name(name),
            None => crate::mermaid::theme_for(style),
        }
        .ok_or_else(|| DiagramError::new(None, "It can't be drawn here.", "No se puede dibujar aquí."))?;
        theme.font_size = f64::from(style.font_px.clamp(10.0, 24.0));
        theme.responsive = false;
        if opts.theme == DiagramTheme::App {
            app_theme(&mut theme, style);
            if is_mindmap {
                crate::mermaid::branch_palette(&mut theme, style);
            }
        }
        crate::mermaid::render_raw(&for_renderer(code), theme).ok_or_else(too_slow)?
    };
    let (svg, links) = extract_links(&svg);
    if svg.len() > crate::mermaid::MAX_SVG
        || !svg.trim_start().starts_with("<svg")
        || !crate::net::svg_is_safe(svg.as_bytes())
    {
        return Err(too_slow());
    }
    let (width, height) = svg_size(&svg).unwrap_or((0.0, 0.0));
    Ok(Drawn { svg, width, height, links })
}

/// Ancho y alto (px CSS) de un SVG, de sus atributos `width`/`height` o del `viewBox`.
pub fn svg_size(svg: &str) -> Option<(f32, f32)> {
    let head = svg.get(..svg.find('>')?)?;
    let attr = |name: &str| -> Option<f32> {
        let at = head.find(&format!(" {name}=\""))? + name.len() + 3;
        let rest = head.get(at..)?;
        rest.get(..rest.find('"')?)?.trim_end_matches("px").parse().ok()
    };
    match (attr("width"), attr("height")) {
        (Some(w), Some(h)) if w > 0.0 && h > 0.0 => Some((w, h)),
        _ => {
            let vb = head.get(head.find("viewBox=\"")? + 9..)?;
            let nums: Vec<f32> = vb.get(..vb.find('"')?)?.split([' ', ',']).filter_map(|n| n.parse().ok()).collect();
            match nums.as_slice() {
                [_, _, w, h] if *w > 0.0 && *h > 0.0 => Some((*w, *h)),
                _ => None,
            }
        }
    }
}

/// Lado máximo del rasterizado, en px físicos.
pub const MAX_RASTER_SIDE: u32 = 8_192;

/// Píxeles de un SVG a la escala pedida (acotada para no pasar de [`MAX_RASTER_SIDE`]), pintado
/// con Blitz sobre `background`. Bloqueante: la UI lo llama en otro hilo.
pub fn rasterize(svg: &str, scale: f32, background: [u8; 3]) -> Result<crate::Rendered, crate::Error> {
    let (w, h) = svg_size(svg).unwrap_or((800.0, 600.0));
    let side = w.max(h).max(1.0);
    let scale = scale.clamp(0.25, 8.0).min(MAX_RASTER_SIDE as f32 / side).max(0.25);
    let width = (w.ceil() as u32).clamp(1, 4_096);
    let html = format!(
        "<!doctype html><html><head><style>html,body{{margin:0;padding:0}}img{{display:block;width:{w}px;height:{h}px}}</style></head><body><img src=\"cid:diagram\"></body></html>"
    );
    let mut opts = crate::RenderOptions {
        viewport_width: width,
        scale,
        background: Some(background),
        max_height: (h.ceil() as u32).clamp(1, 12_000),
        ..Default::default()
    };
    opts.inline_resources.insert("diagram".into(), crate::Bytes::from(svg.as_bytes().to_vec()));
    crate::render(&html, &opts)
}

/// PNG de unos píxeles RGBA.
pub fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, width, height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().ok()?.write_image_data(rgba).ok()?;
    Some(out)
}

/// Un bloque ```` ```mermaid ```` de la nota, en el orden en que la vista previa los numera.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MermaidBlock {
    pub code: String,
    /// Bytes del código dentro de la nota, si el bloque se puede reescribir (no está dentro de una
    /// cita ni de una lista; en ese caso se edita en IureEditor).
    pub code_range: Option<Range<usize>>,
}

/// Bytes de `range` que ocupa el contenido del bloque cercado sin contenedor, o `None`.
fn editable_range(md: &str, block: Range<usize>) -> Option<Range<usize>> {
    let at_line_start = block.start == 0 || md.get(..block.start)?.ends_with('\n');
    let text = md.get(block.clone())?;
    let first_end = text.find('\n')?;
    let open = text.get(..first_end)?;
    let indent = open.len() - open.trim_start_matches(' ').len();
    let fence = open.trim_start();
    if !at_line_start || indent > 3 || !(fence.starts_with("```") || fence.starts_with("~~~")) {
        return None;
    }
    let content_start = block.start + first_end + 1;
    // La última línea, si es la cerca de cierre, no es código.
    let trimmed = text.trim_end_matches(['\n', '\r']);
    let last_start = trimmed.rfind('\n').map_or(0, |i| i + 1);
    let last = trimmed.get(last_start..)?.trim();
    let mark = fence.chars().next()?;
    let content_end = if last_start > first_end && last.starts_with(mark) && last.chars().all(|c| c == mark) {
        block.start + last_start
    } else {
        block.end
    };
    (content_start <= content_end).then_some(content_start..content_end)
}

/// Los bloques Mermaid de la nota en el orden de la vista previa (los que quedan dentro de un
/// mapa mental en vista `map` no se ven y no cuentan).
pub fn mermaid_blocks(md: &str) -> Vec<MermaidBlock> {
    use pulldown_cmark::{CodeBlockKind, Event, Parser, Tag, TagEnd};
    let hidden: Vec<Range<usize>> =
        mindmap::find(md).into_iter().filter(|b| b.view == mindmap::View::MapOnly).map(|b| b.range).collect();
    let mut out = Vec::new();
    let mut current: Option<(Range<usize>, String)> = None;
    for (ev, range) in Parser::new_ext(md, crate::markdown::parser_options()).into_offset_iter() {
        match ev {
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(lang)))
                if lang.trim().eq_ignore_ascii_case("mermaid") =>
            {
                current = Some((range, String::new()));
            }
            Event::Text(t) => {
                if let Some((_, code)) = current.as_mut() {
                    code.push_str(&t);
                }
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some((block, code)) = current.take()
                    && !hidden.iter().any(|h| h.contains(&block.start))
                {
                    out.push(MermaidBlock { code_range: editable_range(md, block), code });
                }
            }
            _ => {}
        }
        if out.len() >= 256 {
            break;
        }
    }
    out
}

/// La nota con el código del bloque Mermaid `k` cambiado. `None` si no existe o no se puede
/// reescribir aquí.
pub fn replace_mermaid(md: &str, k: usize, code: &str) -> Option<String> {
    let range = mermaid_blocks(md).into_iter().nth(k)?.code_range?;
    let mut code = code.trim_end_matches(['\n', '\r']).to_string();
    code.push('\n');
    if md.get(range.clone())? == code {
        return None;
    }
    let mut out = String::with_capacity(md.len() + code.len());
    out.push_str(md.get(..range.start)?);
    out.push_str(&code);
    out.push_str(md.get(range.end..)?);
    Some(out)
}

/// El código de un bloque ```` ```mermaid ```` (o `~~~mermaid`) cerrado, sin las cercas. Un bloque
/// sin cerrar es `None`: se sigue escribiendo.
pub fn mermaid_code(block: &str) -> Option<String> {
    let mut lines = block.lines();
    let first = lines.next()?.trim_start();
    let fence_char = first.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let n = first.chars().take_while(|c| *c == fence_char).count();
    if n < 3 || !first.get(n..)?.trim().eq_ignore_ascii_case("mermaid") {
        return None;
    }
    let body: Vec<&str> = lines.collect();
    let (last, code) = body.split_last()?;
    let closing = last.trim();
    (closing.len() >= n && closing.chars().all(|c| c == fence_char)).then(|| code.join("\n"))
}

/// ¿Es un mapa mental de IureEditor (`> [!MINDMAP…]` con ramas)?
pub fn is_mindmap_block(block: &str) -> bool {
    block.trim_start().starts_with('>') && !mindmap::find(block).is_empty()
}

/// ¿Es un bloque que [`draw_block`] sabe dibujar?
pub fn is_diagram_block(block: &str) -> bool {
    mermaid_code(block).is_some() || is_mindmap_block(block)
}

/// Píxeles de un bloque dibujado.
#[derive(Clone, Debug)]
pub struct BlockPixels {
    /// En px físicos.
    pub width: u32,
    pub height: u32,
    /// RGBA8 opacos, fila a fila.
    pub rgba: Vec<u8>,
    /// Tamaño del dibujo en px CSS (puntos de la UI).
    pub points: (f32, f32),
    /// Mapas mentales: dónde quedó cada nodo (en puntos) y su línea del bloque.
    pub boxes: Vec<mindmap::NodeBox>,
    /// S1.33: las zonas que se editan desde el dibujo (puntos de un cuadrante…), en puntos.
    pub spots: Vec<crate::mermaid_edit::actions::Spot>,
}

/// El SVG de un bloque de diagrama (```` ```mermaid ```` o mapa mental), para exportarlo.
pub fn block_svg(block: &str, style: &NoteStyle, mindmap_layout: Layout) -> Result<String, DiagramError> {
    block_svg_boxed(block, style, mindmap_layout).map(|(svg, _)| svg)
}

fn block_svg_boxed(
    block: &str,
    style: &NoteStyle,
    mindmap_layout: Layout,
) -> Result<(String, Vec<mindmap::NodeBox>), DiagramError> {
    if let Some(code) = mermaid_code(block) {
        return Ok((render_drawn(&code, style)?.svg, Vec::new()));
    }
    let map = mindmap::find(block).into_iter().next().ok_or_else(|| {
        DiagramError::new(
            None,
            "This mind map has no branches to draw.",
            "Este mapa mental no tiene ramas que dibujar.",
        )
    })?;
    Ok(mindmap::draw_boxed(&map.root, style, &map.options.look(mindmap_layout)))
}

/// Dibuja un bloque de Markdown que es diagrama (```` ```mermaid ```` o mapa mental de IureEditor)
/// a `scale` píxeles por punto, sobre `background`. Bloqueante (Blitz): la UI lo llama en otro
/// hilo. Lo usan los editores de la suite (`iure-doc-ui::DiagramRenderer`).
pub fn draw_block(
    block: &str,
    style: &NoteStyle,
    mindmap_layout: Layout,
    scale: f32,
    background: [u8; 3],
) -> Result<BlockPixels, DiagramError> {
    let (svg, boxes) = block_svg_boxed(block, style, mindmap_layout)?;
    let points = svg_size(&svg).unwrap_or((800.0, 600.0));
    let px = rasterize(&svg, scale, background).map_err(|e| {
        DiagramError::new(
            None,
            format!("The diagram could not be drawn ({e})."),
            format!("No se pudo dibujar el diagrama ({e})."),
        )
    })?;
    let spots = crate::mermaid_edit::actions::spots(block, &svg);
    Ok(BlockPixels { width: px.width, height: px.height, rgba: px.rgba, points, boxes, spots })
}

#[cfg(test)]
mod tests {

    #[test]
    fn recognizes_closed_mermaid_fences_and_mind_maps() {
        assert_eq!(mermaid_code("```mermaid\nflowchart LR\n  A-->B\n```").as_deref(), Some("flowchart LR\n  A-->B"));
        assert_eq!(mermaid_code("~~~~ Mermaid\nx\n~~~~").as_deref(), Some("x"));
        assert!(mermaid_code("```mermaid\nflowchart LR").is_none(), "sin cerrar: se sigue escribiendo");
        assert!(mermaid_code("```rust\nfn f() {}\n```").is_none());
        assert!(mermaid_code("````mermaid\nx\n```").is_none(), "la cerca de cierre es más corta");
        assert!(is_mindmap_block("> [!MINDMAP|map] Proyecto\n> - Análisis\n> - Diseño"));
        assert!(!is_mindmap_block("> una cita"));
        assert!(is_diagram_block("```mermaid\ngraph LR; A-->B\n```") && !is_diagram_block("texto"));
    }

    #[test]
    fn draws_blocks_and_explains_errors() {
        let style = NoteStyle::default();
        let px = draw_block("```mermaid\ngraph LR; A-->B\n```", &style, Layout::Right, 1.0, [255, 255, 255]).unwrap();
        assert!(px.width > 10 && px.rgba.len() == (px.width * px.height * 4) as usize);
        let map =
            draw_block("> [!MINDMAP|map] Caso\n> - A\n> - B", &style, Layout::Right, 1.0, [255, 255, 255]).unwrap();
        assert!(map.points.0 > 10.0);
        let err = draw_block("```mermaid\nflowchart LR\n  A[abierto --> B\n```", &style, Layout::Right, 1.0, [255; 3])
            .unwrap_err();
        assert!(err.es.contains("línea"), "{err:?}");
    }
    use super::*;

    const FLOW: &str = "flowchart LR\n  A[Inicio] --> B{¿Listo?}\n  B -- Sí --> C[Enviar]\n";

    #[test]
    fn options_round_trip_and_keep_the_rest() {
        assert_eq!(read_options(FLOW), DiagramOptions::default());
        let opts = DiagramOptions {
            theme: DiagramTheme::Forest,
            layout: Some(Layout::Down),
            shape: Shape::Cloud,
            ..Default::default()
        };
        let code = set_options(FLOW, &opts);
        assert!(
            code.starts_with(
                "---\nconfig:\n  theme: forest\niure:\n  layout: down\n  shape: cloud\n---\nflowchart LR\n"
            ),
            "{code}"
        );
        assert_eq!(read_options(&code), opts);
        // Lo que no es nuestro se conserva; lo nuestro se reemplaza, no se duplica.
        let mine = "---\ntitle: Plan\nconfig:\n  look: handDrawn\n  theme: dark\n---\nflowchart LR\n  A-->B\n";
        let out = set_options(mine, &DiagramOptions { theme: DiagramTheme::Neutral, ..Default::default() });
        // El tema se cambia en su sitio (el cambio en el archivo es de una línea).
        assert_eq!(out, "---\ntitle: Plan\nconfig:\n  look: handDrawn\n  theme: neutral\n---\nflowchart LR\n  A-->B\n");
        // Volver a lo de omisión quita las claves y la cabecera vacía.
        assert_eq!(set_options(&code, &DiagramOptions::default()), FLOW);
        let back = set_options(&out, &DiagramOptions::default());
        assert_eq!(back, "---\ntitle: Plan\nconfig:\n  look: handDrawn\n---\nflowchart LR\n  A-->B\n");
    }

    #[test]
    fn theme_is_applied_here_keeping_size_and_font() {
        let st = NoteStyle::default();
        let app = render(FLOW, &st).unwrap();
        let forest =
            render(&set_options(FLOW, &DiagramOptions { theme: DiagramTheme::Forest, ..Default::default() }), &st)
                .unwrap();
        assert_ne!(app, forest);
        for svg in [&app, &forest] {
            assert!(!svg.contains("width=\"100%\""), "tamaño fijo");
            assert!(svg.contains("font-size=\"14\""), "letra de la nota");
        }
        // El de la app lleva el acento de la nota en los nodos.
        assert!(app.contains("#1a5fb4"), "{app}");
    }

    #[test]
    fn mermaid_mindmaps_can_use_our_layouts() {
        let mm = "mindmap\n  root((Centro))\n    Uno\n      a\n    Dos\n";
        let st = NoteStyle::default();
        let radial = render(mm, &st).unwrap();
        let right =
            render(&set_options(mm, &DiagramOptions { layout: Some(Layout::Right), ..Default::default() }), &st)
                .unwrap();
        let down = render(
            &set_options(
                mm,
                &DiagramOptions { layout: Some(Layout::Down), shape: Shape::Hexagon, ..Default::default() },
            ),
            &st,
        )
        .unwrap();
        assert_ne!(radial, right);
        assert!(down.contains("<polygon"), "hexágonos");
        for s in [&radial, &right, &down] {
            assert!(["Centro", "Uno", "Dos"].iter().all(|l| s.contains(l)), "{s}");
            assert!(crate::net::svg_is_safe(s.as_bytes()));
        }
    }

    #[test]
    fn colors_go_to_theme_variables_and_respect_indentation() {
        let mut colors = DiagramColors::default();
        colors.set(ColorRole::NodeFill, Some([0xdb, 0xea, 0xfe]));
        colors.set(ColorRole::Line, Some([0x1d, 0x4e, 0xd8]));
        let opts = DiagramOptions { colors, ..Default::default() };
        let code = set_options(FLOW, &opts);
        assert!(
            code.starts_with(
                "---\nconfig:\n  themeVariables:\n    primaryColor: \"#dbeafe\"\n    lineColor: \"#1d4ed8\"\n---\n"
            ),
            "{code}"
        );
        assert_eq!(read_options(&code), opts);
        assert_eq!(parse_hex("#abc"), Some([0xaa, 0xbb, 0xcc]));
        assert_eq!(parse_hex("azul"), None);
        // Se aplican: el SVG lleva los colores elegidos.
        let svg = render(&code, &NoteStyle::default()).unwrap();
        assert!(svg.to_ascii_lowercase().contains("#dbeafe") && svg.to_ascii_lowercase().contains("#1d4ed8"), "{svg}");
        // Una cabecera con sangría de cuatro: lo nuevo usa la de sus hermanos (YAML válido).
        let four = "---\nconfig:\n    look: handDrawn\n---\nflowchart LR\n  A-->B\n";
        let out = set_options(four, &opts);
        assert!(out.contains("config:\n    look: handDrawn\n    themeVariables:\n      primaryColor:"), "{out}");
        assert_eq!(read_options(&out).colors, colors);
        // Quitar un color deja los demás; quitar todos quita la sección.
        let mut one = colors;
        one.set(ColorRole::Line, None);
        let out = set_options(&code, &DiagramOptions { colors: one, ..Default::default() });
        assert!(!out.contains("lineColor") && out.contains("primaryColor"), "{out}");
        assert_eq!(set_options(&code, &DiagramOptions::default()), FLOW);
        // Comentarios y otras variables se conservan.
        let rich = "---\n# mío\nconfig:\n  themeVariables:\n    fontFamily: Inter\n---\nflowchart LR\n  A-->B\n";
        let out = set_options(rich, &opts);
        assert!(
            out.starts_with("---\n# mío\nconfig:\n  themeVariables:\n    fontFamily: Inter\n    primaryColor:"),
            "{out}"
        );
        assert_eq!(set_options(&out, &DiagramOptions::default()), rich);
        // Regresión (proptest): una clave sangrada sin padre no arrastra la sangría de lo nuevo.
        let odd = "---\n  layout: down\nconfig:\n  layout: down\n---\nflowchart LR\n  A-->B\n";
        let want = DiagramOptions { palette: Palette::Pastel, ..Default::default() };
        assert_eq!(read_options(&set_options(odd, &want)), want);
        // Regresión (proptest): con claves repetidas gana la última y no queda un valor viejo.
        let dup = "---\niure:\niure:\n  layout: down\n---\nflowchart LR\n  A-->B\n";
        assert_eq!(read_options(dup).layout, Some(Layout::Down));
        assert_eq!(read_options(&set_options(dup, &DiagramOptions::default())), DiagramOptions::default());
        // Regresión (proptest): hermanos con sangrías distintas.
        let mixed = "---\nconfig:\n    lineColor: \"#123456\"\n  layout: down\n---\nflowchart LR\n  A-->B\n";
        let want = DiagramOptions { theme: DiagramTheme::Default, ..Default::default() };
        assert_eq!(read_options(&set_options(mixed, &want)).theme, DiagramTheme::Default);
    }

    #[test]
    fn mermaid_mindmaps_with_root_size_and_palette() {
        let mm = "mindmap\n  root((Un centro bastante largo para un mapa mental))\n    Uno\n    Dos\n";
        let st = NoteStyle::default();
        let opts = DiagramOptions { root: RootSize::Small, palette: Palette::Warm, ..Default::default() };
        let code = set_options(mm, &opts);
        assert!(code.contains("iure:\n  root: small\n  palette: warm\n"), "{code}");
        assert_eq!(read_options(&code), opts);
        // Con opciones propias se dibuja con el radial propio: centro partido y ramas cálidas.
        let svg = render(&code, &st).unwrap();
        assert!(svg.contains("<tspan") && svg.contains("#b45309"), "{svg}");
    }

    #[test]
    fn node_links_become_zones_and_callbacks_go_away() {
        let code = "flowchart LR\n  A[Sitio] --> B[Otro] --> C[Correo]\n  click A \"https://example.com/a?x=1&y=2\" \"Abrir\"\n  click B cb\n  click C \"mailto:ana@example.com\"\n";
        let d = render_drawn(code, &NoteStyle::default()).unwrap();
        assert!(!d.svg.contains("<a ") && !d.svg.contains("onclick") && !d.svg.contains("href"), "{}", d.svg);
        assert!(crate::net::svg_is_safe(d.svg.as_bytes()));
        assert_eq!(d.links.len(), 2, "{:?}", d.links);
        assert_eq!(d.links[0].href, "https://example.com/a?x=1&y=2");
        assert_eq!(d.links[1].href, "mailto:ana@example.com");
        assert!(d.width > 0.0 && d.links.iter().all(|l| l.w > 10.0 && l.x + l.w <= d.width + 1.0));
        let (cx, cy) = (d.links[0].x + d.links[0].w / 2.0, d.links[0].y + d.links[0].h / 2.0);
        assert!(d.links[0].contains(cx, cy) && !d.links[1].contains(cx, cy));
        // Esquemas peligrosos: sin zona.
        let (_, links) = extract_links(
            "<svg><a href=\"javascript:alert(1)\"><rect x=\"1\" y=\"1\" width=\"5\" height=\"5\"/></a></svg>",
        );
        assert!(links.is_empty());
        // Antes de S1.24 un diagrama con `click` a una URL no se dibujaba.
        assert!(render("flowchart LR\n  A-->B\n  click A \"https://x.mx\"\n", &NoteStyle::default()).is_ok());
    }

    #[test]
    fn errors_are_human_with_the_line() {
        let e = render("flowchart TD\n  A --> B\n  C -->", &NoteStyle::default()).unwrap_err();
        assert!(e.es.starts_with("La línea 3 («C -->»)") && e.es.contains("(Detalle: "), "{e:?}");
        assert!(e.en.starts_with("Line 3 (“C -->”)"), "{e:?}");
        assert_eq!(e.line, Some(3));
        let e = check("flowchart TD\n  A[abierto --> B\n").unwrap_err();
        assert!(e.es.contains("línea") && e.line.is_some(), "{e:?}");
        let e = check("pastel\n  a").unwrap_err();
        assert!(e.es.contains("«pastel»") && e.en.contains("pastel"), "{e:?}");
        assert_eq!(check("   ").unwrap_err().es, "El diagrama está vacío.");
        let big = format!("flowchart TD\n{}", "A-->B\n".repeat(crate::mermaid::MAX_LINES + 1));
        assert!(check(&big).unwrap_err().es.contains("demasiado grande"));
        // Con cabecera, la línea cuenta desde el principio del código.
        let e = check("---\nconfig:\n  theme: dark\n---\nnada\n").unwrap_err();
        assert_eq!(e.line, Some(5), "{e:?}");
    }

    #[test]
    fn blocks_in_preview_order_and_editable_only_at_top_level() {
        let md = "# T\n\n```mermaid\nflowchart LR\n  A-->B\n```\n\n> ```mermaid\n> pie\n> \"a\": 1\n> ```\n\n> [!MINDMAP|map] X\n> - a\n\n~~~mermaid\ngantt\n~~~\n";
        let b = mermaid_blocks(md);
        assert_eq!(b.len(), 3);
        assert_eq!(b[0].code, "flowchart LR\n  A-->B\n");
        assert_eq!(md.get(b[0].code_range.clone().unwrap()).unwrap(), "flowchart LR\n  A-->B\n");
        assert_eq!(b[1].code_range, None, "dentro de una cita: se edita en IureEditor");
        assert_eq!(md.get(b[2].code_range.clone().unwrap()).unwrap(), "gantt\n");
        let out = replace_mermaid(md, 0, "flowchart TD\n  X-->Y").unwrap();
        assert_eq!(out, md.replace("flowchart LR\n  A-->B\n", "flowchart TD\n  X-->Y\n"));
        assert_eq!(replace_mermaid(md, 1, "pie"), None);
        assert_eq!(replace_mermaid(md, 0, "flowchart LR\n  A-->B\n"), None, "sin cambios");
        // Sin cerca de cierre (hasta el final de la nota).
        let open = "```mermaid\npie\n\"a\": 1\n";
        assert_eq!(replace_mermaid(open, 0, "pie\n\"b\": 2").unwrap(), "```mermaid\npie\n\"b\": 2\n");
    }

    #[test]
    fn svg_size_raster_and_png() {
        assert_eq!(svg_size("<svg width=\"120\" height=\"40.5\">"), Some((120.0, 40.5)));
        assert_eq!(svg_size("<svg viewBox=\"0 0 300 100\">"), Some((300.0, 100.0)));
        assert_eq!(svg_size("<svg>"), None);
        let svg = render(FLOW, &NoteStyle::default()).unwrap();
        let (w, _) = svg_size(&svg).unwrap();
        let r = rasterize(&svg, 2.0, [255, 255, 255]).unwrap();
        assert_eq!(r.width, (w.ceil() as u32) * 2);
        assert!(r.rgba.chunks(4).any(|p| p[0] < 200), "algo se pintó");
        let png = encode_png(r.width, r.height, &r.rgba).unwrap();
        assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
        // Un SVG enorme se rasteriza a una escala menor, no a más de MAX_RASTER_SIDE.
        let huge = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"4000\" height=\"100\"></svg>";
        let r = rasterize(huge, 8.0, [255, 255, 255]).unwrap();
        assert!(r.width <= MAX_RASTER_SIDE, "{}", r.width);
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(64))]
        #[test]
        fn options_never_panic_and_round_trip(
            head in "(---\n|config:\n|  theme: dark\n|  themeVariables:\n|    lineColor: \"#123456\"\n|iure:\n|  layout: down\n|    root: small\n|title: x\n|# nota\n|\\PC|\n){0,30}",
            theme in proptest::sample::select(DiagramTheme::ALL.to_vec()),
            layout in proptest::option::of(proptest::sample::select(Layout::ALL.to_vec())),
            shape in proptest::sample::select(Shape::ALL.to_vec()),
            root in proptest::sample::select(RootSize::ALL.to_vec()),
            palette in proptest::sample::select(Palette::ALL.to_vec()),
            color in proptest::option::of(proptest::array::uniform3(0u8..)),
        ) {
            let code = format!("{head}\nflowchart LR\n  A-->B\n");
            let mut colors = DiagramColors::default();
            colors.set(ColorRole::Line, color);
            colors.set(ColorRole::NodeFill, color.map(|c| [c[2], c[1], c[0]]));
            let opts = DiagramOptions { theme, colors, layout, shape, root, palette };
            let out = set_options(&code, &opts);
            proptest::prop_assert_eq!(read_options(&out), opts);
            let _ = check(&out);
            let _ = mermaid_blocks(&format!("```mermaid\n{out}```\n{head}"));
        }
    }
}
