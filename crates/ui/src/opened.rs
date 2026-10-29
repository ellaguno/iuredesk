//! R1.2: lo que abre el sistema (archivos, `mailto:` y enlaces `iuredesk://`), al arrancar o
//! reenviado por otra instancia. Cada archivo se muestra en su ventana, de solo lectura:
//! - `.eml`: cabecera, adjuntos y texto; «Importar a…» (asistente de importación) y, si trae una
//!   invitación, «Agregar a mi calendario».
//! - `.ics`: la invitación (Aceptar / Tal vez / Rechazar) o los eventos (Agregar a mi calendario).
//! - `.vcf`: los contactos (Agregar a contactos).

use std::path::PathBuf;

use egui::{Context, RichText, Ui};
use iuredesk_engine::open::{OpenTarget, Opened, parse_target};
use iuredesk_engine::session::FileOpened;
use iuredesk_engine::{JobId, Notice, Outcome, Text};
use iuredesk_model::PartStat;
use iurefficient_ui::{pick, primary_button, theme};

use crate::app::DeskApp;

/// Ventanas abiertas y lecturas en curso.
#[derive(Default)]
pub struct OpenedUi {
    windows: Vec<(u64, Opened)>,
    next: u64,
    jobs: Vec<JobId>,
}

impl OpenedUi {
    /// Ventanas de archivos abiertas (pruebas).
    pub fn count(&self) -> usize {
        self.windows.len()
    }

    /// Lecturas de archivos en curso (pruebas).
    pub fn pending(&self) -> usize {
        self.jobs.len()
    }
}

/// Atiende los argumentos (ya con rutas absolutas). Devuelve cuántos se entendieron.
pub(crate) fn handle(app: &mut DeskApp, ctx: &Context, args: &[String]) -> usize {
    let mut n = 0;
    for a in args.iter().take(iuredesk_engine::iuredesk_mcp::bridge::MAX_OPEN_ARGS) {
        let Some(t) = parse_target(a, None) else { continue };
        n += 1;
        match t {
            OpenTarget::Mailto(m) => match app.session.compose_mailto(&m) {
                Some(d) => app.open_composer(d),
                None => app.notice(Notice::error(Text::new(
                    "That email link could not be read.",
                    "No se pudo leer ese enlace de correo.",
                ))),
            },
            OpenTarget::File { path, kind } => {
                let job = app.session.open_file(path, kind);
                app.opened.jobs.push(job);
            }
            OpenTarget::Message(id) => {
                if !app.session.open_message_id(&id) {
                    app.notice(Notice::info(
                        "That message is not in IureDesk (it may not have synced yet).",
                        "Ese mensaje no está en IureDesk (quizá aún no se sincronizó).",
                    ));
                }
            }
            OpenTarget::Event(uid) => match app.session.occurrence_by_uid(&uid) {
                Some(o) => {
                    app.session.cal.anchor = Some(app.session.zone().local(o.start).date());
                    app.session.module = iuredesk_engine::Module::Calendar;
                    crate::event_editor::open_occurrence(app, &o);
                }
                None => app.notice(Notice::info(
                    "That event is not in your calendar.",
                    "Ese evento no está en tu calendario.",
                )),
            },
        }
    }
    ctx.request_repaint();
    n
}

/// Llegó la lectura de un archivo.
pub(crate) fn on_loaded(app: &mut DeskApp, id: JobId, done: FileOpened) {
    app.opened.jobs.retain(|j| *j != id);
    match done.result {
        Ok(o) => {
            // El mismo archivo dos veces: se reemplaza la ventana.
            app.opened.windows.retain(|(_, w)| w.path() != o.path());
            let n = app.opened.next;
            app.opened.next = n.wrapping_add(1);
            app.opened.windows.push((n, o));
        }
        Err(e) => app.notice(Notice::error(e)),
    }
}

enum Act {
    Close,
    Import(PathBuf),
    AddEvents,
    Rsvp(PartStat),
    AddContacts,
}

/// Dibuja las ventanas.
pub(crate) fn frame(app: &mut DeskApp, ctx: &Context) {
    if app.opened.windows.is_empty() {
        return;
    }
    let mut acts: Vec<(u64, Act)> = Vec::new();
    let windows = std::mem::take(&mut app.opened.windows);
    for (n, o) in &windows {
        let title = o.path().file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        let mut open = true;
        egui::Window::new(title)
            .id(egui::Id::unique(("opened-file", *n)))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(600.0)
            .max_height(ctx.content_rect().height() - 80.0)
            .show(ctx, |ui| {
                if let Some(a) = body(app, ui, o) {
                    acts.push((*n, a));
                }
            });
        if !open {
            acts.push((*n, Act::Close));
        }
    }
    app.opened.windows = windows;
    for (n, a) in acts {
        let Some(pos) = app.opened.windows.iter().position(|(k, _)| *k == n) else { continue };
        let r: Option<iuredesk_engine::Result<Outcome>> = match (&a, app.opened.windows.get(pos).map(|w| &w.1)) {
            (Act::Close, _) => None,
            (Act::Import(p), _) => {
                app.import.choose(p.clone());
                app.import.open();
                None
            }
            (Act::AddEvents, Some(Opened::Calendar(c))) => Some(app.session.add_file_events(&c.events)),
            (Act::AddEvents, Some(Opened::Message(m))) => {
                m.invitation.as_ref().map(|i| app.session.add_file_events(std::slice::from_ref(&i.event)))
            }
            (Act::Rsvp(p), Some(Opened::Calendar(c))) => {
                c.invitation.as_ref().map(|i| app.session.respond_file_invitation(i, p.clone()))
            }
            (Act::Rsvp(p), Some(Opened::Message(m))) => {
                m.invitation.as_ref().map(|i| app.session.respond_file_invitation(i, p.clone()))
            }
            (Act::AddContacts, Some(Opened::Contacts(c))) => Some(app.session.add_file_contacts(&c.contacts)),
            _ => None,
        };
        let close = match r {
            None => matches!(a, Act::Close | Act::Import(_)),
            Some(Ok(out)) => {
                app.outcome(ctx, out);
                true
            }
            Some(Err(e)) => {
                app.notice(Notice::error(e.text()));
                false
            }
        };
        if close {
            app.opened.windows.remove(pos);
        }
    }
}

fn field(ui: &mut Ui, label: &str, value: &str) {
    if value.is_empty() {
        return;
    }
    let c = theme::colors(ui);
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(label).color(c.text_muted));
        ui.label(RichText::new(value).color(c.text));
    });
}

fn people(list: &[iuredesk_model::Mailbox]) -> String {
    let mut v: Vec<String> = list
        .iter()
        .take(12)
        .map(|m| {
            if m.name.as_deref().is_some_and(|n| !n.is_empty()) {
                format!("{} <{}>", m.display(), m.addr)
            } else {
                m.addr.clone()
            }
        })
        .collect();
    if list.len() > 12 {
        v.push(format!("+{}", list.len() - 12));
    }
    v.join(", ")
}

fn import_button(ui: &mut Ui, path: &std::path::Path) -> Option<Act> {
    ui.button(pick("Import to…", "Importar a…"))
        .on_hover_text(pick(
            "Copy it into IureDesk with the import assistant",
            "Copiarlo a IureDesk con el asistente de importación",
        ))
        .clicked()
        .then(|| Act::Import(path.to_path_buf()))
}

fn invitation_block(app: &DeskApp, ui: &mut Ui, inv: &iuredesk_engine::calendar::invite::Invitation) -> Option<Act> {
    use iuredesk_engine::calendar::invite::InviteKind;
    let c = theme::colors(ui);
    let mut act = None;
    egui::Frame::new().fill(c.surface).corner_radius(8.0).inner_margin(10.0).show(ui, |ui| {
        ui.label(RichText::new(&inv.event.summary).font(theme::bold_font(ui.ctx(), 15.0)).color(c.text));
        field(ui, pick("When:", "Cuándo:"), &crate::invitation::when(app, &inv.event.start, &inv.event.end));
        field(ui, pick("Where:", "Dónde:"), &inv.event.location);
        if let Some(o) = &inv.organizer {
            field(ui, pick("Organizer:", "Organiza:"), o.display());
        }
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| match inv.kind {
            InviteKind::Request if inv.me.is_some() => {
                if primary_button(ui, pick("Accept", "Aceptar")).clicked() {
                    act = Some(Act::Rsvp(PartStat::Accepted));
                }
                if ui.button(pick("Maybe", "Tal vez")).clicked() {
                    act = Some(Act::Rsvp(PartStat::Tentative));
                }
                if ui.button(pick("Decline", "Rechazar")).clicked() {
                    act = Some(Act::Rsvp(PartStat::Declined));
                }
                if ui.button(pick("Add without replying", "Agregar sin responder")).clicked() {
                    act = Some(Act::AddEvents);
                }
            }
            InviteKind::Cancel | InviteKind::Reply | InviteKind::Counter => {
                ui.label(
                    RichText::new(pick(
                        "Open the email that brought it to act on it.",
                        "Abre el correo que la trajo para atenderla.",
                    ))
                    .color(c.text_muted),
                );
            }
            _ => {
                if primary_button(ui, pick("Add to my calendar", "Agregar a mi calendario")).clicked() {
                    act = Some(Act::AddEvents);
                }
            }
        });
    });
    act
}

fn body(app: &DeskApp, ui: &mut Ui, o: &Opened) -> Option<Act> {
    let c = theme::colors(ui);
    let mut act = None;
    match o {
        Opened::Message(m) => {
            let subject = if m.subject.trim().is_empty() { pick("(No subject)", "(Sin asunto)") } else { &m.subject };
            ui.label(RichText::new(subject).font(theme::bold_font(ui.ctx(), 17.0)).color(c.text));
            field(ui, pick("From:", "De:"), &people(&m.from));
            field(ui, pick("To:", "Para:"), &people(&m.to));
            field(ui, pick("Cc:", "CC:"), &people(&m.cc));
            let date = app.session.zone().local(m.date);
            field(ui, pick("Date:", "Fecha:"), &date.format("%Y-%m-%d %H:%M").to_string());
            if !m.body.attachments.is_empty() {
                let names: Vec<String> = m
                    .body
                    .attachments
                    .iter()
                    .take(20)
                    .map(|a| format!("{} ({} KB)", a.name, a.size.div_ceil(1024)))
                    .collect();
                field(ui, pick("Attachments:", "Adjuntos:"), &names.join(" · "));
            }
            if let Some(inv) = &m.invitation {
                ui.add_space(6.0);
                act = invitation_block(app, ui, inv).or(act);
            }
            ui.separator();
            egui::ScrollArea::vertical().max_height(420.0).auto_shrink([false, true]).show(ui, |ui| {
                ui.add(egui::Label::new(RichText::new(&m.body.text).color(c.text)).wrap().selectable(true));
            });
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(pick(
                        "Read-only: this file is not in your mailbox.",
                        "Solo lectura: este archivo no está en tu buzón.",
                    ))
                    .color(c.text_muted),
                );
                act = import_button(ui, &m.path).or(act.take());
            });
        }
        Opened::Calendar(cal) => {
            if let Some(inv) = &cal.invitation {
                act = invitation_block(app, ui, inv);
            } else {
                ui.label(
                    RichText::new(format!("{} {}", cal.total, pick("events", "eventos")))
                        .font(theme::bold_font(ui.ctx(), 15.0))
                        .color(c.text),
                );
                egui::ScrollArea::vertical().max_height(360.0).auto_shrink([false, true]).show(ui, |ui| {
                    for e in cal.events.iter().take(100) {
                        ui.horizontal_wrapped(|ui| {
                            let when = crate::invitation::when(app, &e.start, &e.end);
                            ui.label(RichText::new(when).color(c.text_muted));
                            ui.label(RichText::new(&e.summary).color(c.text));
                        });
                    }
                });
                let shown = cal.events.len();
                ui.horizontal(|ui| {
                    let label = if shown < cal.total {
                        format!("{} ({shown})", pick("Add to my calendar", "Agregar a mi calendario"))
                    } else {
                        pick("Add to my calendar", "Agregar a mi calendario").to_string()
                    };
                    if primary_button(ui, &label).clicked() {
                        act = Some(Act::AddEvents);
                    }
                    act = import_button(ui, &cal.path).or(act.take());
                });
            }
        }
        Opened::Contacts(list) => {
            ui.label(
                RichText::new(format!(
                    "{} {}",
                    list.total,
                    if list.total == 1 { pick("contact", "contacto") } else { pick("contacts", "contactos") }
                ))
                .font(theme::bold_font(ui.ctx(), 15.0))
                .color(c.text),
            );
            egui::ScrollArea::vertical().max_height(360.0).auto_shrink([false, true]).show(ui, |ui| {
                for k in list.contacts.iter().take(100) {
                    let name = iuredesk_engine::session::contact_display_name(k)
                        .unwrap_or_else(|| pick("(No name)", "(Sin nombre)").to_string());
                    let mail = k.emails.first().map(|e| e.value.clone()).unwrap_or_default();
                    let phone = k.phones.first().map(|p| p.value.clone()).unwrap_or_default();
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(name).color(c.text));
                        for x in [mail, phone, k.org.clone()] {
                            if !x.is_empty() {
                                ui.label(RichText::new(x).color(c.text_muted));
                            }
                        }
                    });
                }
            });
            ui.horizontal(|ui| {
                if primary_button(ui, pick("Add to contacts", "Agregar a contactos")).clicked() {
                    act = Some(Act::AddContacts);
                }
                act = import_button(ui, &list.path).or(act.take());
            });
        }
    }
    act
}
