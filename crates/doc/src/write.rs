//! Escritura de los bloques cambiados, con un estilo fijo: `-` para viñetas, `1.` numeradas,
//! `**negrita**`, `*cursiva*`, `~~tachado~~`, cercas ```` ``` ```` y tablas con `|`.

use crate::{Align, Block, Inline, Item, Meta, Style};

/// El comentario de metadatos: `<!-- iure:kind {"a":"b"} -->`.
pub fn meta(m: &Meta) -> String {
    let obj: serde_json::Map<String, serde_json::Value> =
        m.attrs.iter().map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone()))).collect();
    let json = serde_json::Value::Object(obj).to_string().replace("--", "\\u002d\\u002d");
    format!("<!-- iure:{} {json} -->", m.kind)
}

/// Markdown de un bloque, sin el salto de línea final.
pub fn block(b: &Block) -> String {
    block_alt(b, false)
}

/// ¿`b` es una lista que, pegada a `prev`, se juntaría con ella? (mismo tipo de lista).
pub fn merges_with(prev: Option<&Block>, b: &Block) -> bool {
    matches!((prev, b), (Some(Block::List { ordered: a, .. }), Block::List { ordered: c, .. }) if a == c)
}

/// Como [`block`]; con `alt`, una lista usa el otro marcador (`*` o `1)`) para no juntarse con la
/// lista anterior.
pub fn block_alt(b: &Block, alt: bool) -> String {
    match b {
        // Sin espacios al final de línea: los saltos forzados se escriben con `\`.
        Block::Paragraph(c) => {
            inlines(c, false).lines().map(|l| l.trim_end_matches([' ', '\t'])).collect::<Vec<_>>().join("\n")
        }
        Block::Heading { level, content } => {
            // Un título es una sola línea: los saltos pasan a ser espacios antes de escapar.
            let mut text = guard_line_start(inlines(&one_line(content), false).trim_matches([' ', '\t']));
            // Un `#` al final cerraría el título y se perdería.
            if text.ends_with('#') && !text.ends_with("\\#") {
                text.pop();
                text.push_str("\\#");
            }
            format!("{} {text}", "#".repeat(usize::from((*level).clamp(1, 6))))
                .trim_end_matches([' ', '\t'])
                .to_string()
        }
        Block::List { ordered, start, tight, items } => list(*ordered, *start, *tight, items, alt),
        // Una cita vacía sigue siendo una cita (sin nada, las listas de alrededor se juntarían).
        Block::Quote(blocks) if blocks.is_empty() => ">".to_string(),
        Block::Quote(blocks) => quoted(&blocks_md(blocks)),
        Block::Callout { kind, meta, title, blocks } => {
            let mut head = format!("[!{kind}");
            if !meta.trim().is_empty() {
                head.push('|');
                head.push_str(meta.trim());
            }
            head.push(']');
            if !title.trim().is_empty() {
                head.push(' ');
                head.push_str(title.trim());
            }
            let body = blocks_md(blocks);
            let all = if body.is_empty() { head } else { format!("{head}\n{body}") };
            quoted(&all)
        }
        Block::Code { lang, text } => {
            // Una cerca más larga que cualquier racha de comillas del código.
            let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
            let fence = "`".repeat(longest.max(2) + 1);
            let body = text.strip_suffix('\n').unwrap_or(text);
            // El lenguaje admite escapes con `\\`: se escapan `\\` y la comilla para leerlo igual.
            let lang = lang.replace('\\', "\\\\").replace('`', "\\`").replace(['\n', '\r'], " ");
            format!("{fence}{lang}\n{body}\n{fence}")
        }
        Block::Math(m) => format!("$$\n{}\n$$", m.trim()),
        Block::Table { align, head, rows } => table(align, head, rows),
        Block::Rule => "---".to_string(),
        Block::FrontMatter(y) => format!("---\n{}\n---", y.trim_end()),
        Block::Raw(r) => r.clone(),
    }
}

/// El contenido con los saltos de línea cambiados por espacios.
fn one_line(c: &[Inline]) -> Vec<Inline> {
    c.iter()
        .map(|i| match i {
            Inline::SoftBreak | Inline::Break => Inline::text(" "),
            // Ni el texto, ni el código, ni una fórmula pueden partir la línea.
            Inline::Text { text, style } => Inline::Text { text: text.replace(['\n', '\r'], " "), style: *style },
            Inline::Code { text, style } => Inline::Code { text: text.replace(['\n', '\r'], " "), style: *style },
            Inline::Math(m) => Inline::Math(m.replace(['\n', '\r'], " ")),
            Inline::Html(h) => Inline::Html(h.replace(['\n', '\r'], " ")),
            other => other.clone(),
        })
        .collect()
}

/// Bloques uno tras otro, separados por una línea en blanco.
fn blocks_md(blocks: &[Block]) -> String {
    blocks_joined(blocks, "\n\n")
}

/// Bloques con `sep` entre ellos; dos listas seguidas del mismo tipo alternan el marcador.
fn blocks_joined(blocks: &[Block], sep: &str) -> String {
    let mut out = Vec::with_capacity(blocks.len());
    let mut alt = false;
    let mut prev: Option<&Block> = None;
    for b in blocks {
        alt = merges_with(prev, b) && !alt;
        out.push(block_alt(b, alt));
        prev = Some(b);
    }
    out.join(sep)
}

/// Cada línea con `> ` (las vacías con `>`).
fn quoted(s: &str) -> String {
    s.lines().map(|l| if l.is_empty() { ">".to_string() } else { format!("> {l}") }).collect::<Vec<_>>().join("\n")
}

fn list(ordered: bool, start: u64, tight: bool, items: &[Item], alt: bool) -> String {
    let mut out = Vec::new();
    for (n, item) in items.iter().enumerate() {
        let marker = match (ordered, alt) {
            (true, false) => format!("{}.", start.saturating_add(n as u64)),
            (true, true) => format!("{})", start.saturating_add(n as u64)),
            (false, false) => "-".to_string(),
            (false, true) => "*".to_string(),
        };
        let check = match item.checked {
            Some(true) => "[x] ",
            Some(false) => "[ ] ",
            None => "",
        };
        let indent = " ".repeat(marker.len() + 1);
        let body = if tight { blocks_joined(&item.blocks, "\n") } else { blocks_md(&item.blocks) };
        // Una lista que abre el elemento va en la línea siguiente: `- - -` sería un separador.
        let nested_first = matches!(item.blocks.first(), Some(Block::List { .. }));
        let mut lines = body.lines();
        let first = if nested_first { "" } else { lines.next().unwrap_or("") };
        let mut s = format!("{marker} {check}{first}").trim_end_matches([' ', '\t']).to_string();
        for l in lines {
            s.push('\n');
            if !l.trim_matches([' ', '\t']).is_empty() {
                s.push_str(&indent);
                s.push_str(l.trim_end_matches([' ', '\t']));
            }
        }
        out.push(s);
    }
    out.join(if tight { "\n" } else { "\n\n" })
}

fn table(align: &[Align], head: &[Vec<Inline>], rows: &[Vec<Vec<Inline>>]) -> String {
    let cols = align.len().max(head.len()).max(rows.iter().map(Vec::len).max().unwrap_or(0)).max(1);
    let cell = |c: Option<&Vec<Inline>>| c.map(|c| inlines(&one_line(c), true)).unwrap_or_default();
    let row = |r: &[Vec<Inline>]| {
        let cells: Vec<String> = (0..cols).map(|i| cell(r.get(i))).collect();
        format!("| {} |", cells.join(" | "))
    };
    let sep: Vec<&str> = (0..cols)
        .map(|i| match align.get(i).copied().unwrap_or_default() {
            Align::None => "---",
            Align::Left => ":---",
            Align::Center => ":---:",
            Align::Right => "---:",
        })
        .collect();
    let mut out = vec![row(head), format!("| {} |", sep.join(" | "))];
    out.extend(rows.iter().map(|r| row(r)));
    out.join("\n")
}

/// Texto con las marcas de Markdown escapadas.
fn escape(s: &str, in_table: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        // `|` también fuera de las tablas: unas líneas con `|` podrían leerse como tabla.
        let _ = in_table;
        if matches!(c, '\\' | '*' | '_' | '`' | '[' | ']' | '<' | '>' | '~' | '$' | '|') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Markdown del contenido en línea.
pub fn inlines(c: &[Inline], in_table: bool) -> String {
    let mut out = String::new();
    let mut i = 0;
    while let Some(item) = c.get(i) {
        // Tramos seguidos con el mismo estilo van bajo una sola marca: `*a b*`, no `*a* *b*`
        // (las marcas pegadas, `**`, se leerían como negrita).
        if let Some(st) = style_of(item).filter(|s| *s != Style::default()) {
            let mut inner = String::new();
            while let Some(next) = c.get(i).filter(|n| style_of(n) == Some(st)) {
                inner.push_str(&piece(next, in_table));
                i += 1;
            }
            out.push_str(&styled(&inner, st));
            continue;
        }
        out.push_str(&piece(item, in_table));
        i += 1;
    }
    out.split('\n').map(guard_line_start).collect::<Vec<_>>().join("\n")
}

/// Estilo de un tramo de texto o código (lo demás no lleva estilo propio).
fn style_of(i: &Inline) -> Option<Style> {
    match i {
        Inline::Text { style, .. } | Inline::Code { style, .. } => Some(*style),
        _ => None,
    }
}

/// Markdown de un elemento sin su estilo (el estilo lo pone quien agrupa).
fn piece(i: &Inline, in_table: bool) -> String {
    match i {
        Inline::Text { text, .. } => escape(text, in_table),
        Inline::Code { text: code, .. } => {
            // Delimitador más largo que cualquier racha de comillas del código; con espacio si el
            // código empieza o termina en comilla o espacio (CommonMark quita uno de cada lado).
            let longest = code.split(|c| c != '`').map(str::len).max().unwrap_or(0);
            let ticks = "`".repeat(longest + 1);
            let all_spaces = code.chars().all(|c| c == ' ');
            let edge = code.starts_with(['`', ' ']) || code.ends_with(['`', ' ']);
            let pad = if edge && !all_spaces { " " } else { "" };
            format!("{ticks}{pad}{code}{pad}{ticks}")
        }
        Inline::Link { url, title, content } => {
            let t = if title.is_empty() { String::new() } else { format!(" \"{}\"", title.replace('"', "\\\"")) };
            format!("[{}]({}{t})", inlines(content, in_table), url.replace(' ', "%20"))
        }
        Inline::Image { src, alt, title } => {
            let t = if title.is_empty() { String::new() } else { format!(" \"{}\"", title.replace('"', "\\\"")) };
            format!("![{}]({}{t})", escape(alt, in_table), src.replace(' ', "%20"))
        }
        Inline::Math(m) => format!("${m}$"),
        Inline::Break => "\\\n".to_string(),
        Inline::SoftBreak => "\n".to_string(),
        Inline::Html(h) => h.clone(),
    }
}

/// Escapa lo que, al principio de una línea, abriría un bloque: `#`, `-`, `+`, `=` y `1.` / `1)`.
fn guard_line_start(line: &str) -> String {
    let mut out = line.to_string();
    if let Some(first) = out.chars().next()
        && matches!(first, '#' | '-' | '+' | '=')
    {
        out.insert(0, '\\');
        return out;
    }
    let digits = out.chars().take_while(char::is_ascii_digit).count();
    if (1..=9).contains(&digits)
        && let Some(p) = out.get(digits..digits + 1)
        && (p == "." || p == ")")
    {
        out.insert(digits, '\\');
    }
    out
}

fn styled(text: &str, s: Style) -> String {
    // Los espacios de los bordes van fuera de las marcas (`** a**` no es negrita).
    let core = text.trim();
    if core.is_empty() || s == Style::default() {
        return text.to_string();
    }
    let lead = text.get(..text.len() - text.trim_start().len()).unwrap_or("");
    let tail = text.get(text.trim_end().len()..).unwrap_or("");
    let mut m = core.to_string();
    if s.italic {
        m = format!("*{m}*");
    }
    if s.bold {
        m = format!("**{m}**");
    }
    if s.strike {
        m = format!("~~{m}~~");
    }
    format!("{lead}{m}{tail}")
}
