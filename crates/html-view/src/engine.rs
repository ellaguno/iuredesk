//! Tubería Blitz: documento → estilo/layout → pintura en CPU por franjas → RGBA + enlaces.

use std::sync::atomic::Ordering;
use std::sync::{Arc, LazyLock};

use anyrender::{PaintScene as _, render_to_buffer};
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::traversal::TreeTraverser;
use blitz_dom::{BaseDocument, DocumentConfig, UserAgentStylesheet, local_name};
use blitz_html::HtmlDocument;
use blitz_traits::shell::{ColorScheme, Viewport};
use peniko::kurbo::{Affine, Rect as KRect};
use peniko::{Color, Fill};

use crate::net::MailNet;
use crate::sanitize::Sanitized;
use crate::{Error, LinkRect, Rect, RenderOptions, Rendered, fonts, ua};

/// Alto del viewport para el layout (px CSS). Solo afecta a unidades `vh` y similares: el documento
/// se pinta entero, no solo el viewport.
const LAYOUT_VIEWPORT_HEIGHT: u32 = 900;
/// Alto máximo de cada franja de pintura (px físicos). vello_cpu usa u16 para las dimensiones.
const TILE_HEIGHT: u32 = 4096;
/// Pasadas de `resolve` como máximo (cada una ingiere los recursos pedidos en la anterior).
const MAX_RESOLVE_PASSES: usize = 6;
/// Ancho máximo del búfer respecto al viewport: un boletín de 700 px en 600 px se ve entero; uno
/// de 5000 px se corta.
const MAX_WIDTH_FACTOR: f32 = 2.0;

/// Base ficticia para URLs relativas: nunca se descarga nada de ella.
const MAIL_BASE_URL: &str = "x-iuredesk-mail://message/";

const LIGHT_BG: Color = Color::from_rgba8(255, 255, 255, 255);
const DARK_BG: Color = Color::from_rgba8(0x1e, 0x1f, 0x22, 255);

/// Hoja de agente de usuario extra en modo oscuro: solo se aplica si el correo dice soportarlo, y
/// al ser de origen «agente de usuario» cualquier regla del correo la vence.
static DARK_SHEET: LazyLock<UserAgentStylesheet> = LazyLock::new(|| {
    UserAgentStylesheet::parse(
        "html { background-color: #1e1f22; color: #e3e3e3; color-scheme: dark; } a:link, a:visited { color: #8ab4f8; }",
    )
});

/// Atributos de presentación de correo antiguo emulados con CSS (ver `ua.rs`).
static EMAIL_SHEET: LazyLock<UserAgentStylesheet> = LazyLock::new(|| UserAgentStylesheet::parse(&ua::email_ua_css()));

pub(crate) fn render_sanitized(clean: &Sanitized, opts: &RenderOptions) -> Result<Rendered, Error> {
    let scale = opts.scale;
    let dark = opts.dark_mode && clean.supports_dark;
    let vw_px = css_to_px(opts.viewport_width as f32, scale);
    let vh_px = css_to_px(LAYOUT_VIEWPORT_HEIGHT as f32, scale);

    let net = Arc::new(MailNet::new(
        &opts.inline_resources,
        opts.fetcher.clone(),
        opts.allow_remote_images,
        opts.max_resource_bytes,
    ));
    let mut ua_stylesheets = vec![UserAgentStylesheet::default_sheet(), EMAIL_SHEET.clone()];
    if dark {
        ua_stylesheets.push(DARK_SHEET.clone());
    }
    let config = DocumentConfig {
        viewport: Some(Viewport::new(vw_px, vh_px, scale, if dark { ColorScheme::Dark } else { ColorScheme::Light })),
        // Sin base, Blitz entra en pánico al resolver una URL relativa (`<img src="a.png">`). Esta
        // base resuelve cualquier relativa a un esquema que el proveedor de red ignora.
        base_url: Some(MAIL_BASE_URL.to_owned()),
        net_provider: Some(Arc::clone(&net) as _),
        font_ctx: Some(fonts::font_context()),
        ua_stylesheets,
        ..Default::default()
    };

    let mut html_doc = HtmlDocument::from_html(&clean.html, config);
    let doc: &mut BaseDocument = &mut html_doc;

    // Cada pasada ingiere lo que la anterior pidió (imágenes, hojas, fuentes). El proveedor responde
    // en el acto, así que basta con repetir mientras siga habiendo peticiones nuevas.
    let mut last_served = usize::MAX;
    for _ in 0..MAX_RESOLVE_PASSES {
        doc.resolve(0.0);
        let served = net.served.load(Ordering::Relaxed);
        if served == last_served {
            break;
        }
        last_served = served;
    }

    let Some(root) = doc.try_root_element() else {
        return Err(Error::Empty);
    };
    let layout = root.final_layout();
    let content_w = layout.size.width.max(layout.scrollable_overflow_rect.right);
    let content_h = layout.size.height.max(layout.scrollable_overflow_rect.bottom);
    if !content_h.is_finite() || !content_w.is_finite() {
        return Err(Error::Empty);
    }

    let width_css = content_w.clamp(opts.viewport_width as f32, opts.viewport_width as f32 * MAX_WIDTH_FACTOR);
    let width = css_to_px(width_css, scale).max(1);
    let full_h = css_to_px(content_h, scale).max(1);
    let max_h = css_to_px(opts.max_height as f32, scale).max(1);
    let truncated = full_h > max_h;
    let height = full_h.min(max_h);

    let links = collect_links(doc, scale, height);

    let bg = if dark { DARK_BG } else { LIGHT_BG };
    let row_bytes = (width as usize).saturating_mul(4);
    let mut rgba: Vec<u8> = Vec::with_capacity(row_bytes.saturating_mul(height as usize));
    let mut y = 0u32;
    while y < height {
        let tile_h = (height - y).min(TILE_HEIGHT);
        doc.set_viewport_scroll(blitz_dom::util::Point { x: 0.0, y: f64::from(y) / f64::from(scale) });
        let tile = render_to_buffer::<VelloCpuImageRenderer, _>(
            |scene| {
                scene.fill(
                    Fill::NonZero,
                    Affine::IDENTITY,
                    bg,
                    None,
                    &KRect::new(0.0, 0.0, f64::from(width), f64::from(tile_h)),
                );
                blitz_paint::paint_scene(scene, doc, f64::from(scale), width, tile_h, 0, 0);
            },
            width,
            tile_h,
        );
        if tile.len() != row_bytes.saturating_mul(tile_h as usize) {
            return Err(Error::EnginePanic("tamaño de franja inesperado".to_owned()));
        }
        rgba.extend_from_slice(&tile);
        y += tile_h;
    }
    doc.set_viewport_scroll(blitz_dom::util::Point { x: 0.0, y: 0.0 });

    Ok(Rendered { width, height, rgba, links, blocked: net.take_blocked(), truncated, dark_applied: dark })
}

fn css_to_px(css: f32, scale: f32) -> u32 {
    let v = (css * scale).ceil();
    if v.is_finite() && v > 0.0 { v.min(u32::MAX as f32) as u32 } else { 0 }
}

/// Zonas de cada `<a href>` (una por línea si el enlace se parte), en px físicos.
fn collect_links(doc: &BaseDocument, scale: f32, max_h: u32) -> Vec<LinkRect> {
    let mut links = Vec::new();
    for id in TreeTraverser::new(doc) {
        let Some(node) = doc.get_node(id) else { continue };
        if !node.data.is_element_with_tag_name(&local_name!("a")) {
            continue;
        }
        let Some(href) = node.attr(local_name!("href")) else { continue };
        let href = href.trim();
        if href.is_empty() {
            continue;
        }
        for r in doc.node_client_rects(id) {
            let rect = Rect {
                x: (r.x * f64::from(scale)) as f32,
                y: (r.y * f64::from(scale)) as f32,
                width: (r.width * f64::from(scale)) as f32,
                height: (r.height * f64::from(scale)) as f32,
            };
            if rect.width <= 0.0 || rect.height <= 0.0 || rect.y >= max_h as f32 {
                continue;
            }
            links.push(LinkRect { rect, href: href.to_owned() });
        }
    }
    links
}
