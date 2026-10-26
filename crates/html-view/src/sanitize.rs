//! Saneador de HTML de correo, por tokens (html5ever), antes de entregarlo a Blitz.
//!
//! Se tokeniza el HTML y se vuelve a serializar solo lo permitido. Después Blitz lo vuelve a
//! analizar con el mismo html5ever, así que el árbol resultante equivale al original menos lo
//! quitado. Qué se quita:
//!
//! - `<script>`, `<iframe>`, `<object>`, `<applet>`, `<embed>`, `<frameset>`, `<template>`… con su
//!   contenido; `<base>`, `<meta>` salvo `color-scheme`/`viewport`/`charset` (adiós `refresh`);
//!   `<link>` salvo hojas de estilo (cuya descarga igual la decide el proveedor de red).
//! - Atributos `on*`, `srcdoc`, `formaction`, `ping`; `action`/`method`/`target` de formularios.
//! - URLs con esquemas peligrosos (`javascript:`, `vbscript:`, `file:`, `data:text/html`…), aun
//!   ofuscadas con espacios, tabuladores o mayúsculas.
//! - Dentro de `<svg>`, cualquier `href` que no sea un `#fragmento` o una imagen `data:` (usvg
//!   leería archivos locales).
//! - Comentarios (incluidos los condicionales de Outlook `<!--[if mso]>`, igual que hacen los
//!   clientes que no son Outlook).
//!
//! Lo que NO hace: bloquear recursos remotos. Eso lo hace el proveedor de red (`net.rs`), que es la
//! única puerta hacia fuera y además lleva la lista de bloqueados para la barra «Cargar imágenes».

use std::cell::RefCell;

use html5ever::tendril::StrTendril;
use html5ever::tokenizer::states::RawKind;
use html5ever::tokenizer::{BufferQueue, Tag, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts};

/// HTML saneado y lo que se averiguó al recorrerlo.
#[derive(Clone, Debug, Default)]
pub struct Sanitized {
    /// HTML seguro para Blitz.
    pub html: String,
    /// El correo declara soporte de modo oscuro (`<meta name="color-scheme" content="… dark">` o
    /// reglas `@media (prefers-color-scheme: dark)`).
    pub supports_dark: bool,
    /// Elementos o atributos quitados (para diagnóstico y pruebas).
    pub removed: usize,
}

/// Elementos que se quitan junto con todo su contenido.
const DROP_WITH_CONTENT: &[&str] =
    &["script", "iframe", "object", "applet", "frameset", "frame", "noembed", "noframes", "template", "portal"];

/// Elementos que se quitan pero conservan su contenido (o que son vacíos).
const DROP_TAG_ONLY: &[&str] =
    &["embed", "base", "param", "noscript", "basefont", "bgsound", "keygen", "audio", "video", "source", "track"];

/// Atributos con URL que se validan.
const URL_ATTRS: &[&str] =
    &["href", "src", "background", "poster", "cite", "longdesc", "lowsrc", "dynsrc", "xlink:href", "usemap", "srcset"];

/// Sanea `html`. Nunca falla: con entrada basura devuelve basura inofensiva.
pub fn sanitize(html: &str) -> Sanitized {
    let sink = Sink::default();
    let tok = Tokenizer::new(sink, TokenizerOpts::default());
    let input = BufferQueue::default();
    input.push_back(StrTendril::from_slice(html));
    // Nuestro sink nunca pide pausar por <script>, así que `feed` siempre termina.
    let _ = tok.feed(&input);
    tok.end();
    tok.sink.state.into_inner().finish()
}

#[derive(Default)]
struct Sink {
    state: RefCell<State>,
}

#[derive(Default)]
struct State {
    out: String,
    /// Elemento que se está saltando con su contenido, y anidamiento de su mismo nombre.
    skipping: Option<(String, u32)>,
    /// Profundidad dentro de `<svg>`.
    svg_depth: u32,
    /// El último start tag fue un elemento de texto crudo: su contenido se copia tal cual.
    raw_verbatim: bool,
    supports_dark: bool,
    removed: usize,
}

impl State {
    fn finish(self) -> Sanitized {
        Sanitized { html: self.out, supports_dark: self.supports_dark, removed: self.removed }
    }

    fn tag(&mut self, tag: Tag) -> TokenSinkResult<()> {
        let name: &str = &tag.name;
        // ¿Saltando contenido?
        if let Some((skip_name, depth)) = &mut self.skipping {
            if name == skip_name.as_str() {
                match tag.kind {
                    TagKind::StartTag if !tag.self_closing => *depth = depth.saturating_add(1),
                    TagKind::EndTag => {
                        if *depth == 0 {
                            self.skipping = None;
                        } else {
                            *depth -= 1;
                        }
                    }
                    TagKind::StartTag => {}
                }
            }
            // Los elementos de texto crudo deben seguir cambiando el estado del tokenizador aun
            // dentro de lo saltado, para no confundir su contenido con etiquetas.
            return if tag.kind == TagKind::StartTag { raw_kind(name) } else { TokenSinkResult::Continue };
        }

        if DROP_WITH_CONTENT.contains(&name) {
            self.removed = self.removed.saturating_add(1);
            if tag.kind == TagKind::StartTag && !tag.self_closing {
                self.skipping = Some((name.to_owned(), 0));
                return raw_kind(name);
            }
            return TokenSinkResult::Continue;
        }
        if DROP_TAG_ONLY.contains(&name) {
            self.removed = self.removed.saturating_add(1);
            return TokenSinkResult::Continue;
        }

        match tag.kind {
            TagKind::EndTag => {
                if name == "svg" {
                    self.svg_depth = self.svg_depth.saturating_sub(1);
                }
                self.raw_verbatim = false;
                self.out.push_str("</");
                self.out.push_str(name);
                self.out.push('>');
                TokenSinkResult::Continue
            }
            TagKind::StartTag => self.start_tag(tag),
        }
    }

    fn start_tag(&mut self, tag: Tag) -> TokenSinkResult<()> {
        let name: &str = &tag.name;
        match name {
            "meta" => {
                if !self.meta_allowed(&tag) {
                    self.removed = self.removed.saturating_add(1);
                    return TokenSinkResult::Continue;
                }
            }
            "link" => {
                let is_sheet = attr(&tag, "rel")
                    .is_some_and(|r| r.split_ascii_whitespace().any(|t| t.eq_ignore_ascii_case("stylesheet")));
                if !is_sheet {
                    self.removed = self.removed.saturating_add(1);
                    return TokenSinkResult::Continue;
                }
            }
            "svg" if !tag.self_closing => self.svg_depth = self.svg_depth.saturating_add(1),
            _ => {}
        }
        // <plaintext> convierte el resto del documento en texto: lo emulamos con <pre>.
        let (emit_name, result) =
            if name == "plaintext" { ("pre", TokenSinkResult::Plaintext) } else { (name, raw_kind(name)) };
        let in_svg = self.svg_depth > 0 || name == "svg";
        self.out.push('<');
        self.out.push_str(emit_name);
        let hints = if in_svg { String::new() } else { presentational_hints(&tag) };
        let mut style: Option<&str> = None;
        for a in &tag.attrs {
            let an: &str = &a.name.local;
            let an_lower = an.to_ascii_lowercase();
            if !attr_allowed(name, &an_lower, &a.value, in_svg) {
                self.removed = self.removed.saturating_add(1);
                continue;
            }
            if an_lower == "style" {
                style = Some(&a.value);
                continue;
            }
            self.out.push(' ');
            self.out.push_str(an);
            self.out.push_str("=\"");
            if matches!(an_lower.as_str(), "align" | "valign" | "clear") {
                // Blitz compara `align` sensible a mayúsculas (`ALIGN=CENTER` no centraba).
                escape_into(&mut self.out, &a.value.to_ascii_lowercase(), true);
            } else {
                escape_into(&mut self.out, &a.value, true);
            }
            self.out.push('"');
        }
        if !hints.is_empty() || style.is_some() {
            // Las pistas van primero: el estilo propio del elemento las vence.
            self.out.push_str(" style=\"");
            escape_into(&mut self.out, &hints, true);
            escape_into(&mut self.out, style.unwrap_or(""), true);
            self.out.push('"');
        }
        if tag.self_closing {
            self.out.push_str(" /");
        }
        self.out.push('>');
        self.raw_verbatim = matches!(result, TokenSinkResult::RawData(RawKind::Rawtext | RawKind::ScriptData));
        result
    }

    fn meta_allowed(&mut self, tag: &Tag) -> bool {
        if attr(tag, "http-equiv").is_some() {
            // refresh, set-cookie, content-security-policy…: nada de eso aplica a un correo.
            return attr(tag, "http-equiv").is_some_and(|v| v.eq_ignore_ascii_case("content-type"));
        }
        if attr(tag, "charset").is_some() {
            return true;
        }
        let name = attr(tag, "name").unwrap_or_default().to_ascii_lowercase();
        if name == "color-scheme" || name == "supported-color-schemes" {
            if attr(tag, "content").is_some_and(|c| c.to_ascii_lowercase().contains("dark")) {
                self.supports_dark = true;
            }
            return true;
        }
        name == "viewport"
    }

    fn text(&mut self, text: &str) {
        if self.skipping.is_some() {
            return;
        }
        if self.raw_verbatim {
            // Contenido de <style> (o <xmp>): se copia sin escapar; el tokenizador garantiza que no
            // contiene su propia etiqueta de cierre.
            if text.contains("prefers-color-scheme") {
                self.supports_dark = true;
            }
            self.out.push_str(text);
        } else {
            escape_into(&mut self.out, text, false);
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
            Token::DoctypeToken(d) => {
                if st.out.is_empty() {
                    // Se conserva el doctype: decide modo estándar o «quirks», y muchos boletines
                    // dependen de quirks para sus tablas.
                    st.out.push_str("<!DOCTYPE ");
                    st.out.push_str(d.name.as_deref().unwrap_or("html"));
                    if let Some(p) = &d.public_id {
                        st.out.push_str(" PUBLIC \"");
                        st.out.push_str(&p.replace('"', ""));
                        st.out.push('"');
                        if let Some(s) = &d.system_id {
                            st.out.push_str(" \"");
                            st.out.push_str(&s.replace('"', ""));
                            st.out.push('"');
                        }
                    } else if let Some(s) = &d.system_id {
                        st.out.push_str(" SYSTEM \"");
                        st.out.push_str(&s.replace('"', ""));
                        st.out.push('"');
                    }
                    st.out.push('>');
                }
                TokenSinkResult::Continue
            }
            // Comentarios (y condicionales de Outlook), NUL, EOF y errores de análisis: fuera.
            Token::CommentToken(_) | Token::NullCharacterToken | Token::EOFToken | Token::ParseError(_) => {
                TokenSinkResult::Continue
            }
        }
    }
}

/// Estado de texto crudo que pide el elemento (como haría el constructor de árbol).
fn raw_kind(name: &str) -> TokenSinkResult<()> {
    match name {
        "style" | "xmp" | "iframe" | "noembed" | "noframes" => TokenSinkResult::RawData(RawKind::Rawtext),
        "script" => TokenSinkResult::RawData(RawKind::ScriptData),
        "title" | "textarea" => TokenSinkResult::RawData(RawKind::Rcdata),
        _ => TokenSinkResult::Continue,
    }
}

fn attr<'a>(tag: &'a Tag, name: &str) -> Option<&'a str> {
    tag.attrs.iter().find(|a| str::eq_ignore_ascii_case(&a.name.local, name)).map(|a| &*a.value)
}

/// Atributos de presentación con valor libre, convertidos a CSS en línea (los de valor acotado
/// —`border`, `cellpadding`, `valign`…— los resuelve la hoja de `ua.rs`). Solo se aceptan valores
/// con caracteres inocuos, para que no puedan inyectar otras declaraciones.
fn presentational_hints(tag: &Tag) -> String {
    let name: &str = &tag.name;
    let mut css = String::new();
    let own_style = attr(tag, "style").unwrap_or("").to_ascii_lowercase();
    match name {
        "font" => {
            if let Some(face) = attr(tag, "face").filter(|v| css_safe(v)) {
                css.push_str(&format!("font-family:{face};"));
            }
            if let Some(color) = attr(tag, "color").filter(|v| css_safe(v)) {
                css.push_str(&format!("color:{color};"));
            }
        }
        "body" => {
            if let Some(color) = attr(tag, "text").filter(|v| css_safe(v)) {
                css.push_str(&format!("color:{color};"));
            }
        }
        _ => {}
    }
    if matches!(name, "body" | "table" | "td" | "th" | "tr")
        && !own_style.contains("background")
        && let Some(bg) = attr(tag, "background").map(str::trim).filter(|v| !v.is_empty() && url_allowed(name, v))
    {
        let mut url = String::with_capacity(bg.len());
        for c in bg.chars() {
            match c {
                '"' | '\'' | '(' | ')' | '\\' => url.push_str(&format!("%{:02X}", c as u32)),
                c if c.is_whitespace() || c.is_control() => {}
                c => url.push(c),
            }
        }
        css.push_str(&format!("background-image:url(\"{url}\");"));
    }
    if matches!(name, "table" | "td" | "th")
        && !own_style.contains("border-color")
        && let Some(color) = attr(tag, "bordercolor").filter(|v| css_safe(v))
    {
        css.push_str(&format!("border-color:{color};"));
    }
    css
}

/// ¿Valor seguro para pegar en una declaración CSS? (nombres de fuente, colores).
fn css_safe(v: &str) -> bool {
    !v.trim().is_empty()
        && v.len() < 200
        && v.chars().all(|c| c.is_alphanumeric() || matches!(c, ' ' | ',' | '-' | '_' | '#' | '.' | '\''))
}

fn attr_allowed(element: &str, name: &str, value: &str, in_svg: bool) -> bool {
    if name.starts_with("on") || matches!(name, "srcdoc" | "formaction" | "ping" | "http-equiv" | "nonce") {
        return false;
    }
    if element == "form" && matches!(name, "action" | "method" | "target" | "enctype") {
        return false;
    }
    // Estilos con comportamiento ejecutable de IE/Firefox antiguos.
    if name == "style" {
        let v = value.to_ascii_lowercase();
        return !(v.contains("expression(") || v.contains("-moz-binding") || v.contains("behavior:"));
    }
    if in_svg && name.ends_with("href") {
        let v = normalize_url(value);
        return v.starts_with('#') || (v.starts_with("data:image/") && !v.starts_with("data:image/svg"));
    }
    if name == "srcset" {
        return value.split(',').all(|part| url_allowed(element, part.split_whitespace().next().unwrap_or("")));
    }
    if URL_ATTRS.contains(&name) {
        return url_allowed(element, value);
    }
    true
}

/// Quita lo que los navegadores ignoran dentro de una URL (espacios iniciales, controles,
/// tabuladores y saltos de línea) y pasa a minúsculas, para que `jav&#x09;ascript:` no se cuele.
fn normalize_url(value: &str) -> String {
    value
        .trim_start_matches(|c: char| c <= ' ')
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r') && !c.is_control())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Esquema de una URL normalizada, si tiene.
fn scheme(url: &str) -> Option<&str> {
    let end = url.find([':', '/', '?', '#'])?;
    if url.get(end..end + 1) != Some(":") {
        return None;
    }
    url.get(..end)
}

fn url_allowed(element: &str, value: &str) -> bool {
    let v = normalize_url(value);
    match scheme(&v) {
        // Relativa o fragmento: no hay base (no dejamos <base>), así que no carga nada externo.
        None => true,
        Some("http" | "https") => true,
        Some("mailto" | "tel" | "sms") => matches!(element, "a" | "area"),
        Some("cid") => !matches!(element, "a" | "area"),
        Some("data") => {
            !matches!(element, "a" | "area") && v.starts_with("data:image/") && !v.starts_with("data:image/svg")
        }
        Some(_) => false,
    }
}

/// Escapa texto o un valor de atributo para HTML.
fn escape_into(out: &mut String, s: &str, attr: bool) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if attr => out.push_str("&quot;"),
            '\u{a0}' => out.push_str("&nbsp;"),
            _ => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(h: &str) -> String {
        sanitize(h).html
    }

    #[test]
    fn removes_script_and_content() {
        let out = s("<p>a</p><script>alert(1)</script><p>b</p>");
        assert!(!out.contains("alert"));
        assert!(!out.contains("script"));
        assert!(out.contains("<p>a</p>") && out.contains("<p>b</p>"));
    }

    #[test]
    fn script_with_fake_tags_inside() {
        let out = s("<script>var x='<p>'; document.write('</scr'+'ipt>')</script><b>ok</b>");
        assert!(!out.contains("document"), "{out}");
        assert!(out.contains("<b>ok</b>"));
    }

    #[test]
    fn nested_and_uppercase_script() {
        let out = s("<SCRIPT SRC=//evil/x.js></SCRIPT><ScRiPt>a()</sCrIpT>x");
        assert!(!out.to_ascii_lowercase().contains("script"), "{out}");
        assert!(out.ends_with('x'));
    }

    #[test]
    fn removes_event_handlers() {
        let out = s(r#"<img src="cid:a" onerror="alert(1)" OnLoad=x><body onload=y><svg onload=z>"#);
        assert!(!out.to_ascii_lowercase().contains("onerror"));
        assert!(!out.to_ascii_lowercase().contains("onload"));
        assert!(out.contains(r#"src="cid:a""#));
    }

    #[test]
    fn javascript_urls_obfuscated() {
        for v in [
            "javascript:alert(1)",
            "JaVaScRiPt:alert(1)",
            " \u{1}javascript:alert(1)",
            "java\tscript:alert(1)",
            "java\nscript:alert(1)",
            "vbscript:msgbox(1)",
            "data:text/html;base64,PHNjcmlwdD4=",
            "file:///etc/passwd",
        ] {
            let html = format!("<a href=\"{v}\">x</a>");
            let out = s(&html);
            assert!(!out.contains("href"), "{v:?} → {out}");
        }
        // Entidades: el tokenizador decodifica &#x09; antes de que lo veamos.
        let out = s("<a href=\"jav&#x09;ascript:alert(1)\">x</a><a href=\"&#106;avascript:x\">y</a>");
        assert!(!out.contains("href"), "{out}");
    }

    #[test]
    fn keeps_good_links() {
        let out =
            s(r##"<a href="https://example.com/a?b=1&amp;c=2">x</a><a href="mailto:a@b.c">m</a><a href="#top">t</a>"##);
        assert!(out.contains(r#"href="https://example.com/a?b=1&amp;c=2""#), "{out}");
        assert!(out.contains("mailto:a@b.c"));
        assert!(out.contains("#top"));
    }

    #[test]
    fn drops_iframe_object_embed_form_action() {
        let out = s(
            r#"<iframe src="https://e.vil"><p>in</p></iframe><object data=x><embed src=y></object><embed src=z><form action="https://phish" method=post><input name=p></form>"#,
        );
        for bad in ["iframe", "object", "embed", "e.vil", "phish", "method"] {
            assert!(!out.contains(bad), "{bad} en {out}");
        }
        assert!(out.contains("<form>") && out.contains("<input name=\"p\">"), "{out}");
    }

    #[test]
    fn drops_meta_refresh_and_base() {
        let out = s(
            r#"<meta http-equiv="refresh" content="0;url=https://e.vil"><base href="https://e.vil/"><meta name="color-scheme" content="light dark">"#,
        );
        assert!(!out.contains("refresh") && !out.contains("base"), "{out}");
        assert!(out.contains("color-scheme"));
        assert!(sanitize(r#"<meta name="color-scheme" content="light dark">"#).supports_dark);
    }

    #[test]
    fn dark_mode_detected_from_style() {
        let r = sanitize("<style>@media (prefers-color-scheme: dark) { body { background: #000 } }</style>");
        assert!(r.supports_dark);
        assert!(!sanitize("<style>body{}</style>").supports_dark);
    }

    #[test]
    fn style_content_kept_verbatim() {
        let css = "td > p { color: red } a[href^=\"http\"]::after { content: \"<b>\" }";
        let out = s(&format!("<style>{css}</style>"));
        assert!(out.contains(css), "{out}");
    }

    #[test]
    fn style_attr_expression_dropped() {
        let out = s(r#"<div style="width: expression(alert(1))">x</div><p style="color:red">y</p>"#);
        assert!(!out.contains("expression"));
        assert!(out.contains("color:red"));
    }

    #[test]
    fn svg_external_href_dropped() {
        let out =
            s(r##"<svg><image href="/etc/passwd"/><use xlink:href="https://e.vil/x.svg#a"/><use href="#ok"/></svg>"##);
        assert!(!out.contains("passwd") && !out.contains("e.vil"), "{out}");
        assert!(out.contains("#ok"));
    }

    #[test]
    fn svg_script_dropped() {
        let out = s("<svg><script>alert(1)</script><circle r=1 /></svg>");
        assert!(!out.contains("alert"));
        assert!(out.contains("circle"));
    }

    #[test]
    fn conditional_comments_dropped() {
        let out = s(
            "<!--[if mso]><table><tr><td>MSO</td></tr></table><![endif]--><!--[if !mso]><!--><div>web</div><!--<![endif]-->",
        );
        assert!(!out.contains("MSO"));
        assert!(out.contains("<div>web</div>"));
    }

    #[test]
    fn link_only_stylesheets() {
        let out = s(
            r#"<link rel="prefetch" href="https://t"><link rel="stylesheet" href="https://cdn/x.css"><link rel=icon href=x>"#,
        );
        assert!(!out.contains("prefetch") && !out.contains("icon"));
        assert!(out.contains("stylesheet"));
    }

    #[test]
    fn data_images_only() {
        assert!(s(r#"<img src="data:image/png;base64,AAAA">"#).contains("data:image/png"));
        assert!(!s(r#"<img src="data:image/svg+xml,<svg onload=alert(1)>">"#).contains("data:"));
        assert!(!s(r#"<img src="data:text/html,x">"#).contains("data:"));
    }

    #[test]
    fn attribute_quotes_escaped() {
        let out = s(r#"<p title='a"><script>x</script>'>t</p>"#);
        assert!(out.contains("title=\"a&quot;&gt;&lt;script&gt;x&lt;/script&gt;\""), "{out}");
    }

    #[test]
    fn text_escaped() {
        let out = s("<p>1 &lt; 2 &amp;&amp; &lt;script&gt;</p>");
        assert!(out.contains("1 &lt; 2 &amp;&amp; &lt;script&gt;"), "{out}");
    }

    #[test]
    fn plaintext_becomes_pre() {
        let out = s("<plaintext><script>x</script>");
        assert!(out.starts_with("<pre>"), "{out}");
        assert!(out.contains("&lt;script&gt;"));
    }

    #[test]
    fn noscript_unwrapped() {
        assert_eq!(s("<noscript><p>hola</p></noscript>"), "<p>hola</p>");
    }

    #[test]
    fn unclosed_script_eats_rest() {
        let out = s("<p>a</p><script>alert(1)");
        assert_eq!(out, "<p>a</p>");
    }

    #[test]
    fn font_and_background_hints() {
        let out = s(r##"<font face="Arial, Helvetica" color="#FFFFFF" size="5">x</font>"##);
        assert!(out.contains(r##"style="font-family:Arial, Helvetica;color:#FFFFFF;""##), "{out}");
        let out = s(r#"<td background="cid:bg@x" style="padding:4px">y</td>"#);
        assert!(out.contains(r#"background-image:url(&quot;cid:bg@x&quot;);padding:4px"#), "{out}");
        // El propio estilo con background manda; javascript: nunca llega al CSS.
        assert!(!s(r#"<td background="cid:a" style="background:red">"#).contains("background-image"));
        assert!(!s(r#"<td background="javascript:alert(1)">"#).contains("javascript"));
    }

    #[test]
    fn font_hint_injection_rejected() {
        let out = s(r#"<font color="red;background:url(https://t/p)">x</font>"#);
        assert!(!out.contains("style="), "{out}");
    }

    #[test]
    fn doctype_kept() {
        assert!(s("<!DOCTYPE html><p>x").starts_with("<!DOCTYPE html>"));
        let legacy = s(
            r#"<!DOCTYPE html PUBLIC "-//W3C//DTD XHTML 1.0 Transitional//EN" "http://www.w3.org/TR/xhtml1/DTD/xhtml1-transitional.dtd"><p>x"#,
        );
        assert!(legacy.contains("Transitional"), "{legacy}");
    }
}
