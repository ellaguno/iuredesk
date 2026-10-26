//! [`RichBody`] → HTML apto para correo.
//!
//! Reglas: estilos en línea (los clientes web tiran las hojas `<style>`), sin tablas, sin
//! clases de las que dependa el aspecto, márgenes explícitos en cada bloque (Outlook de
//! escritorio usa Word para pintar y sus márgenes por omisión son otros), colores en hexadecimal
//! de seis dígitos y fuentes con respaldo. Todo el texto se escapa; los enlaces solo admiten
//! `http(s):`, `mailto:` y `tel:`, y las imágenes `cid:`, `http(s):` y `data:image/…`. Así el HTML
//! que sale nunca lleva guiones ni manejadores de eventos aunque el documento venga de un HTML
//! hostil importado.

use super::{Block, Image, Inline, MAX_DEPTH, RichBody, Span, Style};

const FONT: &str = "font-family:Calibri,Arial,Helvetica,sans-serif;font-size:14px;line-height:1.5;color:#222222;";
const P: &str = "margin:0 0 12px 0;";
const LIST: &str = "margin:0 0 12px 0;padding:0 0 0 24px;";
const LI: &str = "margin:0 0 4px 0;";
const QUOTE: &str = "margin:0 0 12px 0;padding:0 0 0 12px;border-left:3px solid #c8c8c8;color:#555555;";
const PRE: &str = "margin:0 0 12px 0;padding:8px;background-color:#f4f4f4;\
font-family:Consolas,Menlo,'Courier New',monospace;font-size:13px;line-height:1.4;white-space:pre-wrap;";
const CODE: &str = "font-family:Consolas,Menlo,'Courier New',monospace;font-size:13px;background-color:#f4f4f4;";
const HR: &str = "border:none;border-top:1px solid #cccccc;margin:12px 0;";
const LINK: &str = "color:#0b57d0;text-decoration:underline;";
const IMG: &str = "max-width:100%;height:auto;border:0;";
const SIGNATURE: &str = "margin:16px 0 0 0;color:#555555;";

/// Documento HTML completo.
pub fn document(body: &RichBody) -> String {
    let mut out = String::with_capacity(512);
    out.push_str("<!DOCTYPE html>\n<html><head>");
    out.push_str("<meta http-equiv=\"Content-Type\" content=\"text/html; charset=utf-8\">");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">");
    out.push_str("</head>\n<body style=\"margin:0;padding:0;\">\n<div style=\"");
    out.push_str(FONT);
    out.push_str("\">\n");
    blocks(&mut out, &body.blocks, 0);
    out.push_str("</div>\n</body></html>\n");
    out
}

/// Solo el contenido, sin envoltorio.
pub fn fragment(body: &RichBody) -> String {
    let mut out = String::new();
    blocks(&mut out, &body.blocks, 0);
    out
}

fn blocks(out: &mut String, bs: &[Block], depth: usize) {
    for b in bs {
        block(out, b, depth);
    }
}

fn block(out: &mut String, b: &Block, depth: usize) {
    // Un documento deserializado podría venir más anidado que lo que produce la importación.
    let deep = depth >= MAX_DEPTH.saturating_mul(2);
    match b {
        Block::Paragraph { content } => {
            out.push_str("<p style=\"");
            out.push_str(P);
            out.push_str("\">");
            if inlines_empty(content) {
                out.push_str("<br>");
            } else {
                inlines(out, content);
            }
            out.push_str("</p>\n");
        }
        Block::Heading { level, content } => {
            let level = (*level).clamp(1, 6);
            let size = match level {
                1 => 24,
                2 => 20,
                3 => 17,
                _ => 15,
            };
            out.push_str(&format!(
                "<h{level} style=\"margin:16px 0 8px 0;font-size:{size}px;font-weight:bold;line-height:1.3;\">"
            ));
            inlines(out, content);
            out.push_str(&format!("</h{level}>\n"));
        }
        Block::List { ordered, start, items } => {
            let tag = if *ordered { "ol" } else { "ul" };
            out.push('<');
            out.push_str(tag);
            if *ordered && *start != 1 {
                out.push_str(&format!(" start=\"{start}\""));
            }
            out.push_str(" style=\"");
            out.push_str(LIST);
            out.push_str("\">\n");
            for item in items {
                out.push_str("<li style=\"");
                out.push_str(LI);
                out.push_str("\">");
                match item.as_slice() {
                    // Un elemento de un solo párrafo va sin <p>: Outlook añadiría espacio.
                    [Block::Paragraph { content }] => inlines(out, content),
                    _ if deep => item.iter().for_each(|b| flat_text(out, b)),
                    _ => {
                        out.push('\n');
                        blocks(out, item, depth.saturating_add(1));
                    }
                }
                out.push_str("</li>\n");
            }
            out.push_str("</");
            out.push_str(tag);
            out.push_str(">\n");
        }
        Block::Quote { blocks: inner } => {
            out.push_str("<blockquote style=\"");
            out.push_str(QUOTE);
            out.push_str("\">\n");
            if deep {
                inner.iter().for_each(|b| flat_text(out, b));
            } else {
                blocks(out, inner, depth.saturating_add(1));
            }
            out.push_str("</blockquote>\n");
        }
        Block::Code { text } => {
            out.push_str("<pre style=\"");
            out.push_str(PRE);
            out.push_str("\">");
            escape_text(out, text);
            out.push_str("</pre>\n");
        }
        Block::Rule => {
            out.push_str("<hr style=\"");
            out.push_str(HR);
            out.push_str("\">\n");
        }
        Block::Signature { blocks: inner } => {
            out.push_str("<div class=\"iuredesk-signature\" style=\"");
            out.push_str(SIGNATURE);
            out.push_str("\">\n");
            if deep {
                inner.iter().for_each(|b| flat_text(out, b));
            } else {
                blocks(out, inner, depth.saturating_add(1));
            }
            out.push_str("</div>\n");
        }
    }
}

/// Último recurso ante anidado excesivo: el texto del bloque como párrafo.
fn flat_text(out: &mut String, b: &Block) {
    let text = RichBody { blocks: vec![b.clone()] }.text_content();
    out.push_str("<p style=\"");
    out.push_str(P);
    out.push_str("\">");
    escape_text(out, &text);
    out.push_str("</p>\n");
}

fn inlines_empty(content: &[Inline]) -> bool {
    content.iter().all(|i| match i {
        Inline::Text(s) => s.text.is_empty(),
        Inline::Image(_) => false,
        Inline::LineBreak => true,
    })
}

fn inlines(out: &mut String, content: &[Inline]) {
    let mut i = 0usize;
    while let Some(item) = content.get(i) {
        match item {
            Inline::Text(span) => {
                // Tramos seguidos con el mismo enlace van en un solo <a>.
                let href = span.link.as_deref().and_then(safe_href);
                let mut j = i;
                if let Some(href) = &href {
                    out.push_str("<a href=\"");
                    escape_attr(out, href);
                    out.push_str("\" style=\"");
                    out.push_str(LINK);
                    out.push_str("\">");
                    while let Some(Inline::Text(s)) = content.get(j) {
                        if s.link.as_deref().and_then(safe_href).as_deref() != Some(href.as_str()) {
                            break;
                        }
                        styled(out, s);
                        j = j.saturating_add(1);
                    }
                    out.push_str("</a>");
                } else {
                    styled(out, span);
                    j = j.saturating_add(1);
                }
                i = j.max(i.saturating_add(1));
            }
            Inline::Image(img) => {
                image(out, img);
                i = i.saturating_add(1);
            }
            Inline::LineBreak => {
                out.push_str("<br>\n");
                i = i.saturating_add(1);
            }
        }
    }
}

fn styled(out: &mut String, s: &Span) {
    let Style { bold, italic, underline, strike, code } = s.style;
    let mut close = Vec::with_capacity(5);
    if code {
        out.push_str("<code style=\"");
        out.push_str(CODE);
        out.push_str("\">");
        close.push("</code>");
    }
    if bold {
        out.push_str("<strong>");
        close.push("</strong>");
    }
    if italic {
        out.push_str("<em>");
        close.push("</em>");
    }
    if underline {
        out.push_str("<u>");
        close.push("</u>");
    }
    if strike {
        out.push_str("<s>");
        close.push("</s>");
    }
    escape_text(out, &s.text);
    for c in close.iter().rev() {
        out.push_str(c);
    }
}

fn image(out: &mut String, img: &Image) {
    let Some(src) = safe_img_src(&img.src) else {
        // Imagen con origen no permitido: queda su texto alternativo.
        if !img.alt.is_empty() {
            out.push('[');
            escape_text(out, &img.alt);
            out.push(']');
        }
        return;
    };
    out.push_str("<img src=\"");
    escape_attr(out, &src);
    out.push_str("\" alt=\"");
    escape_attr(out, &img.alt);
    out.push('"');
    if let Some(w) = img.width.filter(|w| *w > 0 && *w <= 10_000) {
        out.push_str(&format!(" width=\"{w}\""));
    }
    if let Some(h) = img.height.filter(|h| *h > 0 && *h <= 10_000) {
        out.push_str(&format!(" height=\"{h}\""));
    }
    out.push_str(" style=\"");
    out.push_str(IMG);
    out.push_str("\">");
}

/// Escapa texto de contenido. Los espacios dobles se conservan con `&nbsp;` alterno y los saltos
/// de línea (que en un tramo no deberían aparecer) se pintan como `<br>`.
pub(crate) fn escape_text(out: &mut String, s: &str) {
    let mut prev_space = false;
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\n' => out.push_str("<br>\n"),
            '\u{a0}' => out.push_str("&nbsp;"),
            ' ' if prev_space => {
                out.push_str("&nbsp;");
                prev_space = false;
                continue;
            }
            c if c.is_control() && c != '\t' => {}
            c => out.push(c),
        }
        prev_space = c == ' ';
    }
}

/// Escapa un valor de atributo (entre comillas dobles).
pub(crate) fn escape_attr(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
}

/// Quita espacios y controles que los navegadores ignoran dentro de una URL (`java\tscript:`).
fn compact_url(url: &str) -> String {
    url.chars().filter(|c| !c.is_whitespace() && !c.is_control()).collect()
}

/// Destino de enlace permitido, o `None`.
pub fn safe_href(href: &str) -> Option<String> {
    let u = compact_url(href);
    let lower = u.to_ascii_lowercase();
    let ok = ["http://", "https://", "mailto:", "tel:"].iter().any(|p| lower.starts_with(p)) && u.len() <= 8 * 1024;
    ok.then_some(u)
}

/// Origen de imagen permitido, o `None`.
pub fn safe_img_src(src: &str) -> Option<String> {
    let u = compact_url(src);
    let lower = u.to_ascii_lowercase();
    let ok = lower.starts_with("cid:")
        || ((lower.starts_with("https://") || lower.starts_with("http://")) && u.len() <= 8 * 1024)
        || ["data:image/png;", "data:image/jpeg;", "data:image/gif;", "data:image/webp;"]
            .iter()
            .any(|p| lower.starts_with(p));
    ok.then_some(u)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paragraph_styles_and_links() {
        let body = RichBody {
            blocks: vec![Block::Paragraph {
                content: vec![
                    Inline::text("a < b & "),
                    Inline::styled("negrita", Style { bold: true, italic: true, ..Default::default() }),
                    Inline::text(" "),
                    Inline::link("sitio", "https://acme.mx/?a=1&b=\"2\""),
                    Inline::Text(Span {
                        text: " más".into(),
                        style: Style { underline: true, ..Default::default() },
                        link: Some("https://acme.mx/?a=1&b=\"2\"".into()),
                    }),
                    Inline::link("malo", "javascript:alert(1)"),
                ],
            }],
        };
        let h = fragment(&body);
        assert!(h.contains("a &lt; b &amp; "));
        assert!(h.contains("<strong><em>negrita</em></strong>"));
        assert!(h.contains("href=\"https://acme.mx/?a=1&amp;b=&quot;2&quot;\""));
        assert_eq!(h.matches("<a ").count(), 1, "{h}");
        assert!(h.contains("<u> más</u></a>"));
        assert!(!h.contains("javascript"));
        assert!(h.contains("malo"));
    }

    #[test]
    fn unsafe_images_become_alt() {
        let body = RichBody {
            blocks: vec![Block::Paragraph {
                content: vec![
                    Inline::Image(Image { src: "file:///etc/passwd".into(), alt: "x".into(), ..Default::default() }),
                    Inline::Image(Image { src: "cid:a@b".into(), alt: "\"q\"".into(), width: Some(20), height: None }),
                ],
            }],
        };
        let h = fragment(&body);
        assert!(!h.contains("file:"));
        assert!(h.contains("[x]"));
        assert!(h.contains("<img src=\"cid:a@b\" alt=\"&quot;q&quot;\" width=\"20\""));
    }

    #[test]
    fn document_has_no_tables_and_inline_styles() {
        let body = RichBody {
            blocks: vec![
                Block::Heading { level: 9, content: vec![Inline::text("T")] },
                Block::List { ordered: true, start: 2, items: vec![vec![Block::paragraph("uno")]] },
                Block::Quote { blocks: vec![Block::paragraph("citado")] },
                Block::Code { text: "fn  x() {}\n<tag>".into() },
                Block::Rule,
                Block::Signature { blocks: vec![Block::paragraph("Ana")] },
            ],
        };
        let h = document(&body);
        assert!(!h.contains("<table"));
        assert!(!h.contains("<style"));
        assert!(h.contains("<h6 "));
        assert!(h.contains("<ol start=\"2\""));
        assert!(h.contains("<li style=\"margin:0 0 4px 0;\">uno</li>"));
        assert!(h.contains("&lt;tag&gt;"));
        assert!(h.contains("fn &nbsp;x()"));
    }

    #[test]
    fn href_filter() {
        assert!(safe_href(" java\tscript:alert(1)").is_none());
        assert!(safe_href("JAVASCRIPT:x").is_none());
        assert!(safe_href("vbscript:x").is_none());
        assert!(safe_href("data:text/html,x").is_none());
        assert_eq!(safe_href("mailto:a@b.co").as_deref(), Some("mailto:a@b.co"));
        assert!(safe_img_src("data:image/svg+xml;base64,AAA").is_none());
        assert!(safe_img_src("data:image/png;base64,AAA").is_some());
    }
}
