//! UX-6: editor de tareas (doble clic, Intro o «Editar…» en Tareas y en «Mi día»): título,
//! notas, vencimiento (con hora si la fuente la guarda), inicio, prioridad, terminada, lista (de
//! la misma cuenta), recordatorio y repetición. Lo que la fuente no sabe guardar se ve
//! deshabilitado con el motivo; la repetición, si la fuente no la tiene, no se muestra. Guardar va
//! local primero por `pim-sync` (o por `connect` para Iurefficient), con deshacer.

use chrono::{Duration, NaiveDate, NaiveTime, Timelike};
use egui::{Align, Context, Layout, RichText, Ui, vec2};
use iuredesk_engine::Notice;
use iuredesk_engine::calendar::repeat::{Freq, Preset, Repeat};
use iuredesk_engine::session::{TaskCaps, TaskSource};
use iuredesk_model::{EventTime, Task, TaskId, TaskList, TaskListId, TaskStatus};
use iurefficient_ui::{pick, primary_button, theme};

use crate::app::DeskApp;

/// Estado del editor abierto.
pub(crate) struct TaskEditor {
    original: Task,
    caps: TaskCaps,
    targets: Vec<TaskList>,
    title: String,
    notes: String,
    done: bool,
    list: TaskListId,
    due_on: bool,
    due: NaiveDate,
    due_text: String,
    due_timed: bool,
    due_min: i64,
    start_on: bool,
    start: NaiveDate,
    start_text: String,
    priority: u8,
    rem_on: bool,
    rem: NaiveDate,
    rem_text: String,
    rem_min: i64,
    repeat: Repeat,
    focused: bool,
}

const STEP: i64 = 15;
const DAY_MIN: i64 = 24 * 60;

fn hm(min: i64) -> String {
    let min = min.clamp(0, DAY_MIN - 1);
    format!("{:02}:{:02}", min / 60, min % 60)
}

fn minutes(t: NaiveTime) -> i64 {
    i64::from(t.hour()) * 60 + i64::from(t.minute())
}

fn at(d: NaiveDate, min: i64) -> chrono::NaiveDateTime {
    d.and_hms_opt(0, 0, 0).unwrap_or_default() + Duration::minutes(min.clamp(0, DAY_MIN - 1))
}

/// Prioridades del menú: (valor iCalendar, nombre).
fn priorities() -> [(u8, &'static str); 4] {
    [(0, pick("None", "Ninguna")), (1, pick("High", "Alta")), (5, pick("Medium", "Media")), (9, pick("Low", "Baja"))]
}

fn priority_bucket(p: u8) -> u8 {
    match p {
        1..=4 => 1,
        5 => 5,
        6..=9 => 9,
        _ => 0,
    }
}

/// Por qué un campo no se puede cambiar en esta fuente.
fn unsupported(source: TaskSource) -> String {
    match source {
        TaskSource::Iurefficient => {
            pick("Iurefficient tasks don't store this.", "Las tareas de Iurefficient no guardan esto.").to_string()
        }
        s => format!("{} {}", s.label(), pick("doesn't store this.", "no guarda esto.")),
    }
}

impl TaskEditor {
    fn new(app: &DeskApp, t: Task) -> TaskEditor {
        let zone = app.session.zone();
        let today = app.session.today();
        let caps = app.session.task_caps(t.list);
        let targets = app.session.task_move_targets(t.list);
        let (due_on, due, due_timed, due_min) = match &t.due {
            Some(EventTime::Date(d)) => (true, *d, false, 9 * 60),
            Some(EventTime::DateTime { utc, .. }) => {
                let l = zone.local(*utc);
                (true, l.date(), true, minutes(l.time()))
            }
            None => (false, today, false, 9 * 60),
        };
        let start_d = t.start.as_ref().map(|s| match s {
            EventTime::Date(d) => *d,
            EventTime::DateTime { utc, .. } => zone.local(*utc).date(),
        });
        let rem_l = t.reminder.map(|r| zone.local(r));
        let anchor = if due_on { due } else { start_d.unwrap_or(today) };
        let repeat = Repeat::from_rrule(Some(t.rrule.as_str()).filter(|r| !r.is_empty()), anchor);
        TaskEditor {
            caps,
            targets,
            title: t.summary.clone(),
            notes: t.description.clone(),
            done: t.is_done(),
            list: t.list,
            due_on,
            due,
            due_text: due.format("%Y-%m-%d").to_string(),
            due_timed,
            due_min,
            start_on: start_d.is_some(),
            start: start_d.unwrap_or(today),
            start_text: start_d.unwrap_or(today).format("%Y-%m-%d").to_string(),
            priority: priority_bucket(t.priority),
            rem_on: rem_l.is_some(),
            rem: rem_l.map_or(if due_on { due } else { today }, |l| l.date()),
            rem_text: rem_l.map_or(if due_on { due } else { today }, |l| l.date()).format("%Y-%m-%d").to_string(),
            rem_min: rem_l.map_or(9 * 60, |l| minutes(l.time())),
            repeat,
            original: t,
            focused: false,
        }
    }

    /// La tarea con lo editado.
    fn to_task(&self, app: &DeskApp) -> Task {
        let zone = app.session.zone();
        let tzid = Some(iuredesk_engine::time::user_tz(zone).name().to_string());
        let mut t = self.original.clone();
        t.summary = self.title.clone();
        t.description = self.notes.clone();
        t.list = self.list;
        t.due = self.due_on.then(|| {
            if self.due_timed && self.caps.due_time {
                EventTime::DateTime { utc: zone.to_utc(at(self.due, self.due_min)), tzid: tzid.clone() }
            } else {
                EventTime::Date(self.due)
            }
        });
        t.start = self.start_on.then_some(EventTime::Date(self.start));
        // La prioridad exacta (2, 3…) se conserva si el usuario no la cambió.
        if priority_bucket(self.original.priority) != self.priority {
            t.priority = self.priority;
        }
        t.reminder = self.rem_on.then(|| zone.to_utc(at(self.rem, self.rem_min)));
        let anchor = if self.due_on { self.due } else { self.start };
        t.rrule = if self.repeat.freq == Freq::None && self.repeat.raw.is_none() {
            String::new()
        } else {
            self.repeat.to_rrule(anchor, true, zone).unwrap_or_default()
        };
        if self.done && !self.original.is_done() {
            t.status = TaskStatus::Completed;
        } else if !self.done && self.original.is_done() {
            t.status = TaskStatus::NeedsAction;
            t.completed = None;
        }
        t
    }
}

/// Abre el editor de una tarea.
pub(crate) fn open(app: &mut DeskApp, id: TaskId) {
    match app.session.task(id) {
        Some(t) => {
            let e = TaskEditor::new(app, t);
            app.pim.task_editor = Some(e);
        }
        None => app.notice(Notice::info("That task no longer exists.", "Esa tarea ya no existe.")),
    }
}

/// ¿Está abierto? (pruebas)
pub(crate) fn is_open(app: &DeskApp) -> bool {
    app.pim.task_editor.is_some()
}

fn date_field(ui: &mut Ui, text: &mut String, value: &mut NaiveDate, name: &str) {
    let r = ui.add(egui::TextEdit::singleline(text).desired_width(96.0).hint_text(pick("YYYY-MM-DD", "AAAA-MM-DD")));
    crate::widgets::named(&r, egui::Role::TextInput, name);
    if r.changed()
        && let Ok(d) = NaiveDate::parse_from_str(text.trim(), "%Y-%m-%d")
    {
        *value = d;
    }
    if r.lost_focus() {
        *text = value.format("%Y-%m-%d").to_string();
    }
}

fn time_combo(ui: &mut Ui, id: &str, min: &mut i64, name: &str) {
    let mut v = *min;
    let combo = egui::ComboBox::from_id_salt(id).width(74.0).selected_text(hm(v)).height(280.0).show_ui(ui, |ui| {
        let mut m = 0;
        while m < DAY_MIN {
            ui.selectable_value(&mut v, m, hm(m));
            m += STEP;
        }
    });
    crate::widgets::named(&combo.response, egui::Role::ComboBox, name);
    *min = v;
}

/// Una fila «etiqueta: controles», deshabilitada (con el motivo) si la fuente no la guarda.
fn row(ui: &mut Ui, label: &str, enabled: bool, why: &str, add: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        let c = theme::colors(ui);
        ui.add_sized(vec2(110.0, 22.0), egui::Label::new(RichText::new(label).color(c.text_muted)));
        let r = ui.add_enabled_ui(enabled, add).response;
        if !enabled {
            r.on_disabled_hover_text(why);
        }
    });
}

fn body(ui: &mut Ui, e: &mut TaskEditor) {
    let c = theme::colors(ui);
    let caps = e.caps;
    let why = unsupported(caps.source);
    ui.spacing_mut().item_spacing.y = 8.0;
    let t = ui.add(
        egui::TextEdit::singleline(&mut e.title)
            .font(theme::bold_font(ui.ctx(), 18.0))
            .hint_text(pick("Title", "Título"))
            .desired_width(f32::INFINITY),
    );
    crate::widgets::text_label(&t, pick("Task title", "Título de la tarea"));
    if !e.focused {
        e.focused = true;
        t.request_focus();
    }
    let r = ui.checkbox(&mut e.done, pick("Completed", "Terminada"));
    crate::widgets::named(&r, egui::Role::CheckBox, pick("Completed", "Terminada"));
    // Lista.
    let list_name =
        |id: TaskListId, ts: &[TaskList]| ts.iter().find(|l| l.id == id).map(|l| l.name.clone()).unwrap_or_default();
    let can_move = caps.move_list && e.targets.len() > 1;
    let move_why = if caps.move_list {
        pick("There is no other list in this account.", "No hay otra lista en esta cuenta.").to_string()
    } else {
        why.clone()
    };
    row(ui, pick("List", "Lista"), can_move, &move_why, |ui| {
        let current = list_name(e.list, &e.targets);
        let targets = e.targets.clone();
        let combo = egui::ComboBox::from_id_salt("task-list").width(220.0).selected_text(current).show_ui(ui, |ui| {
            for l in &targets {
                ui.selectable_value(&mut e.list, l.id, &l.name);
            }
        });
        crate::widgets::named(&combo.response, egui::Role::ComboBox, pick("List", "Lista"));
    });
    // Vencimiento.
    row(ui, pick("Due", "Vence"), true, "", |ui| {
        let r = ui.checkbox(&mut e.due_on, "");
        crate::widgets::named(&r, egui::Role::CheckBox, pick("Has due date", "Tiene vencimiento"));
        ui.add_enabled_ui(e.due_on, |ui| {
            date_field(ui, &mut e.due_text, &mut e.due, pick("Due date", "Fecha de vencimiento"));
            let tr = ui
                .add_enabled_ui(caps.due_time, |ui| {
                    let r = ui.checkbox(&mut e.due_timed, pick("at", "a las"));
                    crate::widgets::named(&r, egui::Role::CheckBox, pick("Due at a time", "Vence a una hora"));
                    if e.due_timed && caps.due_time {
                        time_combo(ui, "task-due-time", &mut e.due_min, pick("Due time", "Hora de vencimiento"));
                    }
                })
                .response;
            if !caps.due_time {
                tr.on_disabled_hover_text(format!(
                    "{} {}",
                    caps.source.label(),
                    pick("only stores the due date, not the time.", "solo guarda la fecha de vencimiento, no la hora.")
                ));
            }
        });
    });
    // Inicio.
    row(ui, pick("Starts", "Empieza"), caps.start, &why, |ui| {
        let r = ui.checkbox(&mut e.start_on, "");
        crate::widgets::named(&r, egui::Role::CheckBox, pick("Has start date", "Tiene fecha de inicio"));
        ui.add_enabled_ui(e.start_on, |ui| {
            date_field(ui, &mut e.start_text, &mut e.start, pick("Start date", "Fecha de inicio"));
        });
    });
    // Prioridad.
    row(ui, pick("Priority", "Prioridad"), caps.priority, &why, |ui| {
        let name = priorities().iter().find(|(v, _)| *v == e.priority).map_or("", |(_, n)| n);
        let combo = egui::ComboBox::from_id_salt("task-priority").width(140.0).selected_text(name).show_ui(ui, |ui| {
            for (v, n) in priorities() {
                ui.selectable_value(&mut e.priority, v, n);
            }
        });
        crate::widgets::named(&combo.response, egui::Role::ComboBox, pick("Priority", "Prioridad"));
    });
    // Recordatorio.
    row(ui, pick("Reminder", "Recordatorio"), caps.reminder, &why, |ui| {
        let r = ui.checkbox(&mut e.rem_on, "");
        crate::widgets::named(&r, egui::Role::CheckBox, pick("Remind me", "Recordarme"));
        ui.add_enabled_ui(e.rem_on, |ui| {
            date_field(ui, &mut e.rem_text, &mut e.rem, pick("Reminder date", "Fecha del recordatorio"));
            time_combo(ui, "task-rem-time", &mut e.rem_min, pick("Reminder time", "Hora del recordatorio"));
        });
    });
    // Repetición (solo si la fuente la guarda).
    if caps.recurrence {
        let anchor = if e.due_on { e.due } else { e.start };
        row(ui, pick("Repeat", "Repetir"), true, "", |ui| {
            let preset = e.repeat.preset(anchor);
            let mut p = preset;
            let label = if preset == Preset::Custom { e.repeat.describe(anchor) } else { p.label().to_string() };
            let combo =
                egui::ComboBox::from_id_salt("task-repeat").width(240.0).selected_text(label).show_ui(ui, |ui| {
                    for q in Preset::ALL {
                        if q != Preset::Custom {
                            ui.selectable_value(&mut p, q, q.label());
                        }
                    }
                });
            crate::widgets::named(&combo.response, egui::Role::ComboBox, pick("Repeat", "Repetir"));
            if p != preset {
                e.repeat = e.repeat.from_preset(p, anchor);
            }
        });
        if e.repeat.freq != Freq::None || e.repeat.raw.is_some() {
            ui.label(RichText::new(e.repeat.describe(anchor)).color(c.text_muted));
        }
    }
    ui.add_space(4.0);
    ui.label(RichText::new(pick("Notes", "Notas")).color(c.text_muted));
    let n = ui.add(
        egui::TextEdit::multiline(&mut e.notes)
            .desired_rows(6)
            .desired_width(f32::INFINITY)
            .hint_text(pick("Details, links, steps…", "Detalles, enlaces, pasos…")),
    );
    crate::widgets::text_label(&n, pick("Task notes", "Notas de la tarea"));
    let badge = match app_badge(caps.source) {
        Some(b) => b,
        None => return,
    };
    ui.label(RichText::new(badge).color(c.text_muted).size(theme::density(ui.ctx()).small_size()));
}

fn app_badge(s: TaskSource) -> Option<String> {
    match s {
        TaskSource::Local => Some(pick("Saved only on this computer.", "Guardada solo en este equipo.").to_string()),
        other => Some(format!("{} {}", pick("Syncs with", "Se sincroniza con"), other.label())),
    }
}

/// Dibuja el editor si está abierto.
pub(crate) fn show(app: &mut DeskApp, ctx: &Context) {
    let Some(mut e) = app.pim.task_editor.take() else { return };
    let mut open = true;
    let mut action: Option<&'static str> = None;
    egui::Window::new(pick("Task", "Tarea"))
        .id(egui::Id::unique("task-editor"))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_width(520.0)
        .max_height(ctx.content_rect().height() - 40.0)
        .default_pos(ctx.content_rect().center() - vec2(260.0, 260.0))
        .show(ctx, |ui| {
            let editable = e.caps.editable;
            if !editable {
                ui.label(
                    RichText::new(pick(
                        "Iurefficient is not connected: this task can only be changed there.",
                        "Iurefficient no está conectado: esta tarea solo se puede cambiar allá.",
                    ))
                    .color(theme::colors(ui).danger),
                );
            }
            egui::ScrollArea::vertical()
                .auto_shrink([false, true])
                .max_height(ctx.content_rect().height() - 160.0)
                .show(ui, |ui| {
                    ui.add_enabled_ui(editable, |ui| body(ui, &mut e));
                });
            ui.separator();
            ui.horizontal(|ui| {
                if editable {
                    let c = theme::colors(ui);
                    if ui.button(RichText::new(pick("Delete", "Eliminar")).color(c.danger)).clicked() {
                        action = Some("delete");
                    }
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if editable && primary_button(ui, pick("Save", "Guardar")).clicked() {
                        action = Some("save");
                    }
                    if ui.button(pick("Cancel", "Cancelar")).clicked() {
                        action = Some("cancel");
                    }
                });
            });
            // Ctrl+Intro guarda; Esc cierra.
            if editable && ui.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.command) {
                action = Some("save");
            }
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                action = Some("cancel");
            }
        });
    if !open {
        action = Some("cancel");
    }
    match action {
        Some("cancel") => {}
        Some("save") => {
            let t = e.to_task(app);
            match app.session.save_task_edit(t) {
                Ok(iuredesk_engine::Outcome::Notice(n)) if n.kind == iuredesk_engine::NoticeKind::Error => {
                    app.notice(n);
                    app.pim.task_editor = Some(e);
                }
                Ok(out) => app.outcome(ctx, out),
                Err(err) => {
                    app.notice(Notice::error(err.text()));
                    app.pim.task_editor = Some(e);
                }
            }
        }
        Some("delete") => match app.session.delete_task(e.original.id) {
            Ok(out) => app.outcome(ctx, out),
            Err(err) => app.notice(Notice::error(err.text())),
        },
        _ => app.pim.task_editor = Some(e),
    }
}
