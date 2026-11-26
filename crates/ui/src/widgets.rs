//! Piezas pequeñas propias de IureDesk sobre el ui-kit.

use egui::{Color32, Response, RichText, Sense, Stroke, Ui, vec2};
use iuredesk_engine::{DensityPref, ThemePref};
use iurefficient_ui::{Density, Icon, ThemeMode, icons, theme};

/// Densidad del ui-kit para la preferencia.
pub fn kit_density(d: DensityPref) -> Density {
    match d {
        DensityPref::Comfortable => Density::Comfortable,
        DensityPref::Compact => Density::Compact,
        DensityPref::Dense => Density::Dense,
    }
}

/// Tema del ui-kit para la preferencia.
pub fn kit_theme(t: ThemePref) -> ThemeMode {
    match t {
        ThemePref::System => ThemeMode::System,
        ThemePref::Light => ThemeMode::Light,
        ThemePref::Dark => ThemeMode::Dark,
        ThemePref::HighContrast => ThemeMode::HighContrast,
    }
}

/// Icono por nombre (el de los comandos), con uno neutro si no existe.
pub fn icon(name: &str) -> Icon {
    Icon::named(name).unwrap_or(Icon::Info)
}

/// Color 0xRRGGBB.
pub fn rgb(c: u32) -> Color32 {
    Color32::from_rgb(((c >> 16) & 0xFF) as u8, ((c >> 8) & 0xFF) as u8, (c & 0xFF) as u8)
}

/// Mezcla `top` sobre `bottom` con opacidad `a` (0–1).
pub fn mix(bottom: Color32, top: Color32, a: f32) -> Color32 {
    let a = a.clamp(0.0, 1.0);
    let ch = |b: u8, t: u8| (f32::from(b) * (1.0 - a) + f32::from(t) * a).round() as u8;
    Color32::from_rgb(ch(bottom.r(), top.r()), ch(bottom.g(), top.g()), ch(bottom.b(), top.b()))
}

/// Título de un panel («Entrada», «Contactos»), con un dato pequeño al lado.
pub fn pane_title(ui: &mut Ui, title: &str, detail: &str) {
    let c = theme::colors(ui);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        ui.label(RichText::new(title).font(theme::bold_font(ui.ctx(), 17.0)).color(c.text));
        if !detail.is_empty() {
            ui.label(RichText::new(detail).color(c.text_muted));
        }
    });
}

/// Botón principal a todo lo ancho (p. ej. «Nuevo mensaje» arriba de las carpetas).
pub fn wide_primary(ui: &mut Ui, ic: Icon, label: &str) -> Response {
    let c = theme::colors(ui);
    let d = theme::density(ui.ctx());
    let h = d.control_height() + 6.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::click());
    let fill = if resp.hovered() { mix(c.accent, Color32::BLACK, 0.08) } else { c.accent };
    ui.painter().rect_filled(rect, iurefficient_ui::tokens::radius::CONTROL, fill);
    if resp.has_focus() {
        ui.painter().rect_stroke(rect.expand(1.0), 7.0, Stroke::new(2.0, c.focus), egui::StrokeKind::Outside);
    }
    let font = theme::bold_font(ui.ctx(), d.body_size());
    let g = ui.painter().layout_no_wrap(label.to_string(), font, c.on_accent);
    let side = d.icon_size();
    let total = side + 8.0 + g.size().x;
    let x0 = rect.center().x - total / 2.0;
    let ir = egui::Rect::from_center_size(egui::pos2(x0 + side / 2.0, rect.center().y), vec2(side, side));
    icons::paint(ui.painter(), ir, ic, c.on_accent, c.on_accent);
    ui.painter().galley(egui::pos2(x0 + side + 8.0, rect.center().y - g.size().y / 2.0), g, c.on_accent);
    iurefficient_ui::a11y::button(&resp, label);
    resp
}

/// Botón de texto con icono, sin marco mientras no se pasa por encima (barra de acciones).
pub fn tool_button(ui: &mut Ui, ic: Icon, label: &str, tip: &str, enabled: bool) -> Response {
    let c = theme::colors(ui);
    let d = theme::density(ui.ctx());
    let font = theme::body_font(ui.ctx());
    let ink = if enabled { c.text } else { c.text_muted };
    let g = ui.painter().layout_no_wrap(label.to_string(), font, ink);
    let side = d.icon_size();
    let h = d.control_height();
    let w = 8.0 + side + 6.0 + g.size().x + 10.0;
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(vec2(w, h), sense);
    if enabled && resp.hovered() {
        ui.painter().rect_filled(rect, iurefficient_ui::tokens::radius::CONTROL, c.hover);
    }
    if resp.has_focus() {
        ui.painter().rect_stroke(rect, 6.0, Stroke::new(2.0, c.focus), egui::StrokeKind::Inside);
    }
    let ir =
        egui::Rect::from_center_size(egui::pos2(rect.left() + 8.0 + side / 2.0, rect.center().y), vec2(side, side));
    icons::paint(ui.painter(), ir, ic, ink, if enabled { c.accent } else { ink });
    ui.painter().galley(egui::pos2(ir.right() + 6.0, rect.center().y - g.size().y / 2.0), g, ink);
    iurefficient_ui::a11y::button(&resp, label);
    resp.on_hover_text(tip)
}

/// Línea de un dato con icono (correo, teléfono…) en fichas de contacto.
pub fn info_row(ui: &mut Ui, ic: Icon, label: &str, value: &str) {
    let c = theme::colors(ui);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        let (r, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
        icons::paint(ui.painter(), r, ic, c.text_muted, c.accent);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            if !label.is_empty() {
                ui.label(RichText::new(label).size(theme::density(ui.ctx()).small_size()).color(c.text_muted));
            }
            ui.add(egui::Label::new(RichText::new(value).color(c.text)).selectable(true));
        });
    });
}

/// Nombre accesible de un control sin etiqueta visible (campo de texto, desplegable), sin cambiar
/// su papel.
pub fn named(resp: &Response, typ: egui::Role, name: &str) {
    let enabled = resp.enabled();
    let name = name.to_string();
    resp.widget_info(move || egui::WidgetInfo::labeled(typ, enabled, name.clone()));
}

/// Separador horizontal fino con el color del tema.
pub fn hairline(ui: &mut Ui) {
    let c = theme::colors(ui);
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().hline(r.x_range(), r.center().y, Stroke::new(1.0, c.separator));
}

/// Texto de un tamaño en bytes («24 KB»).
pub fn human_size(n: usize) -> String {
    let n = n as f64;
    if n < 1024.0 {
        format!("{n:.0} B")
    } else if n < 1024.0 * 1024.0 {
        format!("{:.0} KB", n / 1024.0)
    } else {
        format!("{:.1} MB", n / 1024.0 / 1024.0)
    }
}

/// Campo de contraseña con botón para mostrarla u ocultarla (estado por `id_salt`).
/// Acepta pegar como cualquier campo de texto.
pub fn secret_field(ui: &mut Ui, value: &mut String, id_salt: &str, hint: &str, width: f32) -> Response {
    let id = ui.make_persistent_id(("secret-visible", id_salt));
    let mut visible = ui.data(|d| d.get_temp::<bool>(id)).unwrap_or(false);
    // El ancho se calcula fuera de la fila: dentro de `horizontal` el disponible puede ser infinito.
    let avail = ui.available_width();
    let w = if width.is_finite() {
        width
    } else if avail.is_finite() {
        (avail - 34.0).max(80.0)
    } else {
        280.0
    };
    let inner = ui.horizontal(|ui| {
        let r = ui.add(egui::TextEdit::singleline(value).password(!visible).hint_text(hint).desired_width(w));
        // Nombre accesible: el texto de ayuda (UX-4; antes el campo no tenía nombre).
        let (enabled, name) = (r.enabled(), hint.to_string());
        let role = if visible { egui::Role::TextInput } else { egui::Role::PasswordInput };
        r.widget_info(|| egui::WidgetInfo::labeled(role, enabled, name.clone()));
        let tip = if visible {
            iurefficient_ui::pick("Hide password", "Ocultar contraseña")
        } else {
            iurefficient_ui::pick("Show password", "Mostrar contraseña")
        };
        let (rect, b) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::click());
        let c = theme::colors(ui);
        let ink = if b.hovered() { c.text } else { c.text_muted };
        let p = ui.painter();
        // Ojo dibujado: contorno de almendra, pupila y, si está oculta, una diagonal.
        let ctr = rect.center();
        let pts: Vec<egui::Pos2> = (0..=24)
            .map(|i| {
                let t = std::f32::consts::TAU * i as f32 / 24.0;
                egui::pos2(ctr.x + 8.0 * t.cos(), ctr.y + 4.6 * t.sin())
            })
            .collect();
        p.add(egui::Shape::line(pts, Stroke::new(1.4, ink)));
        p.circle_filled(ctr, 2.3, ink);
        if !visible {
            p.line_segment([ctr + vec2(-7.0, 6.0), ctr + vec2(7.0, -6.0)], Stroke::new(1.4, ink));
        }
        b.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, tip));
        let b = b.on_hover_text(tip);
        if b.clicked() {
            visible = !visible;
        }
        r
    });
    ui.data_mut(|d| d.insert_temp(id, visible));
    inner.inner
}

/// Nombre accesible de un campo de texto (conserva el papel de campo editable; F7.1).
pub fn text_label(resp: &Response, text: &str) {
    let (enabled, text) = (resp.enabled(), text.to_string());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, enabled, text.clone()));
}

/// UX-27: color propio de una zona (barra lateral, barra de título): su relleno y si el texto de
/// encima va con la paleta oscura (color oscuro) o la clara.
pub fn tint(rgb: Option<[u8; 3]>) -> Option<(Color32, bool)> {
    let [r, g, b] = rgb?;
    // Luminancia percibida (0..1): por debajo de la mitad, texto claro.
    let l = (0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b)) / 255.0;
    Some((Color32::from_rgb(r, g, b), l < 0.5))
}

/// Pone en `ui` (y lo que cuelga de él) el estilo claro u oscuro de la app.
pub fn tint_style(ui: &mut Ui, dark: bool) {
    let style = ui.ctx().style_of(if dark { egui::Theme::Dark } else { egui::Theme::Light });
    ui.set_style(style);
}

/// Panel a la izquierda como `split::left` del kit, con el color propio de la barra lateral si lo
/// hay (UX-27).
pub fn side_left<R>(
    ui: &mut Ui,
    id: &str,
    kind: iurefficient_ui::PaneKind,
    width: f32,
    range: std::ops::RangeInclusive<f32>,
    color: Option<[u8; 3]>,
    add: impl FnOnce(&mut Ui) -> R,
) -> egui::InnerResponse<R> {
    let Some((fill, dark)) = tint(color) else {
        return iurefficient_ui::split::left(ui, id, kind, width, range, add);
    };
    let frame = iurefficient_ui::split::frame(ui, kind).fill(fill);
    let panel = egui::Panel::left(id.to_string());
    let panel = if kind == iurefficient_ui::PaneKind::Rail {
        panel.exact_size(width).resizable(false)
    } else {
        let (lo, hi) = (range.start().min(*range.end()), range.end().max(*range.start()));
        panel.default_size(width.clamp(lo, hi)).size_range(lo..=hi).resizable(true)
    };
    panel.frame(frame).show(ui, |ui| {
        tint_style(ui, dark);
        add(ui)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tint_picks_the_text_palette_by_lightness() {
        assert_eq!(tint(None), None);
        assert_eq!(tint(Some([0x1f, 0x29, 0x37])), Some((Color32::from_rgb(0x1f, 0x29, 0x37), true)));
        assert_eq!(tint(Some([0xf5, 0xf0, 0xe6])).map(|t| t.1), Some(false));
        assert_eq!(tint(Some([0xff, 0xff, 0x00])).map(|t| t.1), Some(false), "amarillo: texto oscuro");
        assert_eq!(tint(Some([0x1e, 0x3a, 0x8a])).map(|t| t.1), Some(true));
    }

    #[test]
    fn sizes_and_colors() {
        assert_eq!(human_size(10), "10 B");
        assert_eq!(human_size(24 * 1024), "24 KB");
        assert_eq!(human_size(3 * 1024 * 1024 / 2), "1.5 MB");
        assert_eq!(rgb(0x3B82F6), Color32::from_rgb(0x3B, 0x82, 0xF6));
        assert_eq!(mix(Color32::BLACK, Color32::WHITE, 0.5), Color32::from_rgb(128, 128, 128));
    }
}
