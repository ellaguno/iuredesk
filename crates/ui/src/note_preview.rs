//! UX-8: vista previa nativa de una nota. El Markdown se convierte en HTML (`html-view::markdown`),
//! se leen sus imágenes locales (acotadas, dentro de la carpeta de notas) y se pinta con Blitz, todo
//! en el runtime de fondo; aquí solo se sube la textura. Caché por huella del contenido: lo que se
//! guarda en IureEditor llega por la vigilancia de `notes_fs`, cambia la huella y se vuelve a
//! pintar (mientras tanto se sigue viendo el render anterior).
//!
//! Enlaces: `http`/`https` al navegador, `mailto:` al redactor, `iureditor:` a IureEditor y las
//! casillas `- [ ]` se marcan aquí (reescriben el Markdown, con deshacer). Las imágenes remotas
//! siguen Ajustes → Privacidad (UX-5): con «Siempre» o de dominios permitidos se cargan solas; si
//! no, un aviso con «Mostrar».
//!
//! S1.19: pulsar un mapa mental abre un menú para elegir su distribución (o la de Preferencias) y
//! la forma de sus nodos; se guarda en el metadato del mapa, con deshacer.
//!
//! UX-15: las respuestas del asistente se pintan igual (Markdown → HTML → Blitz) con su propia
//! caché ([`Owner::Chat`]): sin imágenes locales ni remotas y, mientras se pinta, el texto tal cual.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;

use egui::{Color32, Context, Rect, RichText, Sense, TextureHandle, TextureOptions, Ui, pos2, vec2};
use iuredesk_engine::{Fetched, JobOutput, Text};
use iuredesk_html_view::markdown::{self, NoteLink, NoteOptions, NoteStyle};
use iuredesk_html_view::mindmap::{self, Layout, Shape};
use iuredesk_html_view::{Bytes, LinkRect, RenderOptions};
use iuredesk_model::NoteId;
use iurefficient_ui::{Banner, Severity, pick, theme};

use crate::app::DeskApp;
use crate::html::{HtmlImage, MapFetcher};

/// Marca de las descargas de imágenes de notas (`Session::fetch_remote` usa la misma `tag` que el
/// correo; los ids de mensaje nunca llegan a este bit).
pub const NOTE_TAG: u64 = 1 << 62;
/// Renders guardados.
const MAX_ENTRIES: usize = 8;
/// Renders guardados del chat (una conversación cabe entera).
pub const CHAT_ENTRIES: usize = 48;
/// Bit de los ids de las respuestas del chat (nunca llega a un id de nota real).
pub const CHAT_BIT: u64 = 1 << 61;

/// De quién es el render.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    Note,
    Chat,
}
/// Memoria máxima de las texturas guardadas.
const MAX_TEXTURE_BYTES: usize = 64 * 1024 * 1024;
/// Bytes de imágenes locales por nota (además del tope de cada una en `notes_fs`).
const MAX_LOCAL_BYTES: usize = 48 * 1024 * 1024;
/// Alto máximo del render en px CSS.
const MAX_HEIGHT: u32 = 40_000;

/// Clave de un render.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NoteKey {
    pub id: NoteId,
    /// Huella del texto y del estilo.
    pub hash: u64,
    pub width: u32,
    pub ppp_x100: u32,
    pub remote: bool,
}

/// Resultado de un render (vuelve como `JobOutput::Custom`).
pub struct NoteDone {
    key: NoteKey,
    result: Result<(HtmlImage, Vec<String>), String>,
}

impl NoteDone {
    /// ¿Es de una respuesta del chat?
    pub fn is_chat(&self) -> bool {
        self.key.id.get() & CHAT_BIT != 0
    }
}

enum State {
    Pending,
    Ready {
        tiles: Vec<(TextureHandle, usize)>,
        size_px: [usize; 2],
        links: Vec<LinkRect>,
        blocked: Vec<String>,
        deep_links: Vec<String>,
        scale: f32,
    },
    Failed,
}

/// Caché de vistas previas de notas.
#[derive(Default)]
pub struct NotePreviewCache {
    entries: HashMap<NoteKey, State>,
    order: Vec<NoteKey>,
    bytes: usize,
    /// Último render listo de cada nota (se ve mientras se pinta el nuevo).
    last_ready: HashMap<NoteId, NoteKey>,
    /// Imágenes remotas descargadas por nota, con la huella del texto para el que se pidieron.
    remote: HashMap<NoteId, (u64, Arc<HashMap<String, Bytes>>)>,
    /// Descargas en curso, con la huella del texto para el que se pidieron.
    fetching: HashMap<NoteId, u64>,
    /// Notas cuyas imágenes remotas permitió el usuario en esta sesión.
    allowed: HashSet<NoteId>,
    /// Renders guardados como mucho (0: [`MAX_ENTRIES`]).
    cap: usize,
    /// S1.19: menú abierto de un mapa mental.
    menu: Option<MindmapMenu>,
}

/// S1.19: el menú de un mapa mental (nota, orden del mapa, dónde se pulsó y lo que pide ahora).
#[derive(Clone, Copy, Debug)]
struct MindmapMenu {
    note: NoteId,
    n: usize,
    pos: egui::Pos2,
    layout: Option<Layout>,
    shape: Shape,
    /// Recién abierto: el clic que lo abrió no lo cierra.
    fresh: bool,
}

fn tex_bytes(size: [usize; 2]) -> usize {
    size[0].saturating_mul(size[1]).saturating_mul(4)
}

fn rgb(c: Color32) -> [u8; 3] {
    [c.r(), c.g(), c.b()]
}

impl NotePreviewCache {
    /// Caché que guarda hasta `n` renders.
    pub fn with_capacity(n: usize) -> Self {
        NotePreviewCache { cap: n, ..Default::default() }
    }

    fn cap(&self) -> usize {
        if self.cap == 0 { MAX_ENTRIES } else { self.cap }
    }

    /// ¿Hay renders pendientes? (las capturas y las pruebas esperan).
    pub fn pending(&self) -> bool {
        self.entries.values().any(|s| matches!(s, State::Pending))
    }

    /// Llegó un render.
    pub fn on_done(&mut self, ctx: &Context, done: NoteDone) {
        if !self.entries.contains_key(&done.key) {
            return;
        }
        let state = match done.result {
            Ok((img, deep_links)) => {
                let size = img.size;
                let tiles = img
                    .tiles
                    .into_iter()
                    .enumerate()
                    .map(|(i, t)| {
                        let h = t.size[1];
                        (
                            ctx.load_texture(
                                format!("note-preview-{}-{i}", done.key.id.get()),
                                t,
                                TextureOptions::LINEAR,
                            ),
                            h,
                        )
                    })
                    .collect();
                self.bytes = self.bytes.saturating_add(tex_bytes(size));
                if let Some(old) = self.last_ready.insert(done.key.id, done.key)
                    && old != done.key
                {
                    self.remove(old);
                }
                State::Ready {
                    tiles,
                    size_px: size,
                    links: img.links,
                    blocked: img.blocked,
                    deep_links,
                    scale: done.key.ppp_x100 as f32 / 100.0,
                }
            }
            Err(_) => State::Failed,
        };
        self.entries.insert(done.key, state);
        // Por memoria: los más viejos, nunca el recién llegado.
        let mut i = 0;
        while (self.bytes > MAX_TEXTURE_BYTES || self.order.len() > self.cap()) && i < self.order.len() {
            match self.order.get(i).copied() {
                Some(k) if k != done.key => self.remove(k),
                _ => i += 1,
            }
        }
    }

    fn remove(&mut self, k: NoteKey) {
        if let Some(pos) = self.order.iter().position(|o| *o == k) {
            self.order.remove(pos);
        }
        if let Some(State::Ready { size_px, .. }) = self.entries.remove(&k) {
            self.bytes = self.bytes.saturating_sub(tex_bytes(size_px));
        }
        if self.last_ready.get(&k.id) == Some(&k) {
            self.last_ready.remove(&k.id);
        }
    }

    /// Llegaron las imágenes remotas de una nota.
    pub fn on_fetched(&mut self, f: Fetched) {
        let id = NoteId(f.tag & !NOTE_TAG);
        let Some(hash) = self.fetching.remove(&id) else { return };
        let map: HashMap<String, Bytes> = f.items.into_iter().map(|(u, b)| (u, Bytes::from(b))).collect();
        if self.remote.len() >= 16 && !self.remote.contains_key(&id) {
            self.remote.clear();
        }
        self.remote.insert(id, (hash, Arc::new(map)));
    }
}

/// Estilo de la nota con los colores del tema y la densidad actuales.
fn style(ui: &Ui) -> NoteStyle {
    let c = theme::colors(ui);
    NoteStyle {
        text: rgb(c.text),
        muted: rgb(c.text_muted),
        accent: rgb(c.accent),
        code_bg: rgb(c.surface_alt),
        border: rgb(c.separator),
        font_px: theme::density(ui.ctx()).body_size() + 1.0,
    }
}

/// S1.19: la distribución de Preferencias, en el tipo de `html-view`.
pub fn layout_of(pref: iuredesk_engine::MindmapLayout) -> Layout {
    match pref {
        iuredesk_engine::MindmapLayout::Right => Layout::Right,
        iuredesk_engine::MindmapLayout::Radial => Layout::Radial,
        iuredesk_engine::MindmapLayout::Balanced => Layout::Balanced,
        iuredesk_engine::MindmapLayout::Down => Layout::Down,
    }
}

fn layout_label(l: Layout) -> &'static str {
    match l {
        Layout::Right => pick("To the right", "Hacia la derecha"),
        Layout::Radial => pick("Radial (as in IureEditor)", "Radial (como IureEditor)"),
        Layout::Balanced => pick("Both sides", "A ambos lados"),
        Layout::Down => pick("Top to bottom", "De arriba abajo"),
    }
}

fn shape_label(s: Shape) -> &'static str {
    match s {
        Shape::Default => pick("Pill (default)", "Píldora (omisión)"),
        Shape::Square => pick("Square", "Cuadrado"),
        Shape::Rounded => pick("Rounded", "Redondeado"),
        Shape::Circle => pick("Circle", "Círculo"),
        Shape::Cloud => pick("Cloud", "Nube"),
        Shape::Bang => pick("Burst", "Explosión"),
        Shape::Hexagon => pick("Hexagon", "Hexágono"),
    }
}

fn options(ui: &Ui, mindmap_layout: Layout) -> NoteOptions {
    NoteOptions {
        style: style(ui),
        mindmap_layout,
        mindmap_label: pick("Mind map", "Mapa mental").to_string(),
        mermaid_alt: pick("Diagram", "Diagrama").to_string(),
        mermaid_label: pick(
            "Mermaid diagram that couldn't be drawn here — open it in IureEditor",
            "Diagrama Mermaid que no se pudo dibujar aquí — ábrelo en IureEditor",
        )
        .to_string(),
        math_label: pick(
            "Formula — open it in IureEditor to see it typeset",
            "Fórmula — ábrela en IureEditor para verla compuesta",
        )
        .to_string(),
    }
}

/// Qué pidió el usuario en la vista previa.
pub enum PreviewAct {
    /// Marcar o desmarcar la casilla `n`.
    Task(usize),
    /// Abrir un enlace.
    Open(String),
    /// S1.19: el mapa mental `n`, pulsado en ese punto de la pantalla.
    Mindmap(usize, egui::Pos2),
}

/// Pinta la vista previa de la nota `id` con el texto `body`. Devuelve lo que se pulsó.
pub fn show(app: &mut DeskApp, ui: &mut Ui, id: NoteId, body: &str) -> Option<PreviewAct> {
    let c = theme::colors(ui);
    if body.trim().is_empty() {
        ui.label(
            RichText::new(pick(
                "This note is empty. Press Edit (Ctrl+Shift+E) to write in it.",
                "Esta nota está vacía. Pulsa Editar (Ctrl+Shift+E) para escribir en ella.",
            ))
            .color(c.text_muted),
        );
        return None;
    }
    render(app, ui, Owner::Note, id, body)
}

/// UX-15: una respuesta del asistente (la línea `n` del chat) con su Markdown pintado.
pub fn chat(app: &mut DeskApp, ui: &mut Ui, n: usize, body: &str) {
    let id = NoteId(CHAT_BIT | (n as u64 & (CHAT_BIT - 1)));
    if let Some(PreviewAct::Open(url)) = render(app, ui, Owner::Chat, id, body) {
        open_link(app, &url);
    }
}

fn render(app: &mut DeskApp, ui: &mut Ui, owner: Owner, id: NoteId, body: &str) -> Option<PreviewAct> {
    let ctx = ui.ctx().clone();
    let c = theme::colors(ui);
    let quiet = owner == Owner::Chat;
    let opts = options(ui, layout_of(app.session.prefs.mindmap_layout));
    let bg = rgb(c.surface);
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    body.hash(&mut hasher);
    format!("{:?}{bg:?}", opts).hash(&mut hasher);
    let hash = hasher.finish();
    let width = crate::html::HtmlCache::width_for(ui.available_width());
    let cache = match owner {
        Owner::Note => &mut app.note_preview,
        Owner::Chat => &mut app.chat_preview,
    };
    let remote = cache.remote.get(&id).filter(|(h, _)| *h == hash).map(|(_, m)| Arc::clone(m));
    let key = NoteKey {
        id,
        hash,
        width,
        ppp_x100: (ctx.pixels_per_point() * 100.0).round() as u32,
        remote: remote.is_some(),
    };
    if !cache.entries.contains_key(&key) {
        while cache.order.len() >= cache.cap() {
            // El más viejo que no sea el último listo de esta nota (se ve mientras tanto).
            let keep = cache.last_ready.get(&id).copied();
            let Some(old) = cache.order.iter().copied().find(|k| Some(*k) != keep) else { break };
            cache.remove(old);
        }
        let md = body.to_string();
        let root: Option<PathBuf> = if quiet { None } else { app.session.notes_dir() };
        let max_side = ctx.input(|i| i.max_texture_side).max(64);
        app.session.spawn_blocking(move || {
            let doc = markdown::to_html(&md, &opts);
            let mut inline = HashMap::new();
            // UX-12: mapas mentales dibujados aquí, como imágenes SVG.
            for (cid, svg) in &doc.diagrams {
                inline.insert(cid.clone(), Bytes::from(svg.clone()));
            }
            if let Some(root) = root {
                let mut total = 0usize;
                for (i, rel) in doc.images.iter().enumerate() {
                    let Some(b) = iuredesk_engine::notes_fs::read_note_image(&root, rel) else { continue };
                    total = total.saturating_add(b.len());
                    if total > MAX_LOCAL_BYTES {
                        break;
                    }
                    inline.insert(markdown::image_cid(i), Bytes::from(b));
                }
            }
            let mut ro = RenderOptions {
                viewport_width: key.width.clamp(200, 4096),
                scale: (key.ppp_x100 as f32 / 100.0).clamp(0.5, 4.0),
                inline_resources: inline,
                background: Some(bg),
                max_height: MAX_HEIGHT,
                ..Default::default()
            };
            if let Some(f) = remote {
                ro.allow_remote_images = true;
                ro.fetcher = Some(Arc::new(MapFetcher((*f).clone())));
            }
            let result = iuredesk_html_view::render(&doc.html, &ro)
                .map_err(|e| e.to_string())
                .and_then(|r| crate::html::to_image(r, max_side))
                .map(|img| (img, doc.deep_links));
            JobOutput::Custom(Box::new(NoteDone { key, result }))
        });
        cache.entries.insert(key, State::Pending);
        cache.order.push(key);
    }
    // Lo listo: el de esta clave o, mientras se pinta, el anterior de la misma nota.
    let shown = match cache.entries.get(&key) {
        Some(State::Ready { .. }) => Some(key),
        Some(State::Failed) => None,
        _ => cache.last_ready.get(&id).copied().filter(|k| matches!(cache.entries.get(k), Some(State::Ready { .. }))),
    };
    let failed = matches!(cache.entries.get(&key), Some(State::Failed));
    let Some(State::Ready { tiles, size_px, links, blocked, deep_links, scale }) =
        shown.and_then(|k| cache.entries.get(&k))
    else {
        if quiet {
            // El chat: el texto tal cual hasta que llegue el render.
        } else if failed {
            ui.label(
                RichText::new(pick(
                    "The preview could not be drawn; this is the plain text.",
                    "No se pudo dibujar la vista previa; este es el texto tal cual.",
                ))
                .color(c.text_muted)
                .size(theme::density(&ctx).small_size()),
            );
        } else {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(12.0));
                ui.label(
                    RichText::new(pick("Preparing the preview…", "Preparando la vista previa…")).color(c.text_muted),
                );
            });
        }
        if !quiet {
            ui.add_space(6.0);
        }
        ui.add(
            egui::Label::new(RichText::new(body.chars().take(20_000).collect::<String>()).color(c.text))
                .selectable(true)
                .wrap(),
        );
        return None;
    };
    let (tiles, size_px, links, blocked, deep_links, scale) =
        (tiles.clone(), *size_px, links.clone(), blocked.clone(), deep_links.clone(), *scale);
    // Imágenes remotas (UX-5).
    // Solo con el render de este texto (no el anterior que se ve mientras tanto).
    if !quiet && !blocked.is_empty() && !key.remote && shown == Some(key) && !cache.fetching.contains_key(&id) {
        let auto = cache.allowed.contains(&id) || app.session.note_images_auto(&blocked);
        if auto {
            cache.fetching.insert(id, hash);
            app.session.fetch_remote(NOTE_TAG | id.get(), blocked.clone(), None);
        } else {
            let text = format!(
                "{} ({})",
                pick(
                    "Remote images are hidden to protect your privacy.",
                    "Las imágenes remotas están ocultas para proteger tu privacidad."
                ),
                blocked.len()
            );
            if Banner::new(Severity::Info, &text).action(pick("Show", "Mostrar")).show(ui).action {
                let cache = &mut app.note_preview;
                cache.allowed.insert(id);
                cache.fetching.insert(id, hash);
                app.session.fetch_remote(NOTE_TAG | id.get(), blocked, None);
            }
            ui.add_space(6.0);
        }
    }
    let natural = vec2(size_px[0] as f32 / scale, size_px[1] as f32 / scale);
    let fit = (ui.available_width() / natural.x.max(1.0)).clamp(0.3, 1.0);
    let size = natural * fit;
    let px_per_pt = size_px[0].max(1) as f32 / size.x.max(1.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let full_h = size_px[1].max(1) as f32;
    let mut y = 0usize;
    for (tex, h) in &tiles {
        let top = rect.top() + rect.height() * (y as f32 / full_h);
        let bottom = rect.top() + rect.height() * ((y + h) as f32 / full_h);
        let r = Rect::from_min_max(pos2(rect.left(), top), pos2(rect.right(), bottom));
        ui.painter().image(tex.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        y += h;
    }
    // Lectores de pantalla: el texto de la nota.
    iurefficient_ui::a11y::label(&resp, &body.chars().take(2_000).collect::<String>());
    let mut act = None;
    if let Some(p) = resp.hover_pos() {
        let px = (p - rect.min) * px_per_pt;
        if let Some(l) = links.iter().rev().find(|l| l.rect.contains(px.x, px.y)) {
            let link = markdown::classify(&l.href, &deep_links);
            let tip = match &link {
                NoteLink::Task(_) => Some(pick("Check / uncheck", "Marcar / desmarcar").to_string()),
                NoteLink::Mindmap(_) => {
                    Some(pick("Change how this map is drawn", "Cambiar cómo se dibuja este mapa").to_string())
                }
                NoteLink::Open(u) => Some(u.clone()),
                NoteLink::None => None,
            };
            if let Some(tip) = tip {
                ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                egui::Tooltip::always_open(
                    ctx.clone(),
                    ui.layer_id(),
                    resp.id.with("link"),
                    egui::PopupAnchor::Pointer,
                )
                .show(|ui| ui.label(tip));
            }
            if resp.clicked() {
                act = match link {
                    NoteLink::Task(n) => Some(PreviewAct::Task(n)),
                    NoteLink::Mindmap(n) => Some(PreviewAct::Mindmap(n, p)),
                    NoteLink::Open(u) => Some(PreviewAct::Open(u)),
                    NoteLink::None => None,
                };
            }
        }
    }
    act
}

/// Atiende lo pulsado en la vista previa.
pub fn handle(app: &mut DeskApp, id: NoteId, act: PreviewAct) {
    match act {
        PreviewAct::Task(n) => toggle_task(app, id, n),
        PreviewAct::Open(url) => open_link(app, &url),
        PreviewAct::Mindmap(n, pos) => {
            let Some(note) = app.session.notes().into_iter().find(|x| x.id == id) else { return };
            let Some(b) = mindmap::find(&note.body).into_iter().nth(n) else { return };
            app.note_preview.menu =
                Some(MindmapMenu { note: id, n, pos, layout: b.layout, shape: b.shape, fresh: true });
        }
    }
}

/// S1.19: cambia la distribución y la forma del mapa `n` de la nota, con deshacer.
pub(crate) fn set_mindmap(app: &mut DeskApp, id: NoteId, n: usize, layout: Option<Layout>, shape: Shape) {
    crate::pim::flush_note(app);
    let Some(note) = app.session.notes().into_iter().find(|x| x.id == id) else { return };
    let Some(new_body) = mindmap::set_options(&note.body, n, layout, shape) else { return };
    let text = Text::new("Mind map changed", "Mapa mental cambiado");
    match app.session.set_note_body(id, &new_body, text) {
        Ok(iuredesk_engine::Outcome::Notice(notice)) => app.notice(notice),
        Ok(_) => {}
        Err(e) => app.notice(iuredesk_engine::Notice::error(Text::new(
            format!("Could not save the note: {e}"),
            format!("No se pudo guardar la nota: {e}"),
        ))),
    }
}

/// S1.19: el menú del mapa mental pulsado (si hay uno abierto para la nota `id`). Se cierra con
/// Esc, al pulsar fuera o al elegir.
pub fn mindmap_menu(app: &mut DeskApp, ctx: &Context, id: NoteId) {
    let Some(mut m) = app.note_preview.menu.take() else { return };
    if m.note != id || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        return;
    }
    let pref = layout_of(app.session.prefs.mindmap_layout);
    let screen = ctx.content_rect();
    let w = 240.0;
    let pos = pos2(
        (m.pos.x + 8.0).min(screen.right() - w - 12.0).max(screen.left() + 8.0),
        m.pos.y.clamp(screen.top() + 40.0, (screen.bottom() - 420.0).max(screen.top() + 40.0)),
    );
    let mut pick_opts: Option<(Option<Layout>, Shape)> = None;
    let area = egui::Area::new(egui::Id::unique("s1-19-mindmap-menu"))
        .order(egui::Order::Foreground)
        .fixed_pos(pos)
        .show(ctx, |ui| {
            let c = theme::colors(ui);
            egui::Frame::popup(ui.style()).corner_radius(10.0).inner_margin(10.0).show(ui, |ui| {
                ui.set_width(w);
                ui.label(
                    RichText::new(pick("Layout", "Distribución")).font(theme::bold_font(ui.ctx(), 13.0)).color(c.text),
                );
                let default = format!("{} — {}", pick("As in Settings", "La de Preferencias"), layout_label(pref));
                if ui.selectable_label(m.layout.is_none(), default).clicked() {
                    pick_opts = Some((None, m.shape));
                }
                for l in Layout::ALL {
                    if ui.selectable_label(m.layout == Some(l), layout_label(l)).clicked() {
                        pick_opts = Some((Some(l), m.shape));
                    }
                }
                ui.separator();
                ui.label(
                    RichText::new(pick("Node shape", "Forma de los nodos"))
                        .font(theme::bold_font(ui.ctx(), 13.0))
                        .color(c.text),
                );
                for s in Shape::ALL {
                    if ui.selectable_label(m.shape == s, shape_label(s)).clicked() {
                        pick_opts = Some((m.layout, s));
                    }
                }
            });
        });
    if let Some((layout, shape)) = pick_opts {
        set_mindmap(app, m.note, m.n, layout, shape);
        return;
    }
    if !m.fresh && area.response.clicked_elsewhere() {
        return;
    }
    m.fresh = false;
    app.note_preview.menu = Some(m);
}

/// Marca o desmarca la casilla `n` de la nota, con deshacer.
pub(crate) fn toggle_task(app: &mut DeskApp, id: NoteId, n: usize) {
    crate::pim::flush_note(app);
    let Some(note) = app.session.notes().into_iter().find(|x| x.id == id) else { return };
    let Some(new_body) = markdown::toggle_task(&note.body, n) else { return };
    let text = Text::new("Checkbox changed", "Casilla cambiada");
    match app.session.set_note_body(id, &new_body, text) {
        Ok(iuredesk_engine::Outcome::Notice(notice)) => app.notice(notice),
        Ok(_) => {}
        Err(e) => app.notice(iuredesk_engine::Notice::error(Text::new(
            format!("Could not save the note: {e}"),
            format!("No se pudo guardar la nota: {e}"),
        ))),
    }
}

/// Abre un enlace de la nota: navegador, redactor o IureEditor.
pub(crate) fn open_link(app: &mut DeskApp, url: &str) {
    let lower = url.trim().to_ascii_lowercase();
    if lower.starts_with("mailto:") {
        if let Some(d) = app.session.compose_mailto(url) {
            app.open_composer(d);
        }
        return;
    }
    if lower.starts_with("iureditor:") {
        match deep_link_path(url) {
            Some(p) => app.suite.hub.run(iuredesk_engine::suite::SuiteAction::OpenFile(p)),
            None => app.notice(iuredesk_engine::Notice::error(Text::new(
                "This IureEditor link has no file to open.",
                "Este enlace de IureEditor no indica qué archivo abrir.",
            ))),
        }
        return;
    }
    if !crate::open_url(url) {
        app.notice(iuredesk_engine::Notice::error(Text::new(
            "Could not open the link.",
            "No se pudo abrir el enlace.",
        )));
    }
}

/// Ruta de un deep link `iureditor://open?path=<ruta %-codificada>`.
pub(crate) fn deep_link_path(url: &str) -> Option<PathBuf> {
    let (_, query) = url.split_once('?')?;
    let value = query.split('&').find_map(|kv| kv.strip_prefix("path="))?;
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        match b {
            b'%' => {
                let h = bytes.get(i + 1).and_then(|x| (*x as char).to_digit(16))?;
                let l = bytes.get(i + 2).and_then(|x| (*x as char).to_digit(16))?;
                out.push((h * 16 + l) as u8);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            _ => {
                out.push(b);
                i += 1;
            }
        }
    }
    let s = String::from_utf8(out).ok()?;
    let p = PathBuf::from(s);
    (p.is_absolute() && !s_has_control(&p)).then_some(p)
}

fn s_has_control(p: &std::path::Path) -> bool {
    p.to_string_lossy().chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn deep_links() {
        assert_eq!(deep_link_path("iureditor://open?path=%2Ftmp%2Fa%20b.md"), Some(PathBuf::from("/tmp/a b.md")));
        assert_eq!(deep_link_path("iureditor://open?x=1&path=/tmp/c.md"), Some(PathBuf::from("/tmp/c.md")));
        assert_eq!(deep_link_path("iureditor://open"), None);
        assert_eq!(deep_link_path("iureditor://open?path=relativa.md"), None);
        assert_eq!(deep_link_path("iureditor://open?path=%zz"), None);
    }
}
