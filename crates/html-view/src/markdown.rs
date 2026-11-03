//! UX-8: notas en Markdown → HTML para la vista previa nativa (luego pasa por [`crate::render`],
//! es decir, por el saneador y Blitz, igual que un correo).
//!
//! - `pulldown-cmark` con tablas, tachado, listas de tareas, notas al pie y matemáticas; el bloque
//!   YAML del principio (front matter) no se muestra.
//! - Enlaces: solo `http`, `https`, `mailto` y `iureditor`. Los `iureditor:` se reescriben como
//!   `#iuredesk-link-N` (el saneador no deja pasar esquemas desconocidos y así ni siquiera hace
//!   falta); los demás esquemas y las rutas relativas pierden el enlace y quedan como texto.
//! - Imágenes: las relativas se validan con [`local_image_path`] (nunca absolutas ni con `..`) y
//!   se reescriben como `cid:note-img-N`; quien llama lee los bytes (acotados, dentro de la carpeta
//!   de notas) y los entrega como recursos en línea. Las remotas quedan tal cual: el proveedor de
//!   red las bloquea salvo que se permitan (UX-5). Cualquier otro esquema se descarta.
//! - Casillas de tareas: `#iuredesk-task-N` (N = orden en el documento), para que la UI las marque
//!   con [`toggle_task`], que usa el mismo análisis.
//! - Mapas mentales de IureEditor (`> [!MINDMAP] Centro` + lista, UX-12): se dibujan como SVG
//!   propio ([`crate::mindmap`]) y se entregan como imagen `cid:note-diagram-N`, en
//!   [`NoteHtml::diagrams`]; con la vista `map` la lista no se muestra.
//! - Mermaid y matemáticas: no hay motor en Rust puro que Blitz pueda pintar (Blitz no pinta
//!   MathML y los renderizadores de Mermaid son JS), así que salen como bloque de código con una
//!   etiqueta que invita a abrirlos en IureEditor.
//! - El HTML crudo de la nota pasa tal cual: lo neutraliza el saneador de `render`.
//! - Anidamiento acotado ([`MAX_DEPTH`]): una nota con 10 000 `>` no produce un árbol que agote la
//!   pila de estilo/layout.

use std::fmt::Write as _;

use pulldown_cmark::{CodeBlockKind, CowStr, Event, Options, Parser, Tag, TagEnd};

/// Contenedores anidados como mucho; más adentro se pierden las etiquetas (no el texto).
pub const MAX_DEPTH: usize = 40;
/// Imágenes locales por nota.
pub const MAX_LOCAL_IMAGES: usize = 64;
/// Prefijo de los enlaces de casilla.
pub const TASK_HREF: &str = "#iuredesk-task-";
/// Prefijo de los enlaces a IureEditor reescritos.
pub const DEEP_HREF: &str = "#iuredesk-link-";

/// Colores (RGB) y medidas de la hoja de estilos de la nota, tomados del tema de la UI.
#[derive(Clone, Debug, PartialEq)]
pub struct NoteStyle {
    pub text: [u8; 3],
    pub muted: [u8; 3],
    pub accent: [u8; 3],
    /// Fondo de bloques de código, cabeceras de tabla y etiquetas.
    pub code_bg: [u8; 3],
    /// Bordes de tablas, citas y separadores.
    pub border: [u8; 3],
    /// Tamaño de letra del cuerpo en px CSS (según la densidad).
    pub font_px: f32,
}

impl Default for NoteStyle {
    fn default() -> Self {
        Self {
            text: [0x1f, 0x23, 0x28],
            muted: [0x5f, 0x66, 0x6d],
            accent: [0x1a, 0x5f, 0xb4],
            code_bg: [0xf3, 0xf4, 0xf6],
            border: [0xd8, 0xdb, 0xe0],
            font_px: 14.0,
        }
    }
}

/// Opciones de la conversión.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NoteOptions {
    pub style: NoteStyle,
    /// Etiqueta de los bloques Mermaid («Diagrama Mermaid — ábrelo en IureEditor»).
    pub mermaid_label: String,
    /// Etiqueta de las fórmulas en bloque.
    pub math_label: String,
    /// Centro de un mapa mental sin título («Mapa mental»).
    pub mindmap_label: String,
}

/// Resultado de la conversión.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NoteHtml {
    /// Documento HTML completo (con su `<style>`), aún sin sanear.
    pub html: String,
    /// Imágenes locales en orden: la `i` se pide como [`image_cid`]`(i)`. Rutas relativas ya
    /// validadas por [`local_image_path`].
    pub images: Vec<String>,
    /// Enlaces `iureditor:` originales; el `i` es `#iuredesk-link-i`.
    pub deep_links: Vec<String>,
    /// Casillas de tareas.
    pub tasks: usize,
    /// Diagramas dibujados aquí (mapas mentales): `cid` y SVG, para entregarlos como imágenes.
    pub diagrams: Vec<(String, Vec<u8>)>,
}

/// Qué hacer al pulsar un enlace de la vista previa.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NoteLink {
    /// Marcar o desmarcar la casilla `n` (ver [`toggle_task`]).
    Task(usize),
    /// Abrir fuera: `http`, `https`, `mailto` o un deep link `iureditor:`.
    Open(String),
    /// Nada (ancla interna, nota al pie…).
    None,
}

impl NoteHtml {
    /// Clasifica el `href` de una zona clicable del render.
    pub fn link(&self, href: &str) -> NoteLink {
        classify(href, &self.deep_links)
    }
}

/// Como [`NoteHtml::link`], con solo la lista de deep links.
pub fn classify(href: &str, deep_links: &[String]) -> NoteLink {
    let h = href.trim();
    if let Some(n) = h.strip_prefix(TASK_HREF) {
        return n.parse().map_or(NoteLink::None, NoteLink::Task);
    }
    if let Some(n) = h.strip_prefix(DEEP_HREF) {
        return n
            .parse::<usize>()
            .ok()
            .and_then(|i| deep_links.get(i))
            .map_or(NoteLink::None, |u| NoteLink::Open(u.clone()));
    }
    match link_kind(h) {
        LinkKind::External => NoteLink::Open(h.to_string()),
        _ => NoteLink::None,
    }
}

/// Esquema interno con que se marca el sitio de un diagrama antes de convertir.
const DIAGRAM_SCHEME: &str = "iuredesk-diagram:";

/// `cid` del diagrama `i`.
pub fn diagram_cid(i: usize) -> String {
    format!("note-diagram-{i}@iuredesk")
}

/// La nota con cada mapa mental cambiado por su imagen (y, salvo en la vista `map`, su lista
/// debajo como cita), y los SVG.
fn with_diagrams(md: &str, opts: &NoteOptions) -> (String, Vec<(String, Vec<u8>)>) {
    let blocks = crate::mindmap::find(md);
    if blocks.is_empty() {
        return (md.to_string(), Vec::new());
    }
    let mut out = String::with_capacity(md.len() + blocks.len() * 64);
    let mut diagrams = Vec::new();
    let mut last = 0usize;
    for (i, b) in blocks.iter().enumerate() {
        out.push_str(md.get(last..b.range.start).unwrap_or(""));
        let mut root = b.root.clone();
        if root.label.is_empty() {
            root.label = opts.mindmap_label.clone();
        }
        let alt = escape(&root.label).replace(['[', ']'], "");
        let _ = write!(out, "\n![{alt}]({DIAGRAM_SCHEME}{i})\n\n");
        if b.view == crate::mindmap::View::ListAndMap {
            for l in &b.body {
                let _ = writeln!(out, "> {l}");
            }
            out.push('\n');
        }
        diagrams.push((diagram_cid(i), crate::mindmap::svg(&root, &opts.style).into_bytes()));
        last = b.range.end;
    }
    out.push_str(md.get(last..).unwrap_or(""));
    (out, diagrams)
}

/// `cid` de la imagen local `i`.
pub fn image_cid(i: usize) -> String {
    format!("note-img-{i}@iuredesk")
}

fn parser_options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_MATH
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LinkKind {
    /// http, https, mailto: se abre fuera.
    External,
    /// iureditor: deep link.
    Deep,
    /// `#ancla` (notas al pie).
    Fragment,
    /// Cualquier otra cosa: sin enlace.
    Drop,
}

/// Quita controles y espacios que los navegadores ignoran, y pasa a minúsculas (como el saneador).
fn normalized(url: &str) -> String {
    url.trim_start_matches(|c: char| c <= ' ')
        .chars()
        .filter(|c| !c.is_control())
        .flat_map(char::to_lowercase)
        .collect()
}

fn link_kind(url: &str) -> LinkKind {
    let v = normalized(url);
    if v.starts_with('#') {
        return LinkKind::Fragment;
    }
    if v.starts_with("https://") || v.starts_with("http://") || v.starts_with("mailto:") {
        return LinkKind::External;
    }
    if v.starts_with("iureditor:") {
        return LinkKind::Deep;
    }
    LinkKind::Drop
}

/// Ruta relativa segura de una imagen local, o `None`. Acepta `img/foto.png`, `./a b.png` o
/// `img/a%20b.png`; rechaza rutas absolutas, unidades de Windows, `..`, esquemas (`file:`…),
/// barras invertidas, consultas y caracteres de control. Lo que devuelve solo tiene componentes
/// normales separados por `/`.
pub fn local_image_path(src: &str) -> Option<String> {
    let s = src.trim();
    if s.is_empty() || s.len() > 1024 || s.starts_with('/') || s.contains('\\') || s.contains(['?', '#']) {
        return None;
    }
    // Esquema (`file:`, `c:`…): cualquier `:` antes de la primera `/`.
    let first = s.split('/').next().unwrap_or("");
    if first.contains(':') {
        return None;
    }
    let decoded = percent_decode(s)?;
    if decoded.contains('\\') || decoded.starts_with('/') || decoded.chars().any(char::is_control) {
        return None;
    }
    let mut parts = Vec::new();
    for p in decoded.split('/') {
        match p {
            "" | "." => {}
            ".." => return None,
            p if p.contains(':') => return None,
            p => parts.push(p),
        }
    }
    if parts.is_empty() || parts.len() > 16 {
        return None;
    }
    Some(parts.join("/"))
}

fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while let Some(&c) = b.get(i) {
        if c == b'%' {
            let h = b.get(i + 1).and_then(|x| (*x as char).to_digit(16));
            let l = b.get(i + 2).and_then(|x| (*x as char).to_digit(16));
            match (h, l) {
                (Some(h), Some(l)) => {
                    out.push((h * 16 + l) as u8);
                    i += 3;
                    continue;
                }
                _ => return None,
            }
        }
        out.push(c);
        i += 1;
    }
    String::from_utf8(out).ok()
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

fn rgb(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

/// Hoja de estilos de la nota.
fn stylesheet(s: &NoteStyle) -> String {
    let font = if s.font_px.is_finite() { s.font_px.clamp(9.0, 32.0) } else { 14.0 };
    let (text, muted, accent, code_bg, border) =
        (rgb(s.text), rgb(s.muted), rgb(s.accent), rgb(s.code_bg), rgb(s.border));
    let mut css = String::with_capacity(2048);
    let _ = write!(
        css,
        "html {{ background: transparent; }}\n\
         body {{ margin: 0; padding: 0 2px 16px 2px; color: {text}; font-family: sans-serif; font-size: {font}px; \
         line-height: 1.5; overflow-wrap: break-word; }}\n\
         h1, h2, h3, h4, h5, h6 {{ line-height: 1.25; margin: 1.1em 0 0.45em 0; font-weight: 600; }}\n\
         h1 {{ font-size: 1.6em; border-bottom: 1px solid {border}; padding-bottom: 0.25em; }}\n\
         h2 {{ font-size: 1.35em; border-bottom: 1px solid {border}; padding-bottom: 0.2em; }}\n\
         h3 {{ font-size: 1.15em; }} h4 {{ font-size: 1em; }} h5, h6 {{ font-size: 0.92em; color: {muted}; }}\n\
         p {{ margin: 0 0 0.75em 0; }}\n\
         a {{ color: {accent}; text-decoration: underline; }}\n\
         ul, ol {{ margin: 0 0 0.75em 0; padding-left: 1.6em; }}\n\
         li {{ margin: 0.15em 0; }}\n\
         li > p {{ margin: 0; }}\n\
         li.task {{ list-style-type: none; }}\n\
         a.task {{ text-decoration: none; }}\n\
         span.box {{ display: inline-block; width: 0.95em; height: 0.95em; line-height: 0.95em; margin: 0 0.45em 0 -1.35em; \
         border: 1.5px solid {muted}; border-radius: 3px; text-align: center; font-size: 0.9em; font-weight: 700; \
         vertical-align: -0.1em; color: {accent}; }}\n\
         span.box.done {{ border-color: {accent}; }}\n\
         li.task.done {{ color: {muted}; }}\n\
         blockquote {{ margin: 0 0 0.75em 0; padding: 0.1em 0 0.1em 0.9em; border-left: 3px solid {border}; color: {muted}; }}\n\
         code {{ font-family: monospace; font-size: 0.9em; background: {code_bg}; border-radius: 3px; padding: 0.1em 0.3em; }}\n\
         pre {{ font-family: monospace; font-size: 0.9em; line-height: 1.4; background: {code_bg}; border: 1px solid {border}; \
         border-radius: 6px; padding: 0.7em 0.9em; margin: 0 0 0.8em 0; white-space: pre-wrap; }}\n\
         pre code {{ background: transparent; padding: 0; font-size: 1em; }}\n\
         table {{ border-collapse: collapse; margin: 0 0 0.9em 0; }}\n\
         th, td {{ border: 1px solid {border}; padding: 0.3em 0.65em; vertical-align: top; }}\n\
         th {{ background: {code_bg}; font-weight: 600; }}\n\
         hr {{ border: 0; border-top: 1px solid {border}; margin: 1.1em 0; }}\n\
         img {{ max-width: 100%; }}\n\
         del {{ color: {muted}; }}\n\
         .label {{ font-family: sans-serif; font-size: 0.8em; color: {muted}; margin: 0 0 0.35em 0; }}\n\
         div.special {{ border: 1px dashed {border}; border-radius: 6px; padding: 0.5em 0.7em 0.1em 0.7em; margin: 0 0 0.8em 0; }}\n\
         div.special pre {{ border: 0; margin: 0 0 0.4em 0; padding: 0.4em 0.5em; }}\n\
         .footnote-definition {{ font-size: 0.88em; color: {muted}; margin: 0.3em 0; }}\n\
         .footnote-definition-label {{ margin-right: 0.4em; }}\n\
         .footnote-definition > p {{ display: inline; }}\n\
         sup {{ font-size: 0.75em; }}\n"
    );
    css
}

/// Marco abierto durante la conversión (para cerrar lo que se abrió igual).
enum Frame {
    /// Se emitió tal cual.
    Kept,
    /// Se omitió (anidamiento excesivo o front matter): su cierre también.
    Dropped,
    /// Enlace sin destino permitido: `<span>` en lugar de `<a>`.
    Span,
    /// Bloque Mermaid / fórmula: se cierra el `</code></pre></div>` propio.
    Special,
    /// Elemento de lista de tareas.
    TaskItem,
}

/// Convierte la nota en HTML. Nunca falla (Markdown no tiene errores); la salida aún debe pasar
/// por el saneador, cosa que hace [`crate::render`].
pub fn to_html(md: &str, opts: &NoteOptions) -> NoteHtml {
    let (md, diagrams) = with_diagrams(md, opts);
    let md = md.as_str();
    let events: Vec<Event<'_>> = Parser::new_ext(md, parser_options()).collect();
    let mut out: Vec<Event<'_>> = Vec::with_capacity(events.len());
    let mut stack: Vec<Frame> = Vec::new();
    let mut depth = 0usize;
    let mut images: Vec<String> = Vec::new();
    let mut deep_links: Vec<String> = Vec::new();
    let mut tasks = 0usize;
    let mut in_metadata = false;
    let mut iter = events.into_iter().peekable();
    while let Some(ev) = iter.next() {
        if in_metadata {
            if matches!(ev, Event::End(TagEnd::MetadataBlock(_))) {
                in_metadata = false;
                stack.pop();
            }
            continue;
        }
        match ev {
            Event::Start(tag) => {
                if let Tag::MetadataBlock(_) = tag {
                    in_metadata = true;
                    stack.push(Frame::Dropped);
                    continue;
                }
                // Imágenes y enlaces no son contenedores de bloque: no cuentan para el límite.
                let container = !matches!(
                    tag,
                    Tag::Image { .. }
                        | Tag::Link { .. }
                        | Tag::Emphasis
                        | Tag::Strong
                        | Tag::Strikethrough
                        | Tag::Superscript
                        | Tag::Subscript
                );
                if container && depth >= MAX_DEPTH {
                    stack.push(Frame::Dropped);
                    continue;
                }
                if container {
                    depth += 1;
                }
                match tag {
                    Tag::Link { dest_url, title, link_type, id } => match link_kind(&dest_url) {
                        LinkKind::External | LinkKind::Fragment => {
                            out.push(Event::Start(Tag::Link { dest_url, title, link_type, id }));
                            stack.push(Frame::Kept);
                        }
                        LinkKind::Deep => {
                            let href = format!("{DEEP_HREF}{}", deep_links.len());
                            deep_links.push(dest_url.trim().to_string());
                            out.push(Event::Start(Tag::Link { dest_url: href.into(), title, link_type, id }));
                            stack.push(Frame::Kept);
                        }
                        LinkKind::Drop => {
                            out.push(Event::Html("<span>".into()));
                            stack.push(Frame::Span);
                        }
                    },
                    Tag::Image { dest_url, title, link_type, id } => {
                        let src: CowStr<'_> = match dest_url.strip_prefix(DIAGRAM_SCHEME) {
                            Some(n) => n
                                .parse::<usize>()
                                .ok()
                                .filter(|n| *n < diagrams.len())
                                .map(|n| format!("cid:{}", diagram_cid(n)))
                                .unwrap_or_default()
                                .into(),
                            None => image_src(&dest_url, &mut images).into(),
                        };
                        out.push(Event::Start(Tag::Image { dest_url: src, title, link_type, id }));
                        stack.push(Frame::Kept);
                    }
                    Tag::CodeBlock(CodeBlockKind::Fenced(lang)) if is_special(&lang) => {
                        let label = if is_math(&lang) { &opts.math_label } else { &opts.mermaid_label };
                        out.push(Event::Html(
                            format!("<div class=\"special\"><div class=\"label\">{}</div><pre><code>", escape(label))
                                .into(),
                        ));
                        stack.push(Frame::Special);
                    }
                    Tag::Item if is_task_next(iter.peek()) => {
                        let done = matches!(iter.peek(), Some(Event::TaskListMarker(true)));
                        out.push(Event::Html(
                            if done { "<li class=\"task done\">" } else { "<li class=\"task\">" }.into(),
                        ));
                        stack.push(Frame::TaskItem);
                    }
                    tag => {
                        out.push(Event::Start(tag));
                        stack.push(Frame::Kept);
                    }
                }
            }
            Event::End(end) => {
                let frame = stack.pop().unwrap_or(Frame::Kept);
                let container = !matches!(
                    end,
                    TagEnd::Image
                        | TagEnd::Link
                        | TagEnd::Emphasis
                        | TagEnd::Strong
                        | TagEnd::Strikethrough
                        | TagEnd::Superscript
                        | TagEnd::Subscript
                );
                match frame {
                    Frame::Dropped => continue,
                    Frame::Kept => out.push(Event::End(end)),
                    Frame::Span => out.push(Event::Html("</span>".into())),
                    Frame::Special => out.push(Event::Html("</code></pre></div>".into())),
                    Frame::TaskItem => out.push(Event::Html("</li>\n".into())),
                }
                if container {
                    depth = depth.saturating_sub(1);
                }
            }
            Event::TaskListMarker(done) => {
                let n = tasks;
                tasks += 1;
                let (class, mark) = if done { ("box done", "✓") } else { ("box", "") };
                out.push(Event::InlineHtml(
                    format!("<a class=\"task\" href=\"{TASK_HREF}{n}\"><span class=\"{class}\">{mark}</span></a>")
                        .into(),
                ));
            }
            Event::InlineMath(m) => {
                out.push(Event::InlineHtml(format!("<code class=\"math\">{}</code>", escape(&m)).into()));
            }
            Event::DisplayMath(m) => {
                out.push(Event::InlineHtml(
                    format!(
                        "<div class=\"special\"><div class=\"label\">{}</div><pre><code>{}</code></pre></div>",
                        escape(&opts.math_label),
                        escape(m.trim())
                    )
                    .into(),
                ));
            }
            other => out.push(other),
        }
    }
    let mut body = String::with_capacity(md.len().saturating_mul(3) / 2);
    pulldown_cmark::html::push_html(&mut body, out.into_iter());
    let mut html = String::with_capacity(body.len().saturating_add(4096));
    html.push_str("<!DOCTYPE html><html><head><meta charset=\"utf-8\"><style>");
    html.push_str(&stylesheet(&opts.style));
    html.push_str("</style></head><body>");
    html.push_str(&body);
    html.push_str("</body></html>");
    NoteHtml { html, images, deep_links, tasks, diagrams }
}

fn is_math(lang: &str) -> bool {
    let l = lang.trim().to_ascii_lowercase();
    matches!(l.as_str(), "math" | "latex" | "tex" | "katex")
}

fn is_special(lang: &str) -> bool {
    is_math(lang) || lang.trim().eq_ignore_ascii_case("mermaid")
}

fn is_task_next(next: Option<&Event<'_>>) -> bool {
    matches!(next, Some(Event::TaskListMarker(_)))
}

/// Destino de una imagen tras las reglas de la vista previa.
fn image_src(dest: &str, images: &mut Vec<String>) -> String {
    let v = normalized(dest);
    if v.starts_with("https://") || v.starts_with("http://") {
        return dest.trim().to_string();
    }
    if v.starts_with("data:image/") && !v.starts_with("data:image/svg") {
        return dest.trim().to_string();
    }
    if let Some(rel) = local_image_path(dest) {
        if let Some(i) = images.iter().position(|p| *p == rel) {
            return format!("cid:{}", image_cid(i));
        }
        if images.len() < MAX_LOCAL_IMAGES {
            images.push(rel);
            return format!("cid:{}", image_cid(images.len() - 1));
        }
    }
    // Sin imagen: queda el texto alternativo.
    String::new()
}

/// Marca o desmarca la casilla `n` (en el orden de [`to_html`]) y devuelve el Markdown nuevo; solo
/// cambia el carácter entre corchetes. `None` si no existe.
pub fn toggle_task(md: &str, n: usize) -> Option<String> {
    // Las casillas de un mapa mental en vista `map` no se ven: no cuentan.
    let hidden: Vec<std::ops::Range<usize>> = crate::mindmap::find(md)
        .into_iter()
        .filter(|b| b.view == crate::mindmap::View::MapOnly)
        .map(|b| b.range)
        .collect();
    let mut seen = 0usize;
    for (ev, range) in Parser::new_ext(md, parser_options()).into_offset_iter() {
        if let Event::TaskListMarker(_) = ev
            && hidden.iter().any(|h| h.contains(&range.start))
        {
            continue;
        }
        if let Event::TaskListMarker(done) = ev {
            if seen == n {
                let slice = md.get(range.clone())?;
                // `[ ]`, `[x]` o `[X]`; el carácter del medio es ASCII.
                let open = slice.find('[')?;
                let inner = range.start.checked_add(open)?.checked_add(1)?;
                let c = md.get(inner..inner.checked_add(1)?)?;
                if !matches!(c, " " | "x" | "X") || md.get(inner + 1..inner + 2) != Some("]") {
                    return None;
                }
                let mut out = String::with_capacity(md.len());
                out.push_str(md.get(..inner)?);
                out.push_str(if done { " " } else { "x" });
                out.push_str(md.get(inner + 1..)?);
                return Some(out);
            }
            seen += 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn conv(md: &str) -> NoteHtml {
        to_html(
            md,
            &NoteOptions {
                mermaid_label: "Diagrama Mermaid".into(),
                mindmap_label: "Mapa mental".into(),
                math_label: "Fórmula".into(),
                ..Default::default()
            },
        )
    }

    #[test]
    fn gfm_extensions() {
        let h = conv("| a | b |\n|---|---|\n| 1 | 2 |\n\n~~no~~ y nota[^1]\n\n[^1]: al pie\n");
        assert!(h.html.contains("<table>"), "{}", h.html);
        assert!(h.html.contains("<td>1</td>"));
        assert!(h.html.contains("<del>no</del>"));
        assert!(h.html.contains("footnote-definition"));
    }

    #[test]
    fn front_matter_is_hidden() {
        let h = conv("---\ntags: [a, b]\npinned: true\n---\n# Hola\n");
        assert!(!h.html.contains("pinned"), "{}", h.html);
        assert!(h.html.contains("<h1>Hola</h1>"));
    }

    #[test]
    fn tasks_are_links_and_toggle() {
        let md = "- [ ] uno\n- [x] dos\n- tres\n";
        let h = conv(md);
        assert_eq!(h.tasks, 2);
        assert!(h.html.contains("href=\"#iuredesk-task-0\""));
        assert!(h.html.contains("<li class=\"task done\">"));
        assert_eq!(h.link("#iuredesk-task-1"), NoteLink::Task(1));
        assert_eq!(toggle_task(md, 0).as_deref(), Some("- [x] uno\n- [x] dos\n- tres\n"));
        assert_eq!(toggle_task(md, 1).as_deref(), Some("- [ ] uno\n- [ ] dos\n- tres\n"));
        assert_eq!(toggle_task(md, 2), None);
        // Con front matter: las posiciones siguen siendo las del texto original.
        let fm = "---\ntags: [x]\n---\n- [ ] a\n";
        assert_eq!(toggle_task(fm, 0).as_deref(), Some("---\ntags: [x]\n---\n- [x] a\n"));
        // Mayúscula.
        assert_eq!(toggle_task("* [X] a", 0).as_deref(), Some("* [ ] a"));
    }

    #[test]
    fn links_are_filtered() {
        let h = conv(
            "[w](https://e.org) [m](mailto:a@b.org) [x](javascript:alert(1)) [f](file:///etc/passwd) \
             [r](otra.md) [e](iureditor://open?path=/tmp/a.md)",
        );
        assert!(h.html.contains("href=\"https://e.org\""));
        assert!(h.html.contains("href=\"mailto:a@b.org\""));
        assert!(!h.html.contains("javascript"));
        assert!(!h.html.contains("file:"));
        assert!(!h.html.contains("otra.md"));
        assert_eq!(h.deep_links, vec!["iureditor://open?path=/tmp/a.md".to_string()]);
        assert_eq!(h.link("#iuredesk-link-0"), NoteLink::Open("iureditor://open?path=/tmp/a.md".into()));
        assert_eq!(h.link("https://e.org"), NoteLink::Open("https://e.org".into()));
        assert_eq!(h.link("#fn1"), NoteLink::None);
        assert_eq!(h.link("#iuredesk-link-9"), NoteLink::None);
    }

    #[test]
    fn images_local_remote_and_bad() {
        let h = conv(
            "![a](img/a.png) ![b](https://e.org/b.png) ![c](../fuera.png) ![d](/etc/x.png) ![e](file:///x.png) ![f](img/a.png)",
        );
        assert_eq!(h.images, vec!["img/a.png".to_string()]);
        assert_eq!(h.html.matches("cid:note-img-0@iuredesk").count(), 2);
        assert!(h.html.contains("https://e.org/b.png"));
        assert!(!h.html.contains("fuera"));
        assert!(!h.html.contains("/etc/x.png"));
        assert!(!h.html.contains("file:"));
    }

    #[test]
    fn local_paths() {
        assert_eq!(local_image_path("img/a%20b.png").as_deref(), Some("img/a b.png"));
        assert_eq!(local_image_path("./a.png").as_deref(), Some("a.png"));
        for bad in [
            "../a.png",
            "a/../../b.png",
            "/a.png",
            "C:/a.png",
            "c:a.png",
            "a\\b.png",
            "%2e%2e/a.png",
            "a%2f..%2f..%2fb",
            "",
            "x.png?a",
            "file:a.png",
        ] {
            assert_eq!(local_image_path(bad), None, "{bad}");
        }
    }

    #[test]
    fn mermaid_and_math_are_labelled_code() {
        let h = conv("```mermaid\ngraph TD; A-->B\n```\n\n$e=mc^2$ y\n\n$$\\int x$$\n");
        assert!(h.html.contains("Diagrama Mermaid"));
        assert!(h.html.contains("graph TD; A--&gt;B"));
        assert!(h.html.contains("<code class=\"math\">e=mc^2</code>"));
        assert!(h.html.contains("Fórmula"));
    }

    #[test]
    fn mindmaps_become_diagrams() {
        // UX-12: el mapa de la nota del dueño (vista `map`: sin la lista).
        let md = "Texto\n\n> [!MINDMAP|map] Idea central\n> - Analisis\n> - [ ] Diseño\n\n- [ ] fuera\n";
        let h = conv(md);
        assert_eq!(h.diagrams.len(), 1);
        assert_eq!(h.diagrams[0].0, diagram_cid(0));
        assert!(String::from_utf8_lossy(&h.diagrams[0].1).contains("Idea central"));
        assert!(h.html.contains("cid:note-diagram-0@iuredesk"), "{}", h.html);
        assert!(!h.html.contains("[!MINDMAP") && !h.html.contains("Analisis"), "{}", h.html);
        // La casilla escondida en el mapa no cuenta: la 0 es la de fuera.
        assert_eq!(h.tasks, 1);
        assert_eq!(toggle_task(md, 0).unwrap(), md.replace("- [ ] fuera", "- [x] fuera"));
        // Con lista: la lista se ve debajo del mapa y sus casillas cuentan en orden.
        let md = "> [!MINDMAP] Centro\n> - [ ] uno\n\n- [ ] dos\n";
        let h = conv(md);
        assert!(h.html.contains("uno") && h.html.contains("cid:note-diagram-0@iuredesk"));
        assert_eq!(h.tasks, 2);
        assert_eq!(toggle_task(md, 0).unwrap(), md.replace("[ ] uno", "[x] uno"));
        // Un `iuredesk-diagram:` escrito a mano que no existe no apunta a nada.
        assert!(!conv("![x](iuredesk-diagram:7)").html.contains("cid:note-diagram"));
    }

    #[test]
    fn deep_nesting_is_bounded() {
        let md = format!("{}hola", "> ".repeat(5_000));
        let h = conv(&md);
        assert!(h.html.matches("<blockquote>").count() <= MAX_DEPTH);
        assert!(h.html.contains("hola"));
        let list = (0..500).map(|i| format!("{}- x\n", "  ".repeat(i))).collect::<String>();
        assert!(conv(&list).html.matches("<ul>").count() <= MAX_DEPTH);
    }

    #[test]
    fn raw_html_is_sanitized_later() {
        let h = conv("hola <script>alert(1)</script> <img src=x onerror=alert(1)>");
        let clean = crate::sanitize(&h.html);
        assert!(!clean.html.to_ascii_lowercase().contains("<script"));
        assert!(!clean.html.to_ascii_lowercase().contains("onerror"));
    }

    #[test]
    fn odd_tag_names_are_not_reemitted() {
        // Regresión (proptest en Windows): `<javascript:<script>` salía tal cual del saneador.
        let h = conv("<script></script><javascript:<script>");
        let clean = crate::sanitize(&h.html).html.to_ascii_lowercase();
        assert!(!clean.contains("<javascript"), "{clean}");
        assert!(!clean.contains("<script"), "{clean}");
    }

    #[test]
    fn renders_with_blitz() {
        let h =
            conv("# Título\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n- [ ] x\n\n[l](https://e.org)\n\n```\ncode\n```\n");
        let o = crate::RenderOptions { background: Some([1, 2, 3]), viewport_width: 500, ..Default::default() };
        let r = crate::render(&h.html, &o).unwrap();
        assert!(r.links.iter().any(|l| l.href == "#iuredesk-task-0"), "{:?}", r.links);
        assert!(r.links.iter().any(|l| l.href == "https://e.org"));
        assert_eq!(r.rgba.get(..3), Some(&[1u8, 2, 3][..]));
        assert!(r.height > 100);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// Ninguna nota, por rara que sea, deja pasar `<script>` ni manejadores tras el saneado.
        #[test]
        fn never_script(s in "(\\PC|<script>|</script>|<img src=x onerror=a>|javascript:|\\[a\\]\\(|\\)|`|\\$|> |- \\[ \\] |\n){0,80}") {
            let h = conv(&s);
            let clean = crate::sanitize(&h.html).html.to_ascii_lowercase();
            // En el texto todo `<` sale escapado: lo que empieza por `<` es una etiqueta real.
            // Como texto («onerror», «javascript:») es inofensivo; dentro de una etiqueta, nunca.
            for tag in clean.split('<').skip(1).map(|t| t.split('>').next().unwrap_or("")) {
                prop_assert!(!tag.starts_with("script"), "{}", tag);
                prop_assert!(!tag.contains("onerror"), "{}", tag);
                // (El saneador escribe cada atributo como `nombre="valor"`.)
                prop_assert!(!tag.contains("=\"javascript:"), "{}", tag);
            }
        }

        /// Las rutas aceptadas nunca suben de carpeta ni son absolutas.
        #[test]
        fn local_paths_stay_inside(s in "(\\.\\.|\\.|/|%2e|%2f|%5c|\\\\|a|b|:|C){0,24}") {
            if let Some(p) = local_image_path(&s) {
                prop_assert!(!p.starts_with('/'));
                prop_assert!(p.split('/').all(|c| !c.is_empty() && c != "." && c != ".." && !c.contains(':')));
                prop_assert!(!p.contains('\\'));
            }
        }

        /// Marcar una casilla cambia exactamente un carácter y dos veces deja todo igual.
        #[test]
        fn toggle_roundtrip(items in proptest::collection::vec(("[ xX]", "[a-z ]{0,6}"), 1..8), n in 0usize..10) {
            let md: String = items.iter().map(|(c, t)| format!("- [{c}] {t}\n")).collect();
            match toggle_task(&md, n) {
                Some(once) => {
                    prop_assert_eq!(once.len(), md.len());
                    let back = toggle_task(&once, n);
                    prop_assert!(back.is_some());
                    let back = back.unwrap_or_default().to_lowercase();
                    prop_assert_eq!(back, md.to_lowercase());
                }
                None => prop_assert!(n >= conv(&md).tasks),
            }
        }
    }
}
