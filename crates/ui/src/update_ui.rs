//! R3.2: aviso de versión nueva. Se consulta al arrancar y luego cada día (si está encendido en
//! Ajustes → General); «Buscar actualizaciones» (paleta, CLI, MCP) lo pide a mano y entonces
//! también avisa si ya está al día o si no hubo conexión. La versión nueva sale en un aviso con
//! «Ver» y en Ajustes; la instala iureapps o el usuario con el instalador.

use std::time::Instant;

use egui::{RichText, Ui};
use iuredesk_engine::Notice;
use iuredesk_engine::update::{self, Release, UpdateResult};
use iurefficient_ui::{Severity, pick, section_label, theme};

use crate::app::DeskApp;

/// Estado de la consulta.
#[derive(Default)]
pub struct UpdateState {
    last: Option<Instant>,
    failed: bool,
    fetching: bool,
    /// Versión nueva encontrada (se recuerda mientras la app sigue abierta).
    pub available: Option<Release>,
    /// Versión ya avisada (no repetir el aviso cada día).
    toasted: Option<String>,
}

impl DeskApp {
    /// Versión nueva que se encontró (pruebas).
    pub fn update_available(&self) -> Option<&Release> {
        self.update.available.as_ref()
    }

    /// Entrega una respuesta de la consulta como si llegara del fondo (pruebas).
    pub fn deliver_update(&mut self, r: UpdateResult) {
        on_result(self, r);
    }
}

/// Lanza la consulta automática cuando toca.
pub(crate) fn tick(app: &mut DeskApp) {
    let u = &app.update;
    if u.fetching || !app.session.prefs.update_check {
        return;
    }
    let wait = if u.failed { update::RETRY } else { update::CHECK_EVERY };
    if u.last.is_some_and(|t| t.elapsed() < wait) {
        return;
    }
    request(app, false);
}

fn request(app: &mut DeskApp, manual: bool) {
    app.update.last = Some(Instant::now());
    app.update.fetching = app.session.request_update_check(manual).is_some();
    if manual && !app.update.fetching {
        app.notice(Notice::info(
            "The sample mailbox doesn't check for updates.",
            "El buzón de ejemplo no busca actualizaciones.",
        ));
    }
}

/// `app.check_updates`.
pub(crate) fn check_now(app: &mut DeskApp) {
    if app.update.fetching {
        return;
    }
    request(app, true);
}

/// Llegó la respuesta.
pub(crate) fn on_result(app: &mut DeskApp, r: UpdateResult) {
    app.update.fetching = false;
    match r.result {
        Ok(Some(rel)) => {
            app.update.failed = false;
            let fresh = app.update.toasted.as_deref() != Some(rel.version.as_str());
            if fresh || r.manual {
                app.update.toasted = Some(rel.version.clone());
                let text = format!(
                    "{} {}",
                    pick("A new version of IureDesk is available:", "Hay una versión nueva de IureDesk:"),
                    rel.version
                );
                app.push_update_toast(Severity::Info, text);
            }
            app.update.available = Some(rel);
        }
        Ok(None) => {
            app.update.failed = false;
            app.update.available = None;
            if r.manual {
                app.notice(Notice::success(
                    format!("IureDesk is up to date (version {}).", update::CURRENT),
                    format!("IureDesk está al día (versión {}).", update::CURRENT),
                ));
            }
        }
        Err(t) => {
            app.update.failed = true;
            if r.manual {
                app.notice(Notice::error(t));
            }
        }
    }
}

/// «Ver» del aviso o de Ajustes: la página del release (notas e instaladores).
pub(crate) fn open_release(app: &DeskApp) {
    if let Some(r) = &app.update.available {
        crate::open_url(&r.url);
    }
}

/// Ajustes → General → «Actualizaciones».
pub(crate) fn settings(app: &mut DeskApp, ui: &mut Ui) {
    let c = theme::colors(ui);
    section_label(ui, pick("Updates", "Actualizaciones"));
    ui.label(
        RichText::new(format!("{} {}", pick("You have version", "Tienes la versión"), update::CURRENT))
            .color(c.text_muted),
    );
    let mut on = app.session.prefs.update_check;
    if ui.checkbox(&mut on, pick("Check for new versions every day", "Buscar versiones nuevas cada día")).changed() {
        app.session.prefs.update_check = on;
        app.session.save_prefs();
    }
    ui.horizontal_wrapped(|ui| {
        let busy = app.update.fetching;
        if ui.add_enabled(!busy, egui::Button::new(pick("Check now", "Buscar ahora"))).clicked() {
            check_now(app);
        }
        if busy {
            ui.spinner();
        }
        if let Some(v) = app.update.available.as_ref().map(|r| r.version.clone()) {
            ui.label(RichText::new(format!("{} {v}", pick("Available:", "Disponible:"))).color(c.accent).strong());
            if ui.button(pick("See what's new", "Ver novedades")).clicked() {
                open_release(app);
            }
        }
    });
    ui.label(
        RichText::new(pick(
            "If you installed IureDesk with iureapps, update it from there.",
            "Si instalaste IureDesk con iureapps, actualízalo desde ahí.",
        ))
        .small()
        .color(c.text_muted),
    );
}
