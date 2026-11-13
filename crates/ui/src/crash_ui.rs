//! R3.3: «IureDesk se cerró inesperadamente». Al arrancar, si quedó un informe de fallo sin ver,
//! se ofrece copiarlo, abrir su carpeta o reportarlo en GitHub (el usuario ve el issue antes de
//! enviarlo). Cerrar el aviso lo marca como visto. Nada se envía solo.

use egui::{Context, RichText};
use iuredesk_engine::crash;
use iurefficient_ui::{pick, theme};

use crate::app::DeskApp;

impl DeskApp {
    /// Informes de fallos que se están ofreciendo (pruebas).
    pub fn crash_reports_pending(&self) -> usize {
        self.crash.len()
    }
}

pub(crate) fn frame(app: &mut DeskApp, ctx: &Context) {
    let Some(report) = app.crash.first().cloned() else { return };
    let more = app.crash.len().saturating_sub(1);
    let mut close = false;
    let modal = egui::Modal::new(egui::Id::unique("crash-report")).show(ctx, |ui| {
        ui.set_max_width(460.0);
        let c = theme::colors(ui);
        ui.label(RichText::new(pick("IureDesk closed unexpectedly", "IureDesk se cerró inesperadamente")).strong());
        ui.add_space(6.0);
        ui.add(
            egui::Label::new(RichText::new(pick(
                "Sorry about that. A report was saved on this computer with the technical details (no \
                 mail content). Sending it helps us fix it; nothing is sent unless you do.",
                "Lo sentimos. Se guardó en este equipo un informe con los detalles técnicos (sin \
                 contenido de tus correos). Enviarlo nos ayuda a corregirlo; no se envía nada si tú no lo haces.",
            )))
            .wrap(),
        );
        if more > 0 {
            ui.label(
                RichText::new(format!("{} {more}", pick("Earlier reports too:", "También hay informes anteriores:")))
                    .color(c.text_muted),
            );
        }
        ui.add_space(6.0);
        egui::ScrollArea::vertical().max_height(160.0).show(ui, |ui| {
            ui.add(egui::Label::new(RichText::new(&report.text).monospace().small().color(c.text_muted)).wrap());
        });
        ui.add_space(10.0);
        ui.horizontal_wrapped(|ui| {
            let report_btn = ui.add(
                egui::Button::new(RichText::new(pick("Report on GitHub", "Reportar en GitHub")).color(c.on_accent))
                    .fill(c.accent),
            );
            if report_btn.clicked() {
                ui.ctx().copy_text(report.text.clone());
                crate::open_url(&crash::issue_url(&report));
            }
            if ui.button(pick("Copy report", "Copiar informe")).clicked() {
                ui.ctx().copy_text(report.text.clone());
            }
            if let Some(dir) = report.path.parent()
                && ui.button(pick("Open folder", "Abrir carpeta")).clicked()
                && !crate::capture(dir.display().to_string())
            {
                let _ = iuredesk_desktop::launch::open_detached(dir);
            }
            if ui.button(pick("Close", "Cerrar")).clicked() {
                close = true;
            }
        });
    });
    if close || modal.should_close() {
        if let Some(dir) = app.options.crash_dir.as_deref() {
            crash::acknowledge(dir);
        }
        app.crash.clear();
    }
}
