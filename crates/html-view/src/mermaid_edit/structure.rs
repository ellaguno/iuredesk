//! Diagramas de clases (`classDiagram`) y de entidad-relación (`erDiagram`): agregar clases o
//! entidades, miembros o atributos, relaciones; cambiar el nombre (en todas sus apariciones) y
//! borrar con todo lo suyo. Cada cambio toca solo sus líneas.

use super::MAX_LINES;
use super::flowchart::norm;
use super::sequence::{El, body_indent_after, clean, elements, indent_of, insert_at, replace_line, text_box, union};

/// Elementos que se revisan como mucho.
const MAX_ITEMS: usize = 5_000;

fn is_id_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-'
}

/// Un nombre válido de clase o entidad (letras, números, `_`).
fn ident(s: &str) -> Option<String> {
    let t: String = s.trim().chars().map(|c| if c.is_alphanumeric() || c == '_' { c } else { '_' }).collect();
    let t = t.trim_matches('_').to_string();
    (!t.is_empty() && !t.starts_with(|c: char| c.is_ascii_digit())).then_some(t)
}

/// ¿Aparece `id` como palabra completa en `line`? Devuelve dónde.
fn word_positions(line: &str, id: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(p) = line.get(from..).and_then(|r| r.find(id)) {
        let at = from + p;
        let before = line.get(..at).and_then(|b| b.chars().next_back());
        let after = line.get(at + id.len()..).and_then(|a| a.chars().next());
        if !before.is_some_and(is_id_char) && !after.is_some_and(is_id_char) {
            out.push(at);
        }
        from = at + id.len().max(1);
        if out.len() > 200 {
            break;
        }
    }
    out
}

fn mentions(line: &str, id: &str) -> bool {
    !word_positions(line, id).is_empty()
}

/// Cambia el nombre `old` por `new` en todas las líneas (palabras completas, fuera de comentarios).
fn rename_everywhere(block: &str, old: &str, new: &str) -> String {
    let mut out = String::with_capacity(block.len() + 32);
    for (n, l) in block.split_inclusive('\n').enumerate() {
        let t = l.trim_start();
        if n >= MAX_LINES || t.starts_with("%%") || t.starts_with("```") || t.starts_with("~~~") {
            out.push_str(l);
            continue;
        }
        let mut line = String::with_capacity(l.len() + 16);
        let mut last = 0;
        for p in word_positions(l, old) {
            line.push_str(l.get(last..p).unwrap_or(""));
            line.push_str(new);
            last = p + old.len();
        }
        line.push_str(l.get(last..).unwrap_or(""));
        out.push_str(&line);
    }
    out
}

/// Quita las líneas `lines` (ordenadas o no).
fn remove_lines(block: &str, mut lines: Vec<usize>) -> Option<String> {
    lines.sort_unstable();
    lines.dedup();
    let mut out = block.to_string();
    for n in lines.into_iter().rev() {
        out = replace_line(&out, n, None)?;
    }
    Some(out)
}

/// El bloque `{ … }` que abre en la línea `start` (si la línea termina en `{`): (inicio, cierre).
fn brace_block(block: &str, start: usize) -> Option<(usize, usize)> {
    let lines: Vec<&str> = block.lines().take(MAX_LINES).collect();
    if !lines.get(start)?.trim_end().ends_with('{') {
        return None;
    }
    let end = (start + 1..lines.len()).find(|&i| lines.get(i).is_some_and(|l| l.trim() == "}"))?;
    Some((start, end))
}

/// La línea que abre el bloque de `id` (`class X {` o `X {`), si la hay.
fn open_line(block: &str, id: &str, class_kw: bool) -> Option<usize> {
    block.lines().take(MAX_LINES).position(|l| {
        let t = l.trim();
        let t = if class_kw { t.strip_prefix("class ").map(str::trim_start).unwrap_or("") } else { t };
        t.ends_with('{') && t.strip_prefix(id).is_some_and(|r| r.trim_start().starts_with('{') || r.trim_start().starts_with('['))
    })
}

/// Las líneas dentro de bloques `{ … }` (para no tomarlas por relaciones).
fn inside_braces(block: &str) -> Vec<bool> {
    let mut depth = 0usize;
    block
        .lines()
        .take(MAX_LINES)
        .map(|l| {
            let t = l.trim();
            let inside = depth > 0 && t != "}";
            if t.ends_with('{') {
                depth += 1;
            } else if t == "}" {
                depth = depth.saturating_sub(1);
            }
            inside
        })
        .collect()
}

fn is_header(code: &str, header: &str) -> bool {
    code.lines()
        .take(40)
        .map(str::trim)
        .find(|l| !(l.is_empty() || l.starts_with("```") || l.starts_with("~~~") || l.starts_with("%%") || l.starts_with("---")))
        .is_some_and(|l| l.starts_with(header))
}

/// La caja del primer texto `want` desde `from` y el rectángulo justo antes: (índice, caja).
fn titled_box(els: &[El], from: usize, want: &str) -> Option<(usize, [f32; 4])> {
    (from..els.len()).find_map(|i| {
        let El::Text { x, y, middle, text } = els.get(i)? else { return None };
        if want.is_empty() || *text != want {
            return None;
        }
        let tb = text_box(*x, *y, *middle, text.chars().count());
        // La caja de la clase: el rectángulo más cercano antes que contiene el título.
        let rect = els.get(i.saturating_sub(6)..i)?.iter().rev().find_map(|e| match e {
            El::Shape { tag, rect } if tag.starts_with("<rect") && rect[0] <= *x && rect[2] >= *x && rect[1] <= *y => Some(*rect),
            _ => None,
        });
        Some((i, rect.map_or(tb, |r| union(r, tb))))
    })
}

/// Diagramas de clases.
pub mod class {
    use super::*;

    pub fn is_class(code: &str) -> bool {
        is_header(code, "classDiagram")
    }

    /// Las clases (nombre y nombre visible) en el orden del árbol.
    pub fn classes(code: &str) -> Vec<(String, String)> {
        match mermaid_svg::parse(code) {
            Ok(mermaid_svg::Diagram::Class(c)) => c
                .classes
                .into_iter()
                .take(MAX_ITEMS)
                .map(|k| {
                    let shown = k.label.clone().unwrap_or_else(|| k.name.clone());
                    (k.name, shown)
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    fn exists(code: &str, id: &str) -> bool {
        classes(code).iter().any(|(n, _)| n == id)
    }

    /// Las flechas de relación que se ofrecen.
    pub const KINDS: [(&str, (&str, &str)); 5] = [
        ("-->", ("Association", "Asociación")),
        ("<|--", ("Inherits (is a)", "Herencia (es un)")),
        ("*--", ("Composition (part of)", "Composición (es parte de)")),
        ("o--", ("Aggregation (has)", "Agregación (tiene)")),
        ("..>", ("Dependency (uses)", "Dependencia (usa)")),
    ];

    pub fn add_class(block: &str, code: &str, name: &str) -> Option<String> {
        if !is_class(code) {
            return None;
        }
        let id = ident(name)?;
        if exists(code, &id) {
            return None;
        }
        Some(insert_at(block, None, &format!("{}class {id}", body_indent_after(block, "classDiagram"))))
    }

    /// Agrega un atributo o método (`+nombre: String`, `+calcular()`): dentro de su bloque si lo
    /// tiene; si no, `Clase : miembro`.
    pub fn add_member(block: &str, code: &str, id: &str, member: &str) -> Option<String> {
        if !exists(code, id) {
            return None;
        }
        let m = clean(&member.replace(['{', '}'], " "))?;
        if let Some(open) = open_line(block, id, true)
            && let Some((_, close)) = brace_block(block, open)
        {
            let inner = block.lines().nth(open + 1).filter(|_| open + 1 < close).map(|l| indent_of(l).to_string());
            let indent = inner.unwrap_or_else(|| format!("{}    ", indent_of(block.lines().nth(open).unwrap_or(""))));
            return Some(insert_at(block, Some(close), &format!("{indent}{m}")));
        }
        let last = block.lines().take(MAX_LINES).enumerate().filter(|(_, l)| mentions(l, id)).map(|(n, _)| n).last();
        Some(insert_at(block, last.map(|n| n + 1), &format!("{}{id} : {m}", body_indent_after(block, "classDiagram"))))
    }

    /// Relaciona `id` con `to` (`kind`: índice en [`KINDS`]) con un texto opcional.
    pub fn relate(block: &str, code: &str, id: &str, to: &str, kind: usize, label: &str) -> Option<String> {
        if id == to || !exists(code, id) || !exists(code, to) {
            return None;
        }
        let (arrow, _) = KINDS.get(kind)?;
        // En la herencia, la flecha apunta al padre: `Padre <|-- Hijo`.
        let line = if *arrow == "<|--" { format!("{to} <|-- {id}") } else { format!("{id} {arrow} {to}") };
        let label = clean(&label.replace(':', " ")).map(|l| format!(" : {l}")).unwrap_or_default();
        Some(insert_at(block, None, &format!("{}{line}{label}", body_indent_after(block, "classDiagram"))))
    }

    pub fn rename(block: &str, code: &str, id: &str, new: &str) -> Option<String> {
        let new = ident(new)?;
        if !exists(code, id) || exists(code, &new) {
            return None;
        }
        Some(rename_everywhere(block, id, &new))
    }

    /// Borra la clase: su bloque, sus miembros, sus relaciones, notas y estilos.
    pub fn delete(block: &str, code: &str, id: &str) -> Option<String> {
        if !exists(code, id) {
            return None;
        }
        let inside = inside_braces(block);
        let mut gone: Vec<usize> = Vec::new();
        if let Some(open) = open_line(block, id, true)
            && let Some((a, b)) = brace_block(block, open)
        {
            gone.extend(a..=b);
        }
        for (n, l) in block.lines().take(MAX_LINES).enumerate() {
            if inside.get(n).copied().unwrap_or(false) || gone.contains(&n) {
                continue;
            }
            let t = l.trim_start();
            if !t.starts_with("%%") && mentions(t, id) && !t.starts_with("classDiagram") {
                gone.push(n);
            }
        }
        remove_lines(block, gone)
    }

    /// Zonas: la caja de cada clase.
    pub fn boxes(svg: &str, code: &str) -> Vec<Option<[f32; 4]>> {
        let els = elements(svg);
        let mut cursor = 0;
        classes(code)
            .iter()
            .map(|(_, shown)| {
                let (i, b) = titled_box(&els, cursor, &norm(shown)).or_else(|| titled_box(&els, 0, &norm(shown)))?;
                cursor = i + 1;
                Some(b)
            })
            .collect()
    }
}

/// Diagramas de entidad-relación.
pub mod er {
    use super::*;

    pub fn is_er(code: &str) -> bool {
        is_header(code, "erDiagram")
    }

    /// Las entidades (nombre y nombre visible) en el orden del árbol.
    pub fn entities(code: &str) -> Vec<(String, String)> {
        match mermaid_svg::parse(code) {
            Ok(mermaid_svg::Diagram::Er(e)) => e.entities.into_iter().take(MAX_ITEMS).map(|k| (k.name, k.label)).collect(),
            _ => Vec::new(),
        }
    }

    fn exists(code: &str, id: &str) -> bool {
        entities(code).iter().any(|(n, _)| n == id)
    }

    /// Las cardinalidades que se ofrecen (la flecha de Mermaid y su nombre).
    pub const KINDS: [(&str, (&str, &str)); 4] = [
        ("||--o{", ("One to many", "Uno a muchos")),
        ("||--||", ("One to one", "Uno a uno")),
        ("}o--o{", ("Many to many", "Muchos a muchos")),
        ("||--o|", ("One to zero or one", "Uno a cero o uno")),
    ];

    pub fn add_entity(block: &str, code: &str, name: &str) -> Option<String> {
        if !is_er(code) {
            return None;
        }
        let id = ident(name)?;
        if exists(code, &id) {
            return None;
        }
        Some(insert_at(block, None, &format!("{}{id}", body_indent_after(block, "erDiagram"))))
    }

    /// Agrega un atributo («tipo nombre», p. ej. `string rfc PK`) dentro del bloque de la entidad
    /// (lo crea si no lo tiene).
    pub fn add_attribute(block: &str, code: &str, id: &str, attr: &str) -> Option<String> {
        if !exists(code, id) {
            return None;
        }
        let a = clean(&attr.replace(['{', '}', '"', ':'], " "))?;
        // Un atributo necesita tipo y nombre: con una sola palabra, el tipo es `string`.
        let a = if a.split_whitespace().count() == 1 { format!("string {a}") } else { a };
        if let Some(open) = open_line(block, id, false)
            && let Some((_, close)) = brace_block(block, open)
        {
            let inner = block.lines().nth(open + 1).filter(|_| open + 1 < close).map(|l| indent_of(l).to_string());
            let indent = inner.unwrap_or_else(|| format!("{}    ", indent_of(block.lines().nth(open).unwrap_or(""))));
            return Some(insert_at(block, Some(close), &format!("{indent}{a}")));
        }
        let indent = body_indent_after(block, "erDiagram");
        // Una línea con solo el nombre se vuelve su bloque; si no, el bloque va al final.
        let alone = block.lines().take(MAX_LINES).position(|l| l.trim() == id);
        match alone {
            Some(n) => {
                let ind = indent_of(block.lines().nth(n)?).to_string();
                let out = replace_line(block, n, Some(format!("{ind}{id} {{")))?;
                let out = insert_at(&out, Some(n + 1), &format!("{ind}}}"));
                Some(insert_at(&out, Some(n + 1), &format!("{ind}    {a}")))
            }
            None => {
                let out = insert_at(block, None, &format!("{indent}{id} {{"));
                let out = insert_at(&out, None, &format!("{indent}    {a}"));
                Some(insert_at(&out, None, &format!("{indent}}}")))
            }
        }
    }

    /// Relaciona `id` con `to` (`kind`: índice en [`KINDS`]) con su texto (Mermaid lo pide; vacío:
    /// `""`).
    pub fn relate(block: &str, code: &str, id: &str, to: &str, kind: usize, label: &str) -> Option<String> {
        if !exists(code, id) || !exists(code, to) {
            return None;
        }
        let (arrow, _) = KINDS.get(kind)?;
        let label = clean(&label.replace('"', "'")).unwrap_or_default();
        Some(insert_at(block, None, &format!("{}{id} {arrow} {to} : \"{label}\"", body_indent_after(block, "erDiagram"))))
    }

    pub fn rename(block: &str, code: &str, id: &str, new: &str) -> Option<String> {
        let new = ident(new)?;
        if !exists(code, id) || exists(code, &new) {
            return None;
        }
        Some(rename_everywhere(block, id, &new))
    }

    /// Borra la entidad con su bloque y sus relaciones.
    pub fn delete(block: &str, code: &str, id: &str) -> Option<String> {
        if !exists(code, id) {
            return None;
        }
        let inside = inside_braces(block);
        let mut gone: Vec<usize> = Vec::new();
        if let Some(open) = open_line(block, id, false)
            && let Some((a, b)) = brace_block(block, open)
        {
            gone.extend(a..=b);
        }
        for (n, l) in block.lines().take(MAX_LINES).enumerate() {
            if inside.get(n).copied().unwrap_or(false) || gone.contains(&n) {
                continue;
            }
            let t = l.trim_start();
            if !t.starts_with("%%") && mentions(t, id) && !t.starts_with("erDiagram") {
                gone.push(n);
            }
        }
        remove_lines(block, gone)
    }

    /// Zonas: la caja de cada entidad.
    pub fn boxes(svg: &str, code: &str) -> Vec<Option<[f32; 4]>> {
        let els = elements(svg);
        let mut cursor = 0;
        entities(code)
            .iter()
            .map(|(_, shown)| {
                let (i, b) = titled_box(&els, cursor, &norm(shown)).or_else(|| titled_box(&els, 0, &norm(shown)))?;
                cursor = i + 1;
                Some(b)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn code(b: &str) -> String {
        crate::diagram::mermaid_code(b).unwrap()
    }

    const CL: &str = "```mermaid\nclassDiagram\n    class Expediente {\n        +String numero\n        +abrir()\n    }\n    class Parte\n    Expediente --> Parte : tiene\n    Parte : +String nombre\n```\n";

    #[test]
    fn class_members_relations_rename_delete() {
        let c = code(CL);
        assert_eq!(class::classes(&c).len(), 2);
        let out = class::add_member(CL, &c, "Expediente", "+cerrar()").unwrap();
        assert!(out.contains("        +abrir()\n        +cerrar()\n    }\n"), "{out}");
        let out = class::add_member(CL, &c, "Parte", "+String rfc").unwrap();
        assert!(out.contains("    Parte : +String nombre\n    Parte : +String rfc\n"), "{out}");
        let out = class::add_class(CL, &c, "Audiencia").unwrap();
        assert!(out.ends_with("    class Audiencia\n```\n"), "{out}");
        let out = class::relate(&out, &code(&out), "Audiencia", "Expediente", 1, "").unwrap();
        assert!(out.contains("    Expediente <|-- Audiencia\n```"), "{out}");
        let out = class::rename(CL, &c, "Parte", "Persona").unwrap();
        assert!(out.contains("    class Persona\n    Expediente --> Persona : tiene\n    Persona : +String nombre\n"), "{out}");
        let out = class::delete(CL, &c, "Expediente").unwrap();
        assert_eq!(out, "```mermaid\nclassDiagram\n    class Parte\n    Parte : +String nombre\n```\n");
        assert!(class::rename(CL, &c, "Parte", "Expediente").is_none(), "ya existe");
    }

    const ER: &str = "```mermaid\nerDiagram\n    CLIENTE ||--o{ CASO : abre\n    CLIENTE {\n        string nombre\n    }\n    CASO\n```\n";

    #[test]
    fn er_attributes_relations_rename_delete() {
        let c = code(ER);
        assert_eq!(er::entities(&c).len(), 2);
        let out = er::add_attribute(ER, &c, "CLIENTE", "string rfc PK").unwrap();
        assert!(out.contains("        string nombre\n        string rfc PK\n    }\n"), "{out}");
        let out = er::add_attribute(ER, &c, "CASO", "numero").unwrap();
        assert!(out.contains("    CASO {\n        string numero\n    }\n```"), "{out}");
        assert_eq!(er::entities(&code(&out)).len(), 2);
        let out = er::add_entity(ER, &c, "Juez").unwrap();
        let out = er::relate(&out, &code(&out), "Juez", "CASO", 0, "resuelve").unwrap();
        assert!(out.contains("    Juez\n    Juez ||--o{ CASO : \"resuelve\"\n```"), "{out}");
        let out = er::rename(ER, &c, "CASO", "ASUNTO").unwrap();
        assert!(out.contains("CLIENTE ||--o{ ASUNTO : abre") && out.contains("    ASUNTO\n"), "{out}");
        let out = er::delete(ER, &c, "CLIENTE").unwrap();
        assert_eq!(out, "```mermaid\nerDiagram\n    CASO\n```\n");
    }

    #[test]
    fn boxes_match_the_drawing() {
        let c = code(CL);
        let svg = mermaid_svg::render(&c).unwrap();
        let b = class::boxes(&svg, &c);
        assert!(b.iter().all(Option::is_some), "{b:?}");
        let c = code(ER);
        let svg = mermaid_svg::render(&c).unwrap();
        let b = er::boxes(&svg, &c);
        assert!(b.iter().all(Option::is_some), "{b:?}");
    }

    proptest! {
        #[test]
        fn never_panics(text in ".{0,30}", junk in ".{0,120}", k in 0usize..6) {
            for head in ["classDiagram", "erDiagram"] {
                let block = format!("```mermaid\n{head}\n    A --> B\n    {junk}\n```\n");
                let c = format!("{head}\n    A --> B\n    {junk}");
                let _ = class::add_member(&block, &c, "A", &text);
                let _ = class::relate(&block, &c, "A", "B", k, &text);
                let _ = class::rename(&block, &c, "A", &text);
                let _ = class::delete(&block, &c, "B");
                let _ = er::add_attribute(&block, &c, "A", &text);
                let _ = er::relate(&block, &c, "A", "B", k, &text);
                let _ = er::delete(&block, &c, "A");
                let _ = class::boxes(&junk, &c);
            }
            if let Some(out) = class::add_member(CL, &code(CL), "Parte", &text) {
                prop_assert_eq!(class::classes(&code(&out)).len(), 2);
            }
            if let Some(out) = er::add_attribute(ER, &code(ER), "CASO", &text) {
                prop_assert_eq!(er::entities(&code(&out)).len(), 2);
            }
        }
    }
}
