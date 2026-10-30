//! UX-5: la cápsula de Iurefficient de la lista de correo: una píldora del azul de la marca con
//! el símbolo de Iurefficient (dibujado en código a partir del logotipo: barra vertical y dos
//! medios discos) y el icono del tipo de relación (contacto, cliente, lead, oportunidad, tarea,
//! caso, ticket, aviso). En densidad «Densa» queda solo el símbolo en el color de la marca.

use egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, pos2, vec2};
use iuredesk_engine::IureKind;
use iurefficient_ui::{Icon, icons};

/// Azul del símbolo de Iurefficient (`#042D94`, el del flaticon).
const MARK_BLUE: Color32 = Color32::from_rgb(4, 45, 148);
/// Relleno de la cápsula en tema claro: el azul del símbolo un poco más vivo (a 16 px el azul
/// original se ve casi negro).
const FILL_LIGHT: Color32 = Color32::from_rgb(18, 62, 176);
/// Relleno en tema oscuro: más claro para separarse del fondo casi negro (texto blanco ≥ 4.5:1).
const FILL_DARK: Color32 = Color32::from_rgb(52, 98, 214);

/// Icono del tipo de relación.
pub(crate) fn kind_icon(k: IureKind) -> Icon {
    match k {
        IureKind::Contact => Icon::Person,
        IureKind::Client => Icon::Contacts,
        IureKind::Case => Icon::Folder,
        IureKind::Opportunity => Icon::Money,
        IureKind::Lead => Icon::Bolt,
        IureKind::Ticket => Icon::Comment,
        IureKind::Task => Icon::Check,
        IureKind::Notice => Icon::Bell,
    }
}

/// Relleno de la cápsula según el tema.
pub(crate) fn fill(dark: bool) -> Color32 {
    if dark { FILL_DARK } else { FILL_LIGHT }
}

/// Color del símbolo suelto (densa, pestaña) según el tema.
pub(crate) fn mark_color(dark: bool) -> Color32 {
    if dark { Color32::from_rgb(120, 160, 255) } else { MARK_BLUE }
}

/// Medio disco convexo: centro, radio, `up` = la panza hacia arriba.
fn half_disc(c: Pos2, r: f32, up: bool, base_extra: f32, ink: Color32) -> Shape {
    const STEPS: usize = 14;
    let mut pts = Vec::with_capacity(STEPS + 3);
    let dir = if up { -1.0 } else { 1.0 };
    // Base recta (con su prolongación hasta el borde del símbolo).
    pts.push(pos2(c.x + r, c.y - dir * base_extra));
    for i in 0..=STEPS {
        let a = std::f32::consts::PI * i as f32 / STEPS as f32;
        pts.push(pos2(c.x + r * a.cos(), c.y + dir * r * a.sin()));
    }
    pts.push(pos2(c.x - r, c.y - dir * base_extra));
    // Siempre en el sentido de las agujas del reloj (el suavizado de egui lo espera).
    if up {
        pts.reverse();
    }
    Shape::convex_polygon(pts, ink, Stroke::NONE)
}

/// Pinta el símbolo de Iurefficient centrado en `rect` (alto = alto del símbolo).
pub(crate) fn paint_mark(p: &Painter, rect: Rect, ink: Color32) {
    // Proporciones del flaticon (48 × 48): barra 6.05–12.37, medios discos de radio 12.73 con
    // centro en x = 29.22; alto útil 4.31–43.69.
    let h = rect.height().min(rect.width() * 39.38 / 35.89);
    let k = h / 39.38;
    let w = 35.89 * k;
    let o = pos2(rect.center().x - w / 2.0, rect.center().y - h / 2.0);
    let at = |x: f32, y: f32| pos2(o.x + (x - 6.05) * k, o.y + (y - 4.31) * k);
    p.rect_filled(Rect::from_min_max(at(6.05, 4.31), at(12.37, 43.69)), 0.0, ink);
    let r = 12.73 * k;
    // Medio disco superior: base arriba (y = 4.31), panza hacia abajo hasta y = 21.95.
    p.add(half_disc(at(29.22, 9.23), r, false, 4.92 * k, ink));
    // Inferior: base abajo (y = 43.69), panza hacia arriba hasta y = 26.05.
    p.add(half_disc(at(29.22, 38.77), r, true, 4.92 * k, ink));
}

/// Tamaño de la cápsula para un alto dado.
pub(crate) fn capsule_size(h: f32) -> egui::Vec2 {
    vec2((h * 2.25).round(), h)
}

/// Pinta la cápsula (símbolo + icono del tipo) en `rect`.
pub(crate) fn paint_capsule(p: &Painter, rect: Rect, kind: IureKind, dark: bool) {
    let h = rect.height();
    let bg = fill(dark);
    p.rect_filled(rect, h / 2.0, bg);
    // Borde fino más claro: en tema oscuro la píldora no se funde con la selección.
    p.rect_stroke(
        rect,
        h / 2.0,
        Stroke::new(1.0, Color32::from_white_alpha(if dark { 40 } else { 28 })),
        egui::StrokeKind::Inside,
    );
    let pad = (h * 0.32).round();
    let mark_h = (h * 0.62).round();
    let mark =
        Rect::from_min_size(pos2(rect.left() + pad, rect.center().y - mark_h / 2.0), vec2(mark_h * 0.92, mark_h));
    paint_mark(p, mark, Color32::WHITE);
    // Separador sutil.
    let sx = mark.right() + (h * 0.22).round();
    p.vline(sx, (rect.top() + h * 0.25)..=(rect.bottom() - h * 0.25), Stroke::new(1.0, Color32::from_white_alpha(90)));
    let side = (h * 0.72).round();
    let ir = Rect::from_center_size(pos2((sx + rect.right() - pad * 0.7) / 2.0, rect.center().y), vec2(side, side));
    icons::paint(p, ir, kind_icon(kind), Color32::WHITE, Color32::WHITE);
}

/// Marca compacta (densa): el símbolo en el color de la marca.
pub(crate) fn paint_dot(p: &Painter, rect: Rect, dark: bool) {
    paint_mark(p, rect, mark_color(dark));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// El texto blanco (y el símbolo) sobre la cápsula se lee: contraste ≥ 4.5:1 en ambos temas.
    #[test]
    fn capsule_contrast_is_aa() {
        fn lum(c: Color32) -> f32 {
            let f = |v: u8| {
                let s = v as f32 / 255.0;
                if s <= 0.039_28 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) }
            };
            0.2126 * f(c.r()) + 0.7152 * f(c.g()) + 0.0722 * f(c.b())
        }
        for dark in [false, true] {
            let ratio = (1.0 + 0.05) / (lum(fill(dark)) + 0.05);
            assert!(ratio >= 4.5, "dark={dark} {ratio}");
        }
    }

    #[test]
    fn every_kind_has_an_icon() {
        let kinds = [
            IureKind::Contact,
            IureKind::Client,
            IureKind::Case,
            IureKind::Opportunity,
            IureKind::Lead,
            IureKind::Ticket,
            IureKind::Task,
            IureKind::Notice,
        ];
        let icons: std::collections::HashSet<_> = kinds.iter().map(|k| format!("{:?}", kind_icon(*k))).collect();
        assert_eq!(icons.len(), kinds.len());
    }
}
