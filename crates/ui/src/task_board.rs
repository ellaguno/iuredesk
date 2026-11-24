//! S1.34: las tareas como tablero (columnas por estado; se arrastran entre ellas, con deshacer) y
//! agrupadas por su caso de Iurefficient (con el plan de IureProject de ese caso, si se sigue).
//!
//! El tablero muestra la lista elegida: «Por hacer», «En curso» y «Terminadas»; las canceladas van
//! plegadas debajo, como las terminadas en la lista. Con el teclado: Alt+← / Alt+→ mueve la tarea
//! elegida de columna; el menú contextual tiene «Mover a…».

use std::collections::HashMap;

use egui::{RichText, Sense, Ui, vec2};
use iuredesk_model::{Task, TaskId, TaskStatus};
use iurefficient_ui::{BadgeKind, EmptyState, Icon, badge, icons, pick, theme};

use crate::app::DeskApp;
use crate::pim::RowAct;

/// Lo que se pidió en el tablero.
pub(crate) enum BoardAct {
    Row(TaskId, RowAct),
    /// Pasar la tarea a otro estado (arrastrarla, «Mover a…», Alt+flecha).
    Status(TaskId, TaskStatus),
    OpenProject(std::path::PathBuf),
}

/// S1.35: dónde se puede abrir una tarea fuera de IureDesk: su página en Iurefficient y el plan de
/// IureProject de su caso (si se sigue), con su id en Iurefficient.
pub(crate) struct Origin {
    pub web: Option<String>,
    pub plan: Option<(std::path::PathBuf, String)>,
}

pub(crate) fn origin(app: &DeskApp, t: &Task) -> Origin {
    let web = app.session.iure_task_url(t);
    let plan = app.session.iure_task_id(t).and_then(|id| {
        let case = app.session.task_case(t)?;
        let (file, _) = crate::today_projects::plans_for_case(app, &case.id).into_iter().next()?;
        Some((file, id))
    });
    Origin { web, plan }
}

/// Las opciones «Abrir en Iurefficient» y «Abrir en IureProject» del menú de una tarea.
pub(crate) fn origin_menu(app: &DeskApp, ui: &mut Ui, t: &Task) -> Option<RowAct> {
    let o = origin(app, t);
    let mut act = None;
    if o.web.is_some()
        && icons::menu_item(ui, Some(Icon::Cloud), pick("Open in Iurefficient", "Abrir en Iurefficient")).clicked()
    {
        act = Some(RowAct::OpenIure);
        ui.close();
    }
    if o.plan.is_some()
        && icons::menu_item(ui, Some(Icon::Gantt), pick("Open in IureProject", "Abrir en IureProject")).clicked()
    {
        act = Some(RowAct::OpenProject);
        ui.close();
    }
    act
}

/// Abre la tarea en Iurefficient (`project` = false) o en su plan de IureProject; si no se puede,
/// lo explica.
pub(crate) fn open_origin(app: &mut DeskApp, id: TaskId, project: bool) {
    let Some(t) = app.session.task(id) else { return };
    let o = origin(app, &t);
    match (project, o.web, o.plan) {
        (false, Some(url), _) => {
            crate::open_url(&url);
        }
        (true, _, Some((file, task))) => {
            app.suite.hub.run(iuredesk_engine::suite::SuiteAction::OpenProjectTask { file, task });
        }
        (false, None, _) => app.notice(iuredesk_engine::Notice::info(
            "This task does not come from Iurefficient.",
            "Esta tarea no viene de Iurefficient.",
        )),
        (true, _, None) => app.notice(iuredesk_engine::Notice::info(
            "This task's case has no IureProject plan that you follow.",
            "El caso de esta tarea no tiene un plan de IureProject que sigas.",
        )),
    }
}

/// Las columnas del tablero de una lista (sin «En curso» si la lista no lo tiene).
fn columns(in_progress: bool) -> Vec<(TaskStatus, &'static str)> {
    let mut v = vec![(TaskStatus::NeedsAction, pick("To do", "Por hacer"))];
    if in_progress {
        v.push((TaskStatus::InProcess, pick("In progress", "En curso")));
    }
    v.push((TaskStatus::Completed, pick("Done", "Terminadas")));
    v
}

/// La columna de una tarea (las terminadas por fecha también cuentan como terminadas).
fn column_of(t: &Task) -> TaskStatus {
    if t.is_done() { TaskStatus::Completed } else { t.status.clone() }
}

fn status_name(s: &TaskStatus) -> &'static str {
    match s {
        TaskStatus::NeedsAction => pick("To do", "Por hacer"),
        TaskStatus::InProcess => pick("In progress", "En curso"),
        TaskStatus::Completed => pick("Done", "Terminada"),
        TaskStatus::Cancelled => pick("Cancelled", "Cancelada"),
    }
}

/// Fecha de vencimiento corta, con color si ya venció.
fn due_text(app: &DeskApp, ui: &Ui, t: &Task) -> Option<(String, egui::Color32)> {
    let c = theme::colors(ui);
    let due = t.due.as_ref()?;
    let day = app.session.zone().local(due.to_utc()).date();
    let today = app.session.today();
    let text = day.format("%d/%m").to_string();
    Some(if !t.is_done() && day < today {
        (format!("{} · {text}", pick("Overdue", "Vencida")), c.danger)
    } else if day == today {
        (pick("Today", "Hoy").to_string(), c.accent)
    } else {
        (text, c.text_muted)
    })
}

/// Una tarjeta del tablero (se arrastra con su id).
fn card(
    app: &mut DeskApp,
    ui: &mut Ui,
    t: &Task,
    writable: bool,
    cols: &[(TaskStatus, &str)],
    case: Option<&str>,
) -> Option<BoardAct> {
    let c = theme::colors(ui);
    let selected = app.pim.task_sel == Some(t.id);
    let mut act = None;
    let id = ui.scope_id().with(("tarjeta", t.id.0));
    let inner = |ui: &mut Ui| {
        let frame = egui::Frame::new()
            .fill(if selected { c.selection } else { c.surface })
            .stroke(egui::Stroke::new(1.0, c.separator))
            .corner_radius(6.0)
            .inner_margin(egui::Margin::symmetric(8, 6));
        frame
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.add(egui::Label::new(RichText::new(&t.summary).color(c.text)).wrap().selectable(false));
                ui.horizontal_wrapped(|ui| {
                    if let Some((d, col)) = due_text(app, ui, t) {
                        ui.label(RichText::new(d).color(col).size(theme::density(ui.ctx()).small_size()));
                    }
                    if !t.is_done() && (1..=4).contains(&t.priority) {
                        let (r, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
                        icons::paint(ui.painter(), r, Icon::Flag, c.danger, c.danger);
                    }
                    if let Some(case) = case {
                        badge(ui, case, BadgeKind::Neutral);
                    }
                });
            })
            .response
    };
    let r = if writable { ui.dnd_drag_source(id, t.id, inner).response } else { inner(ui) };
    let r = ui.interact(r.rect, id.with("clic"), Sense::click());
    crate::widgets::named(
        &r,
        egui::Role::ListItem,
        &format!("{} {} · {}", pick("Task", "Tarea"), t.summary, status_name(&column_of(t))),
    );
    if r.clicked() {
        act = Some(BoardAct::Row(t.id, RowAct::Select));
    }
    if r.double_clicked() {
        act = Some(BoardAct::Row(t.id, RowAct::Edit));
    }
    egui::Popup::context_menu(&r).show(|ui| {
        if icons::menu_item(ui, Some(Icon::Rename), pick("Edit…", "Editar…")).clicked() {
            act = Some(BoardAct::Row(t.id, RowAct::Edit));
            ui.close();
        }
        if let Some(a) = origin_menu(app, ui, t) {
            act = Some(BoardAct::Row(t.id, a));
        }
        if writable {
            ui.menu_button(pick("Move to", "Mover a"), |ui| {
                let mut targets: Vec<(TaskStatus, &str)> = cols.to_vec();
                targets.push((TaskStatus::Cancelled, pick("Cancelled", "Canceladas")));
                for (s, name) in targets {
                    if s != column_of(t) && ui.button(name).clicked() {
                        act = Some(BoardAct::Status(t.id, s));
                        ui.close();
                    }
                }
            });
            ui.separator();
            if icons::menu_item(ui, Some(Icon::Trash), pick("Delete", "Eliminar")).clicked() {
                act = Some(BoardAct::Row(t.id, RowAct::Delete));
                ui.close();
            }
        }
    });
    act
}

/// El tablero de una lista.
pub(crate) fn board(
    app: &mut DeskApp,
    ui: &mut Ui,
    tasks: Vec<Task>,
    writable: bool,
    in_progress: bool,
) -> Vec<BoardAct> {
    let c = theme::colors(ui);
    let mut acts = Vec::new();
    let cols = columns(in_progress);
    let cases = app.session.task_cases();
    let known = known_cases(&cases);
    let case_of = |t: &Task| case_for(t, &cases, &known).map(|c| c.title);
    let (cancelled, active): (Vec<Task>, Vec<Task>) =
        tasks.into_iter().partition(|t| t.status == TaskStatus::Cancelled && !t.is_done());
    // Teclado: Alt+← / Alt+→ mueve la tarea elegida de columna.
    if writable
        && let Some(sel) = app.pim.task_sel
        && let Some(t) = active.iter().find(|t| t.id == sel)
        && !ui.ctx().egui_wants_keyboard_input()
    {
        let step = ui.input(|i| {
            if i.modifiers.alt && i.key_pressed(egui::Key::ArrowRight) {
                1
            } else if i.modifiers.alt && i.key_pressed(egui::Key::ArrowLeft) {
                -1
            } else {
                0
            }
        });
        if step != 0
            && let Some(k) = cols.iter().position(|(s, _)| *s == column_of(t))
            && let Some((s, _)) = usize::try_from(k as i64 + step).ok().and_then(|n| cols.get(n))
        {
            acts.push(BoardAct::Status(t.id, s.clone()));
        }
    }
    egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
        ui.horizontal_top(|ui| {
            let width = ((ui.available_width() - 12.0 * cols.len() as f32) / cols.len() as f32).clamp(200.0, 320.0);
            for (status, name) in &cols {
                let here: Vec<&Task> = active.iter().filter(|t| column_of(t) == *status).collect();
                ui.vertical(|ui| {
                    ui.set_width(width);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(*name).strong().color(c.text));
                        ui.label(RichText::new(here.len().to_string()).color(c.text_muted));
                    });
                    ui.add_space(4.0);
                    let frame =
                        egui::Frame::new().fill(c.surface_alt).corner_radius(8.0).inner_margin(egui::Margin::same(6));
                    let (_, dropped) = ui.dnd_drop_zone::<TaskId, ()>(frame, |ui| {
                        ui.set_min_size(vec2(width - 12.0, 120.0));
                        ui.spacing_mut().item_spacing.y = 6.0;
                        for t in &here {
                            if let Some(a) = card(app, ui, t, writable, &cols, case_of(t).as_deref()) {
                                acts.push(a);
                            }
                        }
                        if here.is_empty() {
                            ui.label(
                                RichText::new(pick("Drag tasks here", "Arrastra tareas aquí")).color(c.text_muted),
                            );
                        }
                    });
                    if let Some(id) = dropped {
                        acts.push(BoardAct::Status(*id, status.clone()));
                    }
                });
                ui.add_space(12.0);
            }
        });
        // Las canceladas, plegadas (como las terminadas en la lista).
        if !cancelled.is_empty() {
            ui.add_space(12.0);
            let label = format!("{} ({})", pick("Cancelled", "Canceladas"), cancelled.len());
            if ui.add(egui::Button::new(RichText::new(label).color(c.text_muted)).frame(false)).clicked() {
                app.pim.show_cancelled = !app.pim.show_cancelled;
            }
            if app.pim.show_cancelled {
                ui.set_max_width(360.0);
                for t in &cancelled {
                    if let Some(a) = card(app, ui, t, writable, &cols, case_of(t).as_deref()) {
                        acts.push(a);
                    }
                }
            }
        }
    });
    acts
}

/// El número de un caso («241/2026» de «241/2026 · Arrendamiento»).
fn case_number(title: &str) -> &str {
    title.split(" · ").next().unwrap_or(title).trim()
}

/// ¿El texto nombra ese número (palabra completa)?
fn mentions(text: &str, number: &str) -> bool {
    if number.len() < 3 {
        return false;
    }
    text.match_indices(number).any(|(i, _)| {
        let before = text.get(..i).and_then(|b| b.chars().next_back());
        let after = text.get(i + number.len()..).and_then(|a| a.chars().next());
        !before.is_some_and(|c| c.is_alphanumeric() || c == '/')
            && !after.is_some_and(|c| c.is_alphanumeric() || c == '/')
    })
}

/// El caso de una tarea: el que trae de Iurefficient o, si no tiene, el caso conocido cuyo número
/// menciona su título o sus notas («Presentar escrito caso 241/2026»).
pub(crate) fn case_for(
    t: &Task,
    cases: &HashMap<String, iuredesk_store::TaskCase>,
    known: &[iuredesk_store::TaskCase],
) -> Option<iuredesk_store::TaskCase> {
    if let Some(c) = t.remote_id.as_ref().and_then(|r| cases.get(r)) {
        return Some(c.clone());
    }
    known
        .iter()
        .find(|c| mentions(&t.summary, case_number(&c.title)) || mentions(&t.description, case_number(&c.title)))
        .cloned()
}

/// Los casos conocidos (los de las tareas de Iurefficient), sin repetir.
pub(crate) fn known_cases(cases: &HashMap<String, iuredesk_store::TaskCase>) -> Vec<iuredesk_store::TaskCase> {
    let mut v: Vec<iuredesk_store::TaskCase> = cases.values().cloned().collect();
    v.sort_by(|a, b| a.id.cmp(&b.id));
    v.dedup_by(|a, b| a.id == b.id);
    v
}

/// Las tareas pendientes de todas las listas, agrupadas por su caso (las de cada caso, con su plan
/// de IureProject si se sigue); al final, las que no tienen caso.
pub(crate) fn by_case(app: &mut DeskApp, ui: &mut Ui) -> Vec<BoardAct> {
    let c = theme::colors(ui);
    let mut acts = Vec::new();
    let lists = app.session.task_lists();
    let cases = app.session.task_cases();
    let known = known_cases(&cases);
    let mut groups: HashMap<Option<iuredesk_store::TaskCase>, Vec<(Task, bool)>> = HashMap::new();
    for l in &lists {
        let writable = app.session.iure_can_write() || !app.session.is_iure_task(l.id);
        for t in app.session.tasks(l.id).into_iter().filter(|t| !t.is_done() && t.status != TaskStatus::Cancelled) {
            let case = case_for(&t, &cases, &known);
            groups.entry(case).or_default().push((t, writable));
        }
    }
    let mut keys: Vec<Option<iuredesk_store::TaskCase>> = groups.keys().cloned().collect();
    // Los casos por nombre; «Sin caso» al final.
    keys.sort_by(|a, b| match (a, b) {
        (Some(x), Some(y)) => x.title.to_lowercase().cmp(&y.title.to_lowercase()),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.set_max_width(760.0);
        if keys.is_empty() {
            ui.add_space(20.0);
            EmptyState::new(Icon::Check, pick("All done", "Todo listo"))
                .body(pick("Nothing pending.", "No hay nada pendiente."))
                .show(ui);
        }
        for key in keys {
            let Some(items) = groups.get(&key) else { continue };
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                match &key {
                    Some(case) => {
                        badge(ui, pick("Case", "Caso"), BadgeKind::Accent);
                        ui.label(RichText::new(&case.title).strong().color(c.text));
                    }
                    None => {
                        ui.label(RichText::new(pick("No case", "Sin caso")).strong().color(c.text));
                    }
                }
                ui.label(RichText::new(items.len().to_string()).color(c.text_muted));
            });
            // El plan de IureProject de este caso (si se sigue): su estado y abrirlo.
            if let Some(case) = &key {
                for (file, line) in crate::today_projects::plans_for_case(app, &case.id) {
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
                        icons::paint(ui.painter(), r, Icon::Gantt, c.text_muted, c.accent);
                        ui.add(
                            egui::Label::new(
                                RichText::new(line).color(c.text_muted).size(theme::density(ui.ctx()).small_size()),
                            )
                            .wrap(),
                        );
                        if ui.small_button(pick("Open in IureProject", "Abrir en IureProject")).clicked() {
                            acts.push(BoardAct::OpenProject(file.clone()));
                        }
                    });
                }
            }
            ui.add_space(2.0);
            for (t, writable) in items {
                if let Some(a) = crate::pim::task_row(app, ui, t, *writable) {
                    acts.push(BoardAct::Row(t.id, a));
                }
            }
        }
    });
    acts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_task_that_names_a_case_number_joins_it() {
        let c = iuredesk_store::TaskCase { id: "c-241".into(), title: "241/2026 · Arrendamiento".into() };
        let cases: HashMap<String, iuredesk_store::TaskCase> = [("iure:1".to_string(), c.clone())].into();
        let known = known_cases(&cases);
        let t = |s: &str| Task { summary: s.into(), ..Default::default() };
        assert_eq!(case_for(&t("Presentar escrito caso 241/2026"), &cases, &known), Some(c.clone()));
        assert_eq!(case_for(&t("Ver 1241/2026"), &cases, &known), None, "otro número");
        assert_eq!(case_for(&t("Ver 241/20267"), &cases, &known), None);
        assert_eq!(case_for(&t("Comprar pan"), &cases, &known), None);
    }
}
