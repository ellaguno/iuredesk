//! Apps hermanas en la interfaz (F4.2): menú de los adjuntos («Abrir en IureEditor», «OCR con
//! IureOCR», «Guardar en Iurefficient»), «Leer en voz alta» y «Dictar» (IureVoz), transcribir la
//! grabación de una reunión (IureTranscribe → minuta del asistente), la invitación a instalar la
//! app que falta en lugar de un error, y Ajustes → «Suite» (apps detectadas y «Usar IureDesk
//! desde Claude / Copilot»).

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};

use egui::{Context, RichText, Ui};
use iuredesk_engine::iuredesk_integrations::{InstallHint, SisterApp};
use iuredesk_engine::suite::{SuiteAction, SuiteEvent, SuiteHub};
use iuredesk_engine::{Notice, Session};
use iuredesk_model::MessageId;
use iurefficient_ui::{BadgeKind, Icon, badge, icons, pick, primary_button, section_label, theme};

use crate::app::DeskApp;

/// Qué se eligió en el selector de archivos.
enum Picked {
    Transcript { title: String },
}

/// Estado de la suite en la interfaz.
pub struct SuiteUi {
    pub hub: SuiteHub,
    /// Invitación a instalar (diálogo).
    pub hint: Option<InstallHint>,
    ocr: Option<(String, String)>,
    picking: Option<(Picked, Receiver<Option<PathBuf>>)>,
    agents_asked: bool,
}

impl SuiteUi {
    pub fn new(session: &Session) -> Self {
        SuiteUi { hub: SuiteHub::new(session), hint: None, ocr: None, picking: None, agents_asked: false }
    }

    /// El diálogo de instalación está abierto.
    pub fn modal(&self) -> bool {
        self.hint.is_some()
    }
}

/// Selector de archivos en segundo plano (el portal puede tardar): el resultado llega por el canal.
pub(crate) fn pick_file(session: &Session, title: &str, folder: bool) -> Receiver<Option<PathBuf>> {
    let (tx, rx) = channel();
    let title = title.to_string();
    let waker = session.waker();
    session.runtime().spawn_blocking(move || {
        let d = rfd::FileDialog::new().set_title(&title);
        let p = if folder { d.pick_folder() } else { d.pick_file() };
        let _ = tx.send(p);
        waker();
    });
    rx
}

/// Cada cuadro: resultados de las apps hermanas y diálogos.
pub fn frame(app: &mut DeskApp, ctx: &Context) {
    app.suite.hub.poll();
    if let Some((_, rx)) = &app.suite.picking
        && let Ok(p) = rx.try_recv()
        && let Some((what, _)) = app.suite.picking.take()
        && let Some(path) = p
    {
        match what {
            Picked::Transcript { title } => {
                let title = if title.trim().is_empty() {
                    path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
                } else {
                    title
                };
                app.notice(Notice::info("Reading the recording…", "Leyendo la grabación…"));
                app.suite.hub.run(SuiteAction::Transcribe { file: path, title });
            }
        }
    }
    for ev in app.suite.hub.take_events() {
        match ev {
            SuiteEvent::Done(t) => {
                app.notice(Notice { kind: iuredesk_engine::NoticeKind::Success, text: t, undo: false, secs: None })
            }
            SuiteEvent::NotInstalled(h) => app.suite.hint = Some(h),
            SuiteEvent::NotAvailable(t) => {
                app.notice(Notice { kind: iuredesk_engine::NoticeKind::Info, text: t, undo: false, secs: Some(8) })
            }
            SuiteEvent::Failed(t) => app.notice(Notice::error(t)),
            SuiteEvent::Dictated { window, text } => {
                if text.trim().is_empty() {
                    app.notice(Notice::info("Nothing was dictated.", "No se dictó nada."));
                } else if let Some(w) = app.composers.iter_mut().find(|w| w.id == window) {
                    let mut doc = w.editor.doc.clone();
                    let at = w.editor.sel.1.min(doc.text.chars().count());
                    doc.insert(at, text.trim(), crate::rich_edit::CharStyle::default());
                    w.editor.set_doc(doc);
                }
            }
            SuiteEvent::Transcript { title, text } => {
                app.ai.set_transcript(&title, &text);
                app.session.module = iuredesk_engine::Module::Assistant;
                app.notice(Notice::success(
                    "Transcript ready: review it and write the minutes.",
                    "Transcripción lista: revísala y redacta la minuta.",
                ));
            }
            SuiteEvent::OcrText { name, text } => app.suite.ocr = Some((name, text)),
        }
    }
    hint_dialog(app, ctx);
    ocr_window(app, ctx);
}

fn hint_dialog(app: &mut DeskApp, ctx: &Context) {
    let Some(h) = app.suite.hint.clone() else { return };
    let es = iuredesk_engine::lang::spanish();
    let mut close = false;
    let resp = egui::Modal::new(egui::Id::unique("suite-hint")).show(ctx, |ui| {
        ui.set_width(420.0);
        let c = theme::colors(ui);
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::hover());
            icons::paint(ui.painter(), r, Icon::Info, c.accent, c.accent);
            ui.label(
                RichText::new(format!("{} {}", h.app.name(), pick("is not installed", "no está instalada")))
                    .font(theme::bold_font(ui.ctx(), 17.0))
                    .color(c.text),
            );
        });
        ui.add_space(6.0);
        ui.add(egui::Label::new(RichText::new(h.text.get(es)).color(c.text)).wrap());
        ui.add(egui::Label::new(RichText::new(h.app.description().get(es)).color(c.text_muted)).wrap());
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if primary_button(ui, &format!("{} {}", pick("Get", "Descargar"), h.app.name())).clicked() {
                crate::open_url(&h.download_url);
                close = true;
            }
            if ui.button(pick("Not now", "Ahora no")).clicked() {
                close = true;
            }
        });
    });
    if close || resp.should_close() {
        app.suite.hint = None;
    }
}

fn ocr_window(app: &mut DeskApp, ctx: &Context) {
    let Some((name, text)) = app.suite.ocr.clone() else { return };
    let mut open = true;
    egui::Window::new(format!("{} — {name}", pick("Recognized text", "Texto reconocido")))
        .id(egui::Id::unique("suite-ocr"))
        .open(&mut open)
        .default_size([520.0, 420.0])
        .collapsible(false)
        .show(ctx, |ui| {
            if ui.button(pick("Copy", "Copiar")).clicked() {
                ui.ctx().copy_text(text.clone());
            }
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.add(egui::Label::new(RichText::new(&text).color(theme::colors(ui).text)).selectable(true).wrap());
            });
        });
    if !open {
        app.suite.ocr = None;
    }
}

/// Menú de un adjunto (clic o clic derecho en la ficha).
pub fn attachment_menu(app: &mut DeskApp, resp: &egui::Response, msg: MessageId, name: &str) {
    let mut act: Option<SuiteAction> = None;
    let lower = name.to_ascii_lowercase();
    let ocr_ok = [".pdf", ".png", ".jpg", ".jpeg", ".tif", ".tiff"].iter().any(|e| lower.ends_with(e));
    let items = |ui: &mut Ui, act: &mut Option<SuiteAction>| {
        if icons::menu_item(ui, Some(Icon::Notes), pick("Open in IureEditor", "Abrir en IureEditor")).clicked() {
            *act = Some(SuiteAction::OpenAttachment { msg, name: name.to_string() });
            ui.close();
        }
        if ocr_ok && icons::menu_item(ui, Some(Icon::Search), pick("OCR with IureOCR", "OCR con IureOCR")).clicked() {
            *act = Some(SuiteAction::OcrAttachment { msg, name: name.to_string() });
            ui.close();
        }
        if icons::menu_item(
            ui,
            Some(Icon::Folder),
            pick("Save in Iurefficient (IureDav)", "Guardar en Iurefficient (IureDav)"),
        )
        .clicked()
        {
            *act = Some(SuiteAction::SaveAttachment { msg, name: name.to_string() });
            ui.close();
        }
    };
    egui::Popup::menu(resp).show(|ui| items(ui, &mut act));
    resp.context_menu(|ui| items(ui, &mut act));
    if let Some(a) = act {
        app.suite.hub.run(a);
    }
}

/// «Leer en voz alta» del mensaje enfocado.
pub fn read_aloud(app: &mut DeskApp) {
    let Some(id) = app.session.focused() else { return };
    let text = match app.session.body(id) {
        Some(b) => iuredesk_engine::body::split_quoted(&b.text).0.to_string(),
        None => app.session.envelope(id).map(|e| e.preview.clone()).unwrap_or_default(),
    };
    let subject = app.session.envelope(id).map(|e| e.subject.clone()).unwrap_or_default();
    app.suite.hub.run(SuiteAction::Speak(format!("{subject}. {text}")));
}

/// «Cargar grabación o transcripción…»: archivo → IureTranscribe (o lectura directa del texto).
pub fn pick_transcript(app: &mut DeskApp, title: &str) {
    if app.suite.picking.is_some() {
        return;
    }
    let rx = pick_file(&app.session, pick("Recording or transcript", "Grabación o transcripción"), false);
    app.suite.picking = Some((Picked::Transcript { title: title.to_string() }, rx));
}

/// Acciones de la suite en un evento (editor de eventos): grabar/transcribir la reunión.
pub fn event_actions(app: &mut DeskApp, ui: &mut Ui, title: &str) {
    ui.horizontal(|ui| {
        if ui
            .button(pick("Record / transcribe meeting…", "Grabar/transcribir reunión…"))
            .on_hover_text(pick(
                "Record with IureTranscribe, then load the recording to write the minutes with the assistant",
                "Graba con IureTranscribe y luego carga la grabación para redactar la minuta con el asistente",
            ))
            .clicked()
        {
            if app.suite.hub.installed(SisterApp::Transcribe) {
                app.suite.hub.run(SuiteAction::Launch(SisterApp::Transcribe));
                pick_transcript(app, title);
            } else {
                app.suite.hint = Some(SisterApp::Transcribe.install_hint());
            }
        }
    });
}

/// Ajustes → «Suite».
pub fn settings_section(app: &mut DeskApp, ui: &mut Ui) {
    let c = theme::colors(ui);
    let es = iuredesk_engine::lang::spanish();
    section_label(ui, pick("Iurefficient suite", "Suite Iurefficient"));
    if !app.suite.agents_asked {
        app.suite.agents_asked = true;
        app.suite.hub.run(SuiteAction::RefreshAgents);
    }
    match app.suite.hub.suite().map(|s| s.apps()) {
        None => {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(12.0));
                ui.label(RichText::new(pick("Looking for the apps…", "Buscando las apps…")).color(c.text_muted));
            });
        }
        Some(apps) => {
            for a in apps {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(a.app.name()).strong().color(c.text));
                    if a.path.is_some() {
                        badge(ui, pick("Installed", "Instalada"), BadgeKind::Success);
                        if ui.small_button(format!("{} {}", pick("Open", "Abrir"), a.app.name())).clicked() {
                            app.suite.hub.run(SuiteAction::Launch(a.app));
                        }
                    } else {
                        badge(ui, pick("Not installed", "No instalada"), BadgeKind::Neutral);
                        if let Some(h) = &a.hint
                            && ui.small_button(format!("{} {}", pick("Get", "Descargar"), a.app.name())).clicked()
                        {
                            crate::open_url(&h.download_url);
                        }
                    }
                });
                ui.label(
                    RichText::new(a.app.description().get(es))
                        .color(c.text_muted)
                        .size(theme::density(ui.ctx()).small_size()),
                );
            }
        }
    }
    ui.add_space(8.0);
    ui.label(
        RichText::new(pick("AI assistants on this computer", "Asistentes de IA de este equipo")).strong().color(c.text),
    );
    ui.label(
        RichText::new(pick(
            "Let Claude Desktop or GitHub Copilot (VS Code) search your mail, read threads, see your agenda and create drafts — never send.",
            "Deja que Claude Desktop o GitHub Copilot (VS Code) busquen en tu correo, lean hilos, vean tu agenda y creen borradores — nunca envían.",
        ))
        .color(c.text_muted),
    );
    let st = app.suite.hub.agents().cloned();
    if let Some(st) = &st {
        for (name, s) in [("Claude Desktop", &st.claude_desktop), ("VS Code", &st.vscode)] {
            ui.horizontal(|ui| {
                ui.label(RichText::new(name).color(c.text));
                let (t, k) = if !s.installed {
                    (pick("Not installed", "No instalado"), BadgeKind::Neutral)
                } else if s.stale {
                    (pick("Needs update", "Hay que actualizar"), BadgeKind::Warning)
                } else if s.connected {
                    (pick("Connected", "Conectado"), BadgeKind::Success)
                } else {
                    (pick("Not connected", "Sin conectar"), BadgeKind::Neutral)
                };
                badge(ui, t, k);
                if !s.installed && ui.small_button(format!("{} {name}", pick("Get", "Descargar"))).clicked() {
                    crate::open_url(s.download_url);
                }
            });
        }
    }
    ui.horizontal(|ui| {
        let stale = st.as_ref().is_some_and(|s| s.claude_desktop.stale || s.vscode.stale);
        let label = if stale {
            pick("Update connection", "Actualizar conexión")
        } else {
            pick("Use IureDesk from Claude / Copilot", "Usar IureDesk desde Claude / Copilot")
        };
        if ui.add_enabled(!app.suite.hub.busy(), egui::Button::new(label)).clicked() {
            app.suite.hub.run(SuiteAction::RegisterAgents);
        }
        if st.as_ref().is_some_and(|s| s.claude_desktop.connected)
            && ui.button(pick("Disconnect", "Desconectar")).clicked()
        {
            app.suite.hub.run(SuiteAction::UnregisterAgents);
        }
    });
    ui.add_space(14.0);
}

impl DeskApp {
    /// Usa esta suite (pruebas, capturas).
    pub fn set_suite(&mut self, suite: iuredesk_engine::iuredesk_integrations::Suite) {
        let hub = std::mem::replace(&mut self.suite.hub, SuiteHub::new(&self.session));
        self.suite.hub = hub.with_suite(suite);
    }
}
