//! [`RichBody`]: el documento pequeño que edita el redactor.
//!
//! Es un árbol de bloques (párrafos, títulos, listas, citas, código, separadores y la firma) con
//! texto en línea plano: tramos ([`Span`]) con estilo (negrita, cursiva, subrayado, tachado,
//! código) y enlace opcional, imágenes y saltos de línea. Ese aplanado es lo que pide un editor de
//! texto enriquecido y simplifica las conversiones.
//!
//! Conversiones:
//! - [`RichBody::to_html`]: HTML apto para correo (ver [`html`]).
//! - [`RichBody::to_plain`]: `text/plain; format=flowed` (ver [`plain`]).
//! - [`RichBody::from_html`]: importación a lo mejor posible (html5ever), nunca falla.
//! - [`RichBody::from_markdown`] / [`RichBody::to_markdown`]: CommonMark (pulldown-cmark), para
//!   pegar documentos de IureEditor.
//!
//! Todo lo que llega de fuera se acota: profundidad de anidado ([`MAX_DEPTH`]), número de bloques
//! y tamaño del texto.

pub mod html;
mod import_html;
mod markdown;
pub mod plain;

use serde::{Deserialize, Serialize};

/// Profundidad máxima de anidado de bloques (citas, listas). Lo más profundo se aplana.
pub const MAX_DEPTH: usize = 16;
/// Número máximo de bloques que produce una importación.
pub const MAX_BLOCKS: usize = 20_000;
/// Tamaño máximo (bytes) del texto que produce una importación.
pub const MAX_TEXT_BYTES: usize = 8 * 1024 * 1024;

/// Documento del redactor.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RichBody {
    /// Bloques en orden.
    pub blocks: Vec<Block>,
}

/// Bloque del documento.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum Block {
    /// Párrafo.
    Paragraph { content: Vec<Inline> },
    /// Título (nivel 1–6).
    Heading { level: u8, content: Vec<Inline> },
    /// Lista con viñetas o numerada; cada elemento es una lista de bloques.
    List {
        ordered: bool,
        #[serde(default = "one")]
        start: u32,
        items: Vec<Vec<Block>>,
    },
    /// Cita (texto citado de un correo anterior o una cita normal).
    Quote { blocks: Vec<Block> },
    /// Bloque de código (texto preformateado).
    Code { text: String },
    /// Separador horizontal.
    Rule,
    /// Firma de la identidad. Hay como mucho una; se reemplaza al cambiar de identidad.
    Signature { blocks: Vec<Block> },
}

fn one() -> u32 {
    1
}

/// Elemento en línea.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum Inline {
    /// Tramo de texto.
    Text(Span),
    /// Imagen (en línea con `cid:` o remota).
    Image(Image),
    /// Salto de línea dentro del bloque.
    LineBreak,
}

/// Tramo de texto con estilo y enlace opcional.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    /// Texto (sin marcas).
    pub text: String,
    /// Estilo.
    #[serde(default, skip_serializing_if = "Style::is_plain")]
    pub style: Style,
    /// Destino del enlace, si lo es.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

/// Estilo de un tramo.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    /// Código en línea (monoespaciado).
    pub code: bool,
}

impl Style {
    /// Sin ningún estilo.
    pub fn is_plain(&self) -> bool {
        *self == Style::default()
    }
}

/// Imagen.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Image {
    /// `cid:…` para imágenes adjuntas en línea; `https:` o `data:image/…` también se admiten.
    pub src: String,
    /// Texto alternativo.
    #[serde(default)]
    pub alt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
}

impl Span {
    /// Tramo sin estilo.
    pub fn plain(text: impl Into<String>) -> Self {
        Self { text: text.into(), style: Style::default(), link: None }
    }
}

impl Inline {
    /// Texto sin estilo.
    pub fn text(text: impl Into<String>) -> Self {
        Inline::Text(Span::plain(text))
    }

    /// Texto con estilo.
    pub fn styled(text: impl Into<String>, style: Style) -> Self {
        Inline::Text(Span { text: text.into(), style, link: None })
    }

    /// Enlace.
    pub fn link(text: impl Into<String>, href: impl Into<String>) -> Self {
        Inline::Text(Span { text: text.into(), style: Style::default(), link: Some(href.into()) })
    }
}

impl Block {
    /// Párrafo de texto sin estilo.
    pub fn paragraph(text: impl Into<String>) -> Self {
        Block::Paragraph { content: vec![Inline::text(text)] }
    }
}

impl RichBody {
    /// Documento vacío.
    pub fn new() -> Self {
        Self::default()
    }

    /// Documento a partir de texto plano: los párrafos se separan por líneas en blanco y cada
    /// salto de línea simple se conserva.
    pub fn from_plain(text: &str) -> Self {
        let mut blocks = Vec::new();
        let text = text.replace("\r\n", "\n");
        for para in text.split("\n\n") {
            let para = para.trim_matches('\n');
            if para.trim().is_empty() {
                continue;
            }
            let mut content = Vec::new();
            for (i, line) in para.split('\n').enumerate() {
                if i > 0 {
                    content.push(Inline::LineBreak);
                }
                if !line.is_empty() {
                    content.push(Inline::text(line));
                }
            }
            blocks.push(Block::Paragraph { content });
            if blocks.len() >= MAX_BLOCKS {
                break;
            }
        }
        Self { blocks }
    }

    /// ¿No tiene contenido visible?
    pub fn is_empty(&self) -> bool {
        self.blocks.iter().all(block_is_empty)
    }

    /// HTML completo (documento) apto para correo.
    pub fn to_html(&self) -> String {
        html::document(self)
    }

    /// Fragmento HTML (sin `<html>`/`<body>`), para incrustar.
    pub fn to_html_fragment(&self) -> String {
        html::fragment(self)
    }

    /// Texto plano `format=flowed` (RFC 3676), con citas `> `.
    pub fn to_plain(&self) -> String {
        plain::render(self)
    }

    /// Importa HTML (de un correo o del portapapeles). Nunca falla.
    pub fn from_html(html: &str) -> Self {
        import_html::import(html)
    }

    /// Importa Markdown (CommonMark + tachado). Nunca falla.
    pub fn from_markdown(md: &str) -> Self {
        markdown::import(md)
    }

    /// Exporta a Markdown.
    pub fn to_markdown(&self) -> String {
        markdown::export(self)
    }

    /// La firma, si hay.
    pub fn signature(&self) -> Option<&[Block]> {
        self.blocks.iter().find_map(|b| match b {
            Block::Signature { blocks } => Some(blocks.as_slice()),
            _ => None,
        })
    }

    /// Quita la firma (si había) y devuelve su posición.
    pub fn remove_signature(&mut self) -> Option<usize> {
        let pos = self.blocks.iter().position(|b| matches!(b, Block::Signature { .. }))?;
        self.blocks.remove(pos);
        Some(pos)
    }

    /// Referencias `cid:` de las imágenes en línea (sin el prefijo), en orden y sin repetir.
    pub fn content_ids(&self) -> Vec<String> {
        let mut out = Vec::new();
        visit_inlines(&self.blocks, 0, &mut |i| {
            if let Inline::Image(img) = i
                && let Some(cid) = img.src.strip_prefix("cid:").or_else(|| img.src.strip_prefix("CID:"))
                && !out.iter().any(|c: &String| c == cid)
            {
                out.push(cid.to_string());
            }
        });
        out
    }

    /// Texto visible sin formato (para vistas previas y búsqueda).
    pub fn text_content(&self) -> String {
        let mut out = String::new();
        visit_inlines(&self.blocks, 0, &mut |i| match i {
            Inline::Text(s) => out.push_str(&s.text),
            Inline::Image(img) => out.push_str(&img.alt),
            Inline::LineBreak => out.push('\n'),
        });
        out
    }
}

fn block_is_empty(b: &Block) -> bool {
    match b {
        Block::Paragraph { content } | Block::Heading { content, .. } => content.iter().all(|i| match i {
            Inline::Text(s) => s.text.trim().is_empty(),
            Inline::Image(_) => false,
            Inline::LineBreak => true,
        }),
        Block::List { items, .. } => items.iter().all(|it| it.iter().all(block_is_empty)),
        Block::Quote { blocks } | Block::Signature { blocks } => blocks.iter().all(block_is_empty),
        Block::Code { text } => text.trim().is_empty(),
        Block::Rule => false,
    }
}

/// Recorre todos los elementos en línea (con tope de profundidad).
pub(crate) fn visit_inlines(blocks: &[Block], depth: usize, f: &mut dyn FnMut(&Inline)) {
    if depth > MAX_DEPTH.saturating_mul(2) {
        return;
    }
    for b in blocks {
        match b {
            Block::Paragraph { content } | Block::Heading { content, .. } => content.iter().for_each(&mut *f),
            Block::List { items, .. } => {
                for it in items {
                    visit_inlines(it, depth.saturating_add(1), f);
                }
            }
            Block::Quote { blocks } | Block::Signature { blocks } => visit_inlines(blocks, depth.saturating_add(1), f),
            Block::Code { .. } | Block::Rule => {}
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Constructor compartido por las importaciones (HTML y Markdown).
// ---------------------------------------------------------------------------------------------

/// Tipo de contenedor abierto.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FrameKind {
    Quote,
    Signature,
    List { ordered: bool, start: u32 },
    Item,
}

impl FrameKind {
    fn same(self, other: FrameKind) -> bool {
        matches!(
            (self, other),
            (FrameKind::Quote, FrameKind::Quote)
                | (FrameKind::Signature, FrameKind::Signature)
                | (FrameKind::List { .. }, FrameKind::List { .. })
                | (FrameKind::Item, FrameKind::Item)
        )
    }
}

struct Frame {
    kind: FrameKind,
    /// Abierto más allá de [`MAX_DEPTH`]: al cerrarse, sus bloques se vuelcan en el padre.
    flat: bool,
    blocks: Vec<Block>,
    /// Solo listas: elementos ya cerrados.
    items: Vec<Vec<Block>>,
}

/// Contadores de estilo (las etiquetas se pueden anidar: `<b><b>x</b></b>`).
#[derive(Default, Clone, Copy)]
pub(crate) struct StyleDepth {
    pub bold: u16,
    pub italic: u16,
    pub underline: u16,
    pub strike: u16,
    pub code: u16,
}

impl StyleDepth {
    fn style(&self) -> Style {
        Style {
            bold: self.bold > 0,
            italic: self.italic > 0,
            underline: self.underline > 0,
            strike: self.strike > 0,
            code: self.code > 0,
        }
    }

    pub(crate) fn apply(&mut self, delta: Style, push: bool) {
        let f = |v: &mut u16, on: bool| {
            if on {
                *v = if push { v.saturating_add(1) } else { v.saturating_sub(1) };
            }
        };
        f(&mut self.bold, delta.bold);
        f(&mut self.italic, delta.italic);
        f(&mut self.underline, delta.underline);
        f(&mut self.strike, delta.strike);
        f(&mut self.code, delta.code);
    }
}

/// Construye un [`RichBody`] a partir de eventos de apertura/cierre y texto.
pub(crate) struct Builder {
    root: Vec<Block>,
    stack: Vec<Frame>,
    inlines: Vec<Inline>,
    heading: Option<u8>,
    pub(crate) style: StyleDepth,
    links: Vec<Option<String>>,
    blocks: usize,
    text_bytes: usize,
}

impl Builder {
    pub(crate) fn new() -> Self {
        Self {
            root: Vec::new(),
            stack: Vec::new(),
            inlines: Vec::new(),
            heading: None,
            style: StyleDepth::default(),
            links: Vec::new(),
            blocks: 0,
            text_bytes: 0,
        }
    }

    fn full(&self) -> bool {
        self.blocks >= MAX_BLOCKS || self.text_bytes >= MAX_TEXT_BYTES
    }

    fn top_is_list(&self) -> bool {
        matches!(self.stack.last(), Some(f) if matches!(f.kind, FrameKind::List { .. }))
    }

    pub(crate) fn top_is_item(&self) -> bool {
        matches!(self.stack.last(), Some(f) if f.kind == FrameKind::Item)
    }

    fn push_block(&mut self, b: Block) {
        if self.full() {
            return;
        }
        self.blocks = self.blocks.saturating_add(1);
        self.place(b);
    }

    /// Coloca un bloque ya contado en el contenedor superior.
    fn place(&mut self, b: Block) {
        match self.stack.last_mut() {
            // Un bloque directamente dentro de una lista (sin elemento) es un elemento implícito.
            Some(f) if matches!(f.kind, FrameKind::List { .. }) => f.items.push(vec![b]),
            Some(f) => f.blocks.push(b),
            None => self.root.push(b),
        }
    }

    /// Cierra el párrafo (o título) en curso, si tiene contenido.
    pub(crate) fn flush(&mut self) {
        let mut inl = std::mem::take(&mut self.inlines);
        let level = self.heading.take();
        trim_inlines(&mut inl);
        if inl.is_empty() {
            return;
        }
        let b = match level {
            Some(level) => Block::Heading { level, content: inl },
            None => Block::Paragraph { content: inl },
        };
        self.push_block(b);
    }

    /// Empieza un título.
    pub(crate) fn heading(&mut self, level: u8) {
        self.flush();
        self.heading = Some(level.clamp(1, 6));
    }

    /// Abre un contenedor.
    pub(crate) fn open(&mut self, kind: FrameKind) {
        self.flush();
        // Un elemento fuera de una lista abre una lista implícita.
        if kind == FrameKind::Item && !self.top_is_list() {
            self.open(FrameKind::List { ordered: false, start: 1 });
        }
        let flat = self.stack.len() >= MAX_DEPTH;
        self.stack.push(Frame { kind, flat, blocks: Vec::new(), items: Vec::new() });
    }

    /// Cierra el contenedor más reciente de ese tipo (y los que haya abiertos encima). Si no hay
    /// ninguno, no hace nada.
    pub(crate) fn close(&mut self, kind: FrameKind) {
        let Some(pos) = self.stack.iter().rposition(|f| f.kind.same(kind)) else {
            return;
        };
        self.flush();
        while self.stack.len() > pos {
            self.pop_frame();
        }
    }

    /// Cierra el contenedor superior y lo entrega a su padre.
    fn pop_frame(&mut self) {
        let Some(frame) = self.stack.pop() else { return };
        if frame.flat {
            // Ya contados al crearse: se mueven sin volver a contar.
            for b in frame.blocks.into_iter().chain(frame.items.into_iter().flatten()) {
                self.place(b);
            }
            return;
        }
        match frame.kind {
            FrameKind::Item => {
                if let Some(parent) = self.stack.last_mut()
                    && matches!(parent.kind, FrameKind::List { .. })
                {
                    parent.items.push(frame.blocks);
                } else {
                    for b in frame.blocks {
                        self.place(b);
                    }
                }
            }
            FrameKind::List { ordered, start } => {
                let mut items = frame.items;
                if !frame.blocks.is_empty() {
                    items.push(frame.blocks);
                }
                if !items.is_empty() {
                    self.push_block(Block::List { ordered, start, items });
                }
            }
            FrameKind::Quote => {
                if !frame.blocks.is_empty() {
                    self.push_block(Block::Quote { blocks: frame.blocks });
                }
            }
            FrameKind::Signature => {
                if !frame.blocks.is_empty() {
                    self.push_block(Block::Signature { blocks: frame.blocks });
                }
            }
        }
    }

    /// Texto. Con `collapse`, los espacios en blanco seguidos cuentan como uno (HTML).
    pub(crate) fn text(&mut self, s: &str, collapse: bool) {
        if self.full() || s.is_empty() {
            return;
        }
        let mut t = String::with_capacity(s.len());
        if collapse {
            let at_start = match self.inlines.last() {
                None | Some(Inline::LineBreak) => true,
                Some(Inline::Text(sp)) => sp.text.ends_with(' '),
                Some(Inline::Image(_)) => false,
            };
            let mut prev_space = at_start;
            for c in s.chars() {
                if c.is_whitespace() && c != '\u{a0}' {
                    if !prev_space {
                        t.push(' ');
                    }
                    prev_space = true;
                } else if !c.is_control() {
                    t.push(c);
                    prev_space = false;
                }
            }
        } else {
            t.extend(s.chars().filter(|c| !c.is_control() || *c == '\n' || *c == '\t'));
        }
        if t.is_empty() {
            return;
        }
        self.text_bytes = self.text_bytes.saturating_add(t.len());
        let style = self.style.style();
        let link = self.links.iter().rev().find_map(|l| l.clone());
        if let Some(Inline::Text(last)) = self.inlines.last_mut()
            && last.style == style
            && last.link == link
        {
            last.text.push_str(&t);
            return;
        }
        self.inlines.push(Inline::Text(Span { text: t, style, link }));
    }

    pub(crate) fn line_break(&mut self) {
        if !self.full() {
            self.inlines.push(Inline::LineBreak);
        }
    }

    pub(crate) fn image(&mut self, img: Image) {
        if !self.full() {
            self.text_bytes = self.text_bytes.saturating_add(img.src.len());
            self.inlines.push(Inline::Image(img));
        }
    }

    pub(crate) fn push_link(&mut self, href: Option<String>) {
        if self.links.len() < 64 {
            self.links.push(href);
        }
    }

    pub(crate) fn pop_link(&mut self) {
        self.links.pop();
    }

    pub(crate) fn code_block(&mut self, text: &str) {
        self.flush();
        let text = text.strip_suffix('\n').unwrap_or(text);
        self.text_bytes = self.text_bytes.saturating_add(text.len());
        self.push_block(Block::Code { text: text.to_string() });
    }

    pub(crate) fn rule(&mut self) {
        self.flush();
        self.push_block(Block::Rule);
    }

    pub(crate) fn finish(mut self) -> RichBody {
        self.flush();
        while !self.stack.is_empty() {
            self.pop_frame();
        }
        RichBody { blocks: self.root }
    }
}

/// Quita espacios al principio y al final de una secuencia en línea, y saltos de línea sobrantes
/// al final.
fn trim_inlines(inl: &mut Vec<Inline>) {
    while matches!(inl.last(), Some(Inline::LineBreak)) {
        inl.pop();
    }
    if let Some(Inline::Text(s)) = inl.first_mut() {
        let t = s.text.trim_start().to_string();
        s.text = t;
    }
    if let Some(Inline::Text(s)) = inl.last_mut() {
        let t = s.text.trim_end().to_string();
        s.text = t;
    }
    inl.retain(|i| !matches!(i, Inline::Text(s) if s.text.is_empty()));
    let has_content = inl.iter().any(|i| match i {
        Inline::Text(s) => !s.text.trim().is_empty(),
        Inline::Image(_) => true,
        Inline::LineBreak => false,
    });
    if !has_content {
        inl.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_plain_paragraphs_and_breaks() {
        let b = RichBody::from_plain("Hola\nAna\n\n\nAdiós");
        assert_eq!(b.blocks.len(), 2);
        assert_eq!(
            b.blocks[0],
            Block::Paragraph { content: vec![Inline::text("Hola"), Inline::LineBreak, Inline::text("Ana")] }
        );
        assert!(!b.is_empty());
        assert!(RichBody::from_plain("  \n\n ").is_empty());
    }

    #[test]
    fn builder_flattens_deep_nesting() {
        let mut b = Builder::new();
        for _ in 0..100 {
            b.open(FrameKind::Quote);
        }
        b.text("hondo", true);
        for _ in 0..100 {
            b.close(FrameKind::Quote);
        }
        let body = b.finish();
        fn depth(bs: &[Block]) -> usize {
            bs.iter()
                .map(|b| match b {
                    Block::Quote { blocks } => 1 + depth(blocks),
                    _ => 0,
                })
                .max()
                .unwrap_or(0)
        }
        assert!(depth(&body.blocks) <= MAX_DEPTH);
        assert_eq!(body.text_content(), "hondo");
    }

    #[test]
    fn builder_implicit_list_and_unbalanced_close() {
        let mut b = Builder::new();
        b.close(FrameKind::List { ordered: false, start: 1 });
        b.open(FrameKind::Item);
        b.text("uno", true);
        b.close(FrameKind::Item);
        let body = b.finish();
        assert!(matches!(&body.blocks[0], Block::List { items, .. } if items.len() == 1));
    }

    #[test]
    fn signature_and_cids() {
        let mut body = RichBody::from_plain("hola");
        body.blocks.push(Block::Signature { blocks: vec![Block::paragraph("Ana")] });
        body.blocks.push(Block::Paragraph {
            content: vec![Inline::Image(Image { src: "cid:logo@x".into(), ..Default::default() })],
        });
        assert!(body.signature().is_some());
        assert_eq!(body.content_ids(), vec!["logo@x".to_string()]);
        assert_eq!(body.remove_signature(), Some(1));
        assert!(body.signature().is_none());
    }

    #[test]
    fn serde_round_trip() {
        let body = RichBody {
            blocks: vec![
                Block::Heading {
                    level: 2,
                    content: vec![Inline::styled("T", Style { bold: true, ..Default::default() })],
                },
                Block::List { ordered: true, start: 3, items: vec![vec![Block::paragraph("x")]] },
            ],
        };
        let j = serde_json::to_string(&body).unwrap();
        let back: RichBody = serde_json::from_str(&j).unwrap();
        assert_eq!(body, back);
    }
}
