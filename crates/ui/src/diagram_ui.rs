//! S1.19–S1.20: lo que se hace con un diagrama de la vista previa de una nota, sin salir de
//! IureDesk.
//!
//! - **Menú** al pulsar un diagrama: tema (el de la app o uno de Mermaid), distribución y forma
//!   (mapas mentales), ver en grande, editar el código, exportar a SVG o PNG. Lo que cambia el
//!   dibujo reescribe solo la cabecera del bloque o el metadato del mapa, con deshacer.
//! - **Visor** con zoom (botones, `Ctrl` + rueda) y desplazamiento arrastrando.
//! - **Editor de código** con vista previa en vivo: cada cambio se dibuja en otro hilo; si hay un
//!   error se explica en lenguaje humano con la línea y se sigue viendo el último dibujo válido.
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
use iuredesk_html_view::diagram::{self, DiagramError, DiagramOptions, DiagramTheme};
use iuredesk_html_view::markdown::NoteStyle;
use iuredesk_html_view::mindmap::{self, Layout, Shape};
use iuredesk_model::NoteId;
use iurefficient_ui::{Banner, Severity, pick, theme};

use crate::app::DeskApp;

/// Espera tras teclear antes de volver a dibujar.
const DEBOUNCE: Duration = Duration::from_millis(350);
/// Escala del rasterizado del visor y de la vista previa del editor (nitidez al hacer zoom).
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
    /// Exportaciones en curso: su aviso al terminar.
    exports: Vec<Receiver<Notice>>,
}

impl DiagramUi {
    /// ¿Hay trabajo de fondo? (las pruebas esperan).
    pub fn busy(&self) -> bool {
        !self.exports.is_empty()
            || self.viewer.as_ref().is_some_and(|v| v.rx.is_some())
            || self.editor.as_ref().is_some_and(|e| e.rx.is_some() || e.changed_at.is_some())
    }
}

/// Lo que el menú sabe del diagrama al abrirse.
#[derive(Clone, Debug)]
enum Snapshot {
    Mindmap { layout: Option<Layout>, shape: Shape },
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

struct Editor {
    note: NoteId,
    k: usize,
    code: String,
    saved: String,
    changed_at: Option<Instant>,
    generation: u64,
    rx: Option<Receiver<(u64, Drawn)>>,
    last_ok: Option<(TextureHandle, egui::Vec2)>,
    error: Option<DiagramError>,
    style: NoteStyle,
    bg: [u8; 3],
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

fn theme_label(t: DiagramTheme) -> &'static str {
    match t {
        DiagramTheme::App => pick("Like the app", "Como la app"),
        DiagramTheme::Default => pick("Classic Mermaid", "Mermaid clásico"),
        DiagramTheme::Neutral => pick("Neutral", "Neutro"),
        DiagramTheme::Forest => pick("Forest", "Bosque"),
        DiagramTheme::Dark => pick("Dark", "Oscuro"),
        DiagramTheme::Base => pick("Warm", "Cálido"),
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
            Snapshot::Mindmap { layout: b.layout, shape: b.shape }
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

/// S1.19: cambia la distribución y la forma del mapa mental `n`, con deshacer.
pub(crate) fn set_mindmap(app: &mut DeskApp, id: NoteId, n: usize, layout: Option<Layout>, shape: Shape) {
    crate::pim::flush_note(app);
    let Some(body) = note_body(app, id) else { return };
    let Some(new_body) = mindmap::set_options(&body, n, layout, shape) else { return };
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
    Mindmap(Option<Layout>, Shape),
    Mermaid(DiagramOptions),
    View,
    Edit,
    Export { png: bool },
}

/// El menú, si hay uno abierto para la nota `id`. Se cierra con Esc, al pulsar fuera o al elegir.
fn menu(app: &mut DeskApp, ctx: &Context, id: NoteId, style: &NoteStyle, bg: [u8; 3]) {
    let Some(mut m) = app.note_preview.diagrams.menu.take() else { return };
    if m.note != id || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        return;
    }
    let pref = crate::note_preview::layout_of(app.session.prefs.mindmap_layout);
    let screen = ctx.content_rect();
    let w = 250.0;
    let pos = pos2(
        (m.pos.x + 8.0).min(screen.right() - w - 12.0).max(screen.left() + 8.0),
        m.pos.y.clamp(screen.top() + 40.0, (screen.bottom() - 560.0).max(screen.top() + 40.0)),
    );
    let mut act: Option<MenuAct> = None;
    let area = egui::Area::new(egui::Id::unique("s1-20-diagram-menu"))
        .order(egui::Order::Foreground)
        .fixed_pos(pos)
        .show(ctx, |ui| {
            let c = theme::colors(ui);
            let title = |ui: &mut Ui, t: &str| {
                ui.label(RichText::new(t).font(theme::bold_font(ui.ctx(), 13.0)).color(c.text));
            };
            egui::Frame::popup(ui.style()).corner_radius(10.0).inner_margin(10.0).show(ui, |ui| {
                ui.set_width(w);
                match &m.snap {
                    Snapshot::Mindmap { layout, shape } => {
                        title(ui, pick("Layout", "Distribución"));
                        let default =
                            format!("{} — {}", pick("As in Settings", "La de Preferencias"), layout_label(pref));
                        if ui.selectable_label(layout.is_none(), default).clicked() {
                            act = Some(MenuAct::Mindmap(None, *shape));
                        }
                        for l in Layout::ALL {
                            if ui.selectable_label(*layout == Some(l), layout_label(l)).clicked() {
                                act = Some(MenuAct::Mindmap(Some(l), *shape));
                            }
                        }
                        ui.separator();
                        title(ui, pick("Node shape", "Forma de los nodos"));
                        for s in Shape::ALL {
                            if ui.selectable_label(*shape == s, shape_label(s)).clicked() {
                                act = Some(MenuAct::Mindmap(*layout, s));
                            }
                        }
                    }
                    Snapshot::Mermaid { opts, editable, is_mindmap, .. } => {
                        let editable = *editable;
                        ui.add_enabled_ui(editable, |ui| {
                            title(ui, pick("Theme", "Tema"));
                            for t in DiagramTheme::ALL {
                                if ui.selectable_label(opts.theme == t, theme_label(t)).clicked() {
                                    act = Some(MenuAct::Mermaid(DiagramOptions { theme: t, ..*opts }));
                                }
                            }
                            if *is_mindmap {
                                ui.separator();
                                title(ui, pick("Layout", "Distribución"));
                                for l in Layout::ALL {
                                    let on = opts.layout.unwrap_or(Layout::Radial) == l;
                                    if ui.selectable_label(on, layout_label(l)).clicked() {
                                        let layout = (l != Layout::Radial).then_some(l);
                                        act = Some(MenuAct::Mermaid(DiagramOptions { layout, ..*opts }));
                                    }
                                }
                                if opts.layout.is_some_and(|l| l != Layout::Radial) {
                                    title(ui, pick("Node shape", "Forma de los nodos"));
                                    for s in Shape::ALL {
                                        if ui.selectable_label(opts.shape == s, shape_label(s)).clicked() {
                                            act = Some(MenuAct::Mermaid(DiagramOptions { shape: s, ..*opts }));
                                        }
                                    }
                                }
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
        Some(MenuAct::Mindmap(layout, shape)) => {
            if let Target::Mindmap(n) = m.target {
                set_mindmap(app, m.note, n, layout, shape);
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
                    changed_at: Some(Instant::now() - DEBOUNCE),
                    generation: 0,
                    rx: None,
                    last_ok: None,
                    error: None,
                    style: style.clone(),
                    bg,
                });
            }
        }
        Some(MenuAct::Export { png }) => {
            if let Some((svg, name)) = current_svg(app, m.note, m.target, style) {
                export(app, ctx, svg, name, png);
            }
        }
        None => {
            if !m.fresh && area.response.clicked_elsewhere() {
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
/// devuelve el SVG y un nombre para el archivo.
type Job = Box<dyn FnOnce() -> Result<String, DiagramError> + Send>;

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
            let layout = b.layout.unwrap_or(pref);
            Some((Box::new(move || Ok(mindmap::draw(&root, &style, layout, b.shape))), name))
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
    // Resultado del último dibujo (se ignoran los de una versión vieja del código).
    if let Some(rx) = &e.rx
        && let Ok((generation, drawn)) = rx.try_recv()
    {
        e.rx = None;
        if generation == e.generation {
            match drawn {
                Ok((_, px, rgba, size)) => {
                    let img = egui::ColorImage::from_rgba_unmultiplied(px, &rgba);
                    e.last_ok = Some((ctx.load_texture("diagram-editor", img, TextureOptions::LINEAR), size));
                    e.error = None;
                }
                Err(err) => e.error = Some(err),
            }
        }
    }
    // Dibujar de nuevo cuando se dejó de teclear.
    if let Some(t) = e.changed_at
        && e.rx.is_none()
    {
        let wait = DEBOUNCE.saturating_sub(t.elapsed());
        if wait.is_zero() {
            e.changed_at = None;
            e.generation += 1;
            let (code, style, generation) = (e.code.clone(), e.style.clone(), e.generation);
            let (tx, rx) = channel();
            let ctx2 = ctx.clone();
            let bg = e.bg;
            let spawned = std::thread::Builder::new().name("diagram-edit".into()).spawn(move || {
                let job: Job = Box::new(move || diagram::render(&code, &style));
                let _ = tx.send((generation, draw_now(job, bg)));
                ctx2.request_repaint();
            });
            if spawned.is_ok() {
                e.rx = Some(rx);
            }
        } else {
            ctx.request_repaint_after(wait);
        }
    }
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
                if changed && e.error.is_some() {
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
                            e.changed_at = Some(Instant::now());
                        }
                    });
                });
                ui.separator();
                ui.vertical(|ui| {
                    ui.set_width(half);
                    if let Some(err) = &e.error {
                        Banner::new(Severity::Error, error_text(err)).show(ui);
                        if e.last_ok.is_some() {
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
                    egui::ScrollArea::both().id_salt("diagram-preview").max_height(h - 40.0).show(ui, |ui| {
                        match &e.last_ok {
                            Some((tex, size)) => {
                                let fit = (half / size.x.max(1.0)).min(1.0);
                                ui.image((tex.id(), *size * fit));
                            }
                            None if e.error.is_none() => {
                                ui.label(RichText::new(pick("Drawing…", "Dibujando…")).color(c.text_muted));
                            }
                            None => {}
                        }
                    });
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

/// Menú, visor, editor y exportaciones de los diagramas de la nota `id` (cada cuadro).
pub fn show(app: &mut DeskApp, ui: &Ui, id: NoteId) {
    let ctx = ui.ctx().clone();
    let style = crate::note_preview::style(ui);
    let bg = rgb(theme::colors(ui).surface);
    menu(app, &ctx, id, &style, bg);
    viewer(app, &ctx);
    editor(app, &ctx);
    exports(app);
}

/// Pruebas: el código del editor abierto.
pub fn set_editor_code(app: &mut DeskApp, code: &str) {
    if let Some(e) = app.note_preview.diagrams.editor.as_mut() {
        e.code = code.to_string();
        e.changed_at = Some(Instant::now() - DEBOUNCE);
    }
}

/// Pruebas: el error que muestra el editor, si hay.
pub fn editor_error(app: &DeskApp) -> Option<String> {
    app.note_preview.diagrams.editor.as_ref().and_then(|e| e.error.as_ref()).map(|e| error_text(e).to_string())
}

/// Pruebas: ¿el editor tiene un dibujo válido?
pub fn editor_has_drawing(app: &DeskApp) -> bool {
    app.note_preview.diagrams.editor.as_ref().is_some_and(|e| e.last_ok.is_some())
}
