//! Impresión y «Guardar como PDF» (F5.4): HTML → render de Blitz al ancho del papel → páginas →
//! PDF con `pdf-writer` (Rust puro). Cada página es una imagen RGB comprimida con Flate; los cortes
//! de página se buscan en una franja en blanco cercana para no partir líneas de texto.
//!
//! Sin texto seleccionable (es una imagen a ~150 ppp): suficiente para imprimir y archivar; la
//! capa de texto queda pendiente.

use std::io::Write as _;

use pdf_writer::{Content, Filter, Finish, Name, Pdf, Rect, Ref, TextStr};

use crate::{Error, RenderOptions, render};

/// Tamaño del papel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Paper {
    #[default]
    A4,
    Letter,
}

impl Paper {
    /// Ancho y alto en puntos PDF (1/72").
    pub fn points(self) -> (f32, f32) {
        match self {
            Paper::A4 => (595.28, 841.89),
            Paper::Letter => (612.0, 792.0),
        }
    }
}

/// Opciones de impresión.
#[derive(Clone, Debug)]
pub struct PrintOptions {
    pub paper: Paper,
    /// Margen en puntos.
    pub margin: f32,
    /// Título del documento (metadatos del PDF).
    pub title: String,
    /// Recursos `cid:` para las imágenes del correo.
    pub inline_resources: std::collections::HashMap<String, crate::Bytes>,
    /// Máximo de páginas.
    pub max_pages: usize,
}

impl Default for PrintOptions {
    fn default() -> Self {
        PrintOptions {
            paper: Paper::A4,
            margin: 36.0,
            title: String::new(),
            inline_resources: Default::default(),
            max_pages: 200,
        }
    }
}

/// Ancho del contenido en px CSS (96 px = 72 pt).
const PX_PER_PT: f32 = 96.0 / 72.0;
/// Escala del render (2 → ~192 ppp).
const SCALE: f32 = 2.0;

/// Fila «en blanco» (todos los píxeles casi iguales al primero) para cortar ahí.
fn blank_row(rgba: &[u8], width: usize, y: usize) -> bool {
    let start = y.saturating_mul(width).saturating_mul(4);
    let Some(row) = rgba.get(start..start.saturating_add(width.saturating_mul(4))) else { return false };
    let Some(first) = row.get(..3) else { return false };
    let (r, g, b) = (first.first().copied().unwrap_or(0), first.get(1).copied().unwrap_or(0), first.get(2).copied().unwrap_or(0));
    row.chunks_exact(4).all(|p| {
        let d = |a: Option<&u8>, b: u8| a.copied().unwrap_or(0).abs_diff(b) < 8;
        d(p.first(), r) && d(p.get(1), g) && d(p.get(2), b)
    })
}

/// Cortes de página (filas, px físicos) para un búfer de `height` filas.
fn page_breaks(rgba: &[u8], width: usize, height: usize, page_h: usize, max_pages: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut y = 0usize;
    let look = page_h / 8;
    while y < height && out.len() < max_pages {
        let mut end = y.saturating_add(page_h).min(height);
        if end < height {
            // Buscar hacia arriba una fila en blanco.
            let mut cut = end;
            while cut > end.saturating_sub(look) && !blank_row(rgba, width, cut) {
                cut -= 1;
            }
            if cut > y && blank_row(rgba, width, cut) {
                end = cut;
            }
        }
        if end <= y {
            break;
        }
        out.push((y, end));
        y = end;
    }
    out
}

/// Convierte `html` (completo, con cabecera del mensaje ya incluida si se quiere) en un PDF.
pub fn html_to_pdf(html: &str, opts: &PrintOptions) -> Result<Vec<u8>, Error> {
    let (pw, ph) = opts.paper.points();
    let margin = opts.margin.clamp(0.0, pw / 4.0);
    let content_w_pt = pw - 2.0 * margin;
    let content_h_pt = ph - 2.0 * margin;
    let viewport = (content_w_pt * PX_PER_PT).round().max(1.0) as u32;
    let page_h_css = content_h_pt * PX_PER_PT;
    let max_pages = opts.max_pages.clamp(1, 1000);
    let r = render(
        html,
        &RenderOptions {
            viewport_width: viewport,
            scale: SCALE,
            dark_mode: false,
            allow_remote_images: false,
            inline_resources: opts.inline_resources.clone(),
            fetcher: None,
            max_height: ((page_h_css * max_pages as f32) as u32).clamp(1, 200_000),
            ..RenderOptions::default()
        },
    )?;
    let width = r.width as usize;
    let height = r.height as usize;
    let page_h = (page_h_css * SCALE).round().max(1.0) as usize;
    let breaks = page_breaks(&r.rgba, width, height, page_h, max_pages);
    if breaks.is_empty() {
        return Err(Error::Empty);
    }

    let mut pdf = Pdf::new();
    let catalog = Ref::new(1);
    let tree = Ref::new(2);
    let info = Ref::new(3);
    let mut next = 4;
    let mut alloc = || {
        let r = Ref::new(next);
        next += 1;
        r
    };
    let pages: Vec<(Ref, Ref, Ref)> = breaks.iter().map(|_| (alloc(), alloc(), alloc())).collect();
    pdf.catalog(catalog).pages(tree);
    pdf.pages(tree).kids(pages.iter().map(|p| p.0)).count(pages.len() as i32);
    let mut di = pdf.document_info(info);
    if !opts.title.is_empty() {
        di.title(TextStr(&opts.title));
    }
    di.producer(TextStr("IureDesk"));
    di.finish();

    for (&(y0, y1), &(page_id, content_id, image_id)) in breaks.iter().zip(pages.iter()) {
        let rows = y1 - y0;
        // RGB sin alfa, comprimido.
        let mut rgb = Vec::with_capacity(width.saturating_mul(rows).saturating_mul(3));
        let slice = r.rgba.get(y0.saturating_mul(width) * 4..y1.saturating_mul(width) * 4).unwrap_or(&[]);
        for p in slice.chunks_exact(4) {
            rgb.extend_from_slice(p.get(..3).unwrap_or(&[255, 255, 255]));
        }
        let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(&rgb).map_err(|e| Error::EnginePanic(e.to_string()))?;
        let data = enc.finish().map_err(|e| Error::EnginePanic(e.to_string()))?;
        let mut img = pdf.image_xobject(image_id, &data);
        img.filter(Filter::FlateDecode);
        img.width(width as i32);
        img.height(rows as i32);
        img.color_space().device_rgb();
        img.bits_per_component(8);
        img.finish();

        let w_pt = content_w_pt;
        let h_pt = rows as f32 / SCALE / PX_PER_PT;
        let mut c = Content::new();
        c.save_state();
        c.transform([w_pt, 0.0, 0.0, h_pt, margin, ph - margin - h_pt]);
        c.x_object(Name(b"Im1"));
        c.restore_state();
        pdf.stream(content_id, &c.finish());

        let mut page = pdf.page(page_id);
        page.media_box(Rect::new(0.0, 0.0, pw, ph));
        page.parent(tree);
        page.contents(content_id);
        page.resources().x_objects().pair(Name(b"Im1"), image_id);
        page.finish();
    }
    Ok(pdf.finish())
}

/// Escapa texto para meterlo en HTML.
pub fn escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            c => o.push(c),
        }
    }
    o
}

/// HTML imprimible de un mensaje: bloque de cabecera (pares etiqueta/valor) + cuerpo (HTML del
/// correo o texto plano).
pub fn message_html(headers: &[(String, String)], body_html: Option<&str>, body_text: &str) -> String {
    let mut h = String::from(
        "<html><head><meta charset=\"utf-8\"><style>body{font-family:sans-serif;font-size:13px;color:#111;margin:0}\
         table.h{border-collapse:collapse;margin-bottom:12px;width:100%;border-bottom:1px solid #999}\
         table.h td{padding:2px 6px;vertical-align:top}td.k{color:#555;white-space:nowrap;width:1%}\
         pre{white-space:pre-wrap;font-family:sans-serif}</style></head><body><table class=\"h\">",
    );
    for (k, v) in headers {
        h.push_str(&format!("<tr><td class=\"k\">{}</td><td>{}</td></tr>", escape(k), escape(v)));
    }
    h.push_str("</table>");
    match body_html {
        Some(b) => {
            // Solo el contenido de <body> (el saneador quita lo peligroso).
            let lower = b.to_ascii_lowercase();
            let inner = match (lower.find("<body"), lower.rfind("</body>")) {
                (Some(s), Some(e)) => b.get(s..e).and_then(|x| x.find('>').and_then(|g| x.get(g + 1..))).unwrap_or(b),
                _ => b,
            };
            h.push_str(inner);
        }
        None => h.push_str(&format!("<pre>{}</pre>", escape(body_text))),
    }
    h.push_str("</body></html>");
    h
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn count_pages(pdf: &[u8]) -> usize {
        let s = String::from_utf8_lossy(pdf);
        s.matches("/Type /Page").count() - s.matches("/Type /Pages").count()
    }

    #[test]
    fn breaks_prefer_blank_rows() {
        // 10 px de ancho, 100 filas; filas 40..45 con tinta.
        let w = 10;
        let mut rgba = vec![255u8; w * 100 * 4];
        for y in 40..45 {
            for x in 0..w / 2 {
                let i = (y * w + x) * 4;
                rgba[i] = 0;
            }
        }
        let b = page_breaks(&rgba, w, 100, 42, 10);
        assert_eq!(b.first(), Some(&(0, 39)), "corta antes de la línea de texto");
        assert_eq!(b.last().map(|x| x.1), Some(100));
    }

    #[test]
    fn short_and_long_messages() {
        let html = message_html(
            &[("De".into(), "Ana <ana@x.com>".into()), ("Asunto".into(), "Contrato & anexos".into())],
            None,
            "Hola",
        );
        assert!(html.contains("Contrato &amp; anexos"));
        let pdf = html_to_pdf(&html, &PrintOptions { title: "Contrato".into(), ..Default::default() }).unwrap();
        assert!(pdf.starts_with(b"%PDF-"));
        assert!(String::from_utf8_lossy(&pdf).trim_end().ends_with("%%EOF"));
        assert_eq!(count_pages(&pdf), 1);

        let long: String = (0..400).map(|i| format!("<p>Párrafo {i} del expediente.</p>")).collect();
        let html = message_html(&[], Some(&format!("<html><body>{long}</body></html>")), "");
        let pdf = html_to_pdf(&html, &PrintOptions { paper: Paper::Letter, ..Default::default() }).unwrap();
        let n = count_pages(&pdf);
        assert!(n >= 5, "{n} páginas");
        let s = String::from_utf8_lossy(&pdf);
        assert!(s.contains("612 792"), "tamaño carta");
    }
}
