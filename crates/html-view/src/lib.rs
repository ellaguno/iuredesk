//! Render de correo HTML sin webview (capa L5).
//!
//! Tubería: HTML hostil → [`sanitize`] (html5ever, por tokens) → Blitz (`blitz-html` + `stylo` +
//! `taffy` + `parley`) → `blitz-paint` → `vello_cpu` → búfer RGBA8 + mapa de enlaces.
//!
//! - No ejecuta JavaScript (Blitz no tiene motor de JS) y además el saneador quita `<script>`,
//!   manejadores `on*`, `javascript:`, `<iframe>`, `<object>`, `meta refresh`, etc.
//! - Nunca toca la red por sí mismo: las imágenes `cid:` salen de [`RenderOptions::inline_resources`],
//!   las `data:` se decodifican aquí y las remotas solo se piden a un [`Fetcher`] que aporta quien
//!   llama, y solo si [`RenderOptions::allow_remote_images`] es `true`. Las bloqueadas se listan en
//!   [`Rendered::blocked`] para la barra «Cargar imágenes».
//! - No depende de egui: entrega píxeles; la textura la sube `ui`.
//! - Si algo falla (incluido un pánico dentro de Blitz) devuelve [`Error`]; la UI cae entonces a
//!   [`to_plain_text`].
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable,
    clippy::indexing_slicing
)]

mod engine;
mod fonts;
mod net;
pub mod print;
pub mod sanitize;
mod text;
mod ua;

use std::collections::HashMap;
use std::sync::Arc;

pub use blitz_traits::net::Bytes;
pub use sanitize::{Sanitized, sanitize};
pub use text::to_plain_text;

/// Errores del render. Ante cualquiera, la UI muestra [`to_plain_text`].
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// El HTML supera [`RenderOptions::max_input_bytes`].
    #[error("el HTML es demasiado grande ({0} bytes)")]
    TooLarge(usize),
    /// Opciones imposibles (ancho 0, escala no finita…).
    #[error("opciones de render inválidas: {0}")]
    InvalidOptions(&'static str),
    /// El documento no produjo nada que pintar.
    #[error("el documento no tiene contenido que pintar")]
    Empty,
    /// Blitz (o una de sus dependencias) entró en pánico; se atrapó para no tumbar la app.
    #[error("el motor HTML falló: {0}")]
    EnginePanic(String),
}

/// Obtiene recursos remotos (http/https) cuando el usuario los permite.
///
/// Es síncrono a propósito: el render corre en un hilo de fondo y quien implementa decide tiempo
/// de espera, caché y tamaño máximo (p. ej. `engine` descarga por adelantado con tokio y aquí solo
/// consulta su caché). Devolver `None` equivale a «no disponible».
pub trait Fetcher: Send + Sync {
    /// Bytes del recurso `url` (absoluta, http o https), o `None`.
    fn fetch(&self, url: &str) -> Option<Bytes>;
}

/// Opciones de un render.
#[derive(Clone)]
pub struct RenderOptions {
    /// Ancho del viewport en px CSS (lo típico en correo: 600–900).
    pub viewport_width: u32,
    /// Factor de escala (HiDPI). El búfer mide `viewport_width * scale` px de ancho.
    pub scale: f32,
    /// La UI está en modo oscuro. Activa `prefers-color-scheme: dark`; el fondo solo se oscurece si
    /// el correo declara que soporta modo oscuro (si no, se respeta su diseño claro).
    pub dark_mode: bool,
    /// Permitir imágenes/CSS/fuentes remotas (por defecto no: protege de píxeles espía).
    pub allow_remote_images: bool,
    /// Recursos en línea por Content-ID (sin `<>`), p. ej. `"logo@empresa"` → bytes PNG.
    pub inline_resources: HashMap<String, Bytes>,
    /// Quién descarga recursos remotos cuando están permitidos. Sin él no se descarga nada.
    pub fetcher: Option<Arc<dyn Fetcher>>,
    /// Alto máximo en px CSS; lo que sobre se corta y [`Rendered::truncated`] queda en `true`.
    pub max_height: u32,
    /// Tamaño máximo del HTML de entrada.
    pub max_input_bytes: usize,
    /// Tamaño máximo de cada recurso (imagen, hoja de estilo, fuente).
    pub max_resource_bytes: usize,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            viewport_width: 800,
            scale: 1.0,
            dark_mode: false,
            allow_remote_images: false,
            inline_resources: HashMap::new(),
            fetcher: None,
            max_height: 12_000,
            max_input_bytes: 8 * 1024 * 1024,
            max_resource_bytes: 10 * 1024 * 1024,
        }
    }
}

impl std::fmt::Debug for RenderOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RenderOptions")
            .field("viewport_width", &self.viewport_width)
            .field("scale", &self.scale)
            .field("dark_mode", &self.dark_mode)
            .field("allow_remote_images", &self.allow_remote_images)
            .field("inline_resources", &self.inline_resources.len())
            .field("fetcher", &self.fetcher.is_some())
            .field("max_height", &self.max_height)
            .finish_non_exhaustive()
    }
}

/// Rectángulo en píxeles físicos del búfer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    /// ¿Contiene el punto (px físicos)?
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.width && y < self.y + self.height
    }
}

/// Zona clicable de un enlace. Un enlace partido en varias líneas da varias zonas.
#[derive(Clone, Debug, PartialEq)]
pub struct LinkRect {
    pub rect: Rect,
    /// `href` tal cual quedó tras el saneado (http, https, mailto, tel o `#ancla`).
    pub href: String,
}

/// Resultado del render.
#[derive(Clone)]
pub struct Rendered {
    /// Ancho en px físicos.
    pub width: u32,
    /// Alto en px físicos.
    pub height: u32,
    /// Píxeles RGBA8, fila a fila, opacos (`width * height * 4` bytes).
    pub rgba: Vec<u8>,
    /// Enlaces y su zona, en px físicos.
    pub links: Vec<LinkRect>,
    /// Recursos remotos que se bloquearon (URLs únicas, en orden de aparición).
    pub blocked: Vec<String>,
    /// El documento era más alto que [`RenderOptions::max_height`] y se cortó.
    pub truncated: bool,
    /// Se pintó con fondo oscuro porque el correo declara soporte de modo oscuro.
    pub dark_applied: bool,
}

impl std::fmt::Debug for Rendered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Rendered")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("links", &self.links.len())
            .field("blocked", &self.blocked.len())
            .field("truncated", &self.truncated)
            .field("dark_applied", &self.dark_applied)
            .finish_non_exhaustive()
    }
}

impl Rendered {
    /// Enlace bajo el punto (px físicos), si lo hay.
    pub fn link_at(&self, x: f32, y: f32) -> Option<&str> {
        self.links.iter().rev().find(|l| l.rect.contains(x, y)).map(|l| l.href.as_str())
    }
}

/// Sanea y pinta `html`. Nunca entra en pánico: los pánicos internos de Blitz se atrapan y vuelven
/// como [`Error::EnginePanic`].
pub fn render(html: &str, opts: &RenderOptions) -> Result<Rendered, Error> {
    if html.len() > opts.max_input_bytes {
        return Err(Error::TooLarge(html.len()));
    }
    if opts.viewport_width == 0 || opts.viewport_width > 4096 {
        return Err(Error::InvalidOptions("viewport_width fuera de 1..=4096"));
    }
    if !opts.scale.is_finite() || opts.scale < 0.25 || opts.scale > 8.0 {
        return Err(Error::InvalidOptions("scale fuera de 0.25..=8"));
    }
    if opts.max_height == 0 {
        return Err(Error::InvalidOptions("max_height es 0"));
    }
    let clean = sanitize(html);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| engine::render_sanitized(&clean, opts)));
    match result {
        Ok(r) => r,
        Err(payload) => {
            let msg = payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_owned())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "pánico sin mensaje".to_owned());
            Err(Error::EnginePanic(msg))
        }
    }
}

/// Carga (una vez por proceso) las fuentes del sistema. Llamarla al arrancar, en un hilo de fondo,
/// evita que el primer render pague el escaneo de fuentes.
pub fn warm_up() {
    fonts::warm_up();
}
