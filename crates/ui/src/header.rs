//! Barra de encabezado integrada (UI-HEADER): la barra de título de la ventana y la de la app en
//! una sola fila, como Claude Desktop o IureEditor. Izquierda: marca, título del módulo o carpeta
//! y punto de sincronización; centro: búsqueda y «Comandos»; derecha: menú rápido de tema y
//! densidad, campana y avatar de Iurefficient, y los botones de la ventana.
//!
//! Sin decoraciones del sistema la app se encarga de mover (arrastrar el fondo de la barra),
//! maximizar (doble clic), el menú de la ventana (clic derecho), cambiar el tamaño (bordes
//! invisibles de 6 px) y de dibujar un borde de 1 px. Con la barra del sistema
//! ([`DeskApp`] sin [`WindowChrome`]) la fila es la misma, sin botones ni arrastre.

use egui::{
    Align, Color32, Context, CursorIcon, Frame, Id, Layout, Margin, PointerButton, Rect, ResizeDirection, RichText,
    Sense, Stroke, Ui, UiBuilder, ViewportCommand, pos2, vec2,
};
use iuredesk_engine::{DensityPref, Module, SyncStatus, ThemePref};
use iurefficient_ui::widgets::badge::{BadgeKind, count_text, paint_badge};
use iurefficient_ui::{Icon, SearchField, a11y, icons, pick, theme};

use crate::app::DeskApp;

/// Un botón de la ventana.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowButton {
    Minimize,
    Maximize,
    Close,
}

/// Qué botones van a cada lado (en el orden en que se dibujan, de izquierda a derecha).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ButtonLayout {
    pub left: Vec<WindowButton>,
    pub right: Vec<WindowButton>,
}

impl Default for ButtonLayout {
    fn default() -> Self {
        ButtonLayout::standard()
    }
}

impl ButtonLayout {
    /// Minimizar, maximizar y cerrar a la derecha (Windows, Ubuntu, KDE).
    pub fn standard() -> Self {
        ButtonLayout {
            left: Vec::new(),
            right: vec![WindowButton::Minimize, WindowButton::Maximize, WindowButton::Close],
        }
    }

    /// Lee el formato de GNOME (`org.gnome.desktop.wm.preferences button-layout`), p. ej.
    /// `appmenu:minimize,maximize,close` o `close,minimize,maximize:`. Lo de antes de `:` va a la
    /// izquierda. Se ignoran los nombres que no son botones (`appmenu`, `icon`, `spacer`…) y los
    /// repetidos. `None` si el texto está vacío, es demasiado largo o no dice nada útil.
    pub fn parse(setting: &str) -> Option<Self> {
        let s = setting.trim().trim_matches(|c| c == '\'' || c == '"').trim();
        if s.is_empty() || s.len() > 256 {
            return None;
        }
        let (l, r) = s.split_once(':').unwrap_or((s, ""));
        let mut seen: Vec<WindowButton> = Vec::new();
        let mut side = |part: &str| {
            let mut v = Vec::new();
            for name in part.split(',').take(16) {
                let b = match name.trim() {
                    "minimize" => WindowButton::Minimize,
                    "maximize" => WindowButton::Maximize,
                    "close" => WindowButton::Close,
                    _ => continue,
                };
                if !seen.contains(&b) {
                    seen.push(b);
                    v.push(b);
                }
            }
            v
        };
        let left = side(l);
        let right = side(r);
        // Un texto sin ningún nombre conocido ni «:» no es un ajuste válido.
        if left.is_empty() && right.is_empty() && !s.contains(':') {
            return None;
        }
        Some(ButtonLayout { left, right })
    }
}

/// Aspecto de los botones de la ventana.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlStyle {
    /// Redondos, como GNOME/Adwaita (Linux).
    Adwaita,
    /// Rectangulares a todo lo alto, cerrar en rojo al pasar (Windows).
    Windows,
    /// macOS: los semáforos del sistema siguen ahí; solo se deja su hueco a la izquierda.
    MacOs,
}

/// La ventana sin la barra del sistema: estilo y orden de los botones.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowChrome {
    pub style: ControlStyle,
    pub layout: ButtonLayout,
}

impl WindowChrome {
    /// La de este sistema; en Linux con el orden de botones del escritorio si se pudo leer.
    pub fn for_this_os(button_layout: Option<&str>) -> Self {
        if cfg!(target_os = "macos") {
            WindowChrome { style: ControlStyle::MacOs, layout: ButtonLayout { left: Vec::new(), right: Vec::new() } }
        } else if cfg!(target_os = "windows") {
            WindowChrome { style: ControlStyle::Windows, layout: ButtonLayout::standard() }
        } else {
            WindowChrome {
                style: ControlStyle::Adwaita,
                layout: button_layout.and_then(ButtonLayout::parse).unwrap_or_default(),
            }
        }
    }

    /// Por nombre (`adwaita`, `windows`, `macos`), para capturas.
    pub fn named(name: &str) -> Option<Self> {
        let none = || ButtonLayout { left: Vec::new(), right: Vec::new() };
        match name {
            "adwaita" | "linux" | "gnome" => {
                Some(WindowChrome { style: ControlStyle::Adwaita, layout: ButtonLayout::standard() })
            }
            "windows" => Some(WindowChrome { style: ControlStyle::Windows, layout: ButtonLayout::standard() }),
            "macos" | "mac" => Some(WindowChrome { style: ControlStyle::MacOs, layout: none() }),
            _ => None,
        }
    }

    /// ¿La app dibuja borde y bordes de cambio de tamaño? (En macOS los pone el sistema.)
    fn owns_frame(&self) -> bool {
        self.style != ControlStyle::MacOs
    }
}

/// Alto de la barra según la densidad.
pub fn height(ctx: &Context) -> f32 {
    match theme::density(ctx) {
        iurefficient_ui::Density::Comfortable => 40.0,
        iurefficient_ui::Density::Compact => 36.0,
        iurefficient_ui::Density::Dense => 32.0,
    }
}

fn maximized(ctx: &Context) -> bool {
    ctx.input(|i| i.viewport().maximized.unwrap_or(false) || i.viewport().fullscreen.unwrap_or(false))
}

fn toggle_maximize(ctx: &Context) {
    let max = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
    ctx.send_viewport_cmd(ViewportCommand::Maximized(!max));
}

fn press(ctx: &Context, b: WindowButton) {
    match b {
        WindowButton::Minimize => ctx.send_viewport_cmd(ViewportCommand::Minimized(true)),
        WindowButton::Maximize => toggle_maximize(ctx),
        WindowButton::Close => ctx.send_viewport_cmd(ViewportCommand::Close),
    }
}

/// Lo que se ve en el centro: ancho guardado del grupo de la derecha (del cuadro anterior).
fn right_width_id() -> Id {
    Id::new("iuredesk-header-right-width")
}

/// La barra. `full` = con búsqueda y «Comandos» (no en la bienvenida).
pub(crate) fn show(app: &mut DeskApp, ui: &mut Ui, full: bool) {
    let ctx = ui.ctx().clone();
    let c = theme::colors(ui);
    let h = height(&ctx);
    let chrome = app.chrome().cloned();
    let windows = chrome.as_ref().is_some_and(|ch| ch.style == ControlStyle::Windows);
    let right_pad: i8 = if windows { 0 } else { 8 };
    let frame = Frame::new().fill(c.surface_alt).inner_margin(Margin { left: 10, right: right_pad, top: 0, bottom: 0 });
    egui::Panel::top("app-header").exact_size(h).resizable(false).frame(frame).show(ui, |ui| {
        let inner = ui.max_rect();
        let whole = Rect::from_min_max(pos2(inner.left() - 10.0, inner.top()), pos2(inner.right() + 8.0, inner.bottom()));
        // El fondo arrastra la ventana; los controles se agregan después y tienen prioridad.
        if chrome.is_some() {
            drag_area(ui, whole);
        }
        let max = maximized(&ctx);

        // --- Izquierda ---
        let mut left = ui.new_child(UiBuilder::new().max_rect(inner).layout(Layout::left_to_right(Align::Center)));
        left.spacing_mut().item_spacing.x = 8.0;
        if let Some(ch) = &chrome {
            if ch.style == ControlStyle::MacOs && !ctx.input(|i| i.viewport().fullscreen.unwrap_or(false)) {
                left.add_space(68.0);
            }
            if !ch.layout.left.is_empty() {
                for b in &ch.layout.left {
                    if window_button(&mut left, *b, ch.style, max, h).clicked() {
                        press(&ctx, *b);
                    }
                }
                left.add_space(8.0);
            }
        }
        app_mark(&mut left, h);
        let title = app.header_title();
        left.add(egui::Label::new(RichText::new(&title).font(theme::semibold_font(&ctx, 13.5)).color(c.text)).selectable(false).truncate());
        sync_dot(app, &mut left);
        let left_end = left.min_rect().right();

        // --- Centro ---
        let right_w: f32 = ctx.data(|d| d.get_temp(right_width_id())).unwrap_or(220.0);
        let lo = left_end + 16.0;
        let hi = inner.right() - right_w - 16.0;
        if full && hi - lo > 160.0 {
            let cmd_w = 118.0;
            let search_w = ((hi - lo) - cmd_w - 8.0).clamp(160.0, 520.0);
            let group = search_w + 8.0 + cmd_w;
            let x0 = (inner.center().x - group / 2.0).clamp(lo, (hi - group).max(lo));
            let rect = Rect::from_min_max(pos2(x0, inner.top()), pos2((x0 + group).min(hi), inner.bottom()));
            let mut mid = ui.new_child(UiBuilder::new().max_rect(rect).layout(Layout::left_to_right(Align::Center)));
            mid.spacing_mut().item_spacing.x = 8.0;
            search(app, &mut mid, search_w);
            commands_button(app, &mut mid);
        }

        // --- Derecha ---
        let mut right = ui.new_child(UiBuilder::new().max_rect(inner).layout(Layout::right_to_left(Align::Center)));
        right.spacing_mut().item_spacing.x = if windows { 0.0 } else { 8.0 };
        if let Some(ch) = &chrome
            && !ch.layout.right.is_empty()
        {
            for b in ch.layout.right.iter().rev() {
                if window_button(&mut right, *b, ch.style, max, h).clicked() {
                    press(&ctx, *b);
                }
            }
            right.add_space(if windows { 8.0 } else { 6.0 });
        }
        right.spacing_mut().item_spacing.x = 6.0;
        iure_account(app, &mut right);
        quick_menu(app, &mut right);
        let used = (inner.right() - right.min_rect().left()).max(0.0);
        ctx.data_mut(|d| d.insert_temp(right_width_id(), used));
    });
}

/// Fondo de la barra: arrastrar mueve, doble clic maximiza, clic derecho abre el menú de ventana.
fn drag_area(ui: &mut Ui, rect: Rect) {
    let ctx = ui.ctx().clone();
    let resp = ui.interact(rect, Id::new("iuredesk-header-drag"), Sense::click_and_drag());
    a11y::area(&resp, pick("Title bar", "Barra de título"));
    if resp.drag_started_by(PointerButton::Primary) {
        ctx.send_viewport_cmd(ViewportCommand::StartDrag);
    }
    if resp.double_clicked() {
        toggle_maximize(&ctx);
    }
    let max = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
    resp.context_menu(|ui| {
        if icons::menu_item(ui, Some(Icon::Minimize), pick("Minimize", "Minimizar")).clicked() {
            press(&ctx, WindowButton::Minimize);
            ui.close();
        }
        let (icon, label) = if max {
            (Icon::Restore, pick("Restore", "Restaurar"))
        } else {
            (Icon::Maximize, pick("Maximize", "Maximizar"))
        };
        if icons::menu_item(ui, Some(icon), label).clicked() {
            toggle_maximize(&ctx);
            ui.close();
        }
        ui.separator();
        if icons::menu_item(ui, Some(Icon::Close), pick("Close", "Cerrar")).clicked() {
            press(&ctx, WindowButton::Close);
            ui.close();
        }
    });
}

/// Marca de IureDesk dibujada (el mismo sobre-calendario del icono pequeño).
fn app_mark(ui: &mut Ui, h: f32) {
    let side = (h * 0.5).clamp(16.0, 20.0);
    let (r, resp) = ui.allocate_exact_size(vec2(side, side), Sense::hover());
    a11y::label(&resp, "IureDesk");
    let p = ui.painter();
    let k = side / 1024.0;
    let at = |x: f32, y: f32| pos2(r.left() + x * k, r.top() + y * k);
    let navy = iurefficient_ui::tokens::brand::NAVY;
    p.rect_filled(r, 200.0 * k, navy);
    let card = Rect::from_min_max(at(144.0, 200.0), at(880.0, 840.0));
    p.rect_filled(card, 96.0 * k, Color32::WHITE);
    let band = Rect::from_min_max(at(144.0, 200.0), at(880.0, 376.0));
    p.rect_filled(band, egui::CornerRadius { nw: (96.0 * k) as u8, ne: (96.0 * k) as u8, sw: 0, se: 0 }, iurefficient_ui::tokens::brand::BLUE);
    p.add(egui::Shape::line(
        vec![at(248.0, 470.0), at(512.0, 660.0), at(776.0, 470.0)],
        Stroke::new((80.0 * k).max(1.2), navy),
    ));
}

/// Punto de estado de la sincronización junto al título (clic: ventana de estado).
fn sync_dot(app: &mut DeskApp, ui: &mut Ui) {
    let c = theme::colors(ui);
    let status = app.session.sync_status();
    let (_, label) = app.sync_label(&status);
    let (rect, resp) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::click());
    let ink = match &status {
        SyncStatus::Idle => c.success,
        SyncStatus::Busy(_) => {
            let t = ui.input(|i| i.time) as f32;
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
            c.accent.gamma_multiply(0.55 + 0.45 * (t * 4.0).sin().abs())
        }
        SyncStatus::Offline(_) => c.warning,
        SyncStatus::Error { .. } => c.danger,
        SyncStatus::Off => c.text_muted.gamma_multiply(0.6),
    };
    if resp.hovered() {
        ui.painter().circle_filled(rect.center(), 7.0, c.hover);
    }
    ui.painter().circle_filled(rect.center(), 3.5, ink);
    if resp.has_focus() {
        ui.painter().circle_stroke(rect.center(), 7.0, Stroke::new(1.5, c.focus));
    }
    a11y::button(&resp, &label);
    if resp.on_hover_text(&label).clicked() {
        let ctx = ui.ctx().clone();
        app.sync_clicked(&ctx);
    }
}

/// Búsqueda del módulo: correo (y desde los módulos sin búsqueda propia lleva al correo) o
/// contactos.
fn search(app: &mut DeskApp, ui: &mut Ui, width: f32) {
    let scheme = app.session.scheme();
    let contacts = app.session.module == Module::Contacts;
    let id_salt = "header-search";
    let search_id = ui.make_persistent_id(egui::Id::unique(id_salt));
    if app.mail.focus_search {
        app.mail.focus_search = false;
        ui.memory_mut(|m| m.request_focus(search_id));
    }
    let shortcut = iuredesk_engine::command::shortcut("mail.search", scheme).and_then(|c| crate::app::keyboard_shortcut(&c));
    let (text, hint) = if contacts {
        (&mut app.pim.contact_filter, pick("Find a person", "Buscar una persona"))
    } else {
        (&mut app.mail.search, pick("Search mail  (from: has:attachment …)", "Buscar en el correo  (de: tiene:adjunto …)"))
    };
    let mut field = SearchField::new(text).id_salt(id_salt).hint(hint).desired_width(width);
    if let Some(s) = shortcut {
        field = field.shortcut(s);
    }
    let resp = field.show(ui);
    if resp.changed() && !contacts {
        let q = app.mail.search.clone();
        if app.session.module != Module::Mail && !q.is_empty() {
            app.session.module = Module::Mail;
        }
        app.session.set_search(&q);
    }
}

/// «Comandos» con su atajo (abre la paleta).
fn commands_button(app: &mut DeskApp, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    let c = theme::colors(ui);
    let keys = iuredesk_engine::command::shortcut("app.palette", app.session.scheme())
        .map(|k| k.label(cfg!(target_os = "macos")))
        .unwrap_or_default();
    let label = pick("Commands", "Comandos");
    let small = theme::small_font(&ctx);
    let r = ui.add(
        egui::Button::new((
            RichText::new(label).color(c.text),
            RichText::new(&keys).font(small).color(c.text_muted),
        ))
        .fill(c.surface)
        .stroke(Stroke::new(1.0, c.separator)),
    );
    a11y::button(&r, label);
    if r.on_hover_text(pick("Everything IureDesk can do", "Todo lo que IureDesk puede hacer")).clicked() {
        app.run(&ctx, "app.palette");
    }
}

/// Menú rápido: tema y densidad (sustituye al control segmentado de la barra anterior).
fn quick_menu(app: &mut DeskApp, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    let label = pick("Appearance", "Apariencia");
    let r = icons::icon_button(ui, Icon::Sliders, label, true);
    let mut run: Option<String> = None;
    let mut settings = false;
    egui::Popup::menu(&r).show(|ui| {
        ui.set_min_width(190.0);
        iurefficient_ui::section_label(ui, pick("Theme", "Tema"));
        let t = app.session.prefs.theme;
        for (v, l) in [
            (ThemePref::System, pick("System", "Sistema")),
            (ThemePref::Light, pick("Light", "Claro")),
            (ThemePref::Dark, pick("Dark", "Oscuro")),
            (ThemePref::HighContrast, pick("High contrast", "Alto contraste")),
        ] {
            if ui.radio(t == v, l).clicked() {
                run = Some(format!("view.theme.{}", v.key()));
            }
        }
        ui.separator();
        iurefficient_ui::section_label(ui, pick("Density", "Densidad"));
        let d = app.session.prefs.density;
        for (v, l) in [
            (DensityPref::Comfortable, pick("Comfortable", "Cómoda")),
            (DensityPref::Compact, pick("Compact", "Compacta")),
            (DensityPref::Dense, pick("Dense", "Densa")),
        ] {
            if ui.radio(d == v, l).clicked() {
                run = Some(format!("view.density.{}", v.key()));
            }
        }
        ui.separator();
        if icons::menu_item(ui, Some(Icon::Settings), pick("Settings…", "Ajustes…")).clicked() {
            settings = true;
            ui.close();
        }
    });
    if let Some(id) = run {
        app.run(&ctx, &id);
    }
    if settings {
        app.settings_open = true;
    }
}

/// Campana y avatar de Iurefficient, solo si hay sesión (o se recuerda una).
fn iure_account(app: &mut DeskApp, ui: &mut Ui) {
    let (name, email, host) = match app.session.iure_status() {
        iuredesk_engine::iure::IureStatus::Ready { host, email, name } => {
            (name.clone().unwrap_or_default(), email.clone(), host.clone())
        }
        iuredesk_engine::iure::IureStatus::Offline { host, email, .. } => (String::new(), email.clone(), host.clone()),
        _ => return,
    };
    let c = theme::colors(ui);
    let side = theme::density(ui.ctx()).icon_size() + 8.0;
    let (r, resp) = ui.allocate_exact_size(vec2(side, side), Sense::click());
    let who = format!("Iurefficient: {} — {host}", if name.is_empty() { &email } else { &name });
    iurefficient_ui::widgets::avatar::paint_avatar(ui.painter(), r.center(), side - 4.0, &name, &email);
    if resp.has_focus() {
        ui.painter().circle_stroke(r.center(), side / 2.0, Stroke::new(1.5, c.focus));
    }
    a11y::button(&resp, &who);
    if resp.on_hover_text(&who).clicked() {
        app.settings_open = true;
    }
    let unread = app.session.iure_feed().unread;
    let label = pick("Iurefficient notifications", "Avisos de Iurefficient");
    let bell = icons::icon_button(ui, Icon::Bell, label, true);
    if unread > 0 {
        let n = count_text(u32::try_from(unread).unwrap_or(u32::MAX));
        let at = pos2(bell.rect.right() - 3.0, bell.rect.top() + 4.0);
        paint_badge(ui.painter(), at, &n, BadgeKind::Accent, &c, 9.0);
    }
    if bell.clicked() {
        app.iure.bell_open = !app.iure.bell_open;
    }
}

/// Un botón de la ventana.
fn window_button(ui: &mut Ui, b: WindowButton, style: ControlStyle, maximized: bool, h: f32) -> egui::Response {
    let c = theme::colors(ui);
    let (icon, label) = match b {
        WindowButton::Minimize => (Icon::Minimize, pick("Minimize", "Minimizar")),
        WindowButton::Maximize if maximized => (Icon::Restore, pick("Restore", "Restaurar")),
        WindowButton::Maximize => (Icon::Maximize, pick("Maximize", "Maximizar")),
        WindowButton::Close => (Icon::Close, pick("Close", "Cerrar")),
    };
    let resp = match style {
        ControlStyle::Windows => {
            let (rect, resp) = ui.allocate_exact_size(vec2(46.0, h), Sense::click());
            let close = b == WindowButton::Close;
            let hot = resp.hovered() || resp.has_focus();
            let (fill, ink) = if close && hot {
                (Color32::from_rgb(196, 43, 28), Color32::WHITE)
            } else if resp.is_pointer_button_down_on() {
                (c.pressed, c.text)
            } else if hot {
                (c.hover, c.text)
            } else {
                (Color32::TRANSPARENT, c.text)
            };
            ui.painter().rect_filled(rect, 0.0, fill);
            icons::paint(ui.painter(), Rect::from_center_size(rect.center(), vec2(15.0, 15.0)), icon, ink, ink);
            if resp.has_focus() {
                ui.painter().rect_stroke(rect.shrink(2.0), 2.0, Stroke::new(1.5, c.focus), egui::StrokeKind::Inside);
            }
            resp
        }
        ControlStyle::Adwaita | ControlStyle::MacOs => {
            let d = 24.0;
            let (rect, resp) = ui.allocate_exact_size(vec2(d, d), Sense::click());
            let a = if resp.is_pointer_button_down_on() {
                0.26
            } else if resp.hovered() {
                0.17
            } else {
                0.09
            };
            let base = if c.dark { Color32::WHITE } else { Color32::BLACK };
            ui.painter().circle_filled(rect.center(), d / 2.0, base.gamma_multiply(a));
            icons::paint(ui.painter(), Rect::from_center_size(rect.center(), vec2(15.0, 15.0)), icon, c.text, c.text);
            if resp.has_focus() {
                ui.painter().circle_stroke(rect.center(), d / 2.0 + 1.0, Stroke::new(1.5, c.focus));
            }
            resp
        }
    };
    a11y::button(&resp, label);
    resp.on_hover_text(label)
}

/// Bordes invisibles para cambiar el tamaño (sin decoraciones; no si está maximizada) y el borde
/// de 1 px. Va al final del cuadro para quedar encima de los paneles.
pub(crate) fn window_frame(app: &DeskApp, ui: &mut Ui) {
    let Some(chrome) = app.chrome() else { return };
    let ctx = ui.ctx().clone();
    if !chrome.owns_frame() || maximized(&ctx) {
        return;
    }
    let r = ctx.content_rect();
    let c = theme::colors(ui);
    ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, Id::new("iuredesk-window-border"))).rect_stroke(
        r,
        0.0,
        Stroke::new(1.0, c.separator),
        egui::StrokeKind::Inside,
    );
    for (i, (dir, rect, cursor)) in grips(r).into_iter().enumerate() {
        let resp = ui.interact(rect, Id::new(("iuredesk-resize", i)), Sense::drag());
        if resp.hovered() || resp.dragged() {
            ctx.set_cursor_icon(cursor);
        }
        if resp.drag_started_by(PointerButton::Primary) {
            ctx.send_viewport_cmd(ViewportCommand::BeginResize(dir));
        }
    }
}

/// Zonas de cambio de tamaño: 6 px por lado, esquinas de 12 px.
fn grips(r: Rect) -> [(ResizeDirection, Rect, CursorIcon); 8] {
    let (g, k) = (6.0, 12.0);
    let (l, t, rt, b) = (r.left(), r.top(), r.right(), r.bottom());
    let rr = Rect::from_min_max;
    [
        (ResizeDirection::North, rr(pos2(l + k, t), pos2(rt - k, t + g)), CursorIcon::ResizeNorth),
        (ResizeDirection::South, rr(pos2(l + k, b - g), pos2(rt - k, b)), CursorIcon::ResizeSouth),
        (ResizeDirection::West, rr(pos2(l, t + k), pos2(l + g, b - k)), CursorIcon::ResizeWest),
        (ResizeDirection::East, rr(pos2(rt - g, t + k), pos2(rt, b - k)), CursorIcon::ResizeEast),
        (ResizeDirection::NorthWest, rr(pos2(l, t), pos2(l + k, t + k)), CursorIcon::ResizeNorthWest),
        (ResizeDirection::NorthEast, rr(pos2(rt - k, t), pos2(rt, t + k)), CursorIcon::ResizeNorthEast),
        (ResizeDirection::SouthWest, rr(pos2(l, b - k), pos2(l + k, b)), CursorIcon::ResizeSouthWest),
        (ResizeDirection::SouthEast, rr(pos2(rt - k, b - k), pos2(rt, b)), CursorIcon::ResizeSouthEast),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use WindowButton::*;

    #[test]
    fn parses_gnome_button_layouts() {
        let l = ButtonLayout::parse("appmenu:minimize,maximize,close").unwrap();
        assert_eq!(l, ButtonLayout { left: vec![], right: vec![Minimize, Maximize, Close] });
        // gsettings lo imprime entre comillas.
        let l = ButtonLayout::parse("'close,minimize,maximize:'").unwrap();
        assert_eq!(l, ButtonLayout { left: vec![Close, Minimize, Maximize], right: vec![] });
        // GNOME por omisión: solo cerrar.
        assert_eq!(ButtonLayout::parse("appmenu:close").unwrap().right, vec![Close]);
        // Sin «:» todo va a la izquierda; desconocidos, espacios y repetidos se ignoran.
        let l = ButtonLayout::parse(" close , spacer,close,icon ").unwrap();
        assert_eq!(l, ButtonLayout { left: vec![Close], right: vec![] });
        let l = ButtonLayout::parse("close:close,maximize").unwrap();
        assert_eq!(l, ButtonLayout { left: vec![Close], right: vec![Maximize] });
        // Sin botones pero válido.
        assert_eq!(ButtonLayout::parse("appmenu:").unwrap(), ButtonLayout { left: vec![], right: vec![] });
    }

    #[test]
    fn hostile_layouts_fall_back() {
        assert!(ButtonLayout::parse("").is_none());
        assert!(ButtonLayout::parse("   ").is_none());
        assert!(ButtonLayout::parse("tonterías").is_none());
        assert!(ButtonLayout::parse(&"close,".repeat(200)).is_none());
        assert!(ButtonLayout::parse(&":".repeat(40)).is_some_and(|l| l.left.is_empty() && l.right.is_empty()));
        assert_eq!(WindowChrome::for_this_os(Some("")).layout.right.len(), if cfg!(target_os = "macos") { 0 } else { 3 });
    }

    #[test]
    fn grips_cover_the_edges_without_overlap() {
        let r = Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0));
        let g = grips(r);
        for (i, (_, a, _)) in g.iter().enumerate() {
            assert!(r.contains_rect(*a));
            for (_, b, _) in g.iter().skip(i + 1) {
                assert!(!a.intersects(*b) || a.intersect(*b).area() == 0.0);
            }
        }
    }
}
