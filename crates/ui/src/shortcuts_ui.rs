//! Hoja de atajos (UX-26): `?` (o «Atajos de teclado» en la paleta) muestra los atajos del
//! esquema elegido, por grupo, con los de dos teclas de Gmail («g i», «* a»). Sale de los mismos
//! comandos que la guía de uso, así que nunca se desfasa.

use egui::{Context, RichText};
use iuredesk_engine::command::{self, COMMANDS, Group};
use iurefficient_ui::{keycap, pick, theme};

use crate::app::DeskApp;

/// Muestra la hoja si está abierta.
pub(crate) fn show(app: &mut DeskApp, ctx: &Context) {
    if !app.show_shortcuts {
        return;
    }
    let scheme = app.session.scheme();
    let mac = cfg!(target_os = "macos");
    let mut close = false;
    let modal = egui::Modal::new(egui::Id::unique("shortcuts-sheet")).show(ctx, |ui| {
        let c = theme::colors(ui);
        ui.set_width(560.0_f32.min(ctx.content_rect().width() - 48.0));
        ui.horizontal(|ui| {
            ui.heading(pick("Keyboard shortcuts", "Atajos de teclado"));
            ui.label(RichText::new(scheme.label()).color(c.text_muted));
        });
        ui.label(
            RichText::new(pick(
                "Change the scheme in Settings → General. Every command is also in the palette (Ctrl+K).",
                "El esquema se cambia en Ajustes → General. Todos los comandos están en la paleta (Ctrl+K).",
            ))
            .color(c.text_muted)
            .small(),
        );
        ui.add_space(6.0);
        let max_h = (ctx.content_rect().height() - 180.0).max(200.0);
        egui::ScrollArea::vertical().max_height(max_h).show(ui, |ui| {
            // Por grupo, en el orden en que aparece cada uno.
            let mut groups: Vec<Group> = Vec::new();
            for spec in COMMANDS {
                if !groups.contains(&spec.group) {
                    groups.push(spec.group);
                }
            }
            let mut current: Option<Group> = None;
            for spec in groups.iter().flat_map(|g| COMMANDS.iter().filter(move |c| c.group == *g)) {
                let chord = command::shortcut(spec.id, scheme).map(|k| vec![k.label(mac)]);
                let typed =
                    command::typed(spec.id, scheme).map(|t| t.split(' ').map(str::to_string).collect::<Vec<_>>());
                let Some(keys) = chord.or(typed) else { continue };
                if current != Some(spec.group) {
                    current = Some(spec.group);
                    ui.add_space(6.0);
                    ui.label(RichText::new(spec.group.label()).strong().color(c.text));
                    ui.separator();
                }
                ui.horizontal(|ui| {
                    ui.label(RichText::new(spec.label()).color(c.text));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        for k in keys.iter().rev() {
                            keycap(ui, k);
                        }
                    });
                });
            }
        });
        ui.add_space(8.0);
        if ui.button(pick("Close", "Cerrar")).clicked() {
            close = true;
        }
    });
    if close || modal.should_close() {
        app.show_shortcuts = false;
    }
}
