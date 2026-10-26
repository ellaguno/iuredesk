//! Redactor (texto plano). Enviar llega con SMTP en F1; mientras, el borrador se guarda en
//! «Borradores».

use egui::{Align, Context, Layout, RichText, Ui};
use iuredesk_engine::compose::ComposeKind;
use iuredesk_engine::{ComposeDraft, Notice};
use iurefficient_ui::{pick, primary_button, theme};

use crate::app::DeskApp;

/// Una ventana de redacción.
pub struct ComposeWindow {
    id: u64,
    pub draft: ComposeDraft,
    open: bool,
}

impl ComposeWindow {
    pub fn new(id: u64, draft: ComposeDraft) -> Self {
        ComposeWindow { id, draft, open: true }
    }

    fn title(&self) -> String {
        let base = match self.draft.kind {
            ComposeKind::New => pick("New message", "Nuevo mensaje"),
            ComposeKind::Reply => pick("Reply", "Responder"),
            ComposeKind::ReplyAll => pick("Reply all", "Responder a todos"),
            ComposeKind::Forward => pick("Forward", "Reenviar"),
        };
        if self.draft.subject.trim().is_empty() { base.to_string() } else { format!("{base}: {}", self.draft.subject) }
    }
}

/// Dibuja todas las ventanas de redacción abiertas.
pub fn show_all(app: &mut DeskApp, ctx: &Context) {
    let mut save: Vec<ComposeDraft> = Vec::new();
    for w in &mut app.composers {
        let mut open = w.open;
        let mut close = false;
        egui::Window::new(w.title())
            .id(egui::Id::unique(("compose", w.id)))
            .open(&mut open)
            .default_size([620.0, 460.0])
            .collapsible(false)
            .show(ctx, |ui| {
                let (s, c) = form(ui, &mut w.draft);
                if s {
                    save.push(w.draft.clone());
                    close = true;
                }
                if c {
                    close = true;
                }
            });
        w.open = open && !close;
    }
    app.composers.retain(|w| w.open);
    for d in save {
        match app.session.save_draft(&d) {
            Ok(_) => app.notice(Notice::success("Draft saved in Drafts", "Borrador guardado en Borradores")),
            Err(e) => app.notice(Notice::error(e.text())),
        }
    }
}

/// Formulario. Devuelve (guardar, descartar).
fn form(ui: &mut Ui, d: &mut ComposeDraft) -> (bool, bool) {
    let c = theme::colors(ui);
    let mut save = false;
    let mut discard = false;
    egui::Grid::new("compose-head").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
        ui.label(RichText::new(pick("To", "Para")).color(c.text_muted));
        ui.add(
            egui::TextEdit::singleline(&mut d.to)
                .hint_text(pick("Recipients", "Destinatarios"))
                .desired_width(f32::INFINITY),
        );
        ui.end_row();
        ui.label(RichText::new("Cc").color(c.text_muted));
        ui.add(
            egui::TextEdit::singleline(&mut d.cc)
                .hint_text(pick("Copy to", "Con copia a"))
                .desired_width(f32::INFINITY),
        );
        ui.end_row();
        ui.label(RichText::new(pick("Subject", "Asunto")).color(c.text_muted));
        ui.add(
            egui::TextEdit::singleline(&mut d.subject)
                .hint_text(pick("Subject", "Asunto"))
                .desired_width(f32::INFINITY),
        );
        ui.end_row();
    });
    ui.add_space(6.0);
    ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
        ui.horizontal(|ui| {
            ui.add_enabled_ui(false, |ui| {
                primary_button(ui, pick("Send", "Enviar"))
                    .on_disabled_hover_text(pick("Sending arrives in F1", "Envío llega en F1"));
            });
            if ui.button(pick("Save draft", "Guardar borrador")).clicked() {
                save = true;
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button(pick("Discard", "Descartar")).clicked() {
                    discard = true;
                }
            });
        });
        ui.add_space(6.0);
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.add_sized(
                ui.available_size(),
                egui::TextEdit::multiline(&mut d.body).hint_text(pick("Write your message", "Escribe tu mensaje")),
            );
        });
    });
    (save, discard)
}
