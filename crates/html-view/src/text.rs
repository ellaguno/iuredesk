//! Texto plano a partir de HTML: respaldo cuando el render falla y base para citar al responder.
//!
//! Reglas: los bloques (`p`, `div`, `tr`, `li`, `h1`…) cortan línea; `<br>` también; las celdas se
//! separan con tabulador; `<blockquote>` antepone `> ` por nivel; los enlaces cuyo texto difiere de
//! la URL quedan como `texto <url>`; las imágenes muestran su `alt` entre corchetes; se ignoran
//! `<head>`, `<style>`, `<script>` y los comentarios (condicionales de Outlook incluidos).

use std::cell::RefCell;

use html5ever::tendril::StrTendril;
use html5ever::tokenizer::states::RawKind;
use html5ever::tokenizer::{BufferQueue, Tag, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts};

/// Convierte HTML (hostil) a texto plano legible. Nunca falla.
pub fn to_plain_text(html: &str) -> String {
    let sink = Sink::default();
    let tok = Tokenizer::new(sink, TokenizerOpts::default());
    let input = BufferQueue::default();
    input.push_back(StrTendril::from_slice(html));
    let _ = tok.feed(&input);
    tok.end();
    tok.sink.state.into_inner().finish()
}

const BLOCKS: &[&str] = &[
    "p",
    "div",
    "tr",
    "table",
    "li",
    "ul",
    "ol",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "pre",
    "blockquote",
    "section",
    "article",
    "header",
    "footer",
    "center",
    "hr",
    "dl",
    "dt",
    "dd",
    "address",
    "figure",
    "form",
    "fieldset",
    "main",
    "nav",
    "aside",
    "tbody",
    "thead",
    "tfoot",
    "caption",
];

const HIDDEN: &[&str] =
    &["head", "style", "script", "title", "template", "iframe", "object", "noembed", "noframes", "svg"];

#[derive(Default)]
struct Sink {
    state: RefCell<State>,
}

#[derive(Default)]
struct State {
    /// Líneas ya cerradas.
    lines: Vec<String>,
    /// Línea en curso (sin el prefijo de cita).
    cur: String,
    quote: usize,
    /// Nivel de cita con que empezó la línea en curso.
    cur_quote: usize,
    hidden: Option<(String, u32)>,
    pre: u32,
    /// href del enlace abierto y longitud de `cur` al abrirlo.
    link: Option<(String, usize)>,
    list_depth: usize,
    /// Contador de listas ordenadas (None = viñetas).
    ol: Vec<Option<u32>>,
    pending_space: bool,
    cell_started: bool,
}

impl State {
    fn finish(mut self) -> String {
        self.newline();
        // Colapsa más de una línea en blanco seguida y recorta extremos.
        let mut out = String::new();
        let mut blank = 0;
        for l in &self.lines {
            if l.trim_end_matches('>').trim().is_empty() {
                blank += 1;
                if blank > 1 || out.is_empty() {
                    continue;
                }
            } else {
                blank = 0;
            }
            out.push_str(l.trim_end());
            out.push('\n');
        }
        out.trim_end().to_owned()
    }

    fn newline(&mut self) {
        let line = std::mem::take(&mut self.cur);
        let prefix = "> ".repeat(self.cur_quote);
        self.lines.push(format!("{prefix}{line}"));
        self.cur_quote = self.quote;
        self.pending_space = false;
    }

    /// Corta línea si la actual tiene texto.
    fn block_break(&mut self) {
        if !self.cur.trim().is_empty() {
            self.newline();
        }
        self.cur_quote = self.quote;
    }

    fn blank_line(&mut self) {
        self.block_break();
        if self.lines.last().is_some_and(|l| !l.trim_end_matches('>').trim().is_empty()) {
            self.newline();
        }
    }

    fn push_text(&mut self, t: &str) {
        if self.pre > 0 {
            for (i, part) in t.split('\n').enumerate() {
                if i > 0 {
                    self.newline();
                }
                self.cur.push_str(part);
            }
            return;
        }
        for c in t.chars() {
            if c.is_whitespace() && c != '\u{a0}' {
                if !self.cur.is_empty() {
                    self.pending_space = true;
                }
            } else {
                if self.pending_space {
                    self.cur.push(' ');
                    self.pending_space = false;
                }
                self.cur.push(if c == '\u{a0}' { ' ' } else { c });
            }
        }
    }

    fn tag(&mut self, tag: &Tag) -> TokenSinkResult<()> {
        let name: &str = &tag.name;
        let start = tag.kind == TagKind::StartTag;
        if let Some((h, depth)) = &mut self.hidden {
            if name == h.as_str() {
                if start && !tag.self_closing {
                    *depth = depth.saturating_add(1);
                } else if !start {
                    if *depth == 0 {
                        self.hidden = None;
                    } else {
                        *depth -= 1;
                    }
                }
            }
            return if start { raw(name) } else { TokenSinkResult::Continue };
        }
        if HIDDEN.contains(&name) {
            if start && !tag.self_closing {
                self.hidden = Some((name.to_owned(), 0));
            }
            return if start { raw(name) } else { TokenSinkResult::Continue };
        }
        match (name, start) {
            ("br", _) => self.newline(),
            ("p", _) | ("h1" | "h2" | "h3" | "h4" | "h5" | "h6", _) => self.blank_line(),
            ("blockquote", true) => {
                self.block_break();
                self.quote = self.quote.saturating_add(1);
                self.cur_quote = self.quote;
            }
            ("blockquote", false) => {
                self.block_break();
                self.quote = self.quote.saturating_sub(1);
                self.cur_quote = self.quote;
            }
            ("pre", true) => {
                self.block_break();
                self.pre = self.pre.saturating_add(1);
            }
            ("pre", false) => {
                self.block_break();
                self.pre = self.pre.saturating_sub(1);
            }
            ("ul", true) => {
                self.block_break();
                self.list_depth = self.list_depth.saturating_add(1);
                self.ol.push(None);
            }
            ("ol", true) => {
                self.block_break();
                self.list_depth = self.list_depth.saturating_add(1);
                self.ol.push(Some(0));
            }
            ("ul" | "ol", false) => {
                self.block_break();
                self.list_depth = self.list_depth.saturating_sub(1);
                self.ol.pop();
            }
            ("li", true) => {
                self.block_break();
                let indent = "  ".repeat(self.list_depth.saturating_sub(1));
                let bullet = match self.ol.last_mut() {
                    Some(Some(n)) => {
                        *n = n.saturating_add(1);
                        format!("{n}. ")
                    }
                    _ => "- ".to_owned(),
                };
                self.cur.push_str(&indent);
                self.cur.push_str(&bullet);
            }
            ("tr", true) => {
                self.block_break();
                self.cell_started = false;
            }
            ("td" | "th", true) => {
                if self.cell_started && !self.cur.trim().is_empty() {
                    self.cur.push('\t');
                    self.pending_space = false;
                }
                self.cell_started = true;
            }
            ("hr", _) => {
                self.block_break();
                self.cur.push_str("----");
                self.newline();
            }
            ("img", true) => {
                if let Some(alt) = attr(tag, "alt").map(str::trim).filter(|a| !a.is_empty()) {
                    self.push_text(&format!("[{alt}]"));
                }
            }
            ("a", true) => {
                if let Some(href) = attr(tag, "href") {
                    self.link = Some((href.trim().to_owned(), self.cur.len()));
                }
            }
            ("a", false) => {
                if let Some((href, at)) = self.link.take() {
                    let txt = self.cur.get(at..).unwrap_or("").trim().to_owned();
                    let shown = href.strip_prefix("mailto:").unwrap_or(&href);
                    let useful = href.starts_with("http") || href.starts_with("mailto:");
                    if useful && !txt.is_empty() && txt != shown && txt != href {
                        self.push_text(&format!(" <{href}>"));
                    } else if useful && txt.is_empty() {
                        self.push_text(&href);
                    }
                }
            }
            (n, _) if BLOCKS.contains(&n) => self.block_break(),
            _ => {}
        }
        TokenSinkResult::Continue
    }
}

fn raw(name: &str) -> TokenSinkResult<()> {
    match name {
        "style" | "iframe" | "noembed" | "noframes" => TokenSinkResult::RawData(RawKind::Rawtext),
        "script" => TokenSinkResult::RawData(RawKind::ScriptData),
        "title" => TokenSinkResult::RawData(RawKind::Rcdata),
        _ => TokenSinkResult::Continue,
    }
}

fn attr<'a>(tag: &'a Tag, name: &str) -> Option<&'a str> {
    tag.attrs.iter().find(|a| str::eq_ignore_ascii_case(&a.name.local, name)).map(|a| &*a.value)
}

impl TokenSink for Sink {
    type Handle = ();

    fn process_token(&self, token: Token, _line: u64) -> TokenSinkResult<()> {
        let Ok(mut st) = self.state.try_borrow_mut() else {
            return TokenSinkResult::Continue;
        };
        match token {
            Token::TagToken(tag) => st.tag(&tag),
            Token::CharacterTokens(t) => {
                if st.hidden.is_none() {
                    st.push_text(&t);
                }
                TokenSinkResult::Continue
            }
            _ => TokenSinkResult::Continue,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::to_plain_text as t;

    #[test]
    fn paragraphs_and_breaks() {
        assert_eq!(t("<p>Hola</p><p>mundo<br>adiós</p>"), "Hola\n\nmundo\nadiós");
    }

    #[test]
    fn hides_head_style_script_comments() {
        let out = t(
            "<html><head><title>T</title><style>p{}</style></head><body><!--[if mso]>MSO<![endif]--><script>x()</script>Cuerpo</body>",
        );
        assert_eq!(out, "Cuerpo");
    }

    #[test]
    fn blockquote_quoting() {
        let out = t("<p>Respuesta</p><blockquote><p>Original</p><blockquote>Más viejo</blockquote></blockquote>");
        assert!(out.contains("> Original"), "{out}");
        assert!(out.contains("> > Más viejo"), "{out}");
    }

    #[test]
    fn links_and_images() {
        let out =
            t(r#"<a href="https://x.com/a">Ver</a> <a href="https://x.com/b">https://x.com/b</a> <img alt="Logo">"#);
        assert_eq!(out, "Ver <https://x.com/a> https://x.com/b [Logo]");
    }

    #[test]
    fn lists() {
        let out = t("<ul><li>a</li><li>b</li></ul><ol><li>uno</li><li>dos</li></ol>");
        assert_eq!(out, "- a\n- b\n1. uno\n2. dos");
    }

    #[test]
    fn whitespace_collapsed_and_entities() {
        assert_eq!(t("  a\n\n   b&nbsp;&amp;  c "), "a b & c");
    }

    #[test]
    fn table_cells() {
        assert_eq!(t("<table><tr><td>a</td><td>b</td></tr><tr><td>c</td></tr></table>"), "a\tb\nc");
    }

    #[test]
    fn pre_keeps_lines() {
        assert_eq!(t("<pre>a\n  b</pre>"), "a\n  b");
    }
}
