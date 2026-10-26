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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_colors() {
        assert_eq!(human_size(10), "10 B");
        assert_eq!(human_size(24 * 1024), "24 KB");
        assert_eq!(human_size(3 * 1024 * 1024 / 2), "1.5 MB");
        assert_eq!(rgb(0x3B82F6), Color32::from_rgb(0x3B, 0x82, 0xF6));
        assert_eq!(mix(Color32::BLACK, Color32::WHITE, 0.5), Color32::from_rgb(128, 128, 128));
    }
}
