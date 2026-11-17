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
//!   theme: forest        # tema de Mermaid; sin él, el de la app
//! iure:
//!   layout: right        # solo `mindmap`: right | radial | both | down
//!   shape: cloud         # solo `mindmap`: forma de los nodos
//! ---
//! mindmap
//!   …
//! ```
//!
//! Lo propio va bajo `iure:`, que Mermaid ignora. [`set_options`] reescribe solo esas claves y
//! conserva todo lo demás (título, otras claves de `config`, comentarios).
//!
//! **Tema**: [`DiagramTheme::App`] (el de omisión) parte del claro u oscuro de Mermaid según la
//! nota y le pone los colores de la app (texto, acento, bordes; en mapas mentales, los colores de
//! rama con contraste AA). Los temas de Mermaid se aplican aquí y no dentro de `mermaid-svg`: si
//! se lo dejara a él, perdería la letra de la nota y el tamaño fijo en píxeles que pinta Blitz.

use std::ops::Range;

use crate::markdown::NoteStyle;
use crate::mindmap::{self, Layout, Node, Shape};

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

/// Opciones de dibujo de un diagrama.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct DiagramOptions {
    pub theme: DiagramTheme,
    /// Solo mapas mentales: distribución (`None`: la radial de Mermaid).
    pub layout: Option<Layout>,
    /// Solo mapas mentales con distribución propia: forma de los nodos.
    pub shape: Shape,
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
    if t.starts_with('#') {
        return None;
    }
    let (k, v) = t.split_once(':')?;
    let v = v.split(" #").next().unwrap_or("").trim().trim_matches(['"', '\'']);
    Some((indent, k.trim(), v))
}

/// Valor de `section.key` en la cabecera (un nivel de anidado).
fn header_value<'a>(lines: &[&'a str], section: &str, key: &str) -> Option<&'a str> {
    let mut inside = false;
    for l in lines {
        let Some((indent, k, v)) = yaml_pair(l) else { continue };
        if indent == 0 {
            inside = k == section;
            continue;
        }
        if inside && k == key {
            return Some(v);
        }
    }
    None
}

/// Opciones que trae el código.
pub fn read_options(code: &str) -> DiagramOptions {
    let Some((lines, _)) = frontmatter(code) else { return DiagramOptions::default() };
    DiagramOptions {
        theme: header_value(&lines, "config", "theme").map(DiagramTheme::from_key).unwrap_or_default(),
        layout: header_value(&lines, "iure", "layout").and_then(layout_from_value),
        shape: header_value(&lines, "iure", "shape").and_then(Shape::from_token).unwrap_or_default(),
    }
}

/// El código con las opciones nuevas: reescribe `config.theme`, `iure.layout` e `iure.shape`,
/// conserva el resto de la cabecera y quita la cabecera si queda vacía. Fin de línea `\n`.
pub fn set_options(code: &str, opts: &DiagramOptions) -> String {
    let (lines, body_start) = frontmatter(code).unwrap_or((Vec::new(), 0));
    let body: Vec<&str> = code.lines().skip(body_start).collect();
    // Secciones de primer nivel con sus líneas, sin las claves que se reescriben.
    let mut sections: Vec<(String, Vec<String>)> = Vec::new();
    let mut current: Option<String> = None;
    for l in &lines {
        match yaml_pair(l) {
            Some((0, k, v)) => {
                current = Some(k.to_string());
                let head = if v.is_empty() { format!("{k}:") } else { (*l).to_string() };
                sections.push((head, Vec::new()));
            }
            Some((_, k, _))
                if (current.as_deref() == Some("config") && k == "theme")
                    || (current.as_deref() == Some("iure") && (k == "layout" || k == "shape")) => {}
            _ => {
                if let Some((_, kids)) = sections.last_mut() {
                    kids.push((*l).to_string());
                } else {
                    sections.push(((*l).to_string(), Vec::new()));
                }
            }
        }
    }
    let mut add = |section: &str, line: String| match sections.iter_mut().find(|(h, _)| h == &format!("{section}:")) {
        Some((_, kids)) => kids.insert(0, line),
        None => sections.push((format!("{section}:"), vec![line])),
    };
    // `config` antes que `iure`; dentro de `iure`, la distribución antes que la forma (cada una
    // se inserta al principio de su sección).
    if let Some(t) = opts.theme.key() {
        add("config", format!("  theme: {t}"));
    }
    if let Some(shape) = (opts.shape != Shape::Default).then_some(opts.shape) {
        add("iure", format!("  shape: {}", shape.token()));
    }
    if let Some(l) = opts.layout {
        add("iure", format!("  layout: {}", layout_value(l)));
    }
    // Secciones propias que quedaron vacías (`config:` o `iure:` sin nada) se quitan.
    sections.retain(|(h, kids)| !((h == "config:" || h == "iure:") && kids.iter().all(|k| k.trim().is_empty())));
    let mut out = String::new();
    if !sections.is_empty() {
        out.push_str("---\n");
        for (h, kids) in &sections {
            out.push_str(h);
            out.push('\n');
            for k in kids {
                out.push_str(k);
                out.push('\n');
            }
        }
        out.push_str("---\n");
    }
    for l in body {
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
            } else if section == "config" && k == "theme" {
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
        Node { label: mindmap::plain_label(&n.text), children }
    }
    m.root.as_ref().map(|r| conv(r, 0, &mut count))
}

/// Dibuja el diagrama con sus opciones y el estilo de la nota. El SVG es seguro y de tamaño fijo.
pub fn render(code: &str, style: &NoteStyle) -> Result<String, DiagramError> {
    check(code)?;
    let opts = read_options(code);
    let is_mindmap = kind(code) == "mindmap";
    if is_mindmap
        && let Some(layout) = opts.layout.filter(|l| *l != Layout::Radial)
        && let Some(tree) = mindmap_tree(code)
    {
        return Ok(mindmap::draw(&tree, style, layout, opts.shape));
    }
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
    crate::mermaid::render(&for_renderer(code), theme).ok_or_else(|| {
        DiagramError::new(
            None,
            "It took too long or came out too large to show here.",
            "Tardó demasiado o salió demasiado grande para mostrarlo aquí.",
        )
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    const FLOW: &str = "flowchart LR\n  A[Inicio] --> B{¿Listo?}\n  B -- Sí --> C[Enviar]\n";

    #[test]
    fn options_round_trip_and_keep_the_rest() {
        assert_eq!(read_options(FLOW), DiagramOptions::default());
        let opts = DiagramOptions { theme: DiagramTheme::Forest, layout: Some(Layout::Down), shape: Shape::Cloud };
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
        assert_eq!(out, "---\ntitle: Plan\nconfig:\n  theme: neutral\n  look: handDrawn\n---\nflowchart LR\n  A-->B\n");
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
            head in "(---\n|config:\n|  theme: dark\n|iure:\n|  layout: down\n|title: x\n|\\PC|\n){0,30}",
            theme in proptest::sample::select(DiagramTheme::ALL.to_vec()),
            layout in proptest::option::of(proptest::sample::select(Layout::ALL.to_vec())),
            shape in proptest::sample::select(Shape::ALL.to_vec()),
        ) {
            let code = format!("{head}\nflowchart LR\n  A-->B\n");
            let opts = DiagramOptions { theme, layout, shape };
            let out = set_options(&code, &opts);
            proptest::prop_assert_eq!(read_options(&out), opts);
            let _ = check(&out);
            let _ = mermaid_blocks(&format!("```mermaid\n{out}```\n{head}"));
        }
    }
}
