//! HTML → [`RichBody`], a lo mejor posible, con el tokenizador de html5ever.
//!
//! Sirve para editar borradores guardados como HTML, citar el cuerpo de un correo al responder y
//! pegar desde el portapapeles. Se queda con la estructura (párrafos, títulos, listas, citas,
//! código, separadores, firma) y los estilos básicos (incluidos los de `style="font-weight:…"`
//! que usan Outlook y Gmail); lo demás (tablas de maquetación, colores, tipografías) se aplana a
//! texto. No hay árbol DOM: se trabaja por tokens con pilas acotadas, así que ni el HTML más
//! roto ni el más anidado pueden hacer caer al programa ni agotar la pila.

use std::cell::RefCell;

use html5ever::tendril::StrTendril;
use html5ever::tokenizer::states::RawKind;
use html5ever::tokenizer::{BufferQueue, Tag, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts};

use super::{Builder, FrameKind, Image, RichBody, Style};

/// Tamaño máximo del HTML que se analiza (lo demás se descarta).
const MAX_HTML: usize = 16 * 1024 * 1024;
/// Profundidad máxima de las pilas de etiquetas en línea y de `div`.
const MAX_STACK: usize = 256;

/// Elementos cuyo contenido se descarta entero.
const SKIP: &[&str] = &[
    "script", "style", "head", "title", "template", "iframe", "object", "applet", "noembed", "noframes", "svg", "math",
    "xml", "select", "textarea", "button", "noscript",
];

/// Elementos de bloque genéricos: cierran el párrafo al abrir y al cerrar.
const BLOCKISH: &[&str] = &[
    "p",
    "div",
    "section",
    "article",
    "header",
    "footer",
    "main",
    "nav",
    "aside",
    "center",
    "address",
    "form",
    "fieldset",
    "dd",
    "dt",
    "dl",
    "figure",
    "figcaption",
    "details",
    "summary",
    "tr",
    "table",
    "tbody",
    "thead",
    "tfoot",
    "caption",
    "body",
    "html",
];

/// Importa HTML. Nunca falla.
pub(super) fn import(html: &str) -> RichBody {
    let html = clip(html, MAX_HTML);
    let sink = Sink { state: RefCell::new(State::new()) };
    let tok = Tokenizer::new(sink, TokenizerOpts::default());
    let input = BufferQueue::default();
    input.push_back(StrTendril::from_slice(html));
    // El sink nunca pide pausar (no ejecuta guiones), así que `feed` siempre termina.
    let _ = tok.feed(&input);
    tok.end();
    tok.sink.state.into_inner().finish()
}

/// Recorta `s` a `max` bytes como mucho, en un límite de carácter.
pub(super) fn clip(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    s.get(..end).unwrap_or("")
}

struct Sink {
    state: RefCell<State>,
}

/// Una etiqueta en línea abierta: qué estilo añadió y si abrió un enlace.
struct InlineOpen {
    name: String,
    delta: Style,
    link: bool,
}

struct State {
    b: Builder,
    /// Elemento cuyo contenido se salta, con anidamiento de su mismo nombre.
    skipping: Option<(String, u32)>,
    /// Dentro de `<pre>` (anidamiento) y su texto acumulado.
    pre: u32,
    pre_buf: String,
    inline: Vec<InlineOpen>,
    /// Por cada `<div>` abierto: ¿abrió una firma?
    divs: Vec<bool>,
}

impl State {
    fn new() -> Self {
        Self { b: Builder::new(), skipping: None, pre: 0, pre_buf: String::new(), inline: Vec::new(), divs: Vec::new() }
    }

    fn finish(mut self) -> RichBody {
        if self.pre > 0 {
            let text = std::mem::take(&mut self.pre_buf);
            self.b.code_block(&text);
        }
        self.b.finish()
    }

    fn tag(&mut self, tag: Tag) -> TokenSinkResult<()> {
        let name = tag.name.to_ascii_lowercase();
        let name = name.as_str();
        let start = tag.kind == TagKind::StartTag;

        if let Some((skip, depth)) = &mut self.skipping {
            if name == skip.as_str() {
                if start && !tag.self_closing {
                    *depth = depth.saturating_add(1);
                } else if !start {
                    if *depth == 0 {
                        self.skipping = None;
                    } else {
                        *depth = depth.saturating_sub(1);
                    }
                }
            }
            return if start { raw_kind(name) } else { TokenSinkResult::Continue };
        }
        if SKIP.contains(&name) {
            if start && !tag.self_closing {
                self.skipping = Some((name.to_string(), 0));
                return raw_kind(name);
            }
            return TokenSinkResult::Continue;
        }

        if self.pre > 0 {
            match (name, start) {
                ("pre", true) => self.pre = self.pre.saturating_add(1),
                ("pre", false) => {
                    self.pre = self.pre.saturating_sub(1);
                    if self.pre == 0 {
                        let text = std::mem::take(&mut self.pre_buf);
                        self.b.code_block(&text);
                    }
                }
                ("br", true) => self.pre_buf.push('\n'),
                _ => {}
            }
            return TokenSinkResult::Continue;
        }

        match (name, start) {
            ("pre", true) => {
                self.b.flush();
                self.pre = 1;
            }
            ("br", _) => self.b.line_break(),
            ("hr", true) => self.b.rule(),
            ("img", true) => self.image(&tag),
            ("blockquote", true) => self.b.open(FrameKind::Quote),
            ("blockquote", false) => self.b.close(FrameKind::Quote),
            ("ul" | "ol" | "menu", true) => {
                let start = attr(&tag, "start").and_then(|s| s.trim().parse::<u32>().ok()).unwrap_or(1);
                self.b.open(FrameKind::List { ordered: name == "ol", start });
            }
            ("ul" | "ol" | "menu", false) => self.b.close(FrameKind::List { ordered: false, start: 1 }),
            ("li", true) => {
                // Un <li> sin cerrar se cierra al abrir el siguiente.
                self.b.close_item_if_open();
                self.b.open(FrameKind::Item);
            }
            ("li", false) => self.b.close(FrameKind::Item),
            ("h1" | "h2" | "h3" | "h4" | "h5" | "h6", true) => {
                let level = name.get(1..).and_then(|l| l.parse::<u8>().ok()).unwrap_or(1);
                self.b.heading(level);
            }
            ("h1" | "h2" | "h3" | "h4" | "h5" | "h6", false) => self.b.flush(),
            ("div", true) => {
                let sig = is_signature(&tag);
                if sig {
                    self.b.open(FrameKind::Signature);
                } else {
                    self.b.flush();
                }
                if self.divs.len() < MAX_STACK {
                    self.divs.push(sig);
                }
            }
            ("div", false) => {
                if self.divs.pop() == Some(true) {
                    self.b.close(FrameKind::Signature);
                } else {
                    self.b.flush();
                }
            }
            ("td" | "th", true) => self.b.text(" ", true),
            (n, _) if BLOCKISH.contains(&n) => self.b.flush(),
            (n, true) if !tag.self_closing && is_inline(n) => self.open_inline(n, &tag),
            (n, false) if is_inline(n) => self.close_inline(n),
            _ => {}
        }
        TokenSinkResult::Continue
    }

    fn open_inline(&mut self, name: &str, tag: &Tag) {
        if self.inline.len() >= MAX_STACK {
            return;
        }
        let mut delta = match name {
            "b" | "strong" => Style { bold: true, ..Style::default() },
            "i" | "em" | "cite" | "dfn" | "var" => Style { italic: true, ..Style::default() },
            "u" | "ins" => Style { underline: true, ..Style::default() },
            "s" | "strike" | "del" => Style { strike: true, ..Style::default() },
            "code" | "tt" | "kbd" | "samp" => Style { code: true, ..Style::default() },
            _ => Style::default(),
        };
        if let Some(css) = attr(tag, "style") {
            let d = css_style(css);
            delta.bold |= d.bold;
            delta.italic |= d.italic;
            delta.underline |= d.underline;
            delta.strike |= d.strike;
        }
        let link = name == "a";
        if link {
            let href = attr(tag, "href").map(str::trim).filter(|h| !h.is_empty() && !h.starts_with('#'));
            self.b.push_link(href.map(str::to_string));
        }
        self.b.style.apply(delta, true);
        self.inline.push(InlineOpen { name: name.to_string(), delta, link });
    }

    fn close_inline(&mut self, name: &str) {
        let Some(pos) = self.inline.iter().rposition(|o| o.name == name) else { return };
        while self.inline.len() > pos {
            let Some(o) = self.inline.pop() else { break };
            self.b.style.apply(o.delta, false);
            if o.link {
                self.b.pop_link();
            }
        }
    }

    fn image(&mut self, tag: &Tag) {
        let src = attr(tag, "src").unwrap_or("").trim().to_string();
        let dim = |n: &str| {
            attr(tag, n).and_then(|v| {
                let digits: String = v.trim().chars().take_while(char::is_ascii_digit).collect();
                digits.parse::<u32>().ok()
            })
        };
        let (width, height) = (dim("width"), dim("height"));
        // Píxeles de rastreo: fuera.
        if src.is_empty() || (width.is_some_and(|w| w <= 1) && height.is_some_and(|h| h <= 1)) {
            return;
        }
        let alt = attr(tag, "alt").unwrap_or("").trim().chars().take(500).collect();
        self.b.image(Image { src, alt, width, height });
    }

    fn text(&mut self, t: &str) {
        if self.skipping.is_some() {
            return;
        }
        if self.pre > 0 {
            if self.pre_buf.len() < super::MAX_TEXT_BYTES {
                self.pre_buf.push_str(t);
            }
            return;
        }
        self.b.text(t, true);
    }
}

impl Builder {
    /// Si el contenedor superior es un elemento de lista, lo cierra.
    fn close_item_if_open(&mut self) {
        if self.top_is_item() {
            self.close(FrameKind::Item);
        }
    }
}

impl TokenSink for Sink {
    type Handle = ();

    fn process_token(&self, token: Token, _line: u64) -> TokenSinkResult<()> {
        let Ok(mut st) = self.state.try_borrow_mut() else {
            return TokenSinkResult::Continue;
        };
        match token {
            Token::TagToken(tag) => st.tag(tag),
            Token::CharacterTokens(t) => {
                st.text(&t);
                TokenSinkResult::Continue
            }
            Token::DoctypeToken(_)
            | Token::CommentToken(_)
            | Token::NullCharacterToken
            | Token::EOFToken
            | Token::ParseError(_) => TokenSinkResult::Continue,
        }
    }
}

/// Estado de texto crudo que pide el elemento (como haría el constructor de árbol).
fn raw_kind(name: &str) -> TokenSinkResult<()> {
    match name {
        "style" | "xmp" | "iframe" | "noembed" | "noframes" | "noscript" => TokenSinkResult::RawData(RawKind::Rawtext),
        "script" => TokenSinkResult::RawData(RawKind::ScriptData),
        "title" | "textarea" => TokenSinkResult::RawData(RawKind::Rcdata),
        _ => TokenSinkResult::Continue,
    }
}

fn is_inline(name: &str) -> bool {
    matches!(
        name,
        "a" | "b"
            | "strong"
            | "i"
            | "em"
            | "cite"
            | "dfn"
            | "var"
            | "u"
            | "ins"
            | "s"
            | "strike"
            | "del"
            | "code"
            | "tt"
            | "kbd"
            | "samp"
            | "span"
            | "font"
            | "small"
            | "big"
            | "sub"
            | "sup"
            | "mark"
            | "abbr"
            | "q"
            | "label"
    )
}

fn attr<'a>(tag: &'a Tag, name: &str) -> Option<&'a str> {
    tag.attrs.iter().find(|a| str::eq_ignore_ascii_case(&a.name.local, name)).map(|a| &*a.value)
}

/// ¿Es el contenedor de una firma (Gmail, Thunderbird, Apple Mail, Outlook o la nuestra)?
fn is_signature(tag: &Tag) -> bool {
    let class = attr(tag, "class").unwrap_or("").to_ascii_lowercase();
    let id = attr(tag, "id").unwrap_or("").to_ascii_lowercase();
    class
        .split_whitespace()
        .any(|c| matches!(c, "gmail_signature" | "moz-signature" | "iuredesk-signature" | "apple-mail-signature"))
        || id == "signature"
        || attr(tag, "data-smartmail").is_some_and(|v| v.eq_ignore_ascii_case("gmail_signature"))
}

/// Estilo en línea que entendemos de una declaración CSS.
fn css_style(css: &str) -> Style {
    let mut s = Style::default();
    for decl in css.split(';').take(64) {
        let Some((prop, value)) = decl.split_once(':') else { continue };
        let prop = prop.trim().to_ascii_lowercase();
        let value = value.trim().to_ascii_lowercase();
        match prop.as_str() {
            "font-weight" => {
                s.bold = value.starts_with("bold") || value.parse::<u32>().is_ok_and(|w| w >= 600);
            }
            "font-style" => s.italic = value.starts_with("italic") || value.starts_with("oblique"),
            "text-decoration" | "text-decoration-line" => {
                s.underline = value.contains("underline");
                s.strike = value.contains("line-through");
            }
            _ => {}
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rich::{Block, Inline, Span};

    #[test]
    fn basic_structure() {
        let body = import(
            "<html><head><title>x</title><style>p{color:red}</style></head><body>\
             <h2>Título</h2><p>Hola <b>Ana</b>, <i>¿cómo</i>   estás?<br>bien</p>\
             <ul><li>uno<li>dos <a href=\"https://acme.mx\">enlace</a></ul>\
             <ol start=\"3\"><li><p>tres</p></li></ol>\
             <blockquote><p>citado</p><blockquote>más</blockquote></blockquote>\
             <pre>  fn x() {\n    <b>y</b>\n}</pre><hr>\
             <div class=\"gmail_signature\">Ana López<br>Acme</div>\
             <script>alert(1)</script></body></html>",
        );
        let b = &body.blocks;
        assert_eq!(b[0], Block::Heading { level: 2, content: vec![Inline::text("Título")] });
        let Block::Paragraph { content } = &b[1] else { panic!("{b:?}") };
        assert_eq!(content[0], Inline::text("Hola "));
        assert_eq!(content[1], Inline::styled("Ana", Style { bold: true, ..Default::default() }));
        assert!(content.contains(&Inline::LineBreak));
        assert!(matches!(&b[2], Block::List { ordered: false, items, .. } if items.len() == 2));
        let Block::List { items, .. } = &b[2] else { panic!() };
        assert!(matches!(&items[1][0], Block::Paragraph { content } if content.iter().any(|i| matches!(i,
            Inline::Text(Span { link: Some(l), .. }) if l == "https://acme.mx"))));
        assert!(matches!(&b[3], Block::List { ordered: true, start: 3, .. }));
        assert!(matches!(&b[4], Block::Quote { blocks } if matches!(&blocks[1], Block::Quote { .. })));
        assert_eq!(b[5], Block::Code { text: "  fn x() {\n    y\n}".into() });
        assert_eq!(b[6], Block::Rule);
        assert!(matches!(&b[7], Block::Signature { .. }));
        assert_eq!(b.len(), 8);
        assert!(!body.text_content().contains("alert"));
        assert!(!body.text_content().contains("color:red"));
    }

    #[test]
    fn css_styles_and_unbalanced_tags() {
        let body = import("<p><span style=\"font-weight:700;text-decoration:underline\">x</span>y</b></i></p><p>z");
        let Block::Paragraph { content } = &body.blocks[0] else { panic!() };
        assert_eq!(content[0], Inline::styled("x", Style { bold: true, underline: true, ..Default::default() }));
        assert_eq!(content[1], Inline::text("y"));
        assert_eq!(body.blocks.len(), 2);
    }

    #[test]
    fn tracking_pixels_and_tables() {
        let body =
            import("<table><tr><td>a</td><td>b</td></tr><tr><td>c</td></tr></table><img src=x width=1 height=1>");
        assert_eq!(body.text_content(), "a bc");
        assert_eq!(body.blocks.len(), 2);
    }

    #[test]
    fn deep_nesting_does_not_overflow() {
        let html = "<blockquote>".repeat(100_000) + "x" + &"</blockquote>".repeat(100_000);
        let body = import(&html);
        assert_eq!(body.text_content(), "x");
        let html = "<b>".repeat(100_000) + "y";
        assert_eq!(import(&html).text_content(), "y");
    }
}
