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

/// Clave de un render.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HtmlKey {
    pub id: MessageId,
    pub width: u32,
    pub ppp_x100: u32,
    pub dark: bool,
    pub remote: bool,
}

/// Resultado de un render (vuelve como `JobOutput::Custom`).
pub struct HtmlDone {
    pub key: HtmlKey,
    pub result: Result<iuredesk_html_view::Rendered, String>,
}

/// Estado de un render.
pub enum HtmlState {
    Pending,
    Ready { tex: TextureHandle, size_px: [usize; 2], links: Vec<LinkRect>, blocked: Vec<String>, scale: f32 },
    Failed,
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
    order: Vec<HtmlKey>,
    /// Mensajes cuyas imágenes remotas se permitieron, con lo descargado.
    remote: HashMap<MessageId, Arc<HashMap<String, Bytes>>>,
    /// Descargas en curso.
    fetching: HashSet<MessageId>,
}

impl HtmlCache {
    /// El render para esta clave, encolándolo si falta.
    pub fn get(
        &mut self,
        session: &mut Session,
        key: HtmlKey,
        html: &str,
        inline: &[(String, Vec<u8>)],
    ) -> Option<&HtmlState> {
        if !self.entries.contains_key(&key) {
            if self.order.len() >= MAX_ENTRIES
                && let Some(old) = self.order.first().copied()
            {
                self.order.remove(0);
                self.entries.remove(&old);
            }
            let html = html.to_string();
            let inline: HashMap<String, Bytes> =
                inline.iter().map(|(k, v)| (k.clone(), Bytes::from(v.clone()))).collect();
            let fetched = if key.remote { self.remote.get(&key.id).cloned() } else { None };
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
                let result = iuredesk_html_view::render(&html, &opts).map_err(|e| e.to_string());
                JobOutput::Custom(Box::new(HtmlDone { key, result }))
            });
            self.entries.insert(key, HtmlState::Pending);
            self.order.push(key);
        }
        self.entries.get(&key)
    }

    /// Llegó un render.
    pub fn on_done(&mut self, ctx: &Context, _job: JobId, done: HtmlDone) {
        let state = match done.result {
            Ok(r) => {
                let size = [r.width as usize, r.height as usize];
                if size[0] == 0 || size[1] == 0 || r.rgba.len() < size[0] * size[1] * 4 {
                    HtmlState::Failed
                } else {
                    let img = ColorImage::from_rgba_unmultiplied(size, &r.rgba);
                    let tex = ctx.load_texture(format!("mail-html-{}", done.key.id.get()), img, TextureOptions::LINEAR);
                    HtmlState::Ready {
                        tex,
                        size_px: size,
                        links: r.links,
                        blocked: r.blocked,
                        scale: done.key.ppp_x100 as f32 / 100.0,
                    }
                }
            }
            Err(_) => HtmlState::Failed,
        };
        if self.entries.contains_key(&done.key) {
            self.entries.insert(done.key, state);
        }
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
