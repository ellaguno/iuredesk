//! Diagramas de secuencia (`sequenceDiagram`): agregar participantes, mensajes y notas; cambiar el
//! nombre de un participante; cambiar el texto, el tipo o el sentido de un mensaje; borrar. Cada
//! cambio toca solo sus líneas; si el código y el árbol no coinciden, la acción no se hace.

use super::MAX_LINES;

/// Texto más largo que se escribe.
const MAX_TEXT: usize = 160;
/// Elementos que se revisan como mucho.
const MAX_ITEMS: usize = 5_000;

/// Flechas de mensaje (las largas primero, para reconocerlas).
const ARROWS: [&str; 10] = ["<<-->>", "<<->>", "-->>", "->>", "-->", "->", "--x", "-x", "--)", "-)"];

/// Los tipos de mensaje que se ofrecen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageKind {
    Sync,
    Reply,
    Async,
    Lost,
}

impl MessageKind {
    pub const ALL: [MessageKind; 4] = [MessageKind::Sync, MessageKind::Reply, MessageKind::Async, MessageKind::Lost];

    fn arrow(self) -> &'static str {
        match self {
            MessageKind::Sync => "->>",
            MessageKind::Reply => "-->>",
            MessageKind::Async => "-)",
            MessageKind::Lost => "-x",
        }
    }

    /// Nombre en inglés y español.
    pub fn label(self) -> (&'static str, &'static str) {
        match self {
            MessageKind::Sync => ("Request", "Solicitud"),
            MessageKind::Reply => ("Reply (dashed)", "Respuesta (punteada)"),
            MessageKind::Async => ("Asynchronous", "Asíncrono"),
            MessageKind::Lost => ("Lost / cancelled", "Perdido / cancelado"),
        }
    }
}

/// Lo que se lee del árbol de `mermaid-svg`: participantes (id y nombre), mensajes (desde, hacia,
/// texto) y notas (participantes y texto), en el orden en que se dibujan.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tree {
    pub participants: Vec<(String, String)>,
    pub messages: Vec<(String, String, String)>,
    pub notes: Vec<(Vec<String>, String)>,
}

fn flatten(items: &[mermaid_svg::ast::SequenceItem], t: &mut Tree, depth: usize) {
    use mermaid_svg::ast::SequenceItem as I;
    if depth > 30 {
        return;
    }
    for it in items.iter().take(MAX_ITEMS) {
        match it {
            I::Message(m) => t.messages.push((m.from.clone(), m.to.clone(), m.text.clone())),
            I::Note(n) => t.notes.push((n.participants.clone(), n.text.clone())),
            I::Alt(bs) | I::Par(bs) | I::Critical(bs) => {
                for b in bs {
                    flatten(&b.items, t, depth + 1);
                }
            }
            I::Loop(b) | I::Opt(b) | I::Break(b) => flatten(&b.items, t, depth + 1),
            I::Rect(r) => flatten(&r.items, t, depth + 1),
            _ => {}
        }
    }
}

/// El árbol del código (vacío si no es de secuencia).
pub fn tree(code: &str) -> Tree {
    let mut t = Tree::default();
    if let Ok(mermaid_svg::Diagram::Sequence(s)) = mermaid_svg::parse(code) {
        t.participants = s.participants.into_iter().take(MAX_ITEMS).map(|p| (p.id, p.display)).collect();
        flatten(&s.items, &mut t, 0);
    }
    t
}

/// ¿Es un diagrama de secuencia?
pub fn is_sequence(code: &str) -> bool {
    code.lines()
        .take(40)
        .map(str::trim)
        .find(|l| !(l.is_empty() || l.starts_with("```") || l.starts_with("~~~") || l.starts_with("%%")))
        .is_some_and(|l| l.starts_with("sequenceDiagram"))
}

/// Un texto de una línea que no rompe la sintaxis (`;` y `#` tienen sentido en Mermaid).
/// Sin `%%` (comentario en Mermaid), por largo que sea el tramo de `%`.
pub(super) fn no_comment(mut t: String) -> String {
    while t.contains("%%") {
        t = t.replace("%%", "%");
    }
    t
}

pub(super) fn clean(s: &str) -> Option<String> {
    let one: String = s
        .chars()
        .map(|c| match c {
            ';' => ',',
            '#' => '№',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    let t: String = one.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(MAX_TEXT).collect();
    // `%%` empieza un comentario: el texto desaparecería (también `%%%`, que tras un reemplazo
    // deja `%%`).
    let t = no_comment(t);
    (!t.is_empty()).then_some(t)
}

/// Un mensaje en una línea del código: sus partes en bytes.
#[derive(Clone, Debug, PartialEq)]
struct MsgLine {
    from: String,
    to: String,
    /// La flecha (sin `+`/`-` de activación).
    arrow: (usize, usize),
    /// El destino (con su `+`/`-` antes).
    to_span: (usize, usize),
    from_span: (usize, usize),
    /// El texto, después de `:`.
    text: (usize, usize),
}

fn read_message(line: &str) -> Option<MsgLine> {
    let start = line.len() - line.trim_start().len();
    let colon = line.find(':')?;
    let head = line.get(..colon)?;
    // La primera flecha (la más larga en esa posición).
    let (pos, arrow) = (start..head.len())
        .filter(|i| head.is_char_boundary(*i))
        .find_map(|i| ARROWS.iter().find(|a| head.get(i..).is_some_and(|r| r.starts_with(**a))).map(|a| (i, *a)))?;
    let from = head.get(start..pos)?.trim();
    let after = pos + arrow.len();
    let rest = head.get(after..)?;
    let lead = rest.len() - rest.trim_start_matches([' ', '+', '-']).len();
    let to = rest.get(lead..)?.trim_end();
    let valid = |s: &str| !s.is_empty() && !s.contains(char::is_whitespace);
    if !valid(from) || !valid(to) {
        return None;
    }
    let text_start = colon + 1;
    Some(MsgLine {
        from: from.to_string(),
        to: to.to_string(),
        arrow: (pos, after),
        from_span: (start, start + from.len()),
        to_span: (after + lead, after + lead + to.len()),
        text: (text_start, line.len()),
    })
}

/// ¿Es una línea de nota? Sus participantes y dónde empieza el texto.
fn read_note(line: &str) -> Option<(Vec<String>, usize)> {
    let t = line.trim_start();
    let lower = t.to_ascii_lowercase();
    if !lower.starts_with("note ") {
        return None;
    }
    let colon = line.find(':')?;
    let head = line.get(..colon)?.trim();
    let who = ["right of", "left of", "over"]
        .iter()
        .find_map(|k| head.to_ascii_lowercase().find(k).and_then(|p| head.get(p + k.len()..)))?;
    Some((who.split(',').map(|s| s.trim().to_string()).collect(), colon + 1))
}

/// Las líneas de mensajes y de notas del bloque, en orden.
fn lines_of(block: &str) -> (Vec<(usize, MsgLine)>, Vec<(usize, Vec<String>, usize)>) {
    let mut msgs = Vec::new();
    let mut notes = Vec::new();
    for (n, l) in block.lines().take(MAX_LINES).enumerate() {
        let t = l.trim_start();
        if t.starts_with("%%") || t.starts_with("```") || t.starts_with("~~~") {
            continue;
        }
        if let Some(note) = read_note(l) {
            notes.push((n, note.0, note.1));
        } else if let Some(m) = read_message(l) {
            msgs.push((n, m));
        }
    }
    (msgs, notes)
}

/// La línea del mensaje `k` (si el código y el árbol coinciden).
fn message_at(block: &str, code: &str, k: usize) -> Option<(usize, MsgLine)> {
    let (msgs, _) = lines_of(block);
    let t = tree(code);
    let same = msgs.len() == t.messages.len()
        && msgs.iter().zip(&t.messages).all(|((_, m), (f, to, _))| m.from == *f && m.to == *to);
    if !same {
        return None;
    }
    msgs.into_iter().nth(k)
}

/// La línea de la nota `k` (si coinciden).
fn note_at(block: &str, code: &str, k: usize) -> Option<(usize, usize)> {
    let (_, notes) = lines_of(block);
    let t = tree(code);
    if notes.len() != t.notes.len() {
        return None;
    }
    notes.into_iter().nth(k).map(|(n, _, at)| (n, at))
}

pub(super) fn indent_of(line: &str) -> &str {
    line.get(..line.len() - line.trim_start().len()).unwrap_or("")
}

/// La sangría del cuerpo (la de la primera línea después de `sequenceDiagram`).
pub(super) fn body_indent_after(block: &str, header: &str) -> String {
    let mut seen = false;
    for l in block.lines().take(MAX_LINES) {
        let t = l.trim_start();
        if seen && !t.is_empty() && !t.starts_with("```") && !t.starts_with("~~~") {
            return indent_of(l).to_string();
        }
        if t.starts_with(header) {
            seen = true;
        }
    }
    "    ".into()
}

pub(super) fn replace_line(block: &str, n: usize, new: Option<String>) -> Option<String> {
    let mut out = String::with_capacity(block.len() + 64);
    let mut found = false;
    for (i, l) in block.split_inclusive('\n').enumerate() {
        if i == n {
            found = true;
            if let Some(s) = &new {
                out.push_str(s);
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

/// Inserta `new` antes de la línea `at` (o antes de la cerca de cierre si `None`).
pub(super) fn insert_at(block: &str, at: Option<usize>, new: &str) -> String {
    let lines: Vec<&str> = block.split_inclusive('\n').collect();
    let at = at.unwrap_or_else(|| {
        lines
            .iter()
            .rposition(|l| l.trim_start().starts_with("```") || l.trim_start().starts_with("~~~"))
            .filter(|&i| i > 0)
            .unwrap_or(lines.len())
    });
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

/// ¿Es la declaración de un participante (`participant X …` / `actor X …`)? Su id.
fn declaration(line: &str) -> Option<String> {
    let t = line.trim_start();
    let rest = ["participant ", "actor "].iter().find_map(|k| t.strip_prefix(k))?;
    let id: String = rest.trim_start().chars().take_while(|c| !c.is_whitespace() && *c != '@').collect();
    (!id.is_empty()).then_some(id)
}

/// Agrega un participante con ese nombre, después del último declarado.
pub fn add_participant(block: &str, code: &str, name: &str) -> Option<String> {
    if !is_sequence(code) {
        return None;
    }
    let name = clean(name)?;
    let used: Vec<String> = tree(code).participants.into_iter().map(|(id, _)| id).collect();
    let base: String = name.chars().filter(|c| c.is_alphanumeric() || *c == '_').collect();
    let id = std::iter::once(base.clone())
        .filter(|b| !b.is_empty())
        .chain((1..10_000).map(|k| format!("P{k}")))
        .find(|c| !used.contains(c))?;
    let decl = if id == name { format!("participant {id}") } else { format!("participant {id} as {name}") };
    let last_decl =
        block.lines().take(MAX_LINES).enumerate().filter(|(_, l)| declaration(l).is_some()).map(|(n, _)| n).last();
    let header = block.lines().take(MAX_LINES).position(|l| l.trim_start().starts_with("sequenceDiagram"))?;
    let after = last_decl.unwrap_or(header);
    Some(insert_at(block, Some(after + 1), &format!("{}{decl}", body_indent_after(block, "sequenceDiagram"))))
}

/// Cambia el nombre que se ve de un participante (`as Nombre`).
pub fn rename_participant(block: &str, code: &str, id: &str, name: &str) -> Option<String> {
    let name = clean(name)?;
    tree(code).participants.iter().find(|(p, _)| p == id)?;
    let decl = block.lines().take(MAX_LINES).position(|l| declaration(l).as_deref() == Some(id));
    match decl {
        Some(n) => {
            let line = block.lines().nth(n)?;
            let t = line.trim_start();
            let kw = if t.starts_with("actor") { "actor" } else { "participant" };
            if line.contains('@') {
                return None;
            }
            let new = if name == id {
                format!("{}{kw} {id}", indent_of(line))
            } else {
                format!("{}{kw} {id} as {name}", indent_of(line))
            };
            replace_line(block, n, Some(new))
        }
        None => {
            // Sin declarar: se declara justo antes de su primera aparición (el orden no cambia).
            let first = block
                .lines()
                .take(MAX_LINES)
                .position(|l| read_message(l).is_some_and(|m| m.from == id || m.to == id))?;
            Some(insert_at(
                block,
                Some(first),
                &format!("{}participant {id} as {name}", body_indent_after(block, "sequenceDiagram")),
            ))
        }
    }
}

/// Borra un participante con sus mensajes y sus notas (y `activate`, `create`…).
pub fn delete_participant(block: &str, code: &str, id: &str) -> Option<String> {
    tree(code).participants.iter().find(|(p, _)| p == id)?;
    let mut out = String::with_capacity(block.len());
    for (n, l) in block.split_inclusive('\n').enumerate() {
        let body = l.trim_end_matches(['\n', '\r']);
        let t = body.trim_start();
        let mut words = t.split_whitespace();
        let first = words.next().unwrap_or("");
        let second = words.next().unwrap_or("");
        let gone = n < MAX_LINES
            && (declaration(body).as_deref() == Some(id)
                || read_message(body).is_some_and(|m| m.from == id || m.to == id)
                || read_note(body).is_some_and(|(who, _)| who.iter().any(|w| w == id))
                || (matches!(first, "activate" | "deactivate" | "destroy") && second == id)
                || (first == "create" && declaration(t.get(7..).unwrap_or("")).as_deref() == Some(id)));
        if !gone {
            out.push_str(l);
        }
    }
    Some(out)
}

/// Agrega un mensaje de `from` a `to` al final del diagrama.
pub fn add_message(block: &str, code: &str, from: &str, to: &str, text: &str) -> Option<String> {
    let t = tree(code);
    if !t.participants.iter().any(|(p, _)| p == from) || !t.participants.iter().any(|(p, _)| p == to) {
        return None;
    }
    let text = clean(text)?;
    Some(insert_at(block, None, &format!("{}{from}->>{to}: {text}", body_indent_after(block, "sequenceDiagram"))))
}

/// Agrega una nota a la derecha de `id`, al final del diagrama.
pub fn add_note(block: &str, code: &str, id: &str, text: &str) -> Option<String> {
    tree(code).participants.iter().find(|(p, _)| p == id)?;
    let text = clean(text)?;
    Some(insert_at(block, None, &format!("{}Note right of {id}: {text}", body_indent_after(block, "sequenceDiagram"))))
}

/// Cambia el texto del mensaje `k`.
pub fn set_message_text(block: &str, code: &str, k: usize, text: &str) -> Option<String> {
    let (n, m) = message_at(block, code, k)?;
    let line = block.lines().nth(n)?;
    let text = clean(text)?;
    replace_line(block, n, Some(format!("{} {text}", line.get(..m.text.0)?)))
}

/// Cambia el tipo del mensaje `k`.
pub fn set_message_kind(block: &str, code: &str, k: usize, kind: MessageKind) -> Option<String> {
    let (n, m) = message_at(block, code, k)?;
    let line = block.lines().nth(n)?;
    replace_line(block, n, Some(format!("{}{}{}", line.get(..m.arrow.0)?, kind.arrow(), line.get(m.arrow.1..)?)))
}

/// Invierte el mensaje `k` (de B a A).
pub fn reverse_message(block: &str, code: &str, k: usize) -> Option<String> {
    let (n, m) = message_at(block, code, k)?;
    let line = block.lines().nth(n)?;
    let new = format!(
        "{}{}{}{}{}",
        line.get(..m.from_span.0)?,
        m.to,
        line.get(m.from_span.1..m.to_span.0)?,
        m.from,
        line.get(m.to_span.1..)?
    );
    replace_line(block, n, Some(new))
}

/// Borra el mensaje `k`.
pub fn delete_message(block: &str, code: &str, k: usize) -> Option<String> {
    let (n, _) = message_at(block, code, k)?;
    replace_line(block, n, None)
}

/// Cambia el texto de la nota `k`.
pub fn set_note_text(block: &str, code: &str, k: usize, text: &str) -> Option<String> {
    let (n, at) = note_at(block, code, k)?;
    let line = block.lines().nth(n)?;
    let text = clean(text)?;
    replace_line(block, n, Some(format!("{} {text}", line.get(..at)?)))
}

/// Borra la nota `k`.
pub fn delete_note(block: &str, code: &str, k: usize) -> Option<String> {
    let (n, _) = note_at(block, code, k)?;
    replace_line(block, n, None)
}

// --- Geometría ---

/// Un elemento del SVG: una figura (con su caja) o un texto (con su posición y contenido).
#[derive(Clone, Debug, PartialEq)]
pub(super) enum El {
    Shape { tag: String, rect: [f32; 4] },
    Text { x: f32, y: f32, middle: bool, text: String },
}

fn attr(tag: &str, name: &str) -> Option<f32> {
    let at = tag.find(&format!(" {name}=\""))? + name.len() + 3;
    let rest = tag.get(at..)?;
    rest.get(..rest.find('"')?)?.trim().parse().ok()
}

/// Los elementos del SVG en orden (sin `<defs>`).
pub(super) fn elements(svg: &str) -> Vec<El> {
    let mut out = Vec::new();
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
            let Some(g) = rest.find('>') else { break };
            let tag = rest.get(..g).unwrap_or("");
            let inner = rest.get(g + 1..e).unwrap_or("");
            if let (Some(x), Some(y)) = (attr(tag, "x"), attr(tag, "y")) {
                out.push(El::Text {
                    x,
                    y,
                    middle: tag.contains("text-anchor=\"middle\""),
                    text: super::flowchart::norm(inner),
                });
            }
            i += e + 7;
            continue;
        }
        let Some(e) = rest.find('>') else { break };
        let tag = rest.get(..e + 1).unwrap_or("");
        i += e + 1;
        let rect = if tag.starts_with("<line") {
            match (attr(tag, "x1"), attr(tag, "y1"), attr(tag, "x2"), attr(tag, "y2")) {
                (Some(a), Some(b), Some(c), Some(d)) => Some([a.min(c), b.min(d), a.max(c), b.max(d)]),
                _ => None,
            }
        } else if ["<rect", "<circle", "<ellipse", "<polygon", "<path"].iter().any(|k| tag.starts_with(k)) {
            crate::diagram::shape_box(tag).map(|(x, y, w, h)| [x, y, x + w, y + h])
        } else {
            None
        };
        if let Some(rect) = rect {
            out.push(El::Shape { tag: tag.to_string(), rect });
        }
    }
    out
}

pub(super) fn union(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]
}

/// Caja aproximada de un texto (12 px de alto; 7 px por letra).
pub(super) fn text_box(x: f32, y: f32, middle: bool, chars: usize) -> [f32; 4] {
    let w = (chars as f32 * 7.0).max(14.0);
    let x0 = if middle { x - w / 2.0 } else { x };
    [x0, y - 13.0, x0 + w, y + 4.0]
}

/// Las zonas del dibujo: participantes (en el orden del árbol), mensajes y notas. `None` si no se
/// encontró.
pub fn boxes(svg: &str, t: &Tree) -> (Vec<Option<[f32; 4]>>, Vec<Option<[f32; 4]>>, Vec<Option<[f32; 4]>>) {
    let els = elements(svg);
    // Participantes: la figura (o figuras, un monigote) antes de la primera aparición de su nombre.
    let mut parts = Vec::new();
    for (_, display) in &t.participants {
        let want = super::flowchart::norm(display);
        let at = els.iter().position(|e| matches!(e, El::Text { text, .. } if !want.is_empty() && *text == want));
        let b = at.and_then(|i| {
            let El::Text { x, y, middle, text } = els.get(i)? else { return None };
            let mut b = text_box(*x, *y, *middle, text.chars().count());
            // Las figuras de inmediato antes que rodean al nombre (la caja, o la cabeza y el
            // cuerpo de un monigote); no las líneas de vida ni los mensajes, que bajan más.
            for e in els.get(..i)?.iter().rev().take(8) {
                match e {
                    El::Shape { rect, .. }
                        if rect[1] >= *y - 80.0 && rect[3] <= *y + 40.0 && rect[0] <= *x && rect[2] >= *x - 1.0 =>
                    {
                        b = union(b, *rect)
                    }
                    El::Shape { .. } => {}
                    El::Text { .. } => break,
                }
            }
            Some(b)
        });
        parts.push(b);
    }
    // Mensajes: una línea con punta (o un trazo) seguida de su texto, en orden.
    let mut msgs = Vec::new();
    let mut cursor = 0;
    for (_, _, text) in &t.messages {
        let want = super::flowchart::norm(text);
        let found = (cursor..els.len()).find_map(|i| {
            let El::Text { x, y, middle, text } = els.get(i)? else { return None };
            if *text != want {
                return None;
            }
            // Su línea: la figura de antes o de después (el texto va encima de la línea).
            let line = [i.checked_sub(1), Some(i + 1)].into_iter().flatten().find_map(|j| match els.get(j)? {
                El::Shape { tag, rect } if tag.starts_with("<line") || tag.contains("marker-end") => Some(*rect),
                _ => None,
            })?;
            let b = union(
                text_box(*x, *y, *middle, text.chars().count()),
                [line[0], line[1] - 6.0, line[2], line[3] + 6.0],
            );
            Some((i, b))
        });
        match found {
            Some((i, b)) => {
                cursor = i + 1;
                msgs.push(Some(b));
            }
            None => msgs.push(None),
        }
    }
    // Notas: el rectángulo antes de su texto.
    let mut notes = Vec::new();
    let mut cursor = 0;
    for (_, text) in &t.notes {
        let want = super::flowchart::norm(text);
        let found = (cursor..els.len()).find_map(|i| {
            let El::Text { text, .. } = els.get(i)? else { return None };
            if *text != want {
                return None;
            }
            match els.get(i.checked_sub(1)?)? {
                El::Shape { tag, rect } if tag.starts_with("<rect") => Some((i, *rect)),
                _ => None,
            }
        });
        match found {
            Some((i, b)) => {
                cursor = i + 1;
                notes.push(Some(b));
            }
            None => notes.push(None),
        }
    }
    (parts, msgs, notes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const S: &str = "```mermaid\nsequenceDiagram\n    participant C as Cliente\n    actor A as Abogado\n    C->>A: Consulta\n    loop Cada semana\n        A-->>C: Avance\n    end\n    Note right of A: Revisa\n    A-)C: Factura\n```\n";

    fn code(b: &str) -> String {
        crate::diagram::mermaid_code(b).unwrap()
    }

    #[test]
    fn reads_messages_and_notes() {
        let m = read_message("        A-->>+C: Avance: parte 2").unwrap();
        assert_eq!((m.from.as_str(), m.to.as_str()), ("A", "C"));
        assert!(read_message("    participant C as Cliente").is_none());
        assert_eq!(read_note("    Note over A,C: x").unwrap().0, ["A", "C"]);
        let t = tree(&code(S));
        assert_eq!(t.messages.len(), 3);
        assert_eq!(t.notes.len(), 1);
        assert_eq!(t.participants, [("C".into(), "Cliente".into()), ("A".into(), "Abogado".into())]);
    }

    #[test]
    fn adds_participants_messages_and_notes() {
        let c = code(S);
        let out = add_participant(S, &c, "Juez Pérez").unwrap();
        assert!(out.contains("    actor A as Abogado\n    participant JuezPérez as Juez Pérez\n"), "{out}");
        assert_eq!(tree(&code(&out)).participants.len(), 3);
        let out = add_message(S, &c, "C", "A", "¿Y la audiencia?").unwrap();
        assert!(out.ends_with("    A-)C: Factura\n    C->>A: ¿Y la audiencia?\n```\n"), "{out}");
        // Regresión: `%%` es comentario en Mermaid.
        assert!(add_message(S, &c, "C", "A", "%% ojo").unwrap().contains("C->>A: % ojo\n"));
        let out = add_note(S, &c, "C", "Paga; luego").unwrap();
        assert!(out.contains("    Note right of C: Paga, luego\n```"), "{out}");
        assert!(add_message(S, &c, "C", "Z", "x").is_none());
    }

    #[test]
    fn edits_messages_in_place() {
        let c = code(S);
        let out = set_message_text(S, &c, 1, "Informe").unwrap();
        assert!(out.contains("        A-->>C: Informe\n"), "{out}");
        let out = set_message_kind(S, &c, 0, MessageKind::Async).unwrap();
        assert!(out.contains("    C-)A: Consulta\n"), "{out}");
        let out = reverse_message(S, &c, 2).unwrap();
        assert!(out.contains("    C-)A: Factura\n"), "{out}");
        let out = delete_message(S, &c, 1).unwrap();
        assert!(!out.contains("Avance") && out.contains("    loop Cada semana\n    end\n"), "{out}");
        let out = set_note_text(S, &c, 0, "Revisa todo").unwrap();
        assert!(out.contains("    Note right of A: Revisa todo\n"), "{out}");
        assert!(!delete_note(S, &c, 0).unwrap().contains("Revisa"));
    }

    #[test]
    fn renames_and_deletes_participants() {
        let c = code(S);
        let out = rename_participant(S, &c, "A", "Licenciada López").unwrap();
        assert!(out.contains("    actor A as Licenciada López\n"), "{out}");
        let out = delete_participant(S, &c, "A").unwrap();
        assert_eq!(
            out,
            "```mermaid\nsequenceDiagram\n    participant C as Cliente\n    loop Cada semana\n    end\n```\n"
        );
        // Sin declarar: se declara antes de su primera aparición.
        let s = "```mermaid\nsequenceDiagram\n  X->>Y: hola\n```\n";
        assert_eq!(
            rename_participant(s, &code(s), "Y", "Yolanda").unwrap(),
            "```mermaid\nsequenceDiagram\n  participant Y as Yolanda\n  X->>Y: hola\n```\n"
        );
    }

    #[test]
    fn boxes_match_the_drawing() {
        let c = code(S);
        let svg = mermaid_svg::render(&c).unwrap();
        let t = tree(&c);
        let (p, m, n) = boxes(&svg, &t);
        assert!(p.iter().chain(&m).chain(&n).all(Option::is_some), "{p:?} {m:?} {n:?}");
        // Cliente arriba a la izquierda; Abogado a su derecha; los mensajes de arriba abajo.
        let (c0, a0) = (p[0].unwrap(), p[1].unwrap());
        assert!(c0[2] < a0[0] + 5.0, "{c0:?} {a0:?}");
        assert!(m[0].unwrap()[1] < m[1].unwrap()[1] && m[1].unwrap()[1] < m[2].unwrap()[1]);
        // Regresión: en un diagrama corto, la zona de un participante no toma las líneas de vida
        // ni los mensajes (y no tapa al otro).
        let s = "sequenceDiagram\n    participant C as Cliente\n    participant A as Abogado\n    C->>A: Consulta";
        let svg = mermaid_svg::render(s).unwrap();
        let (p, m, _) = boxes(&svg, &tree(s));
        let (c0, a0, m0) = (p[0].unwrap(), p[1].unwrap(), m[0].unwrap());
        assert!(c0[2] < a0[0], "{c0:?} {a0:?}");
        assert!(c0[3] < m0[1], "{c0:?} {m0:?}");
    }

    proptest! {
        #[test]
        fn never_panics(text in ".{0,30}", junk in ".{0,120}", k in 0usize..4) {
            let block = format!("```mermaid\nsequenceDiagram\n    A->>B: x\n    {junk}\n```\n");
            let c = format!("sequenceDiagram\n    A->>B: x\n    {junk}");
            let _ = lines_of(&block);
            let _ = add_participant(&block, &c, &text);
            let _ = rename_participant(&block, &c, "A", &text);
            let _ = delete_participant(&block, &c, "B");
            let _ = set_message_text(&block, &c, k, &text);
            let _ = set_message_kind(&block, &c, k, MessageKind::Reply);
            let _ = reverse_message(&block, &c, k);
            let _ = delete_message(&block, &c, k);
            let _ = set_note_text(&block, &c, k, &text);
            let _ = boxes(&junk, &Tree::default());
            if let Some(out) = add_message(S, &code(S), "C", "A", &text) {
                prop_assert_eq!(tree(&code(&out)).messages.len(), 4);
            }
        }
    }
}
