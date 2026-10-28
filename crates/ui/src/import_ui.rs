//! Asistente de importación (F4.2): Ajustes → «Importar» y la bienvenida («¿Vienes de Outlook,
//! Thunderbird o Apple Mail?»). Detecta los orígenes instalados, deja elegir un archivo o carpeta
//! (portal de archivos), el nombre de la cuenta local, importa en segundo plano con barra de
//! progreso, cancela y reanuda, y termina con el informe.

use std::path::PathBuf;
use std::sync::mpsc::Receiver;

use egui::{Context, RichText, Ui};
use iuredesk_engine::Session;
use iuredesk_engine::migrate::{Migrate, MigrateState};
use iurefficient_ui::{Banner, Icon, Severity, icons, pick, primary_button, section_label, theme};

use crate::app::DeskApp;

/// Estado del asistente de importación.
pub struct ImportUi {
    pub hub: Migrate,
    pub open: bool,
    /// Orígenes elegidos (ruta, nombre para mostrar).
    pub chosen: Vec<(PathBuf, String)>,
    account_name: String,
    picking: Option<Receiver<Option<PathBuf>>>,
    error: Option<String>,
}

impl ImportUi {
    pub fn new(session: &Session) -> Self {
        ImportUi {
            hub: Migrate::new(session),
            open: false,
            chosen: Vec::new(),
            account_name: String::new(),
            picking: None,
            error: None,
        }
    }

    /// Abre el asistente (y busca los orígenes instalados).
    pub fn open(&mut self) {
        self.open = true;
        self.error = None;
        if self.hub.detected().is_none() {
            self.hub.detect();
        }
    }

    /// Elige un origen por ruta (pruebas y arrastrar).
    pub fn choose(&mut self, path: PathBuf) {
        let label = label_for(&path);
        if !self.chosen.iter().any(|(p, _)| *p == path) {
            self.chosen.push((path, label));
        }
    }

    pub fn modal(&self) -> bool {
        self.open
    }
}

fn lang_tag() -> &'static str {
    if iuredesk_engine::lang::spanish() { "es" } else { "en" }
}

fn label_for(path: &std::path::Path) -> String {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string());
    match Migrate::kind_of(path) {
        Some(k) => format!("{name} — {}", k.label(lang_tag())),
        None => name,
    }
}

/// Oferta en la bienvenida.
pub fn onboarding_offer(app: &mut DeskApp, ui: &mut Ui) {
    let c = theme::colors(ui);
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(pick(
                "Coming from Outlook, Thunderbird or Apple Mail?",
                "¿Vienes de Outlook, Thunderbird o Apple Mail?",
            ))
            .color(c.text_muted),
        );
        if ui.link(pick("Import your mail", "Importa tu correo")).clicked() {
            app.import.open();
        }
    });
}

/// Ajustes → «Importar».
pub fn settings_section(app: &mut DeskApp, ui: &mut Ui) {
    let c = theme::colors(ui);
    section_label(ui, pick("Import", "Importar"));
    ui.label(
        RichText::new(pick(
            "Bring mail, contacts and calendars from Outlook (.pst), Thunderbird, Apple Mail, Gmail Takeout, mbox, .eml, .ics or .vcf. Everything stays on this computer.",
            "Trae correo, contactos y calendarios de Outlook (.pst), Thunderbird, Apple Mail, Gmail Takeout, mbox, .eml, .ics o .vcf. Todo se queda en este equipo.",
        ))
        .color(c.text_muted),
    );
    if ui.button(pick("Import mail…", "Importar correo…")).clicked() {
        app.import.open();
    }
    ui.add_space(14.0);
}

/// Cada cuadro: resultados y la ventana del asistente.
pub fn frame(app: &mut DeskApp, ctx: &Context) {
    if app.import.hub.poll() {
        app.session.reload_external();
    }
    if let Some(rx) = &app.import.picking
        && let Ok(p) = rx.try_recv()
    {
        app.import.picking = None;
        if let Some(p) = p {
            app.import.choose(p);
        }
    }
    if !app.import.open {
        return;
    }
    let mut open = true;
    let state = app.import.hub.state().clone();
    egui::Window::new(pick("Import mail", "Importar correo"))
        .id(egui::Id::unique("import-wizard"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(520.0)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .show(ctx, |ui| match &state {
            MigrateState::Idle => choose_step(app, ui),
            MigrateState::Running => running_step(app, ui),
            MigrateState::Finished(rep) => report_step(app, ui, rep),
            MigrateState::Failed(t) => {
                Banner::new(Severity::Error, t.get()).show(ui);
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if primary_button(ui, pick("Resume", "Reanudar")).clicked()
                        && let Err(t) = app.import.hub.resume()
                    {
                        app.import.error = Some(t.get().to_string());
                    }
                    if ui.button(pick("Back", "Volver")).clicked() {
                        app.import.hub.reset();
                    }
                });
            }
        });
    if !open {
        // Cerrar no detiene una importación en curso (sigue en segundo plano).
        app.import.open = false;
    }
}

fn choose_step(app: &mut DeskApp, ui: &mut Ui) {
    let c = theme::colors(ui);
    ui.label(RichText::new(pick("1. What do you want to bring?", "1. ¿Qué quieres traer?")).strong().color(c.text));
    match app.import.hub.detected() {
        None => {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(12.0));
                ui.label(
                    RichText::new(pick("Looking on this computer…", "Buscando en este equipo…")).color(c.text_muted),
                );
            });
        }
        Some([]) => {
            ui.label(
                RichText::new(pick(
                    "No Thunderbird, Apple Mail or Outlook data found here. Choose a file or folder.",
                    "No se encontraron datos de Thunderbird, Apple Mail ni Outlook. Elige un archivo o carpeta.",
                ))
                .color(c.text_muted),
            );
        }
        Some(found) => {
            let found = found.to_vec();
            for s in found {
                let mut on = app.import.chosen.iter().any(|(p, _)| *p == s.path);
                let size = if s.bytes > 0 {
                    format!(" · {}", crate::widgets::human_size(usize::try_from(s.bytes).unwrap_or(usize::MAX)))
                } else {
                    String::new()
                };
                if ui
                    .checkbox(&mut on, format!("{}{size}", s.label))
                    .on_hover_text(s.path.display().to_string())
                    .changed()
                {
                    if on {
                        app.import.chosen.push((s.path.clone(), s.label.clone()));
                    } else {
                        app.import.chosen.retain(|(p, _)| *p != s.path);
                    }
                }
            }
        }
    }
    ui.horizontal(|ui| {
        let busy = app.import.picking.is_some();
        if ui.add_enabled(!busy, egui::Button::new(pick("Choose a file…", "Elegir un archivo…"))).clicked() {
            app.import.picking =
                Some(crate::suite_ui::pick_file(&app.session, pick("File to import", "Archivo para importar"), false));
        }
        if ui.add_enabled(!busy, egui::Button::new(pick("Choose a folder…", "Elegir una carpeta…"))).clicked() {
            app.import.picking =
                Some(crate::suite_ui::pick_file(&app.session, pick("Folder to import", "Carpeta para importar"), true));
        }
    });
    let detected: Vec<PathBuf> = app.import.hub.detected().unwrap_or(&[]).iter().map(|d| d.path.clone()).collect();
    let mut remove = None;
    for (k, (p, label)) in app.import.chosen.iter().enumerate() {
        if detected.contains(p) {
            continue;
        }
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
            icons::paint(ui.painter(), r, Icon::Folder, c.text_muted, c.accent);
            ui.label(RichText::new(label).color(c.text)).on_hover_text(p.display().to_string());
            if icons::icon_button(ui, Icon::Close, &format!("{} {label}", pick("Remove", "Quitar")), true).clicked() {
                remove = Some(k);
            }
        });
    }
    if let Some(k) = remove
        && k < app.import.chosen.len()
    {
        app.import.chosen.remove(k);
    }
    ui.add_space(8.0);
    ui.label(RichText::new(pick("2. Where should it go?", "2. ¿Dónde lo guardo?")).strong().color(c.text));
    ui.label(
        RichText::new(pick(
            "Mail goes to local accounts (one per source), not to your server. Name for loose files:",
            "El correo va a cuentas locales (una por origen), no a tu servidor. Nombre para archivos sueltos:",
        ))
        .color(c.text_muted),
    );
    let l = ui.label(RichText::new(pick("Local account", "Cuenta local")).color(c.text_muted));
    ui.add(
        egui::TextEdit::singleline(&mut app.import.account_name)
            .hint_text(pick("Imported", "Importado"))
            .desired_width(260.0),
    )
    .labelled_by(l.id);
    if let Some(e) = &app.import.error {
        ui.label(RichText::new(e).color(c.danger));
    }
    ui.add_space(8.0);
    let n = app.import.chosen.len();
    if primary_button(ui, &format!("{} ({n})", pick("Import", "Importar"))).clicked() {
        let paths = app.import.chosen.iter().map(|(p, _)| p.clone()).collect();
        let name = Some(app.import.account_name.clone());
        match app.import.hub.start(paths, name, true) {
            Ok(()) => app.import.error = None,
            Err(t) => app.import.error = Some(t.get().to_string()),
        }
    }
}

fn running_step(app: &mut DeskApp, ui: &mut Ui) {
    let c = theme::colors(ui);
    let p = app.import.hub.progress();
    ui.label(RichText::new(pick("Importing…", "Importando…")).strong().color(c.text));
    let bar = match p.fraction() {
        Some(f) => egui::ProgressBar::new(f).show_percentage(),
        None => egui::ProgressBar::new(0.0).animate(true),
    };
    // Ancho finito: con `INFINITY` en una ventana que aún se mide, egui calculaba un rect NaN y
    // entraba en pánico (la prueba `import_wizard_brings_a_small_mbox` fallaba a veces).
    let w = ui.available_width();
    ui.add(bar.desired_width(if w.is_finite() { w.clamp(120.0, 640.0) } else { 480.0 }));
    if !p.current.is_empty() {
        ui.add(egui::Label::new(RichText::new(&p.current).color(c.text_muted)).truncate());
    }
    let k = p.counts;
    ui.label(
        RichText::new(format!(
            "{} {} · {} {} · {} {} · {} {}",
            k.messages,
            pick("messages", "mensajes"),
            k.contacts,
            pick("contacts", "contactos"),
            k.events,
            pick("events", "eventos"),
            k.skipped,
            pick("skipped", "omitidos")
        ))
        .color(c.text),
    );
    ui.label(
        RichText::new(pick(
            "You can close this window: the import continues in the background.",
            "Puedes cerrar esta ventana: la importación sigue en segundo plano.",
        ))
        .color(c.text_muted)
        .size(theme::density(ui.ctx()).small_size()),
    );
    if ui.button(pick("Cancel", "Cancelar")).clicked() {
        app.import.hub.cancel();
    }
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
}

fn report_step(app: &mut DeskApp, ui: &mut Ui, rep: &iuredesk_engine::iuredesk_import::ImportReport) {
    let c = theme::colors(ui);
    let lang = lang_tag();
    let (sev, title) = if rep.cancelled {
        (
            Severity::Warning,
            pick("Import paused. What was imported is kept.", "Importación en pausa. Lo importado se conserva."),
        )
    } else {
        (Severity::Success, pick("Import finished", "Importación terminada"))
    };
    Banner::new(sev, title).show(ui);
    ui.add_space(6.0);
    ui.add(egui::Label::new(RichText::new(rep.summary(lang)).color(c.text)).wrap());
    if !rep.accounts.is_empty() {
        ui.label(RichText::new(format!("{} {}", pick("In:", "En:"), rep.accounts.join(", "))).color(c.text_muted));
    }
    if !rep.skipped_by_reason.is_empty() || !rep.notices.is_empty() {
        egui::CollapsingHeader::new(pick("Details", "Detalles")).id_salt("import-details").show(ui, |ui| {
            egui::ScrollArea::vertical().max_height(200.0).show(ui, |ui| {
                for s in rep.skipped.iter().take(50) {
                    ui.add(
                        egui::Label::new(
                            RichText::new(format!("• {} — {}", s.location, s.reason.human(lang))).color(c.text_muted),
                        )
                        .wrap(),
                    );
                }
                for n in rep.notices.iter().take(50) {
                    ui.add(egui::Label::new(RichText::new(format!("• {}", n.human(lang))).color(c.text)).wrap());
                }
            });
        });
    }
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if rep.cancelled && primary_button(ui, pick("Resume", "Reanudar")).clicked() {
            let _ = app.import.hub.resume();
        }
        if ui.button(pick("Done", "Listo")).clicked() {
            app.import.hub.reset();
            app.import.chosen.clear();
            app.import.open = false;
        }
    });
}

impl DeskApp {
    /// Asistente de importación con orígenes fijos (capturas y pruebas, sin buscar en el equipo).
    pub fn demo_import(
        &mut self,
        detected: Vec<iuredesk_engine::iuredesk_import::DetectedSource>,
        chosen: Vec<PathBuf>,
    ) {
        self.import.hub.set_detected(detected);
        self.import.open = true;
        for p in chosen {
            self.import.choose(p);
        }
    }

    /// Estado de la importación (pruebas).
    pub fn import_state(&self) -> MigrateState {
        self.import.hub.state().clone()
    }
}
