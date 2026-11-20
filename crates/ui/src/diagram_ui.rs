//! S1.19–S1.24: lo que se hace con un diagrama de la vista previa de una nota, sin salir de
//! IureDesk.
//!
//! - **Menú** al pulsar un diagrama, con submenús que muestran el valor actual: tema (el de la app
//!   o uno de Mermaid), distribución, forma, tamaño del centro y paleta (mapas mentales), colores,
//!   ver en grande, editar el código, exportar a SVG o PNG. Lo que cambia el dibujo reescribe solo
//!   la cabecera del bloque o el metadato del mapa, con deshacer.
//! - **Colores** de un diagrama Mermaid (relleno, borde, texto, líneas, grupos, notas) con vista
//!   previa en vivo; se guardan como `themeVariables` estándar de Mermaid.
//! - **Visor** con zoom (botones, `Ctrl` + rueda) y desplazamiento arrastrando.
//! - **Editor de código** con vista previa en vivo: cada cambio se dibuja en otro hilo; si hay un
//!   error se explica en lenguaje humano con la línea y se sigue viendo el último dibujo válido.
//! - **Enlaces de nodos** (`click A "https://…"`): al pulsar el nodo se pide confirmación antes de
//!   abrirlo; un `mailto:` abre el redactor.
//! - **Exportar**: el diálogo de guardar y la escritura corren en otro hilo; el resultado llega
//!   como aviso.
//!
//! Dibujar y rasterizar nunca pasa en el hilo de la interfaz (`mermaid-svg` puede tardar hasta su
//! tiempo máximo y Blitz rasteriza).

use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use egui::{Context, RichText, TextureHandle, TextureOptions, Ui, pos2, vec2};
use iuredesk_engine::{Notice, Text};
use iuredesk_html_view::diagram::{self, ColorRole, DiagramError, DiagramOptions, DiagramTheme};
use iuredesk_html_view::markdown::NoteStyle;
use iuredesk_html_view::mindmap::{self, Layout, MapOptions, Palette, RootSize, Shape};
use iuredesk_model::NoteId;
use iurefficient_ui::{Banner, Severity, pick, theme};

use crate::app::DeskApp;

/// Espera tras teclear antes de volver a dibujar.
const DEBOUNCE: Duration = Duration::from_millis(350);
/// Escala del rasterizado del visor y de las vistas previas (nitidez al hacer zoom).
const VIEW_SCALE: f32 = 2.0;
/// Escala del PNG exportado.
const PNG_SCALE: f32 = 2.0;

/// Qué diagrama de la nota.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// Mapa mental de IureEditor (`> [!MINDMAP]`), en el orden de `mindmap::find`.
    Mindmap(usize),
    /// Bloque ```` ```mermaid ````, en el orden de `diagram::mermaid_blocks`.
    Mermaid(usize),
}

/// Estado de los diagramas (vive en la caché de la vista previa).
#[derive(Default)]
pub struct DiagramUi {
    menu: Option<Menu>,
    viewer: Option<Viewer>,
    editor: Option<Editor>,
    colors: Option<Colors>,
    /// Enlace de un nodo que espera confirmación.
    confirm: Option<String>,
    /// Exportaciones en curso: su aviso al terminar.
    exports: Vec<Receiver<Notice>>,
}

impl DiagramUi {
    /// ¿Hay trabajo de fondo? (las pruebas esperan).
    pub fn busy(&self) -> bool {
        !self.exports.is_empty()
            || self.viewer.as_ref().is_some_and(|v| v.rx.is_some())
            || self.editor.as_ref().is_some_and(|e| e.live.busy())
            || self.colors.as_ref().is_some_and(|c| c.live.busy())
    }
}

/// Lo que el menú sabe del diagrama al abrirse.
#[derive(Clone, Debug)]
enum Snapshot {
    Mindmap { opts: MapOptions },
    Mermaid { code: String, opts: DiagramOptions, editable: bool, is_mindmap: bool },
}

struct Menu {
    note: NoteId,
    target: Target,
    pos: egui::Pos2,
    snap: Snapshot,
    /// Recién abierto: el clic que lo abrió no lo cierra.
    fresh: bool,
}

/// Resultado de dibujar y rasterizar en otro hilo.
type Drawn = Result<(Arc<String>, [usize; 2], Vec<u8>, egui::Vec2), DiagramError>;

struct Viewer {
    title: String,
    svg: Option<Arc<String>>,
    tex: Option<TextureHandle>,
    /// Tamaño natural en puntos.
    size: egui::Vec2,
    zoom: f32,
    error: Option<DiagramError>,
    rx: Option<Receiver<Drawn>>,
}

/// Vista previa que se vuelve a dibujar, en otro hilo, cuando algo cambia (editor y colores).
struct LivePreview {
    changed_at: Option<Instant>,
    generation: u64,
    rx: Option<Receiver<(u64, Drawn)>>,
    last_ok: Option<(TextureHandle, egui::Vec2)>,
    error: Option<DiagramError>,
    style: NoteStyle,
    bg: [u8; 3],
}

impl LivePreview {
    fn new(style: NoteStyle, bg: [u8; 3]) -> Self {
        // Dibujar ya (sin esperar).
        Self {
            changed_at: Some(Instant::now() - DEBOUNCE),
            generation: 0,
            rx: None,
            last_ok: None,
            error: None,
            style,
            bg,
        }
    }

    fn busy(&self) -> bool {
        self.rx.is_some() || self.changed_at.is_some()
    }

    /// Algo cambió: dibujar cuando se deje de teclear.
    fn changed(&mut self) {
        self.changed_at = Some(Instant::now());
    }

    /// Recoge el último dibujo y, si toca, lanza otro con `code`.
    fn poll(&mut self, ctx: &Context, code: &str) {
        if let Some(rx) = &self.rx
            && let Ok((generation, drawn)) = rx.try_recv()
        {
            self.rx = None;
            // Los de una versión vieja del código se ignoran.
            if generation == self.generation {
                match drawn {
                    Ok((_, px, rgba, size)) => {
                        let img = egui::ColorImage::from_rgba_unmultiplied(px, &rgba);
                        self.last_ok = Some((ctx.load_texture("diagram-live", img, TextureOptions::LINEAR), size));
                        self.error = None;
                    }
                    Err(err) => self.error = Some(err),
                }
            }
        }
        let Some(t) = self.changed_at else { return };
        if self.rx.is_some() {
            return;
        }
        let wait = DEBOUNCE.saturating_sub(t.elapsed());
        if !wait.is_zero() {
            ctx.request_repaint_after(wait);
            return;
        }
        self.changed_at = None;
        self.generation += 1;
        let (code, style, generation, bg) = (code.to_string(), self.style.clone(), self.generation, self.bg);
        let (tx, rx) = channel();
        let ctx2 = ctx.clone();
        let spawned = std::thread::Builder::new().name("diagram-live".into()).spawn(move || {
            let job: Job = Box::new(move || diagram::render(&code, &style));
            let _ = tx.send((generation, draw_now(job, bg)));
            ctx2.request_repaint();
        });
        if spawned.is_ok() {
            self.rx = Some(rx);
        }
    }

    /// El dibujo (o el error y el último dibujo válido), ajustado a `width`.
    fn show(&self, ui: &mut Ui, width: f32, max_height: f32) {
        let c = theme::colors(ui);
        if let Some(err) = &self.error {
            Banner::new(Severity::Error, error_text(err)).show(ui);
            if self.last_ok.is_some() {
                ui.label(
                    RichText::new(pick(
                        "Showing the last version that worked.",
                        "Se ve la última versión que funcionó.",
                    ))
                    .small()
                    .color(c.text_muted),
                );
            }
        }
        egui::ScrollArea::both().id_salt("diagram-live-preview").max_height(max_height).show(ui, |ui| {
            match &self.last_ok {
                Some((tex, size)) => {
                    let fit = (width / size.x.max(1.0)).min(1.0);
                    ui.image((tex.id(), *size * fit));
                }
                None if self.error.is_none() => {
                    ui.label(RichText::new(pick("Drawing…", "Dibujando…")).color(c.text_muted));
                }
                None => {}
            }
        });
    }
}

struct Editor {
    note: NoteId,
    k: usize,
    code: String,
    saved: String,
    live: LivePreview,
}

/// S1.20: colores de un bloque Mermaid, con vista previa.
struct Colors {
    note: NoteId,
    k: usize,
    /// Código del bloque al abrir (los colores se aplican sobre él).
    base: String,
    opts: DiagramOptions,
    saved: DiagramOptions,
    live: LivePreview,
}

pub(crate) fn layout_label(l: Layout) -> &'static str {
    match l {
        Layout::Right => pick("To the right", "Hacia la derecha"),
        Layout::Radial => pick("Radial (as in IureEditor)", "Radial (como IureEditor)"),
        Layout::Balanced => pick("Both sides", "A ambos lados"),
        Layout::Down => pick("Top to bottom", "De arriba abajo"),
    }
}

pub(crate) fn shape_label(s: Shape) -> &'static str {
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

pub(crate) fn root_label(r: RootSize) -> &'static str {
    match r {
        RootSize::Auto => pick("Automatic", "Automático"),
        RootSize::Small => pick("Small", "Chico"),
        RootSize::Medium => pick("Medium", "Mediano"),
        RootSize::Large => pick("Large", "Grande"),
    }
}

pub(crate) fn palette_label(p: Palette) -> &'static str {
    match p {
        Palette::Iure => pick("Like the app", "Como la app"),
        Palette::Pastel => pick("Pastel", "Pastel"),
        Palette::Warm => pick("Warm", "Cálida"),
        Palette::Cool => pick("Cool", "Fría"),
        Palette::Mono => pick("Grays", "Grises"),
    }
}

pub(crate) fn theme_label(t: DiagramTheme) -> &'static str {
    match t {
        DiagramTheme::App => pick("Like the app", "Como la app"),
        DiagramTheme::Default => pick("Classic Mermaid", "Mermaid clásico"),
        DiagramTheme::Neutral => pick("Neutral", "Neutro"),
        DiagramTheme::Forest => pick("Forest", "Bosque"),
        DiagramTheme::Dark => pick("Dark", "Oscuro"),
        DiagramTheme::Base => pick("Warm", "Cálido"),
    }
}

pub(crate) fn color_label(r: ColorRole) -> &'static str {
    match r {
        ColorRole::NodeFill => pick("Node fill", "Relleno de los nodos"),
        ColorRole::NodeBorder => pick("Node border", "Borde de los nodos"),
        ColorRole::Text => pick("Text", "Texto"),
        ColorRole::Line => pick("Lines and arrows", "Líneas y flechas"),
        ColorRole::GroupFill => pick("Groups", "Grupos"),
        ColorRole::NoteFill => pick("Notes", "Notas"),
    }
}

/// El texto del error en el idioma de la interfaz.
fn error_text(e: &DiagramError) -> &str {
    pick(&e.en, &e.es)
}

/// Cuerpo actual de la nota.
fn note_body(app: &DeskApp, id: NoteId) -> Option<String> {
    app.session.notes().into_iter().find(|x| x.id == id).map(|n| n.body)
}

/// Abre el menú del diagrama pulsado.
pub fn open_menu(app: &mut DeskApp, note: NoteId, target: Target, pos: egui::Pos2) {
    let Some(body) = note_body(app, note) else { return };
    let snap = match target {
        Target::Mindmap(n) => {
            let Some(b) = mindmap::find(&body).into_iter().nth(n) else { return };
            Snapshot::Mindmap { opts: b.options }
        }
        Target::Mermaid(k) => {
            let Some(b) = diagram::mermaid_blocks(&body).into_iter().nth(k) else { return };
            Snapshot::Mermaid {
                opts: diagram::read_options(&b.code),
                is_mindmap: diagram::kind(&b.code) == "mindmap",
                editable: b.code_range.is_some(),
                code: b.code,
            }
        }
    };
    app.note_preview.diagrams.menu = Some(Menu { note, target, pos, snap, fresh: true });
}

/// S1.24: pide confirmación para abrir el enlace de un nodo (un `mailto:` abre el redactor).
pub fn open_node_link(app: &mut DeskApp, href: &str) {
    if href.trim().to_ascii_lowercase().starts_with("mailto:") {
        crate::note_preview::open_link(app, href);
    } else {
        app.note_preview.diagrams.confirm = Some(href.to_string());
    }
}

/// Guarda el cuerpo nuevo de la nota, con deshacer y aviso si falla.
fn save_body(app: &mut DeskApp, id: NoteId, body: &str, text: Text) {
    match app.session.set_note_body(id, body, text) {
        Ok(iuredesk_engine::Outcome::Notice(notice)) => app.notice(notice),
        Ok(_) => {}
        Err(e) => app.notice(Notice::error(Text::new(
            format!("Could not save the note: {e}"),
            format!("No se pudo guardar la nota: {e}"),
        ))),
    }
}

/// S1.19: cambia las opciones de dibujo del mapa mental `n`, con deshacer.
pub(crate) fn set_mindmap(app: &mut DeskApp, id: NoteId, n: usize, opts: &MapOptions) {
    crate::pim::flush_note(app);
    let Some(body) = note_body(app, id) else { return };
    let Some(new_body) = mindmap::set_options(&body, n, opts) else { return };
    save_body(app, id, &new_body, Text::new("Mind map changed", "Mapa mental cambiado"));
}

/// S1.20: cambia las opciones de dibujo del bloque Mermaid `k`, con deshacer.
pub(crate) fn set_mermaid_options(app: &mut DeskApp, id: NoteId, k: usize, opts: &DiagramOptions) {
    crate::pim::flush_note(app);
    let Some(body) = note_body(app, id) else { return };
    let Some(block) = diagram::mermaid_blocks(&body).into_iter().nth(k) else { return };
    let code = diagram::set_options(&block.code, opts);
    let Some(new_body) = diagram::replace_mermaid(&body, k, &code) else { return };
    save_body(app, id, &new_body, Text::new("Diagram changed", "Diagrama cambiado"));
}

/// Qué pidió el usuario en el menú.
enum MenuAct {
    Mindmap(MapOptions),
    Mermaid(DiagramOptions),
    View,
    Edit,
    Colors,
    Export { png: bool },
}

/// Un submenú con el valor actual en su título; devuelve lo elegido.
pub(crate) fn choose<T: Copy + PartialEq>(
    ui: &mut Ui,
    title: &str,
    current: T,
    options: &[T],
    label: impl Fn(T) -> String,
) -> Option<T> {
    let mut picked = None;
    ui.menu_button(format!("{title}: {}", label(current)), |ui| {
        for &o in options {
            if ui.selectable_label(o == current, label(o)).clicked() {
                picked = Some(o);
                ui.close();
            }
        }
    });
    picked
}

/// Las opciones de mapa mental (distribución, forma, centro, paleta) como submenús.
fn map_menus(ui: &mut Ui, layout: Option<Layout>, default_label: &str, o: &MapOptionsView) -> Option<MapOptionsView> {
    let mut out = None;
    let layouts: Vec<Option<Layout>> = std::iter::once(None).chain(Layout::ALL.map(Some)).collect();
    let layout_text = |l: Option<Layout>| l.map_or_else(|| default_label.to_string(), |l| layout_label(l).to_string());
    if let Some(l) = choose(ui, pick("Layout", "Distribución"), layout, &layouts, layout_text) {
        out = Some(MapOptionsView { layout: l, ..*o });
    }
    if let Some(s) =
        choose(ui, pick("Node shape", "Forma de los nodos"), o.shape, &Shape::ALL, |s| shape_label(s).into())
    {
        out = Some(MapOptionsView { shape: s, ..*o });
    }
    if let Some(r) =
        choose(ui, pick("Center size", "Tamaño del centro"), o.root, &RootSize::ALL, |r| root_label(r).into())
    {
        out = Some(MapOptionsView { root: r, ..*o });
    }
    if let Some(p) =
        choose(ui, pick("Branch colors", "Colores de las ramas"), o.palette, &Palette::ALL, |p| palette_label(p).into())
    {
        out = Some(MapOptionsView { palette: p, ..*o });
    }
    out
}

/// Opciones de mapa mental comunes a los dos tipos de diagrama.
#[derive(Clone, Copy, Debug, PartialEq)]
struct MapOptionsView {
    layout: Option<Layout>,
    shape: Shape,
    root: RootSize,
    palette: Palette,
}

/// El menú, si hay uno abierto para la nota `id`. Se cierra con Esc, al pulsar fuera o al elegir.
fn menu(app: &mut DeskApp, ctx: &Context, id: NoteId, style: &NoteStyle, bg: [u8; 3]) {
    let Some(mut m) = app.note_preview.diagrams.menu.take() else { return };
    if m.note != id || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        return;
    }
    let pref = crate::note_preview::layout_of(app.session.prefs.mindmap_layout);
    let screen = ctx.content_rect();
    let w = 290.0;
    let pos = pos2(
        (m.pos.x + 8.0).min(screen.right() - w - 12.0).max(screen.left() + 8.0),
        m.pos.y.clamp(screen.top() + 40.0, (screen.bottom() - 380.0).max(screen.top() + 40.0)),
    );
    let mut act: Option<MenuAct> = None;
    let area = egui::Area::new(egui::Id::unique("s1-20-diagram-menu"))
        .order(egui::Order::Foreground)
        .fixed_pos(pos)
        .show(ctx, |ui| {
            let c = theme::colors(ui);
            egui::Frame::popup(ui.style()).corner_radius(10.0).inner_margin(10.0).show(ui, |ui| {
                ui.set_width(w);
                match &m.snap {
                    Snapshot::Mindmap { opts } => {
                        let view = MapOptionsView {
                            layout: opts.layout,
                            shape: opts.shape,
                            root: opts.root,
                            palette: opts.palette,
                        };
                        let default =
                            format!("{} ({})", pick("As in Settings", "La de Preferencias"), layout_label(pref));
                        if let Some(v) = map_menus(ui, opts.layout, &default, &view) {
                            act = Some(MenuAct::Mindmap(MapOptions {
                                layout: v.layout,
                                shape: v.shape,
                                root: v.root,
                                palette: v.palette,
                            }));
                        }
                    }
                    Snapshot::Mermaid { opts, editable, is_mindmap, .. } => {
                        let editable = *editable;
                        ui.add_enabled_ui(editable, |ui| {
                            if let Some(t) = choose(ui, pick("Theme", "Tema"), opts.theme, &DiagramTheme::ALL, |t| {
                                theme_label(t).into()
                            }) {
                                act = Some(MenuAct::Mermaid(DiagramOptions { theme: t, ..*opts }));
                            }
                            let own = *is_mindmap
                                && (opts.layout.is_some()
                                    || opts.shape != Shape::Default
                                    || opts.root != RootSize::Auto
                                    || opts.palette != Palette::Iure);
                            if *is_mindmap {
                                let view = MapOptionsView {
                                    layout: opts.layout,
                                    shape: opts.shape,
                                    root: opts.root,
                                    palette: opts.palette,
                                };
                                if let Some(v) =
                                    map_menus(ui, opts.layout, pick("Mermaid radial", "Radial de Mermaid"), &view)
                                {
                                    act = Some(MenuAct::Mermaid(DiagramOptions {
                                        layout: v.layout,
                                        shape: v.shape,
                                        root: v.root,
                                        palette: v.palette,
                                        ..*opts
                                    }));
                                }
                            }
                            // Los colores de Mermaid no aplican al dibujo propio de mapas mentales.
                            if !own && ui.button(pick("Colors…", "Colores…")).clicked() {
                                act = Some(MenuAct::Colors);
                            }
                        });
                        if !editable {
                            ui.label(
                                RichText::new(pick(
                                    "It's inside a quote or a list: change it in IureEditor.",
                                    "Está dentro de una cita o una lista: cámbialo en IureEditor.",
                                ))
                                .small()
                                .color(c.text_muted),
                            );
                        }
                    }
                }
                ui.separator();
                if ui.selectable_label(false, pick("View larger", "Ver en grande")).clicked() {
                    act = Some(MenuAct::View);
                }
                if let Snapshot::Mermaid { editable, .. } = &m.snap {
                    let r = ui
                        .add_enabled(*editable, egui::Button::selectable(false, pick("Edit code…", "Editar código…")));
                    if r.clicked() {
                        act = Some(MenuAct::Edit);
                    }
                }
                if ui.selectable_label(false, pick("Export as SVG…", "Exportar como SVG…")).clicked() {
                    act = Some(MenuAct::Export { png: false });
                }
                if ui.selectable_label(false, pick("Export as PNG…", "Exportar como PNG…")).clicked() {
                    act = Some(MenuAct::Export { png: true });
                }
            });
        });
    match act {
        Some(MenuAct::Mindmap(opts)) => {
            if let Target::Mindmap(n) = m.target {
                set_mindmap(app, m.note, n, &opts);
            }
        }
        Some(MenuAct::Mermaid(opts)) => {
            if let Target::Mermaid(k) = m.target {
                set_mermaid_options(app, m.note, k, &opts);
            }
        }
        Some(MenuAct::View) => open_viewer(app, ctx, m.note, m.target, style, bg),
        Some(MenuAct::Edit) => {
            if let (Target::Mermaid(k), Snapshot::Mermaid { code, .. }) = (m.target, &m.snap) {
                app.note_preview.diagrams.editor = Some(Editor {
                    note: m.note,
                    k,
                    code: code.clone(),
                    saved: code.clone(),
                    live: LivePreview::new(style.clone(), bg),
                });
            }
        }
        Some(MenuAct::Colors) => {
            if let (Target::Mermaid(k), Snapshot::Mermaid { code, opts, .. }) = (m.target, &m.snap) {
                app.note_preview.diagrams.colors = Some(Colors {
                    note: m.note,
                    k,
                    base: code.clone(),
                    opts: *opts,
                    saved: *opts,
                    live: LivePreview::new(style.clone(), bg),
                });
            }
        }
        Some(MenuAct::Export { png }) => {
            if let Some((svg, name)) = current_svg(app, m.note, m.target, style) {
                export(app, ctx, svg, name, png);
            }
        }
        None => {
            // Un clic dentro de un submenú abierto no cuenta como «fuera».
            let in_submenu = ctx.input(|i| i.pointer.interact_pos()).is_some_and(|p| {
                ctx.layer_id_at(p).is_some_and(|l| l.order == egui::Order::Foreground && l != area.response.layer_id)
            });
            if !m.fresh && area.response.clicked_elsewhere() && !in_submenu {
                return;
            }
            m.fresh = false;
            app.note_preview.diagrams.menu = Some(m);
        }
    }
}

fn rgb(c: egui::Color32) -> [u8; 3] {
    [c.r(), c.g(), c.b()]
}

/// Lo que hace falta para dibujar el diagrama fuera del hilo de la interfaz: una función que
/// devuelve el SVG.
type Job = Box<dyn FnOnce() -> Result<String, DiagramError> + Send>;

/// El dibujo del diagrama (para el visor y la exportación) y un nombre para el archivo.
fn job_for(app: &DeskApp, note: NoteId, target: Target, style: &NoteStyle) -> Option<(Job, String)> {
    let body = note_body(app, note)?;
    let style = style.clone();
    match target {
        Target::Mindmap(n) => {
            let b = mindmap::find(&body).into_iter().nth(n)?;
            let pref = crate::note_preview::layout_of(app.session.prefs.mindmap_layout);
            let mut root = b.root.clone();
            if root.label.is_empty() {
                root.label = pick("Mind map", "Mapa mental").to_string();
            }
            let name = file_stem(&root.label);
            let look = b.options.look(pref);
            Some((Box::new(move || Ok(mindmap::draw(&root, &style, &look))), name))
        }
        Target::Mermaid(k) => {
            let b = diagram::mermaid_blocks(&body).into_iter().nth(k)?;
            let name = format!("{}-{}", pick("diagram", "diagrama"), k + 1);
            Some((Box::new(move || diagram::render(&b.code, &style)), name))
        }
    }
}

/// Nombre de archivo a partir de una etiqueta (sin separadores ni caracteres raros).
fn file_stem(label: &str) -> String {
    let s: String = label
        .chars()
        .map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' { c } else { '-' })
        .take(60)
        .collect();
    let s = s.trim().trim_matches('-').to_string();
    if s.is_empty() { pick("diagram", "diagrama").to_string() } else { s }
}

/// El SVG del diagrama para exportar (lo que ya muestra el visor, o se dibuja en el hilo de la
/// exportación).
fn current_svg(app: &DeskApp, note: NoteId, target: Target, style: &NoteStyle) -> Option<(SvgSource, String)> {
    let (job, name) = job_for(app, note, target, style)?;
    Some((SvgSource::Job(job), name))
}

enum SvgSource {
    Ready(Arc<String>),
    Job(Job),
}

/// Exporta a SVG o PNG: diálogo de guardar, dibujo y escritura en otro hilo; aviso al terminar.
fn export(app: &mut DeskApp, ctx: &Context, svg: SvgSource, name: String, png: bool) {
    let (tx, rx) = channel();
    let ctx = ctx.clone();
    let spawned = std::thread::Builder::new().name("diagram-export".into()).spawn(move || {
        let ext = if png { "png" } else { "svg" };
        let Some(path) = rfd::FileDialog::new()
            .set_title(pick("Export diagram", "Exportar diagrama"))
            .set_file_name(format!("{name}.{ext}"))
            .add_filter(ext.to_ascii_uppercase(), &[ext])
            .save_file()
        else {
            return;
        };
        let svg = match svg {
            SvgSource::Ready(s) => Ok(s.as_str().to_string()),
            SvgSource::Job(job) => job(),
        };
        let bytes = svg.map_err(|e| (e.en, e.es)).and_then(|svg| {
            if !png {
                return Ok(svg.into_bytes());
            }
            diagram::rasterize(&svg, PNG_SCALE, [255, 255, 255])
                .ok()
                .and_then(|r| diagram::encode_png(r.width, r.height, &r.rgba))
                .ok_or_else(|| ("The image couldn't be made.".to_string(), "No se pudo hacer la imagen.".to_string()))
        });
        let notice = match bytes.map(|b| std::fs::write(&path, b).map_err(|e| (e.to_string(), e.to_string()))) {
            Ok(Ok(())) => Notice::success(
                format!("Diagram saved to {}", path.display()),
                format!("Diagrama guardado en {}", path.display()),
            ),
            Ok(Err((en, es))) | Err((en, es)) => Notice::error(Text::new(
                format!("Could not export the diagram: {en}"),
                format!("No se pudo exportar el diagrama: {es}"),
            )),
        };
        let _ = tx.send(notice);
        ctx.request_repaint();
    });
    if spawned.is_ok() {
        app.note_preview.diagrams.exports.push(rx);
    }
}

/// Dibuja y rasteriza en otro hilo; avisa a la interfaz al terminar.
fn spawn_draw(ctx: &Context, job: Job, bg: [u8; 3]) -> Option<Receiver<Drawn>> {
    let (tx, rx) = channel();
    let ctx = ctx.clone();
    std::thread::Builder::new()
        .name("diagram-draw".into())
        .spawn(move || {
            let _ = tx.send(draw_now(job, bg));
            ctx.request_repaint();
        })
        .ok()?;
    Some(rx)
}

fn draw_now(job: Job, bg: [u8; 3]) -> Drawn {
    let svg = job()?;
    let (w, h) = diagram::svg_size(&svg).unwrap_or((800.0, 600.0));
    let r = diagram::rasterize(&svg, VIEW_SCALE, bg).map_err(|e| DiagramError {
        line: None,
        en: format!("It couldn't be shown: {e}"),
        es: format!("No se pudo mostrar: {e}"),
    })?;
    let size = [r.width as usize, r.height as usize];
    let scale = r.width as f32 / w.max(1.0);
    Ok((Arc::new(svg), size, r.rgba, vec2(w, h.min(r.height as f32 / scale.max(0.01)))))
}

fn open_viewer(app: &mut DeskApp, ctx: &Context, note: NoteId, target: Target, style: &NoteStyle, bg: [u8; 3]) {
    let Some((job, name)) = job_for(app, note, target, style) else { return };
    app.note_preview.diagrams.viewer = Some(Viewer {
        title: name,
        svg: None,
        tex: None,
        size: vec2(1.0, 1.0),
        zoom: 1.0,
        error: None,
        rx: spawn_draw(ctx, job, bg),
    });
}

/// Zoom permitido en el visor.
const ZOOM: std::ops::RangeInclusive<f32> = 0.25..=4.0;

fn viewer(app: &mut DeskApp, ctx: &Context) {
    let Some(mut v) = app.note_preview.diagrams.viewer.take() else { return };
    if let Some(rx) = &v.rx
        && let Ok(drawn) = rx.try_recv()
    {
        v.rx = None;
        match drawn {
            Ok((svg, px, rgba, size)) => {
                let img = egui::ColorImage::from_rgba_unmultiplied(px, &rgba);
                v.tex = Some(ctx.load_texture("diagram-viewer", img, TextureOptions::LINEAR));
                v.size = size;
                v.svg = Some(svg);
            }
            Err(e) => v.error = Some(e),
        }
    }
    let mut open = true;
    let mut export_png: Option<bool> = None;
    egui::Window::new(format!("{} — {}", pick("Diagram", "Diagrama"), v.title))
        .id(egui::Id::unique("s1-20-diagram-viewer"))
        .open(&mut open)
        .default_size([900.0, 620.0])
        .resizable(true)
        .collapsible(false)
        .show(ctx, |ui| {
            let c = theme::colors(ui);
            ui.horizontal(|ui| {
                if ui.button("−").on_hover_text(pick("Zoom out", "Alejar")).clicked() {
                    v.zoom = (v.zoom / 1.25).clamp(*ZOOM.start(), *ZOOM.end());
                }
                ui.label(RichText::new(format!("{:.0} %", v.zoom * 100.0)).color(c.text));
                if ui.button("+").on_hover_text(pick("Zoom in", "Acercar")).clicked() {
                    v.zoom = (v.zoom * 1.25).clamp(*ZOOM.start(), *ZOOM.end());
                }
                if ui.button(pick("Fit", "Ajustar")).clicked() {
                    let avail = ui.available_size_before_wrap();
                    let fit = (ui.ctx().content_rect().width() * 0.6 / v.size.x.max(1.0))
                        .min(avail.y.max(300.0) / v.size.y.max(1.0));
                    v.zoom = fit.clamp(*ZOOM.start(), *ZOOM.end());
                }
                if ui.button(pick("Actual size", "Tamaño real")).clicked() {
                    v.zoom = 1.0;
                }
                ui.separator();
                if ui.add_enabled(v.svg.is_some(), egui::Button::new(pick("Export SVG…", "Exportar SVG…"))).clicked()
                {
                    export_png = Some(false);
                }
                if ui.add_enabled(v.svg.is_some(), egui::Button::new(pick("Export PNG…", "Exportar PNG…"))).clicked()
                {
                    export_png = Some(true);
                }
            });
            ui.separator();
            if let Some(e) = &v.error {
                Banner::new(Severity::Error, error_text(e)).show(ui);
                return;
            }
            let Some(tex) = &v.tex else {
                ui.label(RichText::new(pick("Drawing…", "Dibujando…")).color(c.text_muted));
                return;
            };
            // Ctrl + rueda: zoom.
            if ui.ui_contains_pointer() {
                let z = ui.input(|i| i.zoom_delta());
                if (z - 1.0).abs() > f32::EPSILON {
                    v.zoom = (v.zoom * z).clamp(*ZOOM.start(), *ZOOM.end());
                }
            }
            egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
                let size = v.size * v.zoom;
                let r = ui.add(egui::Image::new((tex.id(), size)).sense(egui::Sense::drag()));
                iurefficient_ui::a11y::label(&r, &v.title);
            });
        });
    if let (Some(png), Some(svg)) = (export_png, v.svg.clone()) {
        export(app, ctx, SvgSource::Ready(svg), v.title.clone(), png);
    }
    if open && !ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        app.note_preview.diagrams.viewer = Some(v);
    }
}

fn editor(app: &mut DeskApp, ctx: &Context) {
    let Some(mut e) = app.note_preview.diagrams.editor.take() else { return };
    e.live.poll(ctx, &e.code);
    let mut open = true;
    let mut save = false;
    let mut cancel = false;
    egui::Window::new(pick("Edit diagram", "Editar diagrama"))
        .id(egui::Id::unique("s1-20-diagram-editor"))
        .open(&mut open)
        .default_size([1000.0, 620.0])
        .resizable(true)
        .collapsible(false)
        .show(ctx, |ui| {
            let c = theme::colors(ui);
            // Barra arriba: guardar y cancelar siempre a la vista.
            ui.horizontal(|ui| {
                let changed = e.code.trim_end() != e.saved.trim_end();
                if ui.add_enabled(changed, egui::Button::new(pick("Save", "Guardar"))).clicked() {
                    save = true;
                }
                if ui.button(pick("Cancel", "Cancelar")).clicked() {
                    cancel = true;
                }
                if changed && e.live.error.is_some() {
                    ui.label(
                        RichText::new(pick(
                            "You can save it with the error and fix it later.",
                            "Puedes guardarlo con el error y corregirlo después.",
                        ))
                        .small()
                        .color(c.text_muted),
                    );
                }
            });
            ui.separator();
            let h = (ui.available_height() - 8.0).max(240.0);
            ui.horizontal_top(|ui| {
                let half = (ui.available_width() / 2.0 - 8.0).max(200.0);
                ui.vertical(|ui| {
                    ui.set_width(half);
                    egui::ScrollArea::vertical().id_salt("diagram-code").max_height(h).show(ui, |ui| {
                        let r = ui.add_sized(
                            [half, h],
                            egui::TextEdit::multiline(&mut e.code).code_editor().lock_focus(true),
                        );
                        crate::widgets::named(
                            &r,
                            egui::Role::MultilineTextInput,
                            pick("Diagram code", "Código del diagrama"),
                        );
                        if r.changed() {
                            e.live.changed();
                        }
                    });
                });
                ui.separator();
                ui.vertical(|ui| {
                    ui.set_width(half);
                    e.live.show(ui, half, h - 40.0);
                });
            });
        });
    if save {
        crate::pim::flush_note(app);
        if let Some(body) = note_body(app, e.note)
            && let Some(new_body) = diagram::replace_mermaid(&body, e.k, &e.code)
        {
            save_body(app, e.note, &new_body, Text::new("Diagram changed", "Diagrama cambiado"));
        }
        return;
    }
    if open && !cancel {
        app.note_preview.diagrams.editor = Some(e);
    }
}

/// S1.20: colores de un diagrama Mermaid, con vista previa en vivo.
fn colors(app: &mut DeskApp, ctx: &Context) {
    let Some(mut w) = app.note_preview.diagrams.colors.take() else { return };
    let code = diagram::set_options(&w.base, &w.opts);
    w.live.poll(ctx, &code);
    let mut open = true;
    let mut save = false;
    let mut cancel = false;
    egui::Window::new(pick("Diagram colors", "Colores del diagrama"))
        .id(egui::Id::unique("s1-20-diagram-colors"))
        .open(&mut open)
        .default_size([860.0, 520.0])
        .resizable(true)
        .collapsible(false)
        .show(ctx, |ui| {
            let c = theme::colors(ui);
            ui.horizontal(|ui| {
                if ui.add_enabled(w.opts != w.saved, egui::Button::new(pick("Save", "Guardar"))).clicked() {
                    save = true;
                }
                if ui.button(pick("Cancel", "Cancelar")).clicked() {
                    cancel = true;
                }
                if ui
                    .add_enabled(!w.opts.colors.is_empty(), egui::Button::new(pick("Theme colors", "Colores del tema")))
                    .on_hover_text(pick("Remove all the chosen colors", "Quitar todos los colores elegidos"))
                    .clicked()
                {
                    w.opts.colors = diagram::DiagramColors::default();
                    w.live.changed();
                }
            });
            ui.separator();
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(300.0);
                    ui.label(
                        RichText::new(pick(
                            "Pick a color to replace the theme's; uncheck it to go back.",
                            "Elige un color para cambiar el del tema; desmárcalo para volver a él.",
                        ))
                        .small()
                        .color(c.text_muted),
                    );
                    ui.add_space(6.0);
                    egui::Grid::new("diagram-colors-grid").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                        for role in ColorRole::ALL {
                            let current = w.opts.colors.get(role);
                            let mut on = current.is_some();
                            if ui.checkbox(&mut on, color_label(role)).changed() {
                                w.opts.colors.set(role, on.then_some(current.unwrap_or([0x1d, 0x4e, 0xd8])));
                                w.live.changed();
                            }
                            let mut rgb = current.unwrap_or([0x9c, 0xa3, 0xaf]);
                            let r = ui
                                .add_enabled_ui(on, |ui| egui::color_picker::color_edit_button_srgb(ui, &mut rgb))
                                .inner;
                            if r.changed() {
                                w.opts.colors.set(role, Some(rgb));
                                w.live.changed();
                            }
                            ui.end_row();
                        }
                    });
                });
                ui.separator();
                ui.vertical(|ui| {
                    let width = ui.available_width().max(200.0);
                    w.live.show(ui, width, 420.0);
                });
            });
        });
    if save {
        set_mermaid_options(app, w.note, w.k, &w.opts);
        return;
    }
    if open && !cancel && !ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        app.note_preview.diagrams.colors = Some(w);
    }
}

/// S1.24: «¿Abrir este enlace?» de un nodo del diagrama.
fn confirm(app: &mut DeskApp, ctx: &Context) {
    let Some(url) = app.note_preview.diagrams.confirm.take() else { return };
    let mut decision: Option<bool> = None;
    egui::Window::new(pick("Open link?", "¿Abrir el enlace?"))
        .id(egui::Id::unique("s1-24-diagram-link"))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0))
        .show(ctx, |ui| {
            let c = theme::colors(ui);
            ui.label(
                RichText::new(pick(
                    "This diagram node points to a web page:",
                    "Este nodo del diagrama lleva a una página web:",
                ))
                .color(c.text),
            );
            ui.add_space(4.0);
            ui.label(RichText::new(&url).monospace().color(c.text));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button(pick("Open", "Abrir")).clicked() {
                    decision = Some(true);
                }
                if ui.button(pick("Cancel", "Cancelar")).clicked() {
                    decision = Some(false);
                }
            });
        });
    match decision {
        Some(true) => crate::note_preview::open_link(app, &url),
        Some(false) => {}
        None if ctx.input(|i| i.key_pressed(egui::Key::Escape)) => {}
        None => app.note_preview.diagrams.confirm = Some(url),
    }
}

/// Avisos de las exportaciones que terminaron.
fn exports(app: &mut DeskApp) {
    let mut done = Vec::new();
    app.note_preview.diagrams.exports.retain(|rx| match rx.try_recv() {
        Ok(n) => {
            done.push(n);
            false
        }
        Err(std::sync::mpsc::TryRecvError::Empty) => true,
        Err(std::sync::mpsc::TryRecvError::Disconnected) => false,
    });
    for n in done {
        app.notice(n);
    }
}

/// Menú, ventanas y exportaciones de los diagramas de la nota `id` (cada cuadro).
pub fn show(app: &mut DeskApp, ui: &Ui, id: NoteId) {
    let ctx = ui.ctx().clone();
    let style = crate::note_preview::style(ui);
    let bg = rgb(theme::colors(ui).surface);
    menu(app, &ctx, id, &style, bg);
    viewer(app, &ctx);
    editor(app, &ctx);
    colors(app, &ctx);
    confirm(app, &ctx);
    exports(app);
}

/// Pruebas: el código del editor abierto.
pub fn set_editor_code(app: &mut DeskApp, code: &str) {
    if let Some(e) = app.note_preview.diagrams.editor.as_mut() {
        e.code = code.to_string();
        e.live.changed_at = Some(Instant::now() - DEBOUNCE);
    }
}

/// Pruebas: el error que muestra el editor, si hay.
pub fn editor_error(app: &DeskApp) -> Option<String> {
    app.note_preview.diagrams.editor.as_ref().and_then(|e| e.live.error.as_ref()).map(|e| error_text(e).to_string())
}

/// Pruebas: ¿el editor tiene un dibujo válido?
pub fn editor_has_drawing(app: &DeskApp) -> bool {
    app.note_preview.diagrams.editor.as_ref().is_some_and(|e| e.live.last_ok.is_some())
}

/// Pruebas: elige un color en la ventana de colores abierta (sin el selector de color).
pub fn set_color(app: &mut DeskApp, role: ColorRole, c: Option<[u8; 3]>) {
    if let Some(w) = app.note_preview.diagrams.colors.as_mut() {
        w.opts.colors.set(role, c);
        w.live.changed_at = Some(Instant::now() - DEBOUNCE);
    }
}

/// Pruebas: el enlace que espera confirmación.
pub fn pending_link(app: &DeskApp) -> Option<String> {
    app.note_preview.diagrams.confirm.clone()
}
