//! [`RichBody`] → `text/plain; format=flowed` (RFC 3676).
//!
//! - Las líneas se cortan a [`FLOW_WIDTH`] columnas; cada corte «blando» termina en un espacio,
//!   así un lector con soporte de *flowed* vuelve a unir el párrafo y uno sin él lo ve bien
//!   cortado. Las líneas «duras» (fin de párrafo, salto de línea) nunca terminan en espacio.
//! - Las citas se prefijan con `>` por nivel y un espacio (`> `, `>> `).
//! - Las líneas que empiezan con espacio, `>` o `From ` se «rellenan» con un espacio (§4.4).
//! - La firma va tras el separador estándar `-- ` (con su espacio final).
//! - Listas con `- ` o `1. `, enlaces como `texto <url>`, imágenes como `[texto alternativo]`.

use super::{Block, Inline, MAX_DEPTH, RichBody};

/// Ancho objetivo de línea (sin contar el prefijo de cita).
pub const FLOW_WIDTH: usize = 72;
/// Ancho mínimo aunque la cita sea muy profunda.
const MIN_WIDTH: usize = 20;
/// Separador de firma (RFC 3676 §4.3).
pub const SIG_DELIMITER: &str = "-- ";

/// Renderiza el documento.
pub fn render(body: &RichBody) -> String {
    let mut w = Writer::default();
    w.blocks(&body.blocks, 0, "", 0);
    while w.lines.last().is_some_and(|l| l.trim_start_matches('>').is_empty()) {
        w.lines.pop();
    }
    let mut out = w.lines.join("\n");
    out.push('\n');
    out
}

#[derive(Default)]
struct Writer {
    lines: Vec<String>,
}

impl Writer {
    /// Emite una línea con prefijo de cita y relleno.
    fn emit(&mut self, quote: usize, content: &str, soft: bool) {
        let mut line = String::with_capacity(content.len().saturating_add(quote).saturating_add(2));
        for _ in 0..quote {
            line.push('>');
        }
        let content = if soft { content } else { content.trim_end_matches(' ') };
        if quote > 0 {
            if !content.is_empty() {
                line.push(' ');
            }
        } else if content.starts_with(' ') || content.starts_with('>') || content.starts_with("From ") {
            line.push(' ');
        }
        line.push_str(content);
        self.lines.push(line);
    }

    fn blank(&mut self, quote: usize) {
        self.emit(quote, "", false);
    }

    fn blocks(&mut self, bs: &[Block], quote: usize, first_prefix: &str, depth: usize) {
        let mut first = true;
        for b in bs {
            if !first {
                self.blank(quote);
            }
            let prefix = if first { first_prefix.to_string() } else { " ".repeat(first_prefix.chars().count()) };
            self.block(b, quote, &prefix, depth);
            first = false;
        }
    }

    fn block(&mut self, b: &Block, quote: usize, prefix: &str, depth: usize) {
        let deep = depth >= MAX_DEPTH.saturating_mul(2);
        match b {
            Block::Paragraph { content } | Block::Heading { content, .. } => {
                let text = inline_text(content);
                self.para(quote, prefix, &text);
            }
            Block::List { ordered, start, items } => {
                let indent = " ".repeat(prefix.chars().count());
                for (n, item) in items.iter().enumerate() {
                    let marker = if *ordered {
                        let num = u64::from(*start).saturating_add(n as u64);
                        format!("{num}. ")
                    } else {
                        "- ".to_string()
                    };
                    let p = if n == 0 { format!("{prefix}{marker}") } else { format!("{indent}{marker}") };
                    if deep {
                        let text = RichBody { blocks: item.clone() }.text_content();
                        self.para(quote, &p, &text);
                    } else {
                        self.blocks(item, quote, &p, depth.saturating_add(1));
                    }
                }
            }
            Block::Quote { blocks } => {
                if !prefix.trim().is_empty() {
                    self.emit(quote, prefix.trim_end(), false);
                }
                if deep {
                    let text = RichBody { blocks: blocks.clone() }.text_content();
                    self.para(quote.saturating_add(1), "", &text);
                } else {
                    self.blocks(blocks, quote.saturating_add(1), "", depth.saturating_add(1));
                }
            }
            Block::Code { text } => {
                for (i, line) in text.split('\n').enumerate() {
                    let p = if i == 0 { prefix.to_string() } else { " ".repeat(prefix.chars().count()) };
                    self.emit(quote, &format!("{p}{line}"), false);
                }
            }
            Block::Rule => self.emit(quote, &format!("{prefix}{}", "-".repeat(40)), false),
            Block::Signature { blocks } => {
                // El separador lleva su espacio final aunque sea una línea dura.
                let mut line = ">".repeat(quote);
                if quote > 0 {
                    line.push(' ');
                }
                line.push_str(SIG_DELIMITER);
                self.lines.push(line);
                if deep {
                    let text = RichBody { blocks: blocks.clone() }.text_content();
                    self.para(quote, "", &text);
                } else {
                    self.blocks(blocks, quote, "", depth.saturating_add(1));
                }
            }
        }
    }

    /// Un párrafo: cada línea dura se corta en líneas blandas.
    fn para(&mut self, quote: usize, prefix: &str, text: &str) {
        let indent = " ".repeat(prefix.chars().count());
        let quote_cols = if quote > 0 { quote.saturating_add(1) } else { 0 };
        for (i, hard) in text.split('\n').enumerate() {
            let p = if i == 0 { prefix } else { indent.as_str() };
            let width = FLOW_WIDTH.saturating_sub(quote_cols).saturating_sub(p.chars().count()).max(MIN_WIDTH);
            let pieces = wrap(hard, width);
            let count = pieces.len();
            for (j, piece) in pieces.into_iter().enumerate() {
                let soft = j.saturating_add(1) < count;
                if j == 0 {
                    self.emit(quote, &format!("{p}{piece}"), soft);
                } else {
                    self.emit(quote, &piece, soft);
                }
            }
        }
    }
}

/// Corta una línea dura en trozos de como mucho `width` caracteres (salvo palabras más largas).
/// Todos los trozos menos el último terminan en espacio (corte blando).
fn wrap(line: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut cur_len = 0usize;
    for (i, word) in line.split(' ').enumerate() {
        let wlen = word.chars().count();
        if i == 0 {
            cur.push_str(word);
            cur_len = wlen;
            continue;
        }
        if cur_len.saturating_add(1).saturating_add(wlen) > width && cur_len > 0 {
            cur.push(' ');
            out.push(std::mem::take(&mut cur));
            cur.push_str(word);
            cur_len = wlen;
        } else {
            cur.push(' ');
            cur.push_str(word);
            cur_len = cur_len.saturating_add(1).saturating_add(wlen);
        }
    }
    out.push(cur);
    out
}

/// Texto de una secuencia en línea: enlaces como `texto <url>`, imágenes como `[alt]`.
pub(crate) fn inline_text(content: &[Inline]) -> String {
    let mut out = String::new();
    let mut i = 0usize;
    while let Some(item) = content.get(i) {
        match item {
            Inline::Text(span) => {
                let Some(href) = span.link.as_deref() else {
                    out.push_str(&span.text);
                    i = i.saturating_add(1);
                    continue;
                };
                let mut text = String::new();
                let mut j = i;
                while let Some(Inline::Text(s)) = content.get(j) {
                    if s.link.as_deref() != Some(href) {
                        break;
                    }
                    text.push_str(&s.text);
                    j = j.saturating_add(1);
                }
                out.push_str(&text);
                let shown = href.strip_prefix("mailto:").unwrap_or(href);
                if text.trim() != shown && text.trim() != href {
                    out.push_str(" <");
                    out.push_str(href);
                    out.push('>');
                }
                i = j.max(i.saturating_add(1));
            }
            Inline::Image(img) => {
                out.push('[');
                out.push_str(if img.alt.trim().is_empty() { "image" } else { img.alt.trim() });
                out.push(']');
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rich::{Image, Span, Style};

    #[test]
    fn wraps_with_soft_breaks() {
        let long = "palabra ".repeat(30);
        let body = RichBody::from_plain(long.trim());
        let out = render(&body);
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines.len() > 1);
        for l in &lines[..lines.len() - 1] {
            assert!(l.ends_with(' '), "{l:?}");
            assert!(l.chars().count() <= 78);
        }
        assert!(!lines.last().unwrap().ends_with(' '));
        // Al desplegar (quitar CRLF tras espacio) vuelve el párrafo original.
        assert_eq!(out.replace(" \n", " ").trim_end(), long.trim());
    }

    #[test]
    fn quotes_stuffing_and_signature() {
        let body = RichBody {
            blocks: vec![
                Block::paragraph("From here >x"),
                Block::Paragraph { content: vec![Inline::text(" sangría   ")] },
                Block::Quote {
                    blocks: vec![
                        Block::paragraph("nivel uno"),
                        Block::Quote { blocks: vec![Block::paragraph("nivel dos")] },
                    ],
                },
                Block::Signature { blocks: vec![Block::paragraph("Ana")] },
            ],
        };
        let out = render(&body);
        assert_eq!(out, " From here >x\n\n  sangría\n\n> nivel uno\n>\n>> nivel dos\n\n-- \nAna\n");
    }

    #[test]
    fn lists_links_images() {
        let body = RichBody {
            blocks: vec![
                Block::List {
                    ordered: true,
                    start: 9,
                    items: vec![
                        vec![Block::paragraph("nueve")],
                        vec![
                            Block::paragraph("diez"),
                            Block::List { ordered: false, start: 1, items: vec![vec![Block::paragraph("sub")]] },
                        ],
                    ],
                },
                Block::Paragraph {
                    content: vec![
                        Inline::link("Acme", "https://acme.mx"),
                        Inline::text(" "),
                        Inline::link("https://x.mx", "https://x.mx"),
                        Inline::text(" "),
                        Inline::Text(Span {
                            text: "ana@acme.mx".into(),
                            style: Style::default(),
                            link: Some("mailto:ana@acme.mx".into()),
                        }),
                        Inline::Image(Image { src: "cid:x".into(), alt: "logo".into(), ..Default::default() }),
                    ],
                },
                Block::Code { text: "a  \n  b".into() },
            ],
        };
        let out = render(&body);
        assert!(out.contains("9. nueve\n10. diez\n\n     - sub\n"), "{out}");
        assert!(out.contains("Acme <https://acme.mx> https://x.mx ana@acme.mx[logo]"), "{out}");
        assert!(out.ends_with("a\n   b\n"), "{out:?}");
    }
}
