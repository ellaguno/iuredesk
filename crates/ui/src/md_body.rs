//! S1.27: el cuerpo del redactor con el editor de la suite (`iure-doc-ui`, lo de IureEditor 2.0):
//! se escribe en Markdown con formato al teclear y se convierte al [`RichBody`] del correo (que
//! luego es HTML y texto) al guardar o enviar.
//!
//! La firma va tras la línea `-- ` (la convención del correo, la misma del editor anterior): al
//! convertir, lo que sigue es el bloque [`Block::Signature`], así cambiar de identidad la sigue
//! reemplazando. Las imágenes en línea (`![…](cid:…)`, de la firma o del correo citado) se dibujan
//! con [`CidImages`] desde los adjuntos en línea del borrador.

use std::collections::HashMap;
use std::sync::Arc;

use iuredesk_engine::iuredesk_compose::rich::{Block, RichBody};
use iuredesk_engine::iuredesk_compose::{Attachment, AttachmentKind, AttachmentSource};

/// Lado máximo de una imagen en línea que se dibuja (puntos).
const MAX_IMAGE_SIDE: f32 = 640.0;
/// Bytes máximos de una imagen en línea que se decodifica.
const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;

/// La línea que abre la firma.
const SIG_LINE: &str = "-- ";

/// El cuerpo en Markdown y lo seleccionado (caracteres; se actualiza al dibujar el editor).
#[derive(Clone, Debug, Default)]
pub struct MdBody {
    pub text: String,
    pub sel: (usize, usize),
    pub changed: bool,
}

/// Markdown de un cuerpo, con la firma tras `-- `.
pub(crate) fn rich_to_md(body: &RichBody) -> String {
    let mut b = body.clone();
    let sig = b.signature().map(<[Block]>::to_vec);
    b.remove_signature();
    let mut md = b.to_markdown().trim_end().to_string();
    if let Some(sig) = sig {
        let s = RichBody { blocks: sig }.to_markdown();
        if !md.is_empty() {
            md.push_str("\n\n");
        }
        md.push_str(SIG_LINE);
        md.push('\n');
        md.push_str(s.trim_end());
    }
    md.push('\n');
    soft_breaks(&md)
}

/// Los saltos de línea duros como dos espacios al final (no se ven en el editor) en vez de `\`,
/// fuera de los bloques de código.
fn soft_breaks(md: &str) -> String {
    let mut out = String::with_capacity(md.len());
    let mut fence = false;
    for line in md.split_inclusive('\n') {
        let t = line.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            fence = !fence;
        }
        let body = line.trim_end_matches('\n');
        if !fence && body.ends_with('\\') && !body.ends_with("\\\\") && line.ends_with('\n') {
            out.push_str(body.get(..body.len() - 1).unwrap_or(body));
            out.push_str("  \n");
        } else {
            out.push_str(line);
        }
    }
    out
}

/// ¿La línea abre la firma? (`-- ` o `--`, fuera de los bloques de código.)
fn is_sig_line(line: &str) -> bool {
    line.trim_end_matches('\r') == SIG_LINE || line.trim_end() == "--"
}

/// El cuerpo de un Markdown: lo que sigue a la línea `-- ` es la firma.
pub(crate) fn md_to_rich(md: &str) -> RichBody {
    let mut fence = false;
    let mut at = None;
    let mut pos = 0usize;
    for line in md.split_inclusive('\n') {
        let t = line.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            fence = !fence;
        } else if !fence && is_sig_line(line.trim_end_matches('\n')) {
            at = Some((pos, pos + line.len()));
        }
        pos += line.len();
    }
    match at {
        Some((start, end)) => {
            let mut body = RichBody::from_markdown(md.get(..start).unwrap_or(""));
            let sig = RichBody::from_markdown(md.get(end..).unwrap_or(""));
            if !sig.blocks.is_empty() {
                body.blocks.push(Block::Signature { blocks: sig.blocks });
            }
            body
        }
        None => RichBody::from_markdown(md),
    }
}

impl MdBody {
    pub fn from_rich(body: &RichBody) -> Self {
        MdBody { text: rich_to_md(body), sel: (0, 0), changed: false }
    }

    /// Cambia todo el cuerpo (firma nueva, plantilla, identidad).
    pub fn set_rich(&mut self, body: &RichBody) {
        self.text = rich_to_md(body);
        self.sel = (0, 0);
        self.changed = true;
    }

    pub fn to_rich(&self) -> RichBody {
        md_to_rich(&self.text)
    }

    fn byte(&self, ci: usize) -> usize {
        self.text.char_indices().nth(ci).map_or(self.text.len(), |(b, _)| b)
    }

    /// Escribe `s` en el carácter `at`.
    pub fn insert(&mut self, at: usize, s: &str) {
        let b = self.byte(at);
        self.text.insert_str(b, s);
        self.changed = true;
    }

    /// Quita los caracteres `from..to`.
    pub fn remove(&mut self, from: usize, to: usize) {
        let (a, b) = (self.byte(from.min(to)), self.byte(from.max(to)));
        self.text.replace_range(a..b, "");
        self.changed = true;
    }
}

/// Content-ID de los dibujos del correo (`dibujo-N@iuredesk`).
pub(crate) const DRAW_CID: &str = "dibujo-";
const DRAW_DOMAIN: &str = "@iuredesk";
/// Dibujos por correo, como mucho.
const MAX_DRAWINGS: usize = 40;

/// Un dibujo listo para el correo: su Content-ID, el PNG y su tamaño en puntos.
pub(crate) struct Drawing {
    pub cid: String,
    pub png: Vec<u8>,
    pub size: (u32, u32),
}

/// Diagramas (Mermaid, mapas mentales) y fórmulas (`$…$`, `$$…$$`) del Markdown, dibujados como PNG
/// para enviarlos como imágenes en línea: devuelve el Markdown con `![…](cid:…)` en su lugar y los
/// dibujos. Lo que no se puede dibujar se queda como texto.
pub(crate) fn drawings_to_images(md: &str) -> (String, Vec<Drawing>) {
    use iuredesk_html_view::diagram;
    use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag};
    enum What {
        Diagram,
        Formula(String, bool),
    }
    let mut found: Vec<(std::ops::Range<usize>, What)> = Vec::new();
    let opts = Options::ENABLE_MATH | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES;
    for (ev, r) in Parser::new_ext(md, opts).into_offset_iter() {
        if found.last().is_some_and(|(prev, _)| r.start < prev.end) {
            continue;
        }
        let slice = md.get(r.clone()).unwrap_or("");
        let what = match ev {
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info)))
                if info.split_whitespace().next().is_some_and(|l| l.eq_ignore_ascii_case("mermaid")) =>
            {
                Some(What::Diagram)
            }
            Event::Start(Tag::BlockQuote(_))
                if slice
                    .trim_start()
                    .trim_start_matches('>')
                    .trim_start()
                    .to_ascii_uppercase()
                    .starts_with("[!MINDMAP") =>
            {
                Some(What::Diagram)
            }
            Event::DisplayMath(m) => Some(What::Formula(m.to_string(), true)),
            Event::InlineMath(m) => Some(What::Formula(m.to_string(), false)),
            _ => None,
        };
        if let Some(w) = what
            && found.len() < MAX_DRAWINGS
        {
            found.push((r, w));
        }
    }
    let pick = iurefficient_ui::lang::pick;
    let mut out = String::with_capacity(md.len());
    let mut drawings = Vec::new();
    let mut at = 0usize;
    for (r, what) in found {
        let slice = md.get(r.clone()).unwrap_or("");
        let drawn = match &what {
            What::Diagram => diagram::draw_block(
                slice,
                &iuredesk_html_view::markdown::NoteStyle::default(),
                iuredesk_html_view::mindmap::Layout::Radial,
                2.0,
                [255, 255, 255],
            )
            .ok()
            .and_then(|p| diagram::encode_png(p.width, p.height, &p.rgba).map(|png| (png, p.points))),
            What::Formula(latex, display) => {
                iure_doc_export::math::render_math(latex.trim(), *display, 14.0, [0x1f, 0x23, 0x28], 2.0)
                    .ok()
                    .and_then(|m| diagram::encode_png(m.width, m.height, &m.rgba).map(|png| (png, m.points)))
            }
        };
        let Some((png, (w, h))) = drawn else { continue };
        let cid = format!("{DRAW_CID}{}{DRAW_DOMAIN}", drawings.len() + 1);
        let alt = match &what {
            What::Diagram => pick("Diagram", "Diagrama").to_string(),
            What::Formula(latex, _) => latex.trim().chars().filter(|c| !matches!(c, '[' | ']')).take(80).collect(),
        };
        out.push_str(md.get(at..r.start).unwrap_or(""));
        out.push_str(&format!("![{alt}](cid:{cid})"));
        // Un bloque reemplazado conserva su salto de línea final.
        if matches!(what, What::Diagram | What::Formula(_, true)) && slice.ends_with('\n') {
            out.push('\n');
        }
        at = r.end;
        drawings.push(Drawing { cid, png, size: (w.round() as u32, h.round() as u32) });
    }
    out.push_str(md.get(at..).unwrap_or(""));
    (out, drawings)
}

/// Pone a las imágenes de los dibujos su tamaño en puntos (los PNG van al doble, para pantallas
/// densas).
pub(crate) fn size_drawings(blocks: &mut [Block], sizes: &HashMap<String, (u32, u32)>) {
    use iuredesk_engine::iuredesk_compose::rich::Inline;
    let fix = |content: &mut Vec<Inline>| {
        for i in content.iter_mut() {
            if let Inline::Image(img) = i
                && let Some((w, h)) = img.src.strip_prefix("cid:").and_then(|c| sizes.get(c))
            {
                img.width = Some(*w);
                img.height = Some(*h);
            }
        }
    };
    for b in blocks.iter_mut() {
        match b {
            Block::Paragraph { content } | Block::Heading { content, .. } => fix(content),
            Block::List { items, .. } => {
                for it in items.iter_mut() {
                    size_drawings(it, sizes);
                }
            }
            Block::Quote { blocks } | Block::Signature { blocks } => size_drawings(blocks, sizes),
            _ => {}
        }
    }
}

/// Imágenes en línea del correo (`![…](cid:…)`) dibujadas desde los adjuntos en línea del borrador.
pub(crate) struct CidImages {
    store: Arc<iuredesk_store::Store>,
    sources: HashMap<String, AttachmentSource>,
    key: u64,
}

impl CidImages {
    pub(crate) fn new(store: Arc<iuredesk_store::Store>, attachments: &[Attachment]) -> Self {
        use std::hash::{Hash, Hasher};
        let mut sources = HashMap::new();
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for a in attachments.iter().take(500) {
            if let AttachmentKind::Inline { content_id } = &a.kind {
                let cid = content_id.trim().trim_start_matches('<').trim_end_matches('>').to_ascii_lowercase();
                cid.hash(&mut h);
                format!("{:?}", a.source).hash(&mut h);
                sources.insert(cid, a.source.clone());
            }
        }
        CidImages { store, sources, key: h.finish() }
    }
}

/// El `cid:` de un bloque que es solo una imagen en línea.
fn cid_of(block: &str) -> Option<String> {
    let t = block.trim();
    let rest = t.strip_prefix("![")?;
    let open = rest.find("](cid:")?;
    let src = rest.get(open + 2..)?.strip_suffix(')')?;
    let cid = src.strip_prefix("cid:")?.split_whitespace().next()?;
    Some(cid.to_ascii_lowercase())
}

impl iure_doc_ui::DiagramRenderer for CidImages {
    fn accepts(&self, block: &str) -> bool {
        cid_of(block).is_some_and(|c| self.sources.contains_key(&c))
    }

    fn key(&self) -> u64 {
        self.key
    }

    fn render(&self, block: &str, _scale: f32) -> Result<iure_doc_ui::DiagramImage, String> {
        let pick = iurefficient_ui::lang::pick;
        let missing = || pick("The image is not available.", "La imagen no está disponible.").to_string();
        let src = cid_of(block).and_then(|c| self.sources.get(&c)).ok_or_else(missing)?;
        let bytes = match src {
            AttachmentSource::Bytes(b) => b.clone(),
            AttachmentSource::Path(p) => std::fs::read(p).map_err(|_| missing())?,
            AttachmentSource::Blob(h) => self.store.blobs().get(h).map_err(|_| missing())?,
        };
        if bytes.len() > MAX_IMAGE_BYTES {
            return Err(pick("The image is too big.", "La imagen es muy grande.").into());
        }
        let mut reader =
            image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().map_err(|_| missing())?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(8_000);
        limits.max_image_height = Some(8_000);
        limits.max_alloc = Some(256 * 1024 * 1024);
        reader.limits(limits);
        let img = reader.decode().map_err(|_| missing())?.to_rgba8();
        let (w, h) = (img.width() as f32, img.height() as f32);
        let k = (MAX_IMAGE_SIDE / w.max(h).max(1.0)).min(1.0);
        Ok(iure_doc_ui::DiagramImage {
            width: img.width() as usize,
            height: img.height() as usize,
            rgba: img.into_raw(),
            points: [w * k, h * k],
            hotspots: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iuredesk_engine::iuredesk_compose::rich::{Inline, Span};

    #[test]
    fn the_signature_survives_the_trip_through_markdown() {
        let body = RichBody {
            blocks: vec![
                Block::Paragraph { content: vec![Inline::Text(Span::plain("Hola **Ana**"))] },
                Block::Signature {
                    blocks: vec![Block::Paragraph { content: vec![Inline::Text(Span::plain("Eduardo"))] }],
                },
            ],
        };
        let md = rich_to_md(&body);
        assert!(md.contains("\n\n-- \nEduardo"), "{md:?}");
        // Los saltos duros, sin la `\` que se veía en el editor.
        assert_eq!(soft_breaks("Laura\\\nDespacho\n```\nx\\\n```\n"), "Laura  \nDespacho\n```\nx\\\n```\n");
        let back = md_to_rich(&md);
        assert!(back.signature().is_some_and(|s| !s.is_empty()), "la firma vuelve a ser firma");
        assert!(back.text_content().contains("Eduardo"));
        // Sin firma, nada raro; un `--` dentro de código no es firma.
        assert!(md_to_rich("Hola\n\n```\n-- \n```\n").signature().is_none());
        assert!(md_to_rich("Hola\n\n--\nFirma").signature().is_some());
    }

    #[test]
    fn drawings_become_inline_images() {
        let md = "Hola $x^2$ y\n\n```mermaid\nflowchart LR\n  A-->B\n```\n\n$$\n\\frac{1}{2}\n$$\n\nFin `$no$`\n";
        let (out, d) = drawings_to_images(md);
        assert_eq!(d.len(), 3, "{out}");
        assert!(d.iter().all(|x| x.png.starts_with(b"\x89PNG") && x.size.0 > 0));
        assert!(out.contains("![x^2](cid:dibujo-1@iuredesk)"), "{out}");
        assert!(
            out.contains("![Diagrama](cid:dibujo-2@iuredesk)") || out.contains("![Diagram](cid:dibujo-2@iuredesk)")
        );
        assert!(out.contains("`$no$`"), "el código no se toca");
        let mut body = md_to_rich(&out);
        let sizes: HashMap<String, (u32, u32)> = d.iter().map(|x| (x.cid.clone(), x.size)).collect();
        size_drawings(&mut body.blocks, &sizes);
        assert_eq!(body.content_ids().len(), 3);
        assert!(body.to_html().contains("width="), "{}", body.to_html());
    }

    #[test]
    fn edits_work_by_character() {
        let mut b = MdBody { text: "Hola mundo\n".into(), ..Default::default() };
        b.insert(5, "gran ");
        assert_eq!(b.text, "Hola gran mundo\n");
        b.remove(5, 10);
        assert_eq!(b.text, "Hola mundo\n");
        assert!(b.changed);
        assert_eq!(cid_of("![logo](cid:Logo@Acme)").as_deref(), Some("logo@acme"));
        assert_eq!(cid_of("![x](https://x/y.png)"), None);
    }
}
