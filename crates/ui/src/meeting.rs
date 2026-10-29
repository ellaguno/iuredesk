//! «Unirse a la reunión» (Teams, Meet, Zoom, Webex…): el mismo botón en «Mi día», el editor de
//! eventos y la tarjeta de recordatorios, con «Copiar enlace» en su menú contextual. El enlace se
//! abre con el manejador `https` del sistema (`xdg-open` en Linux), que entrega la reunión al
//! navegador o a la app de escritorio; si no se puede, se copia y se dice con palabras.

use egui::{RichText, Ui, vec2};
use iuredesk_engine::today::MeetingLink;
use iurefficient_ui::{pick, theme};

use crate::app::DeskApp;

/// Dibuja el botón. `filled`: acento lleno (la tarjeta destacada); si no, botón normal.
/// Devuelve `true` si se pulsó «Unirse».
pub(crate) fn join_button(ui: &mut Ui, link: &MeetingLink, filled: bool) -> bool {
    let c = theme::colors(ui);
    let ctx = ui.ctx().clone();
    let label = format!("{} · {}", pick("Join", "Unirse"), link.provider);
    let text = RichText::new(label).font(theme::medium_font(&ctx, theme::density(&ctx).body_size()));
    let button = if filled {
        egui::Button::new(text.color(c.on_accent)).fill(c.accent).corner_radius(8).min_size(vec2(0.0, 30.0))
    } else {
        egui::Button::new(text)
    };
    let r = ui.add(button);
    iurefficient_ui::a11y::button(&r, pick("Join the meeting", "Unirse a la reunión"));
    let mut join = r.clicked();
    r.context_menu(|ui| {
        if ui.button(pick("Join the meeting", "Unirse a la reunión")).clicked() {
            join = true;
            ui.close();
        }
        if ui.button(pick("Copy link", "Copiar enlace")).clicked() {
            ui.ctx().copy_text(link.url.clone());
            ui.close();
        }
    });
    join
}

/// Abre el enlace de la reunión; si el sistema no puede, lo copia y lo explica.
pub(crate) fn join(app: &mut DeskApp, ctx: &egui::Context, url: &str) {
    if crate::open_url(url) {
        return;
    }
    ctx.copy_text(url.to_string());
    app.notice(iuredesk_engine::Notice::error(iuredesk_engine::Text::new(
        "The meeting link could not be opened in your browser. We copied it: paste it in your browser or meeting app.",
        "No se pudo abrir el enlace de la reunión en tu navegador. Lo copiamos: pégalo en tu navegador o en la app de la reunión.",
    )));
}
