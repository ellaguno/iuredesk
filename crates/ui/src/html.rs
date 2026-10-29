//! Cuerpos HTML: se pintan con `html-view` en el runtime de fondo y se suben como textura cuando
//! están listos. Mientras tanto se ve el texto plano. Las imágenes remotas se bloquean hasta que
//! el usuario pulsa «Mostrar»; entonces el motor las descarga y se vuelve a pintar.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use egui::{ColorImage, Context, TextureHandle, TextureOptions};
use iuredesk_engine::{Fetched, JobId, JobOutput, Session};
use iuredesk_html_view::{Bytes, Fetcher, LinkRect, RenderOptions};
use iuredesk_model::MessageId;

/// Ancho de render redondeado a este paso (no se vuelve a pintar por cada píxel de cambio).
const WIDTH_STEP: u32 = 48;
/// Renders guardados.
const MAX_ENTRIES: usize = 24;
/// Memoria máxima de las texturas guardadas (RGBA): un boletín largo a 2× puede pasar de 50 MB,
/// así que el tope por número no basta (R2.1).
pub const MAX_TEXTURE_BYTES: usize = 96 * 1024 * 1024;
/// Mensajes con imágenes remotas descargadas que se recuerdan.
const MAX_REMOTE: usize = 16;

/// Clave de un render.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HtmlKey {
    pub id: MessageId,
    pub width: u32,
    pub ppp_x100: u32,
    pub dark: bool,
    pub remote: bool,
    /// Con la historia citada (si no, se pinta sin ella).
    pub full: bool,
}

/// Resultado de un render (vuelve como `JobOutput::Custom`). La imagen ya viene convertida en el
/// hilo de fondo: en el de la interfaz solo se sube la textura.
pub struct HtmlDone {
    pub key: HtmlKey,
    pub result: Result<HtmlImage, String>,
}

/// Render listo para subir: franjas horizontales de como mucho el lado máximo de textura de la
/// GPU (un boletín largo pasa de 8 192 px de alto y `load_texture` no lo admitiría).
pub struct HtmlImage {
    tiles: Vec<ColorImage>,
    size: [usize; 2],
    links: Vec<LinkRect>,
    blocked: Vec<String>,
}

/// Lado máximo de textura si aún no se conoce el de la GPU (el mínimo que egui garantiza).
const DEFAULT_MAX_SIDE: usize = 2_048;

/// Convierte el render en franjas de imagen de egui (fuera del hilo de la interfaz).
fn to_image(r: iuredesk_html_view::Rendered, max_side: usize) -> Result<HtmlImage, String> {
    let size = [r.width as usize, r.height as usize];
    let row = size[0].saturating_mul(4);
    let need = row.saturating_mul(size[1]);
    if size[0] == 0 || size[1] == 0 || r.rgba.len() < need {
        return Err("render vacío".into());
    }
    let max_side = max_side.max(64);
    if size[0] > max_side {
        return Err("render más ancho que la textura máxima".into());
    }
    let mut tiles = Vec::with_capacity(size[1].div_ceil(max_side));
    let mut y = 0;
    while y < size[1] {
        let h = max_side.min(size[1] - y);
        let bytes = r.rgba.get(y.saturating_mul(row)..(y + h).saturating_mul(row)).unwrap_or(&[]);
        tiles.push(ColorImage::from_rgba_unmultiplied([size[0], h], bytes));
        y += h;
    }
    Ok(HtmlImage { tiles, size, links: r.links, blocked: r.blocked })
}

/// Estado de un render.
pub enum HtmlState {
    Pending,
    /// `tiles`: franjas de arriba abajo con su alto en px.
    Ready {
        tiles: Vec<(TextureHandle, usize)>,
        size_px: [usize; 2],
        links: Vec<LinkRect>,
        blocked: Vec<String>,
        scale: f32,
    },
    Failed,
}

/// Bytes de una textura RGBA.
fn tex_bytes(size: [usize; 2]) -> usize {
    size[0].saturating_mul(size[1]).saturating_mul(4)
}

/// Recursos remotos descargados, para el render con imágenes.
struct MapFetcher(HashMap<String, Bytes>);

impl Fetcher for MapFetcher {
    fn fetch(&self, url: &str) -> Option<Bytes> {
        self.0.get(url).cloned()
    }
}

/// Caché de renders y estado de imágenes remotas por mensaje.
#[derive(Default)]
pub struct HtmlCache {
    entries: HashMap<HtmlKey, HtmlState>,
    /// Del más viejo al más reciente en uso.
    order: Vec<HtmlKey>,
    /// Bytes de las texturas listas.
    bytes: usize,
    /// Mensajes cuyas imágenes remotas se permitieron, con lo descargado.
    remote: HashMap<MessageId, Arc<HashMap<String, Bytes>>>,
    /// Descargas en curso.
    fetching: HashSet<MessageId>,
    /// Lado máximo de textura de la GPU (0 = aún no se sabe).
    max_side: usize,
}

impl HtmlCache {
    /// El lado máximo de textura de la GPU (`ctx.input(|i| i.max_texture_side)`).
    pub fn set_max_texture_side(&mut self, side: usize) {
        self.max_side = side;
    }

    /// El render para esta clave, encolándolo si falta.
    pub fn get(
        &mut self,
        session: &mut Session,
        key: HtmlKey,
        html: &str,
        inline: &[(String, Vec<u8>)],
    ) -> Option<&HtmlState> {
        if self.entries.contains_key(&key) {
            // Usado de nuevo: pasa al final (LRU).
            if self.order.last() != Some(&key)
                && let Some(pos) = self.order.iter().position(|k| *k == key)
            {
                self.order.remove(pos);
                self.order.push(key);
            }
        } else {
            self.evict(MAX_ENTRIES.saturating_sub(1), MAX_TEXTURE_BYTES);
            let html = html.to_string();
            let inline: HashMap<String, Bytes> =
                inline.iter().map(|(k, v)| (k.clone(), Bytes::from(v.clone()))).collect();
            let fetched = if key.remote { self.remote.get(&key.id).cloned() } else { None };
            let max_side = if self.max_side == 0 { DEFAULT_MAX_SIDE } else { self.max_side };
            session.spawn_blocking(move || {
                let scale = (key.ppp_x100 as f32 / 100.0).clamp(0.5, 4.0);
                let mut opts = RenderOptions {
                    viewport_width: key.width.clamp(200, 4096),
                    scale,
                    dark_mode: key.dark,
                    inline_resources: inline,
                    ..Default::default()
                };
                if let Some(f) = fetched {
                    opts.allow_remote_images = true;
                    opts.fetcher = Some(Arc::new(MapFetcher((*f).clone())));
                }
                let result = iuredesk_html_view::render(&html, &opts)
                    .map_err(|e| e.to_string())
                    .and_then(|r| to_image(r, max_side));
                JobOutput::Custom(Box::new(HtmlDone { key, result }))
            });
            self.entries.insert(key, HtmlState::Pending);
            self.order.push(key);
        }
        self.entries.get(&key)
    }

    /// Quita los renders más viejos hasta quedar en `max_entries` y `max_bytes`.
    fn evict(&mut self, max_entries: usize, max_bytes: usize) {
        while (self.order.len() > max_entries || self.bytes > max_bytes) && !self.order.is_empty() {
            let old = self.order.remove(0);
            if let Some(HtmlState::Ready { size_px, .. }) = self.entries.remove(&old) {
                self.bytes = self.bytes.saturating_sub(tex_bytes(size_px));
            }
        }
    }

    /// Llegó un render.
    pub fn on_done(&mut self, ctx: &Context, _job: JobId, done: HtmlDone) {
        // Si ya no se espera (se desalojó mientras se pintaba), no se sube.
        if !self.entries.contains_key(&done.key) {
            return;
        }
        let state = match done.result {
            Ok(r) => {
                let size = r.size;
                let tiles = r
                    .tiles
                    .into_iter()
                    .enumerate()
                    .map(|(i, img)| {
                        let h = img.size[1];
                        let name = format!("mail-html-{}-{i}", done.key.id.get());
                        (ctx.load_texture(name, img, TextureOptions::LINEAR), h)
                    })
                    .collect();
                self.bytes = self.bytes.saturating_add(tex_bytes(size));
                HtmlState::Ready {
                    tiles,
                    size_px: size,
                    links: r.links,
                    blocked: r.blocked,
                    scale: done.key.ppp_x100 as f32 / 100.0,
                }
            }
            Err(_) => HtmlState::Failed,
        };
        self.entries.insert(done.key, state);
        // El recién llegado se queda aunque solo él pase del tope (es el que se está viendo).
        self.evict_keeping(done.key);
    }

    /// Desaloja por memoria sin tocar `keep`.
    fn evict_keeping(&mut self, keep: HtmlKey) {
        let mut i = 0;
        while self.bytes > MAX_TEXTURE_BYTES && i < self.order.len() {
            match self.order.get(i).copied() {
                Some(k) if k != keep => {
                    self.order.remove(i);
                    if let Some(HtmlState::Ready { size_px, .. }) = self.entries.remove(&k) {
                        self.bytes = self.bytes.saturating_sub(tex_bytes(size_px));
                    }
                }
                _ => i += 1,
            }
        }
    }

    /// Memoria de las texturas guardadas (diagnóstico).
    pub fn texture_bytes(&self) -> usize {
        self.bytes
    }

    /// Renders guardados (diagnóstico).
    pub fn renders(&self) -> usize {
        self.entries.len()
    }

    /// El usuario pidió ver las imágenes remotas de `id`.
    pub fn allow_remote(&mut self, session: &mut Session, id: MessageId, urls: Vec<String>) {
        if self.fetching.insert(id) {
            session.fetch_remote(id.get(), urls);
        }
    }

    /// ¿Se permitieron (y descargaron) las imágenes de `id`?
    pub fn remote_ready(&self, id: MessageId) -> bool {
        self.remote.contains_key(&id)
    }

    /// ¿Se están descargando?
    pub fn remote_loading(&self, id: MessageId) -> bool {
        self.fetching.contains(&id) && !self.remote.contains_key(&id)
    }

    /// Llegaron las descargas.
    pub fn on_fetched(&mut self, f: Fetched) {
        let id = MessageId(f.tag);
        self.fetching.remove(&id);
        let map: HashMap<String, Bytes> = f.items.into_iter().map(|(u, b)| (u, Bytes::from(b))).collect();
        if self.remote.len() >= MAX_REMOTE && !self.remote.contains_key(&id) {
            // Se olvida uno cualquiera: volver a verlo solo pide descargar otra vez.
            if let Some(k) = self.remote.keys().next().copied() {
                self.remote.remove(&k);
            }
        }
        self.remote.insert(id, Arc::new(map));
    }

    /// Ancho de render para un ancho disponible (en puntos).
    pub fn width_for(points: f32) -> u32 {
        let w = points.clamp(240.0, 1600.0) as u32;
        (w / WIDTH_STEP).max(1) * WIDTH_STEP
    }

    /// Hay renders pendientes (las capturas esperan).
    pub fn pending(&self) -> bool {
        self.entries.values().any(|s| matches!(s, HtmlState::Pending))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(id: u64) -> HtmlKey {
        HtmlKey { id: MessageId(id), width: 960, ppp_x100: 100, dark: false, remote: false, full: false }
    }

    fn rendered(w: u32, h: u32) -> iuredesk_html_view::Rendered {
        iuredesk_html_view::Rendered {
            width: w,
            height: h,
            rgba: vec![255; (w * h * 4) as usize],
            links: Vec::new(),
            blocked: Vec::new(),
            truncated: false,
            dark_applied: false,
        }
    }

    /// Simula un render de `w × h` px que llega para `k` (ya pedido).
    fn arrive(cache: &mut HtmlCache, ctx: &Context, k: HtmlKey, w: u32, h: u32) {
        cache.entries.insert(k, HtmlState::Pending);
        cache.order.push(k);
        let result = to_image(rendered(w, h), ctx.input(|i| i.max_texture_side));
        cache.on_done(ctx, 0, HtmlDone { key: k, result });
    }

    /// R2.1: un boletín más alto que la textura máxima se parte en franjas (antes `load_texture`
    /// entraba en pánico); uno más ancho falla con error y se ve el texto.
    #[test]
    fn tall_renders_are_tiled() {
        let img = to_image(rendered(600, 5_000), 2_048).unwrap();
        assert_eq!(img.tiles.iter().map(|t| t.size[1]).collect::<Vec<_>>(), vec![2_048, 2_048, 904]);
        assert!(img.tiles.iter().all(|t| t.size[0] == 600));
        assert!(to_image(rendered(3_000, 100), 2_048).is_err());
        assert!(to_image(rendered(0, 100), 2_048).is_err());
    }

    #[test]
    fn texture_memory_stays_under_budget() {
        let ctx = Context::default();
        let mut cache = HtmlCache::default();
        // Cada boletín: 1000 × 6000 px = 24 MB. Con 10, sin tope por memoria serían 240 MB.
        for id in 1..=10 {
            arrive(&mut cache, &ctx, key(id), 1000, 6000);
            assert!(cache.texture_bytes() <= MAX_TEXTURE_BYTES, "{}", cache.texture_bytes());
        }
        assert_eq!(cache.renders(), MAX_TEXTURE_BYTES / (24_000_000));
        // Se conservan los más recientes.
        assert!(cache.entries.contains_key(&key(10)));
        assert!(!cache.entries.contains_key(&key(1)));
    }

    #[test]
    fn a_single_huge_render_is_kept() {
        let ctx = Context::default();
        let mut cache = HtmlCache::default();
        arrive(&mut cache, &ctx, key(1), 100, 100);
        // Más grande que todo el tope: se queda (es el que se ve) y desaloja a los demás.
        arrive(&mut cache, &ctx, key(2), 2000, 14000);
        assert!(cache.entries.contains_key(&key(2)));
        assert!(!cache.entries.contains_key(&key(1)));
        assert_eq!(cache.texture_bytes(), 2000 * 14000 * 4);
    }
}
