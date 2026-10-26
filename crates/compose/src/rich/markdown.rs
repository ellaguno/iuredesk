//! Markdown ↔ [`RichBody`] (CommonMark + tachado `~~x~~`), para pegar documentos de IureEditor y
//! exportar un correo a Markdown. El subrayado, que Markdown no tiene, va como `<u>…</u>` (HTML
//! en línea que CommonMark permite y que la importación reconoce).

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use super::{Block, Builder, FrameKind, Image, Inline, MAX_DEPTH, RichBody, Span, Style};

/// Tamaño máximo del Markdown que se analiza.
const MAX_MD: usize = 8 * 1024 * 1024;

/// Importa Markdown. Nunca falla.
pub(super) fn import(md: &str) -> RichBody {
    let md = super::import_html::clip(md, MAX_MD);
    let mut b = Builder::new();
    let mut code: Option<String> = None;
    let mut image: Option<(String, String)> = None;
    let mut image_depth = 0u32;
    for ev in Parser::new_ext(md, Options::ENABLE_STRIKETHROUGH) {
        // Dentro de una imagen, el texto es su texto alternativo.
        if let Some((_, alt)) = &mut image {
            match ev {
                Event::Start(Tag::Image { .. }) => image_depth = image_depth.saturating_add(1),
                Event::End(TagEnd::Image) if image_depth > 0 => image_depth = image_depth.saturating_sub(1),
                Event::End(TagEnd::Image) => {
                    if let Some((src, alt)) = image.take() {
                        b.image(Image { src, alt, width: None, height: None });
                    }
                }
                Event::Text(t) | Event::Code(t) if alt.len() < 1_000 => alt.push_str(&t),
                _ => {}
            }
            continue;
        }
        if let Some(buf) = &mut code {
            match ev {
                Event::End(TagEnd::CodeBlock) => {
                    let text = std::mem::take(buf);
                    code = None;
                    b.code_block(&text);
                }
                Event::Text(t) if buf.len() < super::MAX_TEXT_BYTES => buf.push_str(&t),
                _ => {}
            }
            continue;
        }
        match ev {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {}
                Tag::Heading { level, .. } => b.heading(heading_level(level)),
                Tag::BlockQuote(_) => b.open(FrameKind::Quote),
                Tag::CodeBlock(_) => {
                    b.flush();
                    code = Some(String::new());
                }
                Tag::List(start) => {
                    let ordered = start.is_some();
                    let start = start.map(|s| u32::try_from(s).unwrap_or(u32::MAX)).unwrap_or(1);
                    b.open(FrameKind::List { ordered, start });
                }
                Tag::Item => b.open(FrameKind::Item),
                Tag::Emphasis => b.style.apply(Style { italic: true, ..Style::default() }, true),
                Tag::Strong => b.style.apply(Style { bold: true, ..Style::default() }, true),
                Tag::Strikethrough => b.style.apply(Style { strike: true, ..Style::default() }, true),
                Tag::Link { dest_url, .. } => b.push_link(Some(dest_url.to_string())),
                Tag::Image { dest_url, .. } => {
                    image = Some((dest_url.to_string(), String::new()));
                    image_depth = 0;
                }
                _ => {}
            },
            Event::End(end) => match end {
                TagEnd::Paragraph | TagEnd::Heading(_) => b.flush(),
                TagEnd::BlockQuote(_) => b.close(FrameKind::Quote),
                TagEnd::List(_) => b.close(FrameKind::List { ordered: false, start: 1 }),
                TagEnd::Item => b.close(FrameKind::Item),
                TagEnd::Emphasis => b.style.apply(Style { italic: true, ..Style::default() }, false),
                TagEnd::Strong => b.style.apply(Style { bold: true, ..Style::default() }, false),
                TagEnd::Strikethrough => b.style.apply(Style { strike: true, ..Style::default() }, false),
                TagEnd::Link => b.pop_link(),
                _ => {}
            },
            Event::Text(t) => b.text(&t, false),
            Event::Code(t) => {
                let code = Style { code: true, ..Style::default() };
                b.style.apply(code, true);
                b.text(&t, false);
                b.style.apply(code, false);
            }
            Event::SoftBreak => b.text(" ", false),
            Event::HardBreak => b.line_break(),
            Event::Rule => b.rule(),
            Event::InlineHtml(h) => match h.trim().to_ascii_lowercase().as_str() {
                "<u>" | "<ins>" => b.style.apply(Style { underline: true, ..Style::default() }, true),
                "</u>" | "</ins>" => b.style.apply(Style { underline: true, ..Style::default() }, false),
                "<br>" | "<br/>" | "<br />" => b.line_break(),
                _ => {}
            },
            _ => {}
        }
    }
    if let Some(text) = code {
        b.code_block(&text);
    }
    b.finish()
}

fn heading_level(l: HeadingLevel) -> u8 {
    match l {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// Exporta a Markdown.
pub(super) fn export(body: &RichBody) -> String {
    let lines = blocks(&body.blocks, 0);
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

fn blocks(bs: &[Block], depth: usize) -> Vec<String> {
    let mut out = Vec::new();
    for (i, b) in bs.iter().enumerate() {
        if i > 0 {
            out.push(String::new());
        }
        out.extend(block(b, depth));
    }
    out
}

fn block(b: &Block, depth: usize) -> Vec<String> {
    let deep = depth >= MAX_DEPTH.saturating_mul(2);
    match b {
        Block::Paragraph { content } => paragraph(content, ""),
        Block::Heading { level, content } => {
            let hashes = "#".repeat(usize::from((*level).clamp(1, 6)));
            let text = inlines(content).replace('\n', " ");
            vec![format!("{hashes} {}", text.trim())]
        }
        Block::List { ordered, start, items } => {
            let mut out = Vec::new();
            for (n, item) in items.iter().enumerate() {
                let marker = if *ordered {
                    format!("{}. ", u64::from(*start).saturating_add(n as u64))
                } else {
                    "- ".to_string()
                };
                let pad = " ".repeat(marker.len());
                let inner = if deep { paragraph(&[Inline::text(text_of(item))], "") } else { blocks(item, depth + 1) };
                for (j, line) in inner.into_iter().enumerate() {
                    if j == 0 {
                        out.push(format!("{marker}{line}"));
                    } else if line.is_empty() {
                        out.push(String::new());
                    } else {
                        out.push(format!("{pad}{line}"));
                    }
                }
            }
            out
        }
        Block::Quote { blocks: inner } => {
            let inner = if deep { paragraph(&[Inline::text(text_of(inner))], "") } else { blocks(inner, depth + 1) };
            inner.into_iter().map(|l| if l.is_empty() { ">".to_string() } else { format!("> {l}") }).collect()
        }
        Block::Code { text } => {
            let mut fence = "```".to_string();
            while text.contains(fence.as_str()) && fence.len() < 64 {
                fence.push('`');
            }
            let mut out = vec![fence.clone()];
            out.extend(text.split('\n').map(str::to_string));
            out.push(fence);
            out
        }
        Block::Rule => vec!["---".to_string()],
        Block::Signature { blocks: inner } => {
            let mut out = vec!["---".to_string(), String::new()];
            out.extend(if deep { paragraph(&[Inline::text(text_of(inner))], "") } else { blocks(inner, depth + 1) });
            out
        }
    }
}

fn text_of(bs: &[Block]) -> String {
    RichBody { blocks: bs.to_vec() }.text_content()
}

/// Un párrafo: los saltos de línea duros terminan en `\`.
fn paragraph(content: &[Inline], _prefix: &str) -> Vec<String> {
    let text = inlines(content);
    let parts: Vec<&str> = text.split('\n').collect();
    let n = parts.len();
    parts
        .into_iter()
        .enumerate()
        .map(|(i, l)| {
            let l = escape_line_start(l);
            if i.saturating_add(1) < n { format!("{l}\\") } else { l }
        })
        .collect()
}

/// Escapa lo que al principio de una línea se leería como bloque (`#`, `-`, `>`, `1.`…).
fn escape_line_start(line: &str) -> String {
    let t = line.trim_start();
    if t.starts_with(['#', '-', '+', '>', '=']) {
        let lead = line.len().saturating_sub(t.len());
        return format!("{}\\{t}", line.get(..lead).unwrap_or(""));
    }
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 && t.get(digits..).is_some_and(|r| r.starts_with('.') || r.starts_with(')')) {
        let lead = line.len().saturating_sub(t.len());
        let (num, rest) = t.split_at(digits);
        return format!("{}{num}\\{rest}", line.get(..lead).unwrap_or(""));
    }
    line.to_string()
}

fn inlines(content: &[Inline]) -> String {
    let mut out = String::new();
    let mut i = 0usize;
    while let Some(item) = content.get(i) {
        match item {
            Inline::Text(span) => {
                if let Some(href) = &span.link {
                    let mut inner = String::new();
                    let mut j = i;
                    while let Some(Inline::Text(s)) = content.get(j) {
                        if s.link.as_ref() != Some(href) {
                            break;
                        }
                        inner.push_str(&styled(s));
                        j = j.saturating_add(1);
                    }
                    out.push('[');
                    out.push_str(&inner);
                    out.push_str("](");
                    out.push_str(&url(href));
                    out.push(')');
                    i = j.max(i.saturating_add(1));
                } else {
                    out.push_str(&styled(span));
                    i = i.saturating_add(1);
                }
            }
            Inline::Image(img) => {
                out.push_str("![");
                out.push_str(&escape(&img.alt));
                out.push_str("](");
                out.push_str(&url(&img.src));
                out.push(')');
                i = i.saturating_add(1);
            }
            Inline::LineBreak => {
                out.push('\n');
                i = i.saturating_add(1);
            }
        }
    }
    out
}

fn url(u: &str) -> String {
    if u.chars().any(|c| c.is_whitespace() || c == '(' || c == ')' || c == '<' || c == '>') {
        format!("<{}>", u.replace('<', "%3C").replace('>', "%3E"))
    } else {
        u.to_string()
    }
}

fn styled(s: &Span) -> String {
    let core = s.text.trim();
    if core.is_empty() {
        return escape(&s.text);
    }
    let lead = s.text.len().saturating_sub(s.text.trim_start().len());
    let trail_start = s.text.trim_end().len();
    let lead_ws = s.text.get(..lead).unwrap_or("");
    let trail_ws = s.text.get(trail_start..).unwrap_or("");
    let Style { bold, italic, underline, strike, code } = s.style;
    let mut inner = if code {
        let ticks = if core.contains('`') { "``" } else { "`" };
        let pad = if core.starts_with('`') || core.ends_with('`') { " " } else { "" };
        format!("{ticks}{pad}{core}{pad}{ticks}")
    } else {
        escape(core)
    };
    if strike {
        inner = format!("~~{inner}~~");
    }
    if italic {
        inner = format!("*{inner}*");
    }
    if bold {
        inner = format!("**{inner}**");
    }
    if underline {
        inner = format!("<u>{inner}</u>");
    }
    format!("{lead_ws}{inner}{trail_ws}")
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '~' | '|' | '!') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "# Minuta\n\nHola **Ana** y *Juan*, ver ~~viejo~~ `código` <u>sub</u> [sitio](https://acme.mx).\\\nOtra línea.\n\n- uno\n- dos\n  1. a\n  2. b\n\n> cita\n>\n> > anidada\n\n```\nfn main() {}\n```\n\n---\n\n![logo](cid:logo@acme)\n";

    #[test]
    fn import_structure() {
        let body = import(DOC);
        let b = &body.blocks;
        assert_eq!(b[0], Block::Heading { level: 1, content: vec![Inline::text("Minuta")] });
        let Block::Paragraph { content } = &b[1] else { panic!("{b:?}") };
        assert!(content.contains(&Inline::styled("Ana", Style { bold: true, ..Default::default() })));
        assert!(content.contains(&Inline::styled("viejo", Style { strike: true, ..Default::default() })));
        assert!(content.contains(&Inline::styled("código", Style { code: true, ..Default::default() })));
        assert!(content.contains(&Inline::styled("sub", Style { underline: true, ..Default::default() })));
        assert!(content.contains(&Inline::link("sitio", "https://acme.mx")));
        assert!(content.contains(&Inline::LineBreak));
        assert!(matches!(&b[2], Block::List { ordered: false, items, .. } if items.len() == 2));
        assert!(matches!(&b[3], Block::Quote { blocks } if blocks.len() == 2));
        assert_eq!(b[4], Block::Code { text: "fn main() {}".into() });
        assert_eq!(b[5], Block::Rule);
        assert_eq!(body.content_ids(), vec!["logo@acme".to_string()]);
    }

    #[test]
    fn round_trip() {
        let body = import(DOC);
        let md = export(&body);
        let again = import(&md);
        assert_eq!(body, again, "{md}");
    }

    #[test]
    fn escapes_markdown_syntax_in_text() {
        let body = RichBody {
            blocks: vec![Block::paragraph("# no es título *ni* [enlace]"), Block::paragraph("1. tampoco lista")],
        };
        let md = export(&body);
        assert_eq!(import(&md), body, "{md}");
    }
}
