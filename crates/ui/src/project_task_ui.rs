//! «Correo → tarea del plan»: del correo abierto, una tarea en un plan de IureProject sin abrir
//! IureProject. El formulario sale lleno con el asunto, de quién y cuándo, un extracto y el enlace
//! `iuredesk://message/…` (que IureProject abre como hipervínculo de la tarea); el usuario elige
//! el plan (los que sigue en «Mi día», el último usado primero, u otro archivo), la fecha límite y
//! el responsable. Se agrega por MCP (`task_add_list`) en segundo plano.

use std::path::PathBuf;
use std::sync::mpsc::Receiver;

use egui::{Context, RichText};
use iuredesk_engine::Notice;
use iuredesk_engine::iuredesk_integrations::{NewProjectTask, SisterApp};
use iuredesk_engine::suite::SuiteAction;
use iurefficient_ui::{pick, theme};

use crate::app::DeskApp;

/// Caracteres del extracto del correo en las notas.
const EXCERPT: usize = 600;

/// El formulario abierto.
pub struct Form {
    pub(crate) plan: Option<String>,
    pub(crate) name: String,
    pub(crate) deadline: String,
    pub(crate) resource: String,
    pub(crate) notes: String,
    link: Option<(String, String)>,
    picking: Option<Receiver<Option<PathBuf>>>,
}

/// Abre el formulario con el correo enfocado (o avisa por qué no).
pub fn open(app: &mut DeskApp) {
    if !app.suite.hub.installed(SisterApp::Project) {
        app.suite.hint = Some(SisterApp::Project.install_hint());
        return;
    }
    let Some(id) = app.session.focused() else {
        app.notice(Notice::info("Open a message first.", "Primero abre un correo."));
        return;
    };
    let zone = app.session.zone();
    let Some(env) = app.session.envelope(id).cloned() else { return };
    let from = env.from.first().map_or_else(String::new, |m| match &m.name {
        Some(n) if !n.trim().is_empty() => format!("{} <{}>", n.trim(), m.addr),
        _ => m.addr.clone(),
    });
    let when = zone.local(env.date).format("%Y-%m-%d %H:%M").to_string();
    let subject = if env.subject.trim().is_empty() {
        pick("(no subject)", "(sin asunto)").to_string()
    } else {
        env.subject.trim().to_string()
    };
    // La tarea, sin «RE:» ni «RV:»; las notas llevan el asunto tal cual.
    let (base, _) = iuredesk_model::normalize_subject(&subject);
    let task_name = if base.trim().is_empty() { subject.clone() } else { base };
    let excerpt = excerpt(&env.preview);
    let mut notes = format!(
        "**{}** {from}  \n**{}** {when}  \n**{}** {subject}",
        pick("From:", "De:"),
        pick("Date:", "Fecha:"),
        pick("Subject:", "Asunto:")
    );
    if !excerpt.trim().is_empty() {
        notes.push_str("\n\n> ");
        notes.push_str(&excerpt.trim().replace('\n', "\n> "));
    }
    let link = env
        .message_id
        .as_deref()
        .filter(|m| !m.trim().is_empty())
        .map(|m| (iuredesk_engine::open::message_link(m), format!("{} {subject}", pick("Email:", "Correo:"))));
    // El plan: el último usado o el primero que se sigue.
    let followed = &app.session.prefs.projects;
    let plan = app.suite.last_plan.clone().filter(|p| followed.contains(p)).or_else(|| followed.first().cloned());
    app.suite.project_task = Some(Form {
        plan,
        name: task_name,
        deadline: String::new(),
        resource: String::new(),
        notes,
        link,
        picking: None,
    });
}

/// El extracto del correo sin la historia citada («El 15/10/2026 03:29, Laura escribió: > …»).
fn excerpt(preview: &str) -> String {
    const WROTE: [&str; 5] = ["escribió:", "escribio:", "wrote:", "-----Original", "________________"];
    let mut cut = preview.len();
    for w in WROTE {
        if let Some(i) = preview.find(w) {
            // La atribución empieza en el «El »/«On » anterior (si está cerca).
            let head = preview.get(..i).unwrap_or_default();
            let start = ["El ", "On ", "De: ", "From: "]
                .iter()
                .filter_map(|p| head.rfind(p))
                .max()
                .filter(|&s| i - s <= 120)
                .unwrap_or(i);
            cut = cut.min(start);
        }
    }
    if let Some(i) = preview.find(" > ") {
        cut = cut.min(i);
    }
    let text = preview.get(..cut).unwrap_or(preview).trim();
    let mut out: String = text.chars().take(EXCERPT).collect();
    if text.chars().count() > EXCERPT {
        out.push('…');
    }
    out
}

/// Nombre corto de un plan (su archivo sin extensión).
fn plan_label(path: &str) -> String {
    std::path::Path::new(path).file_stem().map_or_else(|| path.to_string(), |s| s.to_string_lossy().into_owned())
}

/// ¿Una fecha `AAAA-MM-DD` válida (o vacía)?
fn valid_date(s: &str) -> bool {
    let s = s.trim();
    s.is_empty() || chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok()
}

/// La ventana, cada cuadro.
pub fn window(app: &mut DeskApp, ctx: &Context) {
    let Some(mut form) = app.suite.project_task.take() else { return };
    if let Some(rx) = &form.picking
        && let Ok(choice) = rx.try_recv()
    {
        form.picking = None;
        if let Some(p) = choice {
            form.plan = Some(p.to_string_lossy().into_owned());
        }
    }
    let followed = app.session.prefs.projects.clone();
    let mut keep = true;
    let mut submit = false;
    let mut pick_other = false;
    egui::Window::new(pick("New task in a project", "Tarea nueva en un proyecto"))
        .id(egui::Id::unique("project-task"))
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center())
        .collapsible(false)
        .resizable(true)
        .default_width(480.0)
        .show(ctx, |ui| {
            let c = theme::colors(ui);
            egui::Grid::new("project-task-grid").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                let plan_l = ui.label(pick("Plan", "Plan"));
                ui.horizontal(|ui| {
                    let current =
                        form.plan.as_deref().map_or_else(|| pick("Choose…", "Elegir…").to_string(), plan_label);
                    egui::ComboBox::from_id_salt("project-task-plan")
                        .selected_text(current)
                        .width(260.0)
                        .show_ui(ui, |ui| {
                            for p in &followed {
                                if ui.selectable_label(form.plan.as_deref() == Some(p), plan_label(p)).clicked() {
                                    form.plan = Some(p.clone());
                                }
                            }
                        })
                        .response
                        .labelled_by(plan_l.id);
                    if ui.button(pick("Other plan…", "Otro plan…")).clicked() {
                        pick_other = true;
                    }
                });
                ui.end_row();
                let l = ui.label(pick("Task", "Tarea"));
                ui.add(egui::TextEdit::singleline(&mut form.name).desired_width(320.0)).labelled_by(l.id);
                ui.end_row();
                let l = ui.label(pick("Due", "Fecha límite"));
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut form.deadline).hint_text("AAAA-MM-DD").desired_width(110.0))
                        .labelled_by(l.id);
                    if !valid_date(&form.deadline) {
                        ui.label(RichText::new(pick("Use YYYY-MM-DD", "Usa AAAA-MM-DD")).color(c.danger));
                    }
                });
                ui.end_row();
                let l = ui.label(pick("Owner", "Responsable"));
                ui.add(
                    egui::TextEdit::singleline(&mut form.resource)
                        .hint_text(pick("optional", "opcional"))
                        .desired_width(220.0),
                )
                .labelled_by(l.id);
                ui.end_row();
            });
            ui.add_space(4.0);
            let l = ui.label(RichText::new(pick("Notes", "Notas")).color(c.text_muted));
            ui.add(egui::TextEdit::multiline(&mut form.notes).desired_rows(6).desired_width(f32::INFINITY))
                .labelled_by(l.id);
            if form.link.is_some() {
                ui.label(
                    RichText::new(pick(
                        "The task links back to this email.",
                        "La tarea lleva un enlace a este correo.",
                    ))
                    .color(c.text_muted)
                    .small(),
                );
            }
            ui.add_space(6.0);
            let ready = form.plan.is_some() && !form.name.trim().is_empty() && valid_date(&form.deadline);
            ui.horizontal(|ui| {
                let create = ui.add_enabled_ui(ready, |ui| {
                    iurefficient_ui::primary_button(ui, pick("Create task", "Crear tarea"))
                });
                if create.inner.clicked() {
                    submit = true;
                }
                if ui.button(pick("Cancel", "Cancelar")).clicked() {
                    keep = false;
                }
            });
        });
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        keep = false;
    }
    if pick_other && form.picking.is_none() {
        form.picking = Some(crate::suite_ui::pick_file(
            &app.session,
            pick("Choose an IureProject plan", "Elige un plan de IureProject"),
            false,
        ));
    }
    if submit && let Some(plan) = form.plan.clone() {
        let deadline = Some(form.deadline.trim().to_string()).filter(|d| !d.is_empty());
        let resource = Some(form.resource.trim().to_string()).filter(|r| !r.is_empty());
        let task = NewProjectTask {
            name: form.name.trim().to_string(),
            notes: form.notes.clone(),
            deadline,
            resource,
            link: form.link.clone(),
        };
        app.suite.last_plan = Some(plan.clone());
        app.notice(Notice::info("Adding the task…", "Agregando la tarea…"));
        app.suite.hub.run(SuiteAction::AddProjectTask { file: PathBuf::from(plan), task });
        return;
    }
    if keep {
        app.suite.project_task = Some(form);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_and_labels() {
        assert!(valid_date("") && valid_date(" 2026-11-25 "));
        assert!(!valid_date("25/11/2026") && !valid_date("2026-02-30"));
        assert_eq!(plan_label("/casos/Caso Ramírez.iurp"), "Caso Ramírez");
    }

    #[test]
    fn the_excerpt_leaves_the_quoted_history_out() {
        let p = "De acuerdo, nos vemos entonces. El 15/10/2026 03:29, Laura Mendez escribió: > Great, I'll loop";
        assert_eq!(excerpt(p), "De acuerdo, nos vemos entonces.");
        assert_eq!(excerpt("Sounds good. On Tue, Ana wrote: hi"), "Sounds good.");
        assert_eq!(excerpt("Sin historia."), "Sin historia.");
        assert!(excerpt(&"x".repeat(2_000)).ends_with('…'));
    }
}
