//! Campos de un servidor de correo (nombre, puerto, seguridad, usuario), compartidos por
//! «Configurar manualmente» del asistente y el editor de «Ajustes → Cuentas». Al cambiar la
//! seguridad, el puerto pasa al de siempre (993/143, 995/110, 465/587) si no se había cambiado.

use egui::{RichText, Ui};
use iuredesk_engine::onboarding::{ProbeStatus, port_after_change};
use iuredesk_model::account::{Security, ServerConfig};
use iurefficient_ui::{pick, segmented, theme};

/// Ancho de los campos de texto.
const FIELD_W: f32 = 260.0;

/// Dibuja los campos de `s`. Devuelve `true` si cambió algo.
pub(crate) fn server_fields(ui: &mut Ui, id: &str, s: &mut ServerConfig) -> bool {
    let before = s.clone();
    egui::Grid::new(id).num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
        let l = ui.label(pick("Server", "Servidor"));
        ui.add(egui::TextEdit::singleline(&mut s.host).desired_width(FIELD_W).hint_text("mail.example.com"))
            .labelled_by(l.id);
        ui.end_row();
        let l = ui.label(pick("Port", "Puerto"));
        ui.add(egui::DragValue::new(&mut s.port).range(1..=65535)).labelled_by(l.id);
        ui.end_row();
        ui.label(pick("Security", "Seguridad"));
        let old = s.security;
        segmented(
            ui,
            &mut s.security,
            &[(Security::Tls, "SSL/TLS"), (Security::StartTls, "STARTTLS"), (Security::Plain, pick("None", "Ninguna"))],
        );
        if s.security != old {
            s.port = port_after_change((s.protocol, old), (s.protocol, s.security), s.port);
        }
        ui.end_row();
        let l = ui.label(pick("User name", "Usuario"));
        ui.add(egui::TextEdit::singleline(&mut s.username).desired_width(FIELD_W)).labelled_by(l.id);
        ui.end_row();
    });
    if s.security == Security::Plain && s.host.trim() != "127.0.0.1" && s.host.trim() != "localhost" {
        let c = theme::colors(ui);
        ui.label(
            RichText::new(pick(
                "Without security your password travels unencrypted.",
                "Sin seguridad tu contraseña viaja sin cifrar.",
            ))
            .color(c.warning)
            .size(theme::density(ui.ctx()).small_size()),
        );
    }
    *s != before
}

/// Resultado de la prueba de un servidor, en una línea.
pub(crate) fn probe_line(ui: &mut Ui, st: Option<&ProbeStatus>) {
    let c = theme::colors(ui);
    match st {
        Some(ProbeStatus::Running) => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new(pick("Testing…", "Probando…")).color(c.text_muted));
            });
        }
        Some(ProbeStatus::Ok) => {
            ui.label(RichText::new(pick("Connection works", "La conexión funciona")).color(c.success));
        }
        Some(ProbeStatus::Failed(t)) => {
            ui.add(egui::Label::new(RichText::new(t.get()).color(c.danger)).wrap());
        }
        None => {}
    }
}
