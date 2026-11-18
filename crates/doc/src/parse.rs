//! Lectura: Markdown → bloques de primer nivel con su texto original.

use std::collections::BTreeMap;
use std::ops::Range;

use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use crate::{Align, Block, Document, Inline, Item, MAX_BLOCKS, MAX_BYTES, MAX_DEPTH, Meta, Node, Style};

pub(crate) fn options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_MATH
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
}

/// Rangos (en bytes) de los bloques de primer nivel, en orden.
fn top_level(md: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (ev, r) in Parser::new_ext(md, options()).into_offset_iter() {
        match ev {
            Event::Start(_) => {
                if depth == 0 {
                    start = r.start;
                }
                depth += 1;
            }
            Event::End(_) => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    out.push(start..r.end.max(start));
                }
            }
            // Separador, HTML suelto u otro evento sin contenedor en el primer nivel.
            Event::Rule | Event::Html(_) | Event::DisplayMath(_) | Event::Text(_) if depth == 0 => {
                out.push(r);
            }
            _ => {}
        }
    }
    out
}

/// Trozos de primer nivel que cubren el texto entero, en orden y sin huecos (ver [`crate::chunks`]).
pub(crate) fn chunks(md: &str) -> Vec<Range<usize>> {
    if md.is_empty() {
        return Vec::new();
    }
    if md.len() > MAX_BYTES {
        return std::iter::once(0..md.len()).collect();
    }
    // El primero empieza en 0: lleva lo que va antes del primer bloque.
    let mut starts = vec![0usize];
    for r in top_level(md).into_iter().skip(1) {
        let last = starts.last().copied().unwrap_or(0);
        if r.start > last && md.is_char_boundary(r.start) && starts.len() < MAX_BLOCKS {
            starts.push(r.start);
        }
    }
    starts.iter().enumerate().map(|(i, &s)| s..starts.get(i + 1).copied().unwrap_or(md.len())).collect()
}

pub(crate) fn parse(md: &str) -> Document {
    if md.len() > MAX_BYTES {
        return Document { prefix: String::new(), nodes: vec![raw_node(md)] };
    }
    let ranges = top_level(md);
    let Some(first) = ranges.first() else {
        return Document { prefix: md.to_string(), nodes: Vec::new() };
    };
    let prefix = md.get(..first.start).unwrap_or("").to_string();
    let mut nodes: Vec<Node> = Vec::with_capacity(ranges.len());
    let mut pending_meta: Option<(Meta, usize)> = None;
    for (i, r) in ranges.iter().enumerate() {
        if nodes.len() >= MAX_BLOCKS {
            // Lo que sobra se pega al último bloque como texto: nada se pierde.
            if let Some(last) = nodes.last_mut()
                && let Some(src) = last.source.as_mut()
            {
                src.push_str(md.get(r.start..).unwrap_or(""));
                last.block = Block::Raw(src.trim_end().to_string());
            }
            break;
        }
        let next = ranges.get(i + 1).map_or(md.len(), |n| n.start);
        let slice_start = pending_meta.as_ref().map_or(r.start, |(_, s)| *s);
        let source = md.get(slice_start..next).unwrap_or("").to_string();
        let content = md.get(r.start..r.end.min(next)).unwrap_or("");
        // Un comentario `<!-- iure:… -->` es el metadato del bloque siguiente.
        if let Some(meta) = meta_comment(content)
            && i + 1 < ranges.len()
            && pending_meta.is_none()
        {
            pending_meta = Some((meta, r.start));
            continue;
        }
        let trimmed = source.trim_end_matches(['\n', '\r', ' ', '\t']);
        let trailing = source.get(trimmed.len()..).unwrap_or("").to_string();
        let block = to_block(content);
        nodes.push(Node { block, meta: pending_meta.take().map(|(m, _)| m), source: Some(source), trailing });
    }
    Document { prefix, nodes }
}

fn raw_node(md: &str) -> Node {
    Node {
        block: Block::Raw(md.trim_end().to_string()),
        meta: None,
        source: Some(md.to_string()),
        trailing: String::new(),
    }
}

/// `<!-- iure:kind {"a":"b"} -->` → metadatos.
fn meta_comment(s: &str) -> Option<Meta> {
    let inner = s.trim().strip_prefix("<!--")?.strip_suffix("-->")?.trim();
    let rest = inner.strip_prefix("iure:")?;
    let (kind, json) = rest.split_once(char::is_whitespace).unwrap_or((rest, "{}"));
    if !Meta::valid_kind(kind) {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(json.trim()).ok()?;
    let obj = v.as_object()?;
    let mut attrs = BTreeMap::new();
    for (k, v) in obj.iter().take(64) {
        let text = match v {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        attrs.insert(k.chars().take(64).collect(), text.chars().take(4_096).collect());
    }
    Some(Meta { kind: kind.to_string(), attrs })
}

/// El bloque de un trozo de primer nivel. Si es demasiado profundo o raro, [`Block::Raw`].
fn to_block(content: &str) -> Block {
    let events: Vec<Event<'_>> = Parser::new_ext(content, options()).collect();
    let mut depth = 0usize;
    let mut max = 0usize;
    for e in &events {
        match e {
            Event::Start(_) => {
                depth += 1;
                max = max.max(depth);
            }
            Event::End(_) => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    if max > MAX_DEPTH {
        return Block::Raw(content.trim_end().to_string());
    }
    let mut it = events.into_iter().peekable();
    let mut blocks = read_blocks(&mut it, None);
    match (blocks.len(), blocks.pop()) {
        (1, Some(b)) => b,
        _ => Block::Raw(content.trim_end().to_string()),
    }
}

type Events<'a> = std::iter::Peekable<std::vec::IntoIter<Event<'a>>>;

/// Bloques hasta el fin de `until` (o del todo).
fn read_blocks(it: &mut Events<'_>, until: Option<&TagEnd>) -> Vec<Block> {
    let mut out = Vec::new();
    while let Some(ev) = it.next() {
        match ev {
            Event::End(e) if Some(&e) == until => break,
            // En un elemento de lista apretado, el texto puede empezar con cursiva, enlace…
            Event::Start(
                tag @ (Tag::Emphasis | Tag::Strong | Tag::Strikethrough | Tag::Link { .. } | Tag::Image { .. }),
            ) => {
                let mut content = Vec::new();
                read_inline_tag(it, tag, Style::default(), &mut content);
                content.extend(read_inlines_loose(it));
                out.push(Block::Paragraph(content));
            }
            Event::Start(tag) => {
                if let Some(b) = read_block(it, tag) {
                    out.push(b);
                }
            }
            Event::Rule => out.push(Block::Rule),
            Event::DisplayMath(m) => out.push(Block::Math(m.to_string())),
            Event::Html(h) => out.push(Block::Raw(h.trim_end().to_string())),
            // Texto suelto en una lista «apretada»: es un párrafo.
            Event::Text(_)
            | Event::Code(_)
            | Event::InlineMath(_)
            | Event::InlineHtml(_)
            | Event::SoftBreak
            | Event::HardBreak => {
                let mut content = Vec::new();
                push_inline(&mut content, ev, Style::default());
                content.extend(read_inlines_loose(it));
                out.push(Block::Paragraph(content));
            }
            _ => {}
        }
    }
    out
}

/// Contenido en línea suelto (en elementos de lista apretados) hasta el próximo bloque.
fn read_inlines_loose(it: &mut Events<'_>) -> Vec<Inline> {
    let mut out = Vec::new();
    while let Some(ev) = it.peek() {
        let inline = matches!(
            ev,
            Event::Text(_)
                | Event::Code(_)
                | Event::InlineMath(_)
                | Event::SoftBreak
                | Event::HardBreak
                | Event::InlineHtml(_)
                | Event::Start(Tag::Emphasis | Tag::Strong | Tag::Strikethrough | Tag::Link { .. } | Tag::Image { .. })
        );
        if !inline {
            break;
        }
        let Some(ev) = it.next() else { break };
        match ev {
            Event::Start(tag) => read_inline_tag(it, tag, Style::default(), &mut out),
            other => push_inline(&mut out, other, Style::default()),
        }
    }
    out
}

fn read_block(it: &mut Events<'_>, tag: Tag<'_>) -> Option<Block> {
    Some(match tag {
        Tag::Paragraph => {
            // `$$…$$` solo en su párrafo es una fórmula en bloque.
            if let Some(Event::DisplayMath(_)) = it.peek() {
                let mut look = it.clone();
                if let (Some(Event::DisplayMath(m)), Some(Event::End(TagEnd::Paragraph))) = (look.next(), look.next()) {
                    it.next();
                    it.next();
                    return Some(Block::Math(m.to_string()));
                }
            }
            Block::Paragraph(read_inlines(it, &TagEnd::Paragraph, Style::default()))
        }
        Tag::Heading { level, .. } => {
            let level_n = match level {
                HeadingLevel::H1 => 1,
                HeadingLevel::H2 => 2,
                HeadingLevel::H3 => 3,
                HeadingLevel::H4 => 4,
                HeadingLevel::H5 => 5,
                HeadingLevel::H6 => 6,
            };
            Block::Heading { level: level_n, content: read_inlines(it, &TagEnd::Heading(level), Style::default()) }
        }
        Tag::BlockQuote(kind) => {
            let blocks = read_blocks(it, Some(&TagEnd::BlockQuote(kind)));
            callout(&blocks).unwrap_or(Block::Quote(blocks))
        }
        Tag::CodeBlock(kind) => {
            let lang = match kind {
                CodeBlockKind::Fenced(l) => l.trim().to_string(),
                CodeBlockKind::Indented => String::new(),
            };
            let mut text = String::new();
            for e in it.by_ref() {
                match e {
                    Event::Text(t) => text.push_str(&t),
                    Event::End(TagEnd::CodeBlock) => break,
                    _ => {}
                }
            }
            Block::Code { lang, text }
        }
        Tag::List(start) => {
            let ordered = start.is_some();
            let mut items = Vec::new();
            let mut tight = true;
            while let Some(e) = it.next() {
                match e {
                    Event::End(TagEnd::List(_)) => break,
                    Event::Start(Tag::Item) => {
                        let mut checked = None;
                        if let Some(Event::TaskListMarker(c)) = it.peek() {
                            checked = Some(*c);
                            it.next();
                        }
                        // En una lista «suelta» cada elemento empieza con un párrafo.
                        if matches!(it.peek(), Some(Event::Start(Tag::Paragraph))) {
                            tight = false;
                        }
                        items.push(Item { checked, blocks: read_blocks(it, Some(&TagEnd::Item)) });
                    }
                    _ => {}
                }
            }
            Block::List { ordered, start: start.unwrap_or(1), tight, items }
        }
        Tag::Table(aligns) => {
            let align = aligns
                .iter()
                .map(|a| match a {
                    Alignment::None => Align::None,
                    Alignment::Left => Align::Left,
                    Alignment::Center => Align::Center,
                    Alignment::Right => Align::Right,
                })
                .collect();
            let mut head = Vec::new();
            let mut rows = Vec::new();
            let mut row: Vec<Vec<Inline>> = Vec::new();
            while let Some(e) = it.next() {
                match e {
                    Event::End(TagEnd::Table) => break,
                    Event::Start(Tag::TableCell) => row.push(read_inlines(it, &TagEnd::TableCell, Style::default())),
                    Event::End(TagEnd::TableHead) => head = std::mem::take(&mut row),
                    Event::End(TagEnd::TableRow) => rows.push(std::mem::take(&mut row)),
                    _ => {}
                }
            }
            Block::Table { align, head, rows }
        }
        Tag::MetadataBlock(_) => {
            let mut text = String::new();
            for e in it.by_ref() {
                match e {
                    Event::Text(t) => text.push_str(&t),
                    Event::End(TagEnd::MetadataBlock(_)) => break,
                    _ => {}
                }
            }
            Block::FrontMatter(text)
        }
        // HTML en bloque (también dentro de una lista o una cita): tal cual.
        Tag::HtmlBlock => {
            let mut text = String::new();
            for e in it.by_ref() {
                match e {
                    Event::Html(h) | Event::Text(h) => text.push_str(&h),
                    Event::End(TagEnd::HtmlBlock) => break,
                    _ => {}
                }
            }
            Block::Raw(text.trim_end().to_string())
        }
        // Notas al pie, definiciones…: se conservan tal cual (ver `to_block`).
        _ => {
            skip_to_end(it);
            return None;
        }
    })
}

fn skip_to_end(it: &mut Events<'_>) {
    let mut depth = 1usize;
    for e in it.by_ref() {
        match e {
            Event::Start(_) => depth += 1,
            Event::End(_) => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
    }
}

/// `> [!KIND|meta] título` + cuerpo → [`Block::Callout`].
fn callout(blocks: &[Block]) -> Option<Block> {
    let Some(Block::Paragraph(first)) = blocks.first() else { return None };
    // La primera línea (hasta el primer salto) en texto plano.
    let mut line = String::new();
    let mut rest_at = first.len();
    for (i, inl) in first.iter().enumerate() {
        match inl {
            Inline::SoftBreak | Inline::Break => {
                rest_at = i + 1;
                break;
            }
            Inline::Text { text, .. } => line.push_str(text),
            Inline::Code { text, .. } => line.push_str(text),
            _ => return None,
        }
    }
    let marker = line.trim_start().strip_prefix("[!")?;
    let close = marker.find(']')?;
    let inner = marker.get(..close)?;
    let (kind, meta) = inner.split_once('|').unwrap_or((inner, ""));
    if kind.is_empty() || !kind.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return None;
    }
    let title = marker.get(close + 1..).unwrap_or("").trim().to_string();
    let mut body = Vec::new();
    let tail: Vec<Inline> = first.get(rest_at..).unwrap_or_default().to_vec();
    if !tail.is_empty() {
        body.push(Block::Paragraph(tail));
    }
    body.extend(blocks.get(1..).unwrap_or_default().iter().cloned());
    Some(Block::Callout { kind: kind.to_string(), meta: meta.trim().to_string(), title, blocks: body })
}

fn read_inlines(it: &mut Events<'_>, until: &TagEnd, style: Style) -> Vec<Inline> {
    let mut out = Vec::new();
    while let Some(e) = it.next() {
        match e {
            Event::End(ref end) if end == until => break,
            Event::Start(tag) => read_inline_tag(it, tag, style, &mut out),
            other => push_inline(&mut out, other, style),
        }
    }
    out
}

fn read_inline_tag(it: &mut Events<'_>, tag: Tag<'_>, style: Style, out: &mut Vec<Inline>) {
    match tag {
        Tag::Emphasis => out.extend(read_inlines(it, &TagEnd::Emphasis, Style { italic: true, ..style })),
        Tag::Strong => out.extend(read_inlines(it, &TagEnd::Strong, Style { bold: true, ..style })),
        Tag::Strikethrough => out.extend(read_inlines(it, &TagEnd::Strikethrough, Style { strike: true, ..style })),
        Tag::Link { dest_url, title, .. } => {
            let content = read_inlines(it, &TagEnd::Link, style);
            out.push(Inline::Link { url: dest_url.to_string(), title: title.to_string(), content });
        }
        Tag::Image { dest_url, title, .. } => {
            let alt: String = read_inlines(it, &TagEnd::Image, Style::default())
                .iter()
                .map(|i| match i {
                    Inline::Text { text, .. } => text.as_str(),
                    _ => "",
                })
                .collect();
            out.push(Inline::Image { src: dest_url.to_string(), alt, title: title.to_string() });
        }
        _ => skip_to_end(it),
    }
}

fn push_inline(out: &mut Vec<Inline>, e: Event<'_>, style: Style) {
    match e {
        Event::Text(t) => {
            // Tramos seguidos con el mismo estilo se juntan.
            if let Some(Inline::Text { text, style: s }) = out.last_mut()
                && *s == style
            {
                text.push_str(&t);
            } else {
                out.push(Inline::Text { text: t.to_string(), style });
            }
        }
        Event::Code(c) => out.push(Inline::Code { text: c.to_string(), style }),
        Event::InlineMath(m) => out.push(Inline::Math(m.to_string())),
        Event::DisplayMath(m) => out.push(Inline::Html(format!("$${m}$$"))),
        Event::SoftBreak => out.push(Inline::SoftBreak),
        Event::HardBreak => out.push(Inline::Break),
        Event::InlineHtml(h) | Event::Html(h) => out.push(Inline::Html(h.to_string())),
        Event::FootnoteReference(r) => out.push(Inline::Html(format!("[^{r}]"))),
        _ => {}
    }
}
