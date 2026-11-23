//! Pastel, línea de tiempo, Kanban y Gantt de Mermaid: agregar, cambiar y borrar sus elementos
//! desde el dibujo. Como en los demás tipos, cada cambio toca solo sus líneas y, si el código y el
//! árbol de `mermaid-svg` no coinciden, la acción no se hace.

use super::MAX_LINES;
use super::flowchart::norm;
use super::sequence::{El, body_indent_after, clean, elements, indent_of, insert_at, replace_line, text_box, union};

/// Elementos que se revisan como mucho.
const MAX_ITEMS: usize = 5_000;

/// ¿La primera línea con contenido empieza con `header`?
fn is_kind(code: &str, header: &str) -> bool {
    code.lines()
        .take(40)
        .map(str::trim)
        .find(|l| {
            !(l.is_empty()
                || l.starts_with("```")
                || l.starts_with("~~~")
                || l.starts_with("%%")
                || l.starts_with("---"))
        })
        .is_some_and(|l| l.starts_with(header))
}

/// Las líneas del cuerpo (sin cercas ni comentarios), con su número.
fn body(block: &str) -> impl Iterator<Item = (usize, &str)> {
    block.lines().take(MAX_LINES).enumerate().filter(|(_, l)| {
        let t = l.trim_start();
        !(t.is_empty() || t.starts_with("```") || t.starts_with("~~~") || t.starts_with("%%"))
    })
}

/// La caja del primer texto `want` desde `from` y la figura justo antes (si la hay): (índice, caja).
fn text_with_shape(els: &[El], from: usize, want: &str, shape_before: bool) -> Option<(usize, [f32; 4])> {
    (from..els.len()).find_map(|i| {
        let El::Text { x, y, middle, text } = els.get(i)? else { return None };
        if want.is_empty() || *text != want {
            return None;
        }
        let tb = text_box(*x, *y, *middle, text.chars().count());
        let b = match i.checked_sub(1).and_then(|j| els.get(j)) {
            Some(El::Shape { rect, .. }) if shape_before => union(tb, *rect),
            _ => tb,
        };
        Some((i, b))
    })
}

/// Pastel (`pie`): porciones `"Nombre" : valor`.
pub mod pie {
    use super::*;

    pub fn is_pie(code: &str) -> bool {
        is_kind(code, "pie")
    }

    /// Las porciones (nombre y valor) del árbol.
    pub fn entries(code: &str) -> Vec<(String, f64)> {
        match mermaid_svg::parse(code) {
            Ok(mermaid_svg::Diagram::Pie(p)) => {
                p.entries.into_iter().take(MAX_ITEMS).map(|e| (e.label, e.value)).collect()
            }
            _ => Vec::new(),
        }
    }

    /// Las líneas de las porciones: (línea, nombre, dónde empieza el valor).
    fn lines(block: &str) -> Vec<(usize, String, usize)> {
        body(block)
            .filter_map(|(n, l)| {
                let t = l.trim_start();
                let start = l.len() - t.len();
                let rest = t.strip_prefix('"')?;
                let close = rest.find('"')?;
                let after = rest.get(close + 1..)?;
                let colon = after.find(':')?;
                after
                    .get(..colon)?
                    .trim()
                    .is_empty()
                    .then(|| (n, rest.get(..close).unwrap_or("").to_string(), start + 1 + close + 1 + colon + 1))
            })
            .collect()
    }

    fn at(block: &str, code: &str, k: usize) -> Option<(usize, String, usize)> {
        let ls = lines(block);
        let es = entries(code);
        (ls.len() == es.len()).then_some(())?;
        ls.into_iter().nth(k)
    }

    fn label(s: &str) -> Option<String> {
        clean(&s.replace('"', "'"))
    }

    fn value(s: &str) -> Option<String> {
        let v: f64 = s.trim().replace(',', ".").parse().ok()?;
        (v.is_finite() && v >= 0.0).then(|| s.trim().replace(',', "."))
    }

    /// Agrega una porción con «Nombre valor» (el número al final).
    pub fn add(block: &str, code: &str, text: &str) -> Option<String> {
        if !is_pie(code) {
            return None;
        }
        let t = text.trim();
        let cut = t.rfind(char::is_whitespace)?;
        let (name, num) = (t.get(..cut)?, t.get(cut..)?);
        let (name, num) = (label(name)?, value(num)?);
        let last = lines(block).last().map(|(n, _, _)| *n);
        let indent = body_indent_after(block, "pie");
        Some(insert_at(block, last.map(|n| n + 1), &format!("{indent}\"{name}\" : {num}")))
    }

    pub fn set_value(block: &str, code: &str, k: usize, text: &str) -> Option<String> {
        let (n, _, v) = at(block, code, k)?;
        let line = block.lines().nth(n)?;
        replace_line(block, n, Some(format!("{} {}", line.get(..v)?, value(text)?)))
    }

    pub fn rename(block: &str, code: &str, k: usize, text: &str) -> Option<String> {
        let (n, _, v) = at(block, code, k)?;
        let line = block.lines().nth(n)?;
        let rest = line.get(v..)?;
        replace_line(block, n, Some(format!("{}\"{}\" :{rest}", indent_of(line), label(text)?)))
    }

    pub fn delete(block: &str, code: &str, k: usize) -> Option<String> {
        let (n, _, _) = at(block, code, k)?;
        replace_line(block, n, None)
    }

    /// Zonas: cada porción en la leyenda (su cuadrito y su nombre).
    pub fn boxes(svg: &str, code: &str) -> Vec<Option<[f32; 4]>> {
        let els = elements(svg);
        let mut cursor = 0;
        entries(code)
            .iter()
            .map(|(l, _)| {
                let (i, b) = text_with_shape(&els, cursor, &norm(l), true)?;
                cursor = i + 1;
                Some(b)
            })
            .collect()
    }
}

/// Línea de tiempo (`timeline`): periodos `Periodo : evento : evento` (y `: evento` debajo).
pub mod timeline {
    use super::*;

    pub fn is_timeline(code: &str) -> bool {
        is_kind(code, "timeline")
    }

    /// Los periodos (nombre y eventos) del árbol, en orden.
    pub fn periods(code: &str) -> Vec<(String, Vec<String>)> {
        match mermaid_svg::parse(code) {
            Ok(mermaid_svg::Diagram::Timeline(t)) => {
                t.sections.into_iter().flat_map(|s| s.periods).take(MAX_ITEMS).map(|p| (p.label, p.events)).collect()
            }
            _ => Vec::new(),
        }
    }

    /// Un periodo en el código: su línea, la última de sus líneas y sus eventos (línea y número
    /// de parte entre `:`).
    #[derive(Clone, Debug, PartialEq)]
    struct Period {
        line: usize,
        last: usize,
        events: Vec<(usize, usize)>,
    }

    fn source(block: &str) -> Vec<Period> {
        let mut out: Vec<Period> = Vec::new();
        for (n, l) in body(block) {
            let t = l.trim_start();
            let word = t.split_whitespace().next().unwrap_or("");
            if matches!(word, "timeline" | "title" | "section" | "accTitle" | "accDescr") {
                continue;
            }
            if t.starts_with(':') {
                if let Some(p) = out.last_mut() {
                    let parts = t.split(':').count().saturating_sub(1);
                    p.events.extend((1..=parts).map(|k| (n, k)));
                    p.last = n;
                }
                continue;
            }
            let parts = t.split(':').count().saturating_sub(1);
            out.push(Period { line: n, last: n, events: (1..=parts).map(|k| (n, k)).collect() });
        }
        out
    }

    fn at(block: &str, code: &str, k: usize) -> Option<Period> {
        let src = source(block);
        let tree = periods(code);
        let same = src.len() == tree.len() && src.iter().zip(&tree).all(|(s, (_, e))| s.events.len() == e.len());
        same.then_some(())?;
        src.into_iter().nth(k)
    }

    fn text(s: &str) -> Option<String> {
        clean(&s.replace(':', "-"))
    }

    /// Cambia la parte `part` (0: el periodo) de la línea `n`, o la quita (`None`).
    fn set_part(block: &str, n: usize, part: usize, new: Option<&str>) -> Option<String> {
        let line = block.lines().nth(n)?;
        let indent = indent_of(line);
        let t = line.trim_start();
        let mut parts: Vec<String> = t.split(':').map(|p| p.trim().to_string()).collect();
        match new {
            Some(x) => *parts.get_mut(part)? = x.to_string(),
            None => {
                parts.get(part)?;
                parts.remove(part);
            }
        }
        // Una línea de continuación (`: evento`) sin eventos se quita.
        let cont = t.starts_with(':');
        if cont && parts.iter().skip(1).all(String::is_empty) {
            return replace_line(block, n, None);
        }
        let joined = parts.iter().map(String::as_str).collect::<Vec<_>>().join(" : ");
        replace_line(block, n, Some(format!("{indent}{}", joined.trim_start())))
    }

    /// Agrega un periodo con «Periodo: evento» (sin `:`, el evento queda «…» para cambiarlo).
    pub fn add_period(block: &str, code: &str, input: &str) -> Option<String> {
        if !is_timeline(code) {
            return None;
        }
        let (label, event) = input.split_once(':').unwrap_or((input, "…"));
        let (l, e) = (text(label)?, text(event).unwrap_or_else(|| "…".into()));
        let last = source(block).last().map(|p| p.last);
        let indent = body_indent_after(block, "timeline");
        Some(insert_at(block, last.map(|n| n + 1), &format!("{indent}{l} : {e}")))
    }

    pub fn add_event(block: &str, code: &str, k: usize, event: &str) -> Option<String> {
        let p = at(block, code, k)?;
        let e = text(event)?;
        let line = block.lines().nth(p.last)?;
        replace_line(block, p.last, Some(format!("{} : {e}", line.trim_end())))
    }

    pub fn rename_period(block: &str, code: &str, k: usize, label: &str) -> Option<String> {
        let p = at(block, code, k)?;
        set_part(block, p.line, 0, Some(&text(label)?))
    }

    pub fn delete_period(block: &str, code: &str, k: usize) -> Option<String> {
        let p = at(block, code, k)?;
        let mut out = block.to_string();
        for n in (p.line..=p.last).rev() {
            out = replace_line(&out, n, None)?;
        }
        Some(out)
    }

    /// El evento `e` del periodo `k`.
    pub fn rename_event(block: &str, code: &str, k: usize, e: usize, event: &str) -> Option<String> {
        let p = at(block, code, k)?;
        let (n, part) = *p.events.get(e)?;
        set_part(block, n, part, Some(&text(event)?))
    }

    pub fn delete_event(block: &str, code: &str, k: usize, e: usize) -> Option<String> {
        let p = at(block, code, k)?;
        let (n, part) = *p.events.get(e)?;
        set_part(block, n, part, None)
    }

    /// Zonas: cada periodo y cada uno de sus eventos (la caja con su texto), en orden.
    pub fn boxes(svg: &str, code: &str) -> Vec<(Option<[f32; 4]>, Vec<Option<[f32; 4]>>)> {
        let els = elements(svg);
        let mut cursor = 0;
        let mut find = |want: &str| {
            let (i, b) = text_with_shape(&els, cursor, &norm(want), true)?;
            cursor = i + 1;
            Some(b)
        };
        periods(code)
            .iter()
            .map(|(l, es)| {
                let pb = find(l);
                let eb = es.iter().map(|e| find(e)).collect();
                (pb, eb)
            })
            .collect()
    }
}

/// Kanban de Mermaid: columnas (sin sangría) y tarjetas (con sangría) `id[Texto]`.
pub mod kanban {
    use super::*;

    pub fn is_kanban(code: &str) -> bool {
        is_kind(code, "kanban")
    }

    /// Las columnas (id, nombre y tarjetas: id y texto) del árbol.
    pub fn columns(code: &str) -> Vec<(String, String, Vec<(String, String)>)> {
        match mermaid_svg::parse(code) {
            Ok(mermaid_svg::Diagram::Kanban(k)) => k
                .columns
                .into_iter()
                .take(MAX_ITEMS)
                .map(|c| (c.id, c.label, c.tasks.into_iter().map(|t| (t.id, t.text)).collect()))
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Una columna en el código: su línea, la última de sus líneas y las líneas de sus tarjetas.
    #[derive(Clone, Debug, PartialEq)]
    struct Col {
        line: usize,
        last: usize,
        cards: Vec<usize>,
    }

    fn source(block: &str) -> Vec<Col> {
        let mut out: Vec<Col> = Vec::new();
        let mut col_indent: Option<usize> = None;
        let mut started = false;
        for (n, l) in body(block) {
            let t = l.trim_start();
            if !started {
                started = t.starts_with("kanban");
                continue;
            }
            let ind = l.len() - t.len();
            // `@{ … }` en su propia línea es de la tarjeta anterior.
            if t.starts_with("@{") {
                if let Some(c) = out.last_mut() {
                    c.last = n;
                }
                continue;
            }
            match col_indent {
                Some(ci) if ind > ci => {
                    if let Some(c) = out.last_mut() {
                        c.cards.push(n);
                        c.last = n;
                    }
                }
                _ => {
                    col_indent = Some(col_indent.map_or(ind, |c| c.min(ind)));
                    out.push(Col { line: n, last: n, cards: Vec::new() });
                }
            }
        }
        out
    }

    fn checked(block: &str, code: &str) -> Option<(Vec<Col>, Vec<(String, String, Vec<(String, String)>)>)> {
        let src = source(block);
        let tree = columns(code);
        let same = src.len() == tree.len() && src.iter().zip(&tree).all(|(s, (_, _, t))| s.cards.len() == t.len());
        same.then_some((src, tree))
    }

    fn card_text(s: &str) -> Option<String> {
        clean(&s.replace(['[', ']', '{', '}'], " "))
    }

    /// La sangría de las tarjetas (la de alguna; si no, la de las columnas más cuatro).
    fn card_indent(block: &str, src: &[Col]) -> String {
        src.iter()
            .flat_map(|c| c.cards.first())
            .next()
            .and_then(|n| block.lines().nth(*n))
            .map(|l| indent_of(l).to_string())
            .or_else(|| src.first().and_then(|c| block.lines().nth(c.line)).map(|l| format!("{}    ", indent_of(l))))
            .unwrap_or_else(|| "        ".into())
    }

    fn free_id(code: &str) -> String {
        let used: Vec<String> = columns(code)
            .into_iter()
            .flat_map(|(id, _, t)| std::iter::once(id).chain(t.into_iter().map(|(i, _)| i)))
            .collect();
        (1..10_000).map(|k| format!("t{k}")).find(|c| !used.contains(c)).unwrap_or_else(|| "nueva".into())
    }

    /// Cambia el texto de `id[Texto]` (o toda la línea si no tiene corchetes).
    fn set_bracket(block: &str, n: usize, text: &str) -> Option<String> {
        let line = block.lines().nth(n)?;
        let new = match (line.find('['), line.find(']')) {
            (Some(a), Some(b)) if a < b => format!("{}{text}{}", line.get(..a + 1)?, line.get(b..)?),
            _ => format!("{}{text}", indent_of(line)),
        };
        replace_line(block, n, Some(new))
    }

    pub fn add_column(block: &str, code: &str, name: &str) -> Option<String> {
        if !is_kanban(code) {
            return None;
        }
        let name = card_text(name)?;
        let src = source(block);
        let indent = src
            .first()
            .and_then(|c| block.lines().nth(c.line))
            .map(|l| indent_of(l).to_string())
            .unwrap_or_else(|| body_indent_after(block, "kanban"));
        let id = free_id(code);
        Some(insert_at(block, src.last().map(|c| c.last + 1), &format!("{indent}{id}[{name}]")))
    }

    pub fn add_card(block: &str, code: &str, col: usize, text: &str) -> Option<String> {
        let (src, _) = checked(block, code)?;
        let c = src.get(col)?;
        let text = card_text(text)?;
        Some(insert_at(block, Some(c.last + 1), &format!("{}{}[{text}]", card_indent(block, &src), free_id(code))))
    }

    pub fn rename_column(block: &str, code: &str, col: usize, name: &str) -> Option<String> {
        let (src, _) = checked(block, code)?;
        set_bracket(block, src.get(col)?.line, &card_text(name)?)
    }

    pub fn delete_column(block: &str, code: &str, col: usize) -> Option<String> {
        let (src, _) = checked(block, code)?;
        let c = src.get(col)?;
        let mut out = block.to_string();
        for n in (c.line..=c.last).rev() {
            out = replace_line(&out, n, None)?;
        }
        Some(out)
    }

    /// Las líneas de la tarjeta `card` de la columna `col` (con su `@{ … }` si va debajo).
    fn card_lines(c: &Col, card: usize) -> Option<(usize, usize)> {
        let start = *c.cards.get(card)?;
        let end = c.cards.get(card + 1).map(|n| n - 1).unwrap_or(c.last);
        Some((start, end))
    }

    pub fn rename_card(block: &str, code: &str, col: usize, card: usize, text: &str) -> Option<String> {
        let (src, _) = checked(block, code)?;
        set_bracket(block, *src.get(col)?.cards.get(card)?, &card_text(text)?)
    }

    pub fn delete_card(block: &str, code: &str, col: usize, card: usize) -> Option<String> {
        let (src, _) = checked(block, code)?;
        let (a, b) = card_lines(src.get(col)?, card)?;
        let mut out = block.to_string();
        for n in (a..=b).rev() {
            out = replace_line(&out, n, None)?;
        }
        Some(out)
    }

    /// Mueve la tarjeta al final de la columna `to`.
    pub fn move_card(block: &str, code: &str, col: usize, card: usize, to: usize) -> Option<String> {
        let (src, _) = checked(block, code)?;
        if col == to {
            return None;
        }
        let (a, b) = card_lines(src.get(col)?, card)?;
        let dest = src.get(to)?.last;
        let lines: Vec<&str> = block.split_inclusive('\n').collect();
        let moving: String = lines.get(a..=b)?.concat();
        let indent = card_indent(block, &src);
        // La sangría de la tarjeta se ajusta a la de las tarjetas (por si venía de otra columna).
        let moving: String = moving.split_inclusive('\n').map(|l| format!("{indent}{}", l.trim_start())).collect();
        let mut out = String::with_capacity(block.len());
        for (i, l) in lines.iter().enumerate() {
            if (a..=b).contains(&i) {
                continue;
            }
            out.push_str(l);
            if i == dest {
                if !out.ends_with('\n') {
                    out.push('\n');
                }
                out.push_str(&moving);
                if !moving.ends_with('\n') {
                    out.push('\n');
                }
            }
        }
        Some(out)
    }

    /// Zonas: cada columna (su encabezado) y sus tarjetas.
    pub fn boxes(svg: &str, code: &str) -> Vec<(Option<[f32; 4]>, Vec<Option<[f32; 4]>>)> {
        let els = elements(svg);
        let mut cursor = 0;
        let mut find = |want: &str| {
            let (i, b) = text_with_shape(&els, cursor, &norm(want), true)?;
            cursor = i + 1;
            Some(b)
        };
        columns(code)
            .iter()
            .map(|(_, label, cards)| {
                let cb = find(label);
                let tb = cards.iter().map(|(_, t)| find(t)).collect();
                (cb, tb)
            })
            .collect()
    }
}

/// Gantt: secciones y tareas `Nombre :datos`.
pub mod gantt {
    use super::*;

    pub fn is_gantt(code: &str) -> bool {
        is_kind(code, "gantt")
    }

    /// Las secciones (nombre y tareas) del árbol.
    pub fn sections(code: &str) -> Vec<(String, Vec<String>)> {
        match mermaid_svg::parse(code) {
            Ok(mermaid_svg::Diagram::Gantt(g)) => g
                .sections
                .into_iter()
                .take(MAX_ITEMS)
                .map(|s| (s.name, s.tasks.into_iter().map(|t| t.name).collect()))
                .collect(),
            _ => Vec::new(),
        }
    }

    const KEYWORDS: [&str; 15] = [
        "gantt",
        "title",
        "dateFormat",
        "axisFormat",
        "excludes",
        "includes",
        "todayMarker",
        "tickInterval",
        "weekday",
        "weekend",
        "displayMode",
        "click",
        "inclusiveEndDates",
        "topAxis",
        "accTitle",
    ];

    /// Una sección en el código: su línea (`None`: las tareas antes de la primera sección), la
    /// última de sus líneas y las líneas de sus tareas.
    #[derive(Clone, Debug, PartialEq)]
    struct Sec {
        line: Option<usize>,
        last: usize,
        tasks: Vec<usize>,
    }

    fn source(block: &str) -> Vec<Sec> {
        let mut out: Vec<Sec> = Vec::new();
        for (n, l) in body(block) {
            let t = l.trim_start();
            let word = t.split(|c: char| c.is_whitespace() || c == ':').next().unwrap_or("");
            if word == "section" {
                out.push(Sec { line: Some(n), last: n, tasks: Vec::new() });
            } else if KEYWORDS.contains(&word) || !t.contains(':') {
                continue;
            } else {
                if out.is_empty() {
                    out.push(Sec { line: None, last: n, tasks: Vec::new() });
                }
                if let Some(s) = out.last_mut() {
                    s.tasks.push(n);
                    s.last = n;
                }
            }
        }
        out
    }

    fn checked(block: &str, code: &str) -> Option<Vec<Sec>> {
        let src: Vec<Sec> = source(block).into_iter().filter(|s| s.line.is_some() || !s.tasks.is_empty()).collect();
        let tree = sections(code);
        let same = src.len() == tree.len() && src.iter().zip(&tree).all(|(s, (_, t))| s.tasks.len() == t.len());
        same.then_some(src)
    }

    fn name(s: &str) -> Option<String> {
        clean(&s.replace([':', '#'], " "))
    }

    /// Una duración (`5d`, `2w`, `3h`): un número y una unidad.
    fn duration(s: &str) -> Option<String> {
        let t = s.trim().to_ascii_lowercase();
        let unit = t.chars().last()?;
        let num = t.get(..t.len() - unit.len_utf8())?;
        let ok = matches!(unit, 'd' | 'w' | 'h' | 'm') && num.parse::<f64>().is_ok_and(|v| v.is_finite() && v > 0.0);
        ok.then_some(t)
    }

    fn is_duration(s: &str) -> bool {
        duration(s).is_some()
    }

    /// Agrega una sección vacía al final.
    pub fn add_section(block: &str, code: &str, title: &str) -> Option<String> {
        if !is_gantt(code) {
            return None;
        }
        let title = name(title)?;
        let indent = body_indent_after(block, "gantt");
        let last = source(block).last().map(|s| s.last);
        Some(insert_at(block, last.map(|n| n + 1), &format!("{indent}section {title}")))
    }

    /// Agrega una tarea al final de la sección `s` (empieza después de la anterior; dura un día).
    pub fn add_task(block: &str, code: &str, s: usize, task: &str) -> Option<String> {
        let src = checked(block, code)?;
        let sec = src.get(s)?;
        let task = name(task)?;
        let indent = sec
            .tasks
            .first()
            .and_then(|n| block.lines().nth(*n))
            .map(|l| indent_of(l).to_string())
            .unwrap_or_else(|| body_indent_after(block, "gantt"));
        Some(insert_at(block, Some(sec.last + 1), &format!("{indent}{task} :1d")))
    }

    pub fn rename_section(block: &str, code: &str, s: usize, title: &str) -> Option<String> {
        let src = checked(block, code)?;
        let n = src.get(s)?.line?;
        let line = block.lines().nth(n)?;
        replace_line(block, n, Some(format!("{}section {}", indent_of(line), name(title)?)))
    }

    pub fn delete_section(block: &str, code: &str, s: usize) -> Option<String> {
        let src = checked(block, code)?;
        let sec = src.get(s)?;
        let first = sec.line.or(sec.tasks.first().copied())?;
        let mut out = block.to_string();
        for n in (first..=sec.last).rev() {
            out = replace_line(&out, n, None)?;
        }
        Some(out)
    }

    fn task_line(block: &str, code: &str, s: usize, t: usize) -> Option<usize> {
        checked(block, code)?.get(s)?.tasks.get(t).copied()
    }

    pub fn rename_task(block: &str, code: &str, s: usize, t: usize, text: &str) -> Option<String> {
        let n = task_line(block, code, s, t)?;
        let line = block.lines().nth(n)?;
        let colon = line.find(':')?;
        replace_line(block, n, Some(format!("{}{} {}", indent_of(line), name(text)?, line.get(colon..)?)))
    }

    /// La duración actual de la tarea (su último dato, si es una duración).
    pub fn task_duration(block: &str, code: &str, s: usize, t: usize) -> Option<String> {
        let n = task_line(block, code, s, t)?;
        let line = block.lines().nth(n)?;
        let meta = line.get(line.find(':')? + 1..)?;
        meta.split(',').next_back().map(str::trim).filter(|d| is_duration(d)).map(str::to_string)
    }

    /// Cambia la duración (`5d`): el último dato si es una duración o una fecha de fin; si no, se
    /// agrega.
    pub fn set_duration(block: &str, code: &str, s: usize, t: usize, text: &str) -> Option<String> {
        let d = duration(text)?;
        let n = task_line(block, code, s, t)?;
        let line = block.lines().nth(n)?;
        let colon = line.find(':')?;
        let meta = line.get(colon + 1..)?;
        let mut parts: Vec<String> = meta.split(',').map(|p| p.trim().to_string()).collect();
        let last_is_end = parts.last().is_some_and(|p| {
            is_duration(p) || (parts.len() >= 2 && p.chars().next().is_some_and(|c| c.is_ascii_digit()))
        });
        if last_is_end {
            if let Some(p) = parts.last_mut() {
                *p = d;
            }
        } else {
            parts.push(d);
        }
        let parts: Vec<String> = parts.into_iter().filter(|p| !p.is_empty()).collect();
        replace_line(block, n, Some(format!("{}{}", line.get(..colon + 1)?, parts.join(", "))))
    }

    pub fn delete_task(block: &str, code: &str, s: usize, t: usize) -> Option<String> {
        let n = task_line(block, code, s, t)?;
        replace_line(block, n, None)
    }

    // --- Arrastrar una barra (cambiar su inicio) ---

    /// Días desde 1970-01-01 de una fecha civil (algoritmo de H. Hinnant).
    fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
        let y = if m <= 2 { y - 1 } else { y };
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let mp = (m + 9) % 12;
        let doy = (153 * mp + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    }

    fn civil_from_days(z: i64) -> (i64, i64, i64) {
        let z = z + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        (if m <= 2 { yoe + era * 400 + 1 } else { yoe + era * 400 }, m, d)
    }

    /// `AAAA-MM-DD` en días (solo fechas válidas de 1900 a 2200).
    pub(crate) fn parse_date(s: &str) -> Option<i64> {
        let s = s.trim();
        let mut it = s.split('-');
        let (y, m, d) =
            (it.next()?.parse::<i64>().ok()?, it.next()?.parse::<i64>().ok()?, it.next()?.parse::<i64>().ok()?);
        let ok = it.next().is_none()
            && s.len() == 10
            && (1900..=2200).contains(&y)
            && (1..=12).contains(&m)
            && (1..=31).contains(&d);
        let days = ok.then(|| days_from_civil(y, m, d))?;
        (civil_from_days(days) == (y, m, d)).then_some(days)
    }

    pub(crate) fn format_date(days: i64) -> String {
        let (y, m, d) = civil_from_days(days);
        format!("{y:04}-{m:02}-{d:02}")
    }

    /// ¿Se pueden arrastrar las barras? Solo con fechas `YYYY-MM-DD` (el formato por omisión).
    pub fn draggable(code: &str) -> bool {
        match mermaid_svg::parse(code) {
            Ok(mermaid_svg::Diagram::Gantt(g)) => g.date_format.as_deref().is_none_or(|f| f.trim() == "YYYY-MM-DD"),
            _ => false,
        }
    }

    /// La escala del eje: (x del primer día marcado, ese día, píxeles por día), leída de las
    /// fechas `AAAA-MM-DD` del eje.
    pub fn scale(svg: &str) -> Option<(f32, i64, f32)> {
        let ticks: Vec<(f32, i64)> = elements(svg)
            .into_iter()
            .filter_map(|e| match e {
                El::Text { x, text, middle: false, .. } => parse_date(&text).map(|d| (x - 2.0, d)),
                _ => None,
            })
            .take(400)
            .collect();
        let (a, b) = (ticks.first()?, ticks.iter().find(|t| t.1 != ticks.first().map_or(0, |f| f.1))?);
        let ppd = (b.0 - a.0) / (b.1 - a.1) as f32;
        (ppd.is_finite() && ppd > 0.0).then_some((a.0, a.1, ppd))
    }

    /// Las barras (sin su nombre) de las tareas, en orden.
    pub fn bars(svg: &str, code: &str) -> Vec<Option<[f32; 4]>> {
        let els = elements(svg);
        let mut cursor = 0;
        sections(code)
            .into_iter()
            .flat_map(|(_, ts)| ts)
            .map(|t| {
                let (i, _) = text_with_shape(&els, cursor, &norm(&t), true)?;
                cursor = i + 1;
                match els.get(i.checked_sub(1)?)? {
                    El::Shape { tag, rect } if tag.starts_with("<rect") => Some(*rect),
                    _ => None,
                }
            })
            .collect()
    }

    /// Pone el inicio de la tarea en `start` (días): cambia su fecha de inicio o su `after x` por la
    /// fecha; si no tenía inicio, se la agrega.
    pub fn set_start(block: &str, code: &str, s: usize, t: usize, start: i64) -> Option<String> {
        if !draggable(code) {
            return None;
        }
        let n = task_line(block, code, s, t)?;
        let line = block.lines().nth(n)?;
        let colon = line.find(':')?;
        let meta = line.get(colon + 1..)?;
        let lead = meta.get(..meta.len() - meta.trim_start().len())?;
        let mut parts: Vec<String> = meta.split(',').map(|p| p.trim().to_string()).collect();
        let date = format_date(start);
        let at = parts.iter().position(|p| parse_date(p).is_some() || p.starts_with("after "));
        match at {
            // Una fecha que es el fin (la última, con una fecha de inicio antes) no se toca.
            Some(i) => *parts.get_mut(i)? = date,
            None => {
                let i = parts.len().saturating_sub(1);
                parts.insert(i, date);
            }
        }
        replace_line(block, n, Some(format!("{}{lead}{}", line.get(..colon + 1)?, parts.join(", "))))
    }

    /// Zonas: cada sección (su nombre) y cada tarea (su barra con su nombre).
    pub fn boxes(svg: &str, code: &str) -> Vec<(Option<[f32; 4]>, Vec<Option<[f32; 4]>>)> {
        let els = elements(svg);
        // Las tareas: en orden, la barra y su nombre.
        let mut cursor = 0;
        let secs = sections(code);
        let tasks: Vec<Vec<Option<[f32; 4]>>> = secs
            .iter()
            .map(|(_, ts)| {
                ts.iter()
                    .map(|t| {
                        let (i, b) = text_with_shape(&els, cursor, &norm(t), true)?;
                        cursor = i + 1;
                        Some(b)
                    })
                    .collect()
            })
            .collect();
        // Las secciones: su nombre (va después de sus barras), sin figura.
        let mut cursor = 0;
        secs.iter()
            .zip(tasks)
            .map(|((n, _), tb)| {
                let sb = text_with_shape(&els, cursor, &norm(n), false).map(|(i, b)| {
                    cursor = i + 1;
                    b
                });
                (sb, tb)
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

    const PIE: &str = "```mermaid\npie title Casos\n    \"Civil\" : 30\n    \"Penal\" : 20\n```\n";

    #[test]
    fn pie_slices() {
        let c = code(PIE);
        let out = pie::add(PIE, &c, "Laboral 12,5").unwrap();
        assert!(out.contains("    \"Penal\" : 20\n    \"Laboral\" : 12.5\n```"), "{out}");
        assert_eq!(pie::entries(&code(&out)).len(), 3);
        assert!(pie::set_value(PIE, &c, 0, "45").unwrap().contains("    \"Civil\" : 45\n"));
        assert!(pie::rename(PIE, &c, 1, "Penal federal").unwrap().contains("    \"Penal federal\" : 20\n"));
        assert!(!pie::delete(PIE, &c, 0).unwrap().contains("Civil"));
        assert!(pie::add(PIE, &c, "Sin número").is_none());
        let svg = mermaid_svg::render(&c).unwrap();
        assert!(pie::boxes(&svg, &c).iter().all(Option::is_some));
    }

    const TL: &str = "```mermaid\ntimeline\n    title Caso\n    2025 : Demanda\n    2026 : Audiencia : Sentencia\n         : Apelación\n```\n";

    #[test]
    fn timeline_periods_and_events() {
        let c = code(TL);
        assert_eq!(timeline::periods(&c)[1].1.len(), 3);
        let out = timeline::add_event(TL, &c, 0, "Contestación").unwrap();
        assert!(out.contains("    2025 : Demanda : Contestación\n"), "{out}");
        let out = timeline::add_event(TL, &c, 1, "Amparo").unwrap();
        assert!(out.contains("         : Apelación : Amparo\n"), "{out}");
        let out = timeline::rename_event(TL, &c, 1, 1, "Sentencia firme").unwrap();
        assert!(out.contains("    2026 : Audiencia : Sentencia firme\n"), "{out}");
        let out = timeline::delete_event(TL, &c, 1, 2).unwrap();
        assert!(!out.contains("Apelación") && out.contains("Sentencia\n```"), "{out}");
        let out = timeline::delete_period(TL, &c, 1).unwrap();
        assert_eq!(out, "```mermaid\ntimeline\n    title Caso\n    2025 : Demanda\n```\n");
        let out = timeline::add_period(TL, &c, "2027: Ejecución").unwrap();
        assert!(out.contains("         : Apelación\n    2027 : Ejecución\n```"), "{out}");
        let out = timeline::add_period(TL, &c, "2028").unwrap();
        assert_eq!(timeline::periods(&code(&out)).len(), 3, "{out}");
        assert!(timeline::rename_period(TL, &c, 0, "Enero 2025").unwrap().contains("    Enero 2025 : Demanda\n"));
        let svg = mermaid_svg::render(&c).unwrap();
        let b = timeline::boxes(&svg, &c);
        assert!(b.iter().all(|(p, es)| p.is_some() && es.iter().all(Option::is_some)), "{b:?}");
    }

    const KB: &str = "```mermaid\nkanban\n    Pendiente\n        t1[Redactar]\n        t2[Revisar]@{ priority: 'High' }\n    hecho[Hecho]\n        t3[Firmar]\n```\n";

    #[test]
    fn kanban_cards_move_between_columns() {
        let c = code(KB);
        assert_eq!(kanban::columns(&c).len(), 2);
        let out = kanban::move_card(KB, &c, 0, 0, 1).unwrap();
        assert_eq!(
            out,
            "```mermaid\nkanban\n    Pendiente\n        t2[Revisar]@{ priority: 'High' }\n    hecho[Hecho]\n        t3[Firmar]\n        t1[Redactar]\n```\n"
        );
        assert_eq!(kanban::columns(&code(&out))[1].2.len(), 2);
        let out = kanban::add_card(KB, &c, 1, "Archivar").unwrap();
        assert!(out.contains("        t3[Firmar]\n        t4[Archivar]\n```"), "{out}");
        let out = kanban::add_column(KB, &c, "En revisión").unwrap();
        assert!(out.contains("        t3[Firmar]\n    t4[En revisión]\n```"), "{out}");
        assert!(
            kanban::rename_card(KB, &c, 0, 1, "Revisar todo")
                .unwrap()
                .contains("        t2[Revisar todo]@{ priority: 'High' }\n")
        );
        assert!(kanban::rename_column(KB, &c, 1, "Listo").unwrap().contains("    hecho[Listo]\n"));
        assert!(!kanban::delete_column(KB, &c, 0).unwrap().contains("Redactar"));
        assert!(!kanban::delete_card(KB, &c, 1, 0).unwrap().contains("Firmar"));
        let svg = mermaid_svg::render(&c).unwrap();
        let b = kanban::boxes(&svg, &c);
        assert!(b.iter().all(|(p, ts)| p.is_some() && ts.iter().all(Option::is_some)), "{b:?}");
    }

    const GT: &str = "```mermaid\ngantt\n    title Plan\n    dateFormat YYYY-MM-DD\n    section Demanda\n    Redactar :a1, 2026-01-05, 5d\n    Presentar :after a1, 2d\n    section Juicio\n    Audiencia :2026-02-01, 1d\n```\n";

    #[test]
    fn gantt_sections_and_tasks() {
        let c = code(GT);
        assert_eq!(gantt::sections(&c).len(), 2);
        let out = gantt::add_task(GT, &c, 0, "Notificar").unwrap();
        assert!(out.contains("    Presentar :after a1, 2d\n    Notificar :1d\n    section Juicio"), "{out}");
        assert_eq!(gantt::sections(&code(&out))[0].1.len(), 3);
        assert_eq!(gantt::task_duration(GT, &c, 0, 0).as_deref(), Some("5d"));
        let out = gantt::set_duration(GT, &c, 0, 1, "1w").unwrap();
        assert!(out.contains("    Presentar :after a1, 1w\n"), "{out}");
        assert!(gantt::set_duration(GT, &c, 0, 1, "pronto").is_none());
        assert!(
            gantt::rename_task(GT, &c, 1, 0, "Audiencia inicial")
                .unwrap()
                .contains("    Audiencia inicial :2026-02-01, 1d\n")
        );
        assert!(gantt::rename_section(GT, &c, 1, "Juicio oral").unwrap().contains("    section Juicio oral\n"));
        let out = gantt::delete_section(GT, &c, 1).unwrap();
        assert!(!out.contains("Juicio") && !out.contains("Audiencia"), "{out}");
        assert!(!gantt::delete_task(GT, &c, 0, 0).unwrap().contains("Redactar"));
        let out = gantt::add_section(GT, &c, "Ejecución").unwrap();
        assert!(out.contains("    Audiencia :2026-02-01, 1d\n    section Ejecución\n```"), "{out}");
        // Y ya se le pueden agregar tareas.
        let out = gantt::add_task(&out, &code(&out), 2, "Embargo").unwrap();
        assert!(out.contains("    section Ejecución\n    Embargo :1d\n```"), "{out}");
        let svg = mermaid_svg::render(&c).unwrap();
        let b = gantt::boxes(&svg, &c);
        assert!(b.iter().all(|(s, ts)| s.is_some() && ts.iter().all(Option::is_some)), "{b:?}");
    }

    #[test]
    fn gantt_bars_move_by_days() {
        assert_eq!(gantt::format_date(gantt::parse_date("2026-02-28").unwrap() + 1), "2026-03-01");
        assert_eq!(gantt::format_date(gantt::parse_date("2028-02-28").unwrap() + 1), "2028-02-29");
        assert!(gantt::parse_date("2026-02-30").is_none());
        let c = code(GT);
        let svg = mermaid_svg::render(&c).unwrap();
        let (x0, d0, ppd) = gantt::scale(&svg).unwrap();
        assert_eq!(gantt::format_date(d0), "2026-01-05");
        // La barra de «Redactar» empieza en su fecha.
        let bar = gantt::bars(&svg, &c)[0].unwrap();
        assert!(((bar[0] - x0) / ppd).abs() < 0.5, "{bar:?} {x0} {ppd}");
        // Con fecha: se cambia; con `after`: se vuelve fecha; sin inicio: se agrega.
        let out = gantt::set_start(GT, &c, 0, 0, d0 + 3).unwrap();
        assert!(out.contains("    Redactar :a1, 2026-01-08, 5d\n"), "{out}");
        let out = gantt::set_start(GT, &c, 0, 1, d0 + 10).unwrap();
        assert!(out.contains("    Presentar :2026-01-15, 2d\n"), "{out}");
        let g = "```mermaid\ngantt\n    section S\n    A :2026-01-01, 1d\n    B :3d\n```\n";
        let out = gantt::set_start(g, &code(g), 0, 1, d0).unwrap();
        assert!(out.contains("    B :2026-01-05, 3d\n"), "{out}");
        // Otro formato de fecha: no se arrastra.
        let h = "```mermaid\ngantt\n    dateFormat DD-MM-YYYY\n    section S\n    A :01-01-2026, 1d\n```\n";
        assert!(gantt::set_start(h, &code(h), 0, 0, d0).is_none());
    }

    proptest! {
        #[test]
        fn never_panics(text in ".{0,30}", junk in ".{0,120}", k in 0usize..4, j in 0usize..4) {
            for (head, b) in [("pie", PIE), ("timeline", TL), ("kanban", KB), ("gantt", GT)] {
                let block = format!("```mermaid\n{head}\n    {junk}\n```\n");
                let c = format!("{head}\n    {junk}");
                let _ = pie::add(&block, &c, &text);
                let _ = pie::set_value(&block, &c, k, &text);
                let _ = timeline::add_event(&block, &c, k, &text);
                let _ = timeline::delete_event(&block, &c, k, j);
                let _ = kanban::move_card(&block, &c, k, j, k + 1);
                let _ = kanban::add_card(&block, &c, k, &text);
                let _ = gantt::set_duration(&block, &c, k, j, &text);
                let _ = gantt::add_task(&block, &c, k, &text);
                let _ = gantt::delete_section(&block, &c, k);
                let _ = gantt::boxes(&junk, &c);
                // Sobre los diagramas válidos, lo agregado se vuelve a leer.
                let c = code(b);
                if let Some(out) = kanban::add_card(b, &c, 0, &text) {
                    prop_assert_eq!(kanban::columns(&code(&out))[0].2.len(), 3);
                }
                if let Some(out) = gantt::add_task(b, &c, 1, &text) {
                    prop_assert_eq!(gantt::sections(&code(&out))[1].1.len(), 2);
                }
            }
        }
    }
}
