//! UX-13: fondo de «Mi día»: color, degradado, imagen elegida o la imagen del día de Wikimedia
//! Commons (con su crédito). La imagen la trae el motor ([`Session::request_today_image`]); aquí
//! se decodifica fuera del hilo de la UI (con límites), se sube como textura y se pinta
//! «cubriendo» el área sin deformarla.
//!
//! [`Session::request_today_image`]: iuredesk_engine::Session::request_today_image

use egui::{Color32, Rect, Ui, pos2};
use iuredesk_engine::JobOutput;
use iuredesk_engine::lang::Text;
use iuredesk_engine::today_bg::{Potd, TodayBackground, TodayImage};

use crate::app::DeskApp;

/// Lado mayor de la textura (más no se ve y gasta memoria).
const MAX_SIDE: u32 = 2560;
/// Dimensiones aceptadas al decodificar.
const MAX_DECODE: u32 = 12_000;

/// Estado del fondo.
#[derive(Default)]
pub struct BgState {
    /// Para qué fondo y día es lo cargado (o lo pedido).
    key: Option<(TodayBackground, chrono::NaiveDate)>,
    tex: Option<egui::TextureHandle>,
    /// Decodificada y aún sin subir (se sube al pintar, con el contexto de la ventana).
    pending: Option<egui::ColorImage>,
    pub(crate) credit: Option<Potd>,
    pub(crate) error: Option<Text>,
}

/// Imagen ya decodificada (llega por `Event::Custom`).
pub struct BgDecoded {
    for_bg: TodayBackground,
    image: Result<(egui::ColorImage, Option<Potd>), Text>,
}

fn rgb(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

/// ¿Hay un fondo propio (el encabezado va sobre un panel para leerse)?
pub fn active(app: &DeskApp) -> bool {
    !matches!(app.session.prefs.today_background, TodayBackground::None)
}

/// Pide la imagen si el fondo la necesita y aún no está (una vez por fondo y día).
fn ensure(app: &mut DeskApp) {
    let bg = app.session.prefs.today_background.clone();
    let key = (bg.clone(), app.session.today());
    if app.today.bg.key.as_ref() == Some(&key) {
        return;
    }
    app.today.bg = BgState { key: Some(key), ..Default::default() };
    if matches!(bg, TodayBackground::Commons | TodayBackground::Image { .. }) {
        app.session.request_today_image();
    }
}

/// Llegó la imagen del motor: se decodifica en segundo plano.
pub fn on_image(app: &mut DeskApp, img: TodayImage) {
    if img.for_bg != app.session.prefs.today_background {
        return;
    }
    match img.result {
        Err(t) => app.today.bg.error = Some(t),
        Ok((bytes, credit)) => {
            let for_bg = img.for_bg;
            app.session.spawn_blocking(move || {
                let image = decode(&bytes)
                    .map(|i| (i, credit))
                    .ok_or_else(|| Text::new("That image couldn't be read.", "No se pudo leer esa imagen."));
                JobOutput::Custom(Box::new(BgDecoded { for_bg, image }))
            });
        }
    }
}

/// Llegó la imagen decodificada: se sube la textura al pintar.
pub fn on_decoded(app: &mut DeskApp, ctx: &egui::Context, d: BgDecoded) {
    if d.for_bg != app.session.prefs.today_background {
        return;
    }
    match d.image {
        Ok((img, credit)) => {
            app.today.bg.pending = Some(img);
            app.today.bg.credit = credit;
            ctx.request_repaint();
            app.today.bg.error = None;
        }
        Err(t) => app.today.bg.error = Some(t),
    }
}

/// Decodifica con límites y reduce a [`MAX_SIDE`].
fn decode(bytes: &[u8]) -> Option<egui::ColorImage> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DECODE);
    limits.max_image_height = Some(MAX_DECODE);
    limits.max_alloc = Some(512 * 1024 * 1024);
    reader.limits(limits);
    let img = reader.decode().ok()?;
    let img = if img.width().max(img.height()) > MAX_SIDE {
        img.resize(MAX_SIDE, MAX_SIDE, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let rgba = img.to_rgba8();
    let size = [usize::try_from(rgba.width()).ok()?, usize::try_from(rgba.height()).ok()?];
    Some(egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()))
}

/// Pinta el fondo en `rect`.
pub fn paint(app: &mut DeskApp, ui: &Ui, rect: Rect, base: Color32) {
    ensure(app);
    if let Some(img) = app.today.bg.pending.take() {
        app.today.bg.tex = Some(ui.ctx().load_texture("today-background", img, egui::TextureOptions::LINEAR));
    }
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 0.0, base);
    match &app.session.prefs.today_background {
        TodayBackground::None => {}
        TodayBackground::Color { rgb: c } => {
            p.rect_filled(rect, 0.0, rgb(*c));
        }
        TodayBackground::Gradient { from, to } => {
            // Diagonal: arriba a la izquierda `from`, abajo a la derecha `to`.
            let (a, b) = (rgb(*from), rgb(*to));
            let mid = a.lerp_to_gamma(b, 0.5);
            let mut mesh = egui::Mesh::default();
            mesh.colored_vertex(rect.left_top(), a);
            mesh.colored_vertex(rect.right_top(), mid);
            mesh.colored_vertex(rect.right_bottom(), b);
            mesh.colored_vertex(rect.left_bottom(), mid);
            mesh.add_triangle(0, 1, 2);
            mesh.add_triangle(0, 2, 3);
            p.add(egui::Shape::mesh(mesh));
        }
        TodayBackground::Image { .. } | TodayBackground::Commons => {
            if let Some(tex) = &app.today.bg.tex {
                p.image(tex.id(), rect, cover_uv(tex.size_vec2(), rect.size()), Color32::WHITE);
            }
        }
    }
}

/// Parte de la imagen que cubre `area` sin deformarla (centrada).
fn cover_uv(img: egui::Vec2, area: egui::Vec2) -> Rect {
    if img.x <= 0.0 || img.y <= 0.0 || area.x <= 0.0 || area.y <= 0.0 {
        return Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
    }
    let (ia, aa) = (img.x / img.y, area.x / area.y);
    if ia > aa {
        // Imagen más ancha: se recortan los lados.
        let w = aa / ia;
        Rect::from_min_max(pos2((1.0 - w) / 2.0, 0.0), pos2((1.0 + w) / 2.0, 1.0))
    } else {
        let h = ia / aa;
        Rect::from_min_max(pos2(0.0, (1.0 - h) / 2.0), pos2(1.0, (1.0 + h) / 2.0))
    }
}

/// Crédito de la imagen del día (las licencias libres lo piden): una línea discreta; un clic abre
/// su página en Commons.
pub fn credit(app: &DeskApp, ui: &mut Ui) {
    let Some(p) = &app.today.bg.credit else { return };
    let c = iurefficient_ui::theme::colors(ui);
    let mut parts = vec![p.title.clone()];
    if !p.artist.is_empty() {
        parts.push(p.artist.clone());
    }
    if !p.license.is_empty() {
        parts.push(p.license.clone());
    }
    let text = format!(
        "{} · {} · Wikimedia Commons",
        iurefficient_ui::pick("Picture of the day", "Imagen del día"),
        parts.join(" · ")
    );
    let r = ui.add(
        egui::Label::new(
            egui::RichText::new(&text).color(c.text_muted).size(iurefficient_ui::theme::density(ui.ctx()).small_size()),
        )
        .truncate()
        .sense(egui::Sense::click()),
    );
    let r = r.on_hover_text(iurefficient_ui::pick("Open on Wikimedia Commons", "Abrir en Wikimedia Commons"));
    if r.clicked() {
        crate::open_url(&p.page_url);
    }
}

impl DeskApp {
    /// UX-13: fija el fondo de «Mi día» con una imagen ya en memoria (capturas y pruebas, sin red).
    pub fn demo_today_background(&mut self, bg: TodayBackground, image: Option<(&[u8], Option<Potd>)>) {
        self.session.prefs.today_background = bg.clone();
        self.today.bg = BgState { key: Some((bg, self.session.today())), ..Default::default() };
        if let Some((bytes, credit)) = image
            && let Some(img) = decode(bytes)
        {
            self.today.bg.pending = Some(img);
            self.today.bg.credit = credit;
        }
    }

    /// Crédito mostrado (pruebas).
    pub fn today_background_credit(&self) -> Option<&Potd> {
        self.today.bg.credit.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cover_keeps_the_aspect() {
        let u = cover_uv(egui::vec2(2000.0, 1000.0), egui::vec2(1000.0, 1000.0));
        assert!((u.width() - 0.5).abs() < 1e-4 && (u.height() - 1.0).abs() < 1e-4);
        let u = cover_uv(egui::vec2(1000.0, 2000.0), egui::vec2(1000.0, 500.0));
        assert!((u.width() - 1.0).abs() < 1e-4 && (u.height() - 0.25).abs() < 1e-4);
        assert_eq!(cover_uv(egui::vec2(0.0, 0.0), egui::vec2(10.0, 10.0)).width(), 1.0);
    }

    #[test]
    fn decode_is_bounded() {
        assert!(decode(b"no es imagen").is_none());
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(4000, 10, image::Rgba([1, 2, 3, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let img = decode(&png).unwrap();
        assert!(img.size[0] <= MAX_SIDE as usize, "se reduce");
    }
}
