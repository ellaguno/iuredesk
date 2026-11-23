//! S1.33: editar diagramas Mermaid desde el dibujo. Cada acción es un cambio de texto mínimo y local
//! en el bloque (insertar una línea, cambiar un número, una etiqueta): lo demás —comentarios, orden,
//! estilos— queda byte a byte. Sin egui; la app dibuja la barra y pide los datos.
//!
//! Diagramas de cuadrantes ([`quadrant`]) y de flujo ([`flowchart`]).

pub mod flowchart;

/// Líneas de un bloque que se revisan como mucho.
const MAX_LINES: usize = 5_000;
/// Etiqueta más larga que se escribe.
const MAX_LABEL: usize = 120;

/// Una etiqueta que no rompe la sintaxis de Mermaid: una línea, sin `:` ni corchetes (los dos
/// puntos separan la etiqueta de sus coordenadas), recortada.
pub fn clean_label(s: &str) -> String {
    let one: String = s
        .chars()
        .map(|c| match c {
            '\n' | '\r' | '\t' => ' ',
            ':' => '-',
            '[' | ']' | '{' | '}' => ' ',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    let t = one.split_whitespace().collect::<Vec<_>>().join(" ");
    t.chars().take(MAX_LABEL).collect()
}

/// Un número de 0 a 1 con dos decimales (como los escribe la gente).
fn coord(v: f32) -> String {
    let v = if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.5 };
    let s = format!("{:.2}", (v * 100.0).round() / 100.0);
    // `0.50` → `0.5`, `1.00` → `1`.
    let s = s.trim_end_matches('0').trim_end_matches('.').to_string();
    if s.is_empty() { "0".into() } else { s }
}

/// Diagramas de cuadrantes (`quadrantChart`): puntos `Etiqueta: [x, y]`.
pub mod quadrant {
    use super::{MAX_LINES, clean_label, coord};

    /// Un punto del código.
    #[derive(Clone, Debug, PartialEq)]
    pub struct Point {
        /// Línea del bloque (desde 0).
        pub line: usize,
        pub label: String,
        pub x: f32,
        pub y: f32,
    }

    /// Partes de una línea de punto: sangría, etiqueta (con su `:::clase`), coordenadas y el resto.
    struct Parts<'a> {
        indent: &'a str,
        label: &'a str,
        class: &'a str,
        x: f32,
        y: f32,
        /// El tercer número (`[x, y, r]`), si lo hay.
        r: Option<f32>,
        rest: &'a str,
    }

    fn parts(line: &str) -> Option<Parts<'_>> {
        let indent_len = line.len() - line.trim_start().len();
        let indent = line.get(..indent_len)?;
        let body = line.get(indent_len..)?;
        let open = body.find('[')?;
        let head = body.get(..open)?.trim_end();
        let head = head.strip_suffix(':')?;
        let (label, class) = match head.find(":::") {
            Some(c) => (head.get(..c)?.trim(), head.get(c..)?.trim()),
            None => (head.trim(), ""),
        };
        if label.is_empty() || label.contains(':') {
            return None;
        }
        let after = body.get(open + 1..)?;
        let close = after.find(']')?;
        let nums: Vec<f32> =
            after.get(..close)?.split(',').map(|n| n.trim().parse::<f32>()).collect::<Result<_, _>>().ok()?;
        if nums.iter().any(|n| !n.is_finite()) {
            return None;
        }
        let (x, y, r) = match nums.as_slice() {
            [x, y] => (*x, *y, None),
            [x, y, r] => (*x, *y, Some(*r)),
            _ => return None,
        };
        Some(Parts { indent, label, class, x, y, r, rest: after.get(close + 1..)? })
    }

    /// ¿Es un diagrama de cuadrantes?
    pub fn is_quadrant(block: &str) -> bool {
        block.lines().take(40).any(|l| l.trim_start().starts_with("quadrantChart"))
    }

    /// Los puntos, en el orden del código (el mismo en que se dibujan).
    pub fn points(block: &str) -> Vec<Point> {
        if !is_quadrant(block) {
            return Vec::new();
        }
        block
            .lines()
            .take(MAX_LINES)
            .enumerate()
            .filter_map(|(i, l)| {
                let p = parts(l)?;
                Some(Point { line: i, label: p.label.to_string(), x: p.x, y: p.y })
            })
            .collect()
    }

    /// Cambia la línea `line` del bloque por `new` (o la quita con `None`), conservando el resto.
    fn with_line(block: &str, line: usize, new: Option<String>) -> Option<String> {
        let mut out = String::with_capacity(block.len() + 64);
        let mut found = false;
        for (i, l) in block.split_inclusive('\n').enumerate() {
            if i == line {
                found = true;
                if let Some(n) = &new {
                    out.push_str(n);
                    if l.ends_with('\n') {
                        out.push('\n');
                    }
                }
            } else {
                out.push_str(l);
            }
        }
        found.then_some(out)
    }

    /// La línea de un punto con otras coordenadas o etiqueta (el resto igual).
    fn rewrite(line: &str, label: Option<&str>, xy: Option<(f32, f32)>) -> Option<String> {
        let p = parts(line)?;
        let label = label.map_or_else(|| p.label.to_string(), clean_label);
        if label.is_empty() {
            return None;
        }
        let (x, y) = xy.unwrap_or((p.x, p.y));
        let r = p.r.map(|r| format!(", {r}")).unwrap_or_default();
        let class = if p.class.is_empty() { String::new() } else { p.class.to_string() };
        let xs = if xy.is_some() { coord(x) } else { p.x.to_string() };
        let ys = if xy.is_some() { coord(y) } else { p.y.to_string() };
        Some(format!("{}{label}{class}: [{xs}, {ys}{r}]{}", p.indent, p.rest.trim_end_matches(['\n', '\r'])))
    }

    /// Agrega un punto con esa etiqueta en `x`, `y` (de 0 a 1), después del último punto (o al final
    /// del diagrama). `None` si no es un diagrama de cuadrantes o la etiqueta queda vacía.
    pub fn add_point(block: &str, label: &str, x: f32, y: f32) -> Option<String> {
        if !is_quadrant(block) {
            return None;
        }
        let label = clean_label(label);
        if label.is_empty() {
            return None;
        }
        let pts = points(block);
        let lines: Vec<&str> = block.split_inclusive('\n').collect();
        // Después del último punto con su sangría; si no hay, antes de la cerca de cierre (o al
        // final) con la sangría de la primera línea del diagrama después de `quadrantChart`.
        let (at, indent) = match pts.last() {
            Some(p) => {
                let l = lines.get(p.line).copied().unwrap_or("");
                (p.line + 1, l.get(..l.len() - l.trim_start().len()).unwrap_or("").to_string())
            }
            None => {
                let close = lines.iter().rposition(|l| l.trim_start().starts_with("```")).filter(|&i| i > 0);
                let at = close.unwrap_or(lines.len());
                let indent = lines
                    .iter()
                    .skip_while(|l| !l.trim_start().starts_with("quadrantChart"))
                    .nth(1)
                    .filter(|l| !l.trim_start().starts_with("```"))
                    .map(|l| l.get(..l.len() - l.trim_start().len()).unwrap_or("").to_string())
                    .unwrap_or_else(|| "    ".into());
                (at, indent)
            }
        };
        let new = format!("{indent}{label}: [{}, {}]\n", coord(x), coord(y));
        let mut out = String::with_capacity(block.len() + new.len() + 1);
        for (i, l) in lines.iter().enumerate() {
            if i == at {
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                out.push_str(&new);
            }
            out.push_str(l);
        }
        if at >= lines.len() {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(&new);
        }
        Some(out)
    }

    /// Mueve el punto `n` (orden del código) a `x`, `y`.
    pub fn move_point(block: &str, n: usize, x: f32, y: f32) -> Option<String> {
        let p = points(block).into_iter().nth(n)?;
        let line = block.lines().nth(p.line)?;
        with_line(block, p.line, Some(rewrite(line, None, Some((x, y)))?))
    }

    /// Cambia la etiqueta del punto `n`.
    pub fn set_label(block: &str, n: usize, label: &str) -> Option<String> {
        let p = points(block).into_iter().nth(n)?;
        let line = block.lines().nth(p.line)?;
        with_line(block, p.line, Some(rewrite(line, Some(label), None)?))
    }

    /// Quita el punto `n`.
    pub fn delete_point(block: &str, n: usize) -> Option<String> {
        let p = points(block).into_iter().nth(n)?;
        with_line(block, p.line, None)
    }

    /// Dónde quedó el diagrama dibujado, en unidades del SVG.
    #[derive(Clone, Debug, PartialEq)]
    pub struct Geometry {
        /// La gráfica (los cuatro cuadrantes): `[x0, y0, x1, y1]`.
        pub plot: [f32; 4],
        /// Cada punto en el orden del código: `[cx, cy, r]`.
        pub points: Vec<[f32; 3]>,
    }

    impl Geometry {
        /// Las coordenadas (0–1) de un lugar del dibujo, si cae dentro de la gráfica.
        pub fn to_coords(&self, x: f32, y: f32) -> Option<(f32, f32)> {
            let [x0, y0, x1, y1] = self.plot;
            let (w, h) = (x1 - x0, y1 - y0);
            if w <= 0.0 || h <= 0.0 || x < x0 || x > x1 || y < y0 || y > y1 {
                return None;
            }
            Some(((x - x0) / w, 1.0 - (y - y0) / h))
        }

        /// Como [`Geometry::to_coords`], pegado a la orilla si cae fuera (al arrastrar).
        pub fn to_coords_clamped(&self, x: f32, y: f32) -> Option<(f32, f32)> {
            let [x0, y0, x1, y1] = self.plot;
            self.to_coords(x.clamp(x0, x1), y.clamp(y0, y1))
        }
    }

    /// Atributo numérico de una etiqueta SVG.
    fn attr(tag: &str, name: &str) -> Option<f32> {
        let at = tag.find(&format!(" {name}=\""))? + name.len() + 3;
        let rest = tag.get(at..)?;
        rest.get(..rest.find('"')?)?.trim().parse().ok()
    }

    /// Las etiquetas que empiezan con `open` (`"<rect "`, `"<circle "`) del SVG, en orden (acotado).
    fn tags<'a>(svg: &'a str, open: &'static str) -> impl Iterator<Item = &'a str> + 'a {
        svg.match_indices(open).take(10_000).filter_map(move |(i, _)| svg.get(i..).and_then(|t| t.get(..t.find('>')?)))
    }

    /// Lee la gráfica y los puntos del SVG de `mermaid-svg`: los cuadrantes son los cuatro primeros
    /// rectángulos del mismo tamaño (2×2) y los puntos, los círculos en el orden del código.
    pub fn geometry(svg: &str) -> Option<Geometry> {
        let rects: Vec<[f32; 4]> = tags(svg, "<rect ")
            .filter_map(|t| Some([attr(t, "x")?, attr(t, "y")?, attr(t, "width")?, attr(t, "height")?]))
            .collect();
        let same = |p: &[f32; 4], q: &[f32; 4]| {
            let [_, _, pw, ph] = *p;
            let [_, _, qw, qh] = *q;
            (pw - qw).abs() < 0.5 && (ph - qh).abs() < 0.5 && pw > 0.0 && ph > 0.0
        };
        let quad = rects.windows(4).find(|w| match w {
            [a, b, c, d] => same(a, b) && same(a, c) && same(a, d),
            _ => false,
        })?;
        let x0 = quad.iter().map(|&[x, _, _, _]| x).fold(f32::INFINITY, f32::min);
        let y0 = quad.iter().map(|&[_, y, _, _]| y).fold(f32::INFINITY, f32::min);
        let x1 = quad.iter().map(|&[x, _, w, _]| x + w).fold(f32::NEG_INFINITY, f32::max);
        let y1 = quad.iter().map(|&[_, y, _, h]| y + h).fold(f32::NEG_INFINITY, f32::max);
        let points =
            tags(svg, "<circle ").filter_map(|t| Some([attr(t, "cx")?, attr(t, "cy")?, attr(t, "r")?])).collect();
        Some(Geometry { plot: [x0, y0, x1, y1], points })
    }
}

/// Las acciones del dibujo, sin egui: la app las pasa a su editor (iure-doc) y llama a [`apply`] o
/// [`drag`] con lo que se eligió.
pub mod actions {
    use super::flowchart::{self, EdgeKind, Shape};
    use super::quadrant;
    use crate::markdown::NoteStyle;

    /// Qué dibuja el botón.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum IconKind {
        Add,
        Rename,
        Delete,
        /// Unir con otro elemento.
        Connect,
        /// Cambiar la forma.
        Shape,
        /// Invertir una flecha.
        Reverse,
    }

    /// Lo que pide la acción.
    #[derive(Clone, Debug, PartialEq)]
    pub enum Input {
        None,
        /// Un texto (`prompt` en inglés y español; `initial` ya escrito).
        Text {
            prompt: (&'static str, &'static str),
            initial: String,
        },
        /// Elegir una opción (nombres en inglés y español).
        Choice(Vec<(&'static str, &'static str)>),
        /// Elegir una opción y luego escribir un texto.
        ChoiceText {
            options: Vec<(&'static str, &'static str)>,
            prompt: (&'static str, &'static str),
        },
        /// Pulsar otra zona del dibujo.
        OtherSpot((&'static str, &'static str)),
    }

    /// Una acción.
    #[derive(Clone, Debug, PartialEq)]
    pub struct Action {
        pub id: u32,
        pub icon: IconKind,
        /// Nombre en inglés y español.
        pub label: (&'static str, &'static str),
        pub input: Input,
    }

    /// Sobre qué: una zona (su número) o un hueco (en unidades del dibujo).
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub enum Target {
        Spot(usize),
        Canvas([f32; 2]),
    }

    /// El dato que se pidió.
    #[derive(Clone, Debug, PartialEq)]
    pub enum Data {
        None,
        Text(String),
        Choice(usize),
        ChoiceText(usize, String),
        Spot(usize),
    }

    const ADD: u32 = 1;
    const RENAME: u32 = 2;
    const DELETE: u32 = 3;
    const FLOW_NEXT: u32 = 10;
    const FLOW_CONNECT: u32 = 11;
    const FLOW_TEXT: u32 = 12;
    const FLOW_SHAPE: u32 = 13;
    const FLOW_DELETE: u32 = 14;
    const FLOW_LOOSE: u32 = 15;
    const EDGE_TEXT: u32 = 20;
    const EDGE_KIND: u32 = 21;
    const EDGE_REVERSE: u32 = 22;
    const EDGE_DELETE: u32 = 23;
    /// Las zonas de las flechas van desde aquí (las de los nodos, desde 0).
    const EDGE_BASE: usize = 100_000;

    fn shapes() -> Vec<(&'static str, &'static str)> {
        Shape::ALL.iter().map(|s| s.label()).collect()
    }

    /// El código Mermaid del bloque si es un diagrama de flujo.
    fn flow_code(block: &str) -> Option<String> {
        crate::diagram::mermaid_code(block).filter(|c| flowchart::is_flowchart(c))
    }

    /// El id del nodo `n` (orden del árbol) y su texto.
    fn flow_node(code: &str, n: usize) -> Option<(String, String)> {
        flowchart::nodes(code).into_iter().nth(n).map(|(id, text, _)| (id, text))
    }

    /// Una zona pulsable del dibujo: número, rectángulo `[x0, y0, x1, y1]` (unidades del dibujo) y si
    /// se arrastra.
    #[derive(Clone, Debug, PartialEq)]
    pub struct Spot {
        pub id: usize,
        pub rect: [f32; 4],
        pub draggable: bool,
    }

    /// El código Mermaid del bloque si es un diagrama de cuadrantes.
    fn quadrant_code(block: &str) -> Option<String> {
        crate::diagram::mermaid_code(block).filter(|c| quadrant::is_quadrant(c))
    }

    /// La geometría del bloque dibujado (se dibuja el SVG, sin pintarlo).
    fn geometry(block: &str, style: &NoteStyle) -> Option<quadrant::Geometry> {
        let code = quadrant_code(block)?;
        let svg = crate::diagram::render_drawn(&code, style).ok()?.svg;
        quadrant::geometry(&svg)
    }

    /// Las zonas de un SVG ya dibujado de este bloque (para no dibujarlo dos veces).
    pub fn spots(block: &str, svg: &str) -> Vec<Spot> {
        if let Some(code) = flow_code(block) {
            let nodes = flowchart::nodes(&code);
            let edges = flowchart::edges(&code).len();
            return flowchart::boxes(svg, &nodes)
                .into_iter()
                .enumerate()
                .filter_map(|(id, b)| b.map(|rect| Spot { id, rect, draggable: false }))
                .chain(flowchart::edge_boxes(svg, edges).into_iter().enumerate().map(|(e, rect)| Spot {
                    id: EDGE_BASE + e,
                    rect,
                    draggable: false,
                }))
                .collect();
        }
        if quadrant_code(block).is_none() {
            return Vec::new();
        }
        let Some(g) = quadrant::geometry(svg) else { return Vec::new() };
        g.points
            .iter()
            .enumerate()
            .map(|(id, &[cx, cy, r])| {
                let r = r.max(4.0) + 3.0;
                Spot { id, rect: [cx - r, cy - r, cx + r, cy + r], draggable: true }
            })
            .collect()
    }

    /// Las acciones de la zona `id`.
    pub fn spot_actions(block: &str, id: usize) -> Vec<Action> {
        if let Some(code) = flow_code(block)
            && let Some(e) = id.checked_sub(EDGE_BASE)
        {
            let Some((_, _, label)) = flowchart::edges(&code).into_iter().nth(e) else { return Vec::new() };
            let mut v = vec![
                Action {
                    id: EDGE_TEXT,
                    icon: IconKind::Rename,
                    label: ("Arrow text", "Texto de la flecha"),
                    input: Input::Text {
                        prompt: ("Text on the arrow (empty: none)", "Texto sobre la flecha (vacío: sin texto)"),
                        initial: label.unwrap_or_default(),
                    },
                },
                Action {
                    id: EDGE_KIND,
                    icon: IconKind::Shape,
                    label: ("Arrow type", "Tipo de flecha"),
                    input: Input::Choice(EdgeKind::ALL.iter().map(|k| k.label()).collect()),
                },
            ];
            if flowchart::can_reverse(block, &code, e) {
                v.push(Action {
                    id: EDGE_REVERSE,
                    icon: IconKind::Reverse,
                    label: ("Reverse", "Invertir"),
                    input: Input::None,
                });
            }
            v.push(Action {
                id: EDGE_DELETE,
                icon: IconKind::Delete,
                label: ("Delete arrow", "Eliminar flecha"),
                input: Input::None,
            });
            return v;
        }
        if let Some(code) = flow_code(block) {
            let Some((_, text)) = flow_node(&code, id) else { return Vec::new() };
            return vec![
                Action {
                    id: FLOW_NEXT,
                    icon: IconKind::Add,
                    label: ("Add next step", "Agregar siguiente"),
                    input: Input::ChoiceText {
                        options: shapes(),
                        prompt: ("Text of the new step", "Texto del paso nuevo"),
                    },
                },
                Action {
                    id: FLOW_CONNECT,
                    icon: IconKind::Connect,
                    label: ("Connect to…", "Conectar con…"),
                    input: Input::OtherSpot(("Click the step to connect to", "Pulsa el paso con el que se une")),
                },
                Action {
                    id: FLOW_TEXT,
                    icon: IconKind::Rename,
                    label: ("Change text", "Cambiar texto"),
                    input: Input::Text { prompt: ("Text of the step", "Texto del paso"), initial: text },
                },
                Action {
                    id: FLOW_SHAPE,
                    icon: IconKind::Shape,
                    label: ("Change shape", "Cambiar forma"),
                    input: Input::Choice(shapes()),
                },
                Action {
                    id: FLOW_DELETE,
                    icon: IconKind::Delete,
                    label: ("Delete step", "Eliminar paso"),
                    input: Input::None,
                },
            ];
        }
        let Some(code) = quadrant_code(block) else { return Vec::new() };
        let Some(p) = quadrant::points(&code).into_iter().nth(id) else { return Vec::new() };
        vec![
            Action {
                id: RENAME,
                icon: IconKind::Rename,
                label: ("Rename", "Cambiar nombre"),
                input: Input::Text { prompt: ("Element name", "Nombre del elemento"), initial: p.label },
            },
            Action { id: DELETE, icon: IconKind::Delete, label: ("Delete", "Eliminar"), input: Input::None },
        ]
    }

    /// Las acciones de un hueco en `at`.
    pub fn canvas_actions(block: &str, at: [f32; 2], style: &NoteStyle) -> Vec<Action> {
        if flow_code(block).is_some() {
            return vec![Action {
                id: FLOW_LOOSE,
                icon: IconKind::Add,
                label: ("Add a step", "Agregar paso"),
                input: Input::ChoiceText {
                    options: shapes(),
                    prompt: ("Text of the new step", "Texto del paso nuevo"),
                },
            }];
        }
        let inside = geometry(block, style).is_some_and(|g| g.to_coords(at[0], at[1]).is_some());
        if !inside {
            return Vec::new();
        }
        vec![Action {
            id: ADD,
            icon: IconKind::Add,
            label: ("Add element here", "Agregar elemento aquí"),
            input: Input::Text { prompt: ("Element name", "Nombre del elemento"), initial: String::new() },
        }]
    }

    /// Hace la acción: el bloque nuevo.
    pub fn apply(block: &str, style: &NoteStyle, target: Target, action: u32, data: Data) -> Option<String> {
        if let Some(code) = flow_code(block) {
            if let Target::Spot(id) = target
                && let Some(e) = id.checked_sub(EDGE_BASE)
            {
                return match (action, data) {
                    (EDGE_TEXT, Data::Text(t)) => flowchart::set_edge_text(block, &code, e, &t),
                    (EDGE_KIND, Data::Choice(k)) => flowchart::set_edge_kind(block, &code, e, *EdgeKind::ALL.get(k)?),
                    (EDGE_REVERSE, _) => flowchart::reverse_edge(block, &code, e),
                    (EDGE_DELETE, _) => flowchart::delete_edge(block, &code, e),
                    _ => None,
                };
            }
            let node = |n: usize| flow_node(&code, n).map(|(id, _)| id);
            let shape = |k: usize| Shape::ALL.get(k).copied();
            return match (target, action, data) {
                (Target::Spot(n), FLOW_NEXT, Data::ChoiceText(k, text)) => {
                    flowchart::add_after(block, &code, &node(n)?, shape(k)?, &text)
                }
                (Target::Spot(a), FLOW_CONNECT, Data::Spot(b)) => {
                    flowchart::connect(block, &code, &node(a)?, &node(b)?)
                }
                (Target::Spot(n), FLOW_TEXT, Data::Text(text)) => flowchart::set_text(block, &node(n)?, &text),
                (Target::Spot(n), FLOW_SHAPE, Data::Choice(k)) => flowchart::set_shape(block, &node(n)?, shape(k)?),
                (Target::Spot(n), FLOW_DELETE, _) => flowchart::delete(block, &node(n)?),
                (Target::Canvas(_), FLOW_LOOSE, Data::ChoiceText(k, text)) => {
                    flowchart::add_loose(block, &code, shape(k)?, &text)
                }
                _ => None,
            };
        }
        match (target, action, data) {
            (Target::Canvas([x, y]), ADD, Data::Text(label)) => {
                let (cx, cy) = geometry(block, style)?.to_coords(x, y)?;
                quadrant::add_point(block, &label, cx, cy)
            }
            (Target::Spot(n), RENAME, Data::Text(label)) => quadrant::set_label(block, n, &label),
            (Target::Spot(n), DELETE, _) => quadrant::delete_point(block, n),
            _ => None,
        }
    }

    /// La zona `id` se soltó en `to`: el bloque nuevo.
    pub fn drag(block: &str, style: &NoteStyle, id: usize, to: [f32; 2]) -> Option<String> {
        let (x, y) = geometry(block, style)?.to_coords_clamped(to[0], to[1])?;
        quadrant::move_point(block, id, x, y)
    }
}

#[cfg(test)]
mod tests {
    use super::quadrant::*;
    use proptest::prelude::*;

    const Q: &str = "```mermaid\nquadrantChart\n    title Prioridades\n    x-axis Bajo esfuerzo --> Alto esfuerzo\n    y-axis Bajo impacto --> Alto impacto\n    quadrant-1 Planear\n    %% un comentario\n    Campaña A: [0.3, 0.6]\n    Campaña B:::rojo: [0.45, 0.23] radius: 12\n```\n";

    #[test]
    fn reads_the_points_with_their_line() {
        let p = points(Q);
        assert_eq!(p.len(), 2);
        assert_eq!((p[0].label.as_str(), p[0].x, p[0].y, p[0].line), ("Campaña A", 0.3, 0.6, 7));
        assert_eq!(p[1].label, "Campaña B");
        assert!(points("```mermaid\nflowchart TD\n A: [1, 2]\n```").is_empty(), "solo cuadrantes");
    }

    #[test]
    fn adds_after_the_last_point_with_its_indent() {
        let out = add_point(Q, "Nueva: idea", 0.8, 0.123).unwrap();
        assert!(
            out.contains("    Campaña B:::rojo: [0.45, 0.23] radius: 12\n    Nueva- idea: [0.8, 0.12]\n```"),
            "{out}"
        );
        assert_eq!(points(&out).len(), 3);
        // Sin puntos: antes de la cerca.
        let empty = "```mermaid\nquadrantChart\n  title T\n```\n";
        let out = add_point(empty, "Uno", 0.5, 0.5).unwrap();
        assert_eq!(out, "```mermaid\nquadrantChart\n  title T\n  Uno: [0.5, 0.5]\n```\n");
        assert!(add_point(Q, "  \n ", 0.1, 0.1).is_none(), "sin etiqueta");
    }

    #[test]
    fn moves_renames_and_deletes_one_line_only() {
        let moved = move_point(Q, 1, 0.9, 1.4).unwrap();
        assert!(
            moved.contains("    Campaña B:::rojo: [0.9, 1, 12]")
                || moved.contains("    Campaña B:::rojo: [0.9, 1] radius: 12"),
            "{moved}"
        );
        assert_eq!(
            moved.lines().filter(|l| !l.contains("Campaña B")).collect::<Vec<_>>(),
            Q.lines().filter(|l| !l.contains("Campaña B")).collect::<Vec<_>>()
        );
        let renamed = set_label(Q, 0, "Campaña Z").unwrap();
        assert!(renamed.contains("    Campaña Z: [0.3, 0.6]\n"));
        let deleted = delete_point(Q, 0).unwrap();
        assert!(!deleted.contains("Campaña A") && deleted.contains("%% un comentario"));
        assert!(move_point(Q, 9, 0.1, 0.1).is_none());
    }

    #[test]
    fn geometry_matches_what_mermaid_svg_draws() {
        let code = mermaid_code(Q);
        let svg = mermaid_svg::render(&code).unwrap();
        let g = geometry(&svg).unwrap();
        assert_eq!(g.points.len(), 2);
        // Cada círculo, en sus coordenadas.
        for (p, c) in points(Q).iter().zip(&g.points) {
            let (x, y) = g.to_coords(c[0], c[1]).unwrap();
            assert!((x - p.x).abs() < 0.01 && (y - p.y).abs() < 0.01, "{p:?} → {x},{y}");
        }
        assert!(g.to_coords(-5.0, -5.0).is_none(), "fuera de la gráfica");
    }

    fn mermaid_code(block: &str) -> String {
        block.lines().filter(|l| !l.trim_start().starts_with("```")).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn actions_add_rename_drag_and_delete_on_the_real_drawing() {
        use super::actions::{self, Data, Target};
        let style = crate::markdown::NoteStyle::default();
        let svg = crate::diagram::render_drawn(&mermaid_code(Q), &style).unwrap().svg;
        let spots = actions::spots(Q, &svg);
        assert_eq!(spots.len(), 2);
        assert!(spots.iter().all(|s| s.draggable));
        let g = geometry(&svg).unwrap();
        // Agregar en el centro de la gráfica.
        let [x0, y0, x1, y1] = g.plot;
        let mid = [(x0 + x1) / 2.0, (y0 + y1) / 2.0];
        assert_eq!(actions::canvas_actions(Q, mid, &style).len(), 1);
        assert!(actions::canvas_actions(Q, [x0 - 20.0, y0 - 20.0], &style).is_empty(), "fuera de la gráfica");
        let added = actions::apply(Q, &style, Target::Canvas(mid), 1, Data::Text("Centro".into())).unwrap();
        assert!(added.contains("    Centro: [0.5, 0.5]\n"), "{added}");
        // Renombrar, arrastrar a la esquina de arriba a la derecha y borrar.
        let renamed = actions::apply(Q, &style, Target::Spot(0), 2, Data::Text("Otra".into())).unwrap();
        assert!(renamed.contains("    Otra: [0.3, 0.6]"));
        let moved = actions::drag(Q, &style, 0, [x1 + 50.0, y0 - 50.0]).unwrap();
        assert!(moved.contains("    Campaña A: [1, 1]"), "pegado a la orilla: {moved}");
        let deleted = actions::apply(Q, &style, Target::Spot(1), 3, Data::None).unwrap();
        assert!(!deleted.contains("Campaña B"));
        assert!(actions::spot_actions(Q, 9).is_empty());
    }

    #[test]
    fn flowchart_actions_on_the_real_drawing() {
        use super::actions::{self, Data, Target};
        let style = crate::markdown::NoteStyle::default();
        let b = "```mermaid\nflowchart TD\n    A[Inicio] --> B{¿Listo?}\n    B -->|sí| C(Fin)\n```\n";
        let svg = crate::diagram::render_drawn(&mermaid_code(b), &style).unwrap().svg;
        let spots = actions::spots(b, &svg);
        assert_eq!(spots.iter().map(|s| s.id).collect::<Vec<_>>(), [0, 1, 2, 100_000, 100_001]);
        assert_eq!(actions::spot_actions(b, 1).len(), 5);
        let next = actions::apply(b, &style, Target::Spot(2), 10, Data::ChoiceText(3, "¿Otra?".into())).unwrap();
        assert!(next.contains("    C --> N1{¿Otra?}\n"), "{next}");
        let con = actions::apply(b, &style, Target::Spot(2), 11, Data::Spot(0)).unwrap();
        assert!(con.contains("    C --> A\n"), "{con}");
        let shp = actions::apply(b, &style, Target::Spot(0), 13, Data::Choice(2)).unwrap();
        assert!(shp.contains("A([Inicio])"), "{shp}");
        let del = actions::apply(b, &style, Target::Spot(2), 14, Data::None).unwrap();
        assert!(!del.contains("Fin"), "{del}");
        // Las flechas: zonas desde 100 000.
        assert!(spots.iter().any(|s| s.id == 100_001));
        assert_eq!(actions::spot_actions(b, 100_001).len(), 4);
        let dotted = actions::apply(b, &style, Target::Spot(100_001), 21, Data::Choice(1)).unwrap();
        assert!(dotted.contains("    B -.->|sí| C(Fin)\n"), "{dotted}");
        let rev = actions::apply(b, &style, Target::Spot(100_000), 22, Data::None).unwrap();
        assert!(rev.contains("    B{¿Listo?} --> A[Inicio]\n"), "{rev}");
        let loose =
            actions::apply(b, &style, Target::Canvas([1.0, 1.0]), 15, Data::ChoiceText(0, "Suelto".into())).unwrap();
        assert!(loose.contains("    N1[Suelto]\n```"), "{loose}");
    }

    proptest! {
        #[test]
        fn never_panics_and_keeps_the_rest(label in ".{0,40}", x in -2.0f32..3.0, y in -2.0f32..3.0, n in 0usize..4, junk in ".{0,200}") {
            let block = format!("{Q}{junk}");
            let _ = points(&block);
            if let Some(out) = add_point(Q, &label, x, y) {
                prop_assert_eq!(points(&out).len(), 3);
                prop_assert!(out.starts_with("```mermaid\nquadrantChart\n    title Prioridades\n"));
            }
            if let Some(out) = move_point(Q, n, x, y) {
                let p = &points(&out)[n];
                prop_assert!((0.0..=1.0).contains(&p.x) && (0.0..=1.0).contains(&p.y));
            }
            let _ = set_label(&block, n, &label);
            let _ = delete_point(&block, n);
            let _ = geometry(&junk);
        }
    }
}
