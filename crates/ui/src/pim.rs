//! Contactos (lista + ficha), notas (lista + texto con formato Markdown sencillo), tareas (listas
//! con casillas) y el asistente (llega en F3).

use egui::{Align, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};
use iuredesk_model::{ContactId, NoteId, Task, TaskListId};
use iurefficient_ui::list::{Column, ListState, ListView};
use iurefficient_ui::{
    BadgeKind, EmptyState, Icon, PaneKind, avatar, badge, fuzzy, icons, pick, section_label, sidebar_item, split, theme,
};

use crate::app::DeskApp;
use crate::widgets::{hairline, info_row, pane_title};

/// Estado de contactos, notas y tareas.
#[derive(Default)]
pub struct PimState {
    contact: Option<ContactId>,
    pub(crate) contact_filter: String,
    contact_list: ListState,
    contact_cols: Vec<Column>,
    note: Option<NoteId>,
    task_list: Option<TaskListId>,
    show_done: bool,
}

// ---------------------------------------------------------------------------------------------
// Contactos

pub fn contacts(app: &mut DeskApp, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    let all = app.session.contacts().to_vec();
    let filter = app.pim.contact_filter.trim().to_string();
    let shown: Vec<usize> = if filter.is_empty() {
        (0..all.len()).collect()
    } else {
        let mut v: Vec<(i32, usize)> = all
            .iter()
            .enumerate()
            .filter_map(|(i, c)| {
                let extra =
                    format!("{} {} {}", c.org, c.emails.first().map(|e| e.value.as_str()).unwrap_or(""), c.title);
                fuzzy::score_with_extra(&filter, &c.full_name, &extra).map(|s| (s, i))
            })
            .collect();
        v.sort_by_key(|(s, i)| (std::cmp::Reverse(*s), *i));
        v.into_iter().map(|(_, i)| i).collect()
    };
    split::left(ui, "contacts-list", PaneKind::Sidebar, 320.0, 240.0..=480.0, |ui| {
        ui.add_space(4.0);
        pane_title(ui, pick("Contacts", "Contactos"), &all.len().to_string());
        ui.add_space(6.0);
        if app.pim.contact_cols.is_empty() {
            app.pim.contact_cols = vec![Column::new(pick("Name", "Nombre"), 200.0).grow()];
        }
        if shown.is_empty() {
            EmptyState::new(Icon::Contacts, pick("No contacts", "Sin contactos"))
                .body(pick(
                    "People you write to will appear here.",
                    "Las personas con quienes te escribes aparecerán aquí.",
                ))
                .show(ui);
            return;
        }
        if app.pim.contact_list.selection.is_empty()
            && let Some(ct) = shown.first().and_then(|&k| all.get(k))
        {
            app.pim.contact_list.selection.select_only(0, ct.id.get());
        }
        let selected = app.pim.contact;
        let shown_k = shown.clone();
        let all_k = all.clone();
        let out = ListView::new("contacts", &mut app.pim.contact_cols, shown.len())
            .header(false)
            .row_height(46.0)
            .label(pick("Contacts", "Contactos"))
            .show(
                ui,
                &mut app.pim.contact_list,
                move |i| shown_k.get(i).and_then(|&k| all_k.get(k)).map_or(u64::MAX, |c| c.id.get()),
                |row| {
                    let Some(ct) = shown.get(row.index).and_then(|&k| all.get(k)) else { return };
                    let col = *row.colors();
                    let r = row.rect;
                    iurefficient_ui::widgets::avatar::paint_avatar(
                        row.painter(),
                        pos2(r.left() + 24.0, r.center().y),
                        30.0,
                        &ct.full_name,
                        ct.primary_email().unwrap_or(""),
                    );
                    let x = r.left() + 48.0;
                    let l1 = Rect::from_min_max(pos2(x, r.top() + 4.0), pos2(r.right() - 8.0, r.center().y + 1.0));
                    let l2 = Rect::from_min_max(pos2(x, r.center().y - 1.0), pos2(r.right() - 8.0, r.bottom() - 4.0));
                    let bold = theme::bold_font(&ctx, theme::density(&ctx).body_size());
                    row.text_in(l1, Align::Min, &ct.full_name, bold, col.text);
                    let sub =
                        if ct.org.is_empty() { ct.primary_email().unwrap_or("").to_string() } else { ct.org.clone() };
                    row.text_in(l2, Align::Min, &sub, theme::small_font(&ctx), col.text_muted);
                    let _ = selected;
                },
            );
        if out.selection_changed
            && let Some(i) = app.pim.contact_list.selection.cursor()
            && let Some(ct) = shown.get(i).and_then(|&k| all.get(k))
        {
            app.pim.contact = Some(ct.id);
        }
    });
    let current = app
        .pim
        .contact
        .and_then(|id| all.iter().find(|c| c.id == id))
        .or_else(|| shown.first().and_then(|&k| all.get(k)));
    split::center(ui, PaneKind::Detail, |ui| {
        let Some(ct) = current else {
            EmptyState::new(Icon::Person, pick("Select a contact", "Selecciona un contacto")).show(ui);
            return;
        };
        let c = theme::colors(ui);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.set_max_width(640.0);
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 16.0;
                avatar(ui, &ct.full_name, ct.primary_email().unwrap_or(""), 72.0);
                ui.vertical(|ui| {
                    ui.add_space(6.0);
                    ui.label(RichText::new(&ct.full_name).font(theme::bold_font(ui.ctx(), 22.0)).color(c.text));
                    let role = [ct.title.as_str(), ct.org.as_str()]
                        .into_iter()
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                        .join(" · ");
                    if !role.is_empty() {
                        ui.label(RichText::new(role).color(c.text_muted));
                    }
                });
            });
            ui.add_space(14.0);
            let email = ct.primary_email().map(str::to_string);
            ui.horizontal(|ui| {
                if let Some(e) = &email
                    && iurefficient_ui::primary_button(ui, pick("Write", "Escribir")).clicked()
                {
                    {
                        let mut d = app.session.compose_new(None);
                        d.draft.to = iuredesk_engine::compose::lenient_addresses(e);
                        app.open_composer(d);
                    }
                }
                if let Some(e) = &email
                    && ui.button(pick("Show messages", "Ver mensajes")).clicked()
                {
                    let q = format!("de:{e}");
                    app.mail.search = q.clone();
                    app.session.set_search(&q);
                }
            });
            ui.add_space(14.0);
            hairline(ui);
            ui.add_space(10.0);
            for e in &ct.emails {
                info_row(ui, Icon::Mail, &label_kind(&e.label), &e.value);
                ui.add_space(6.0);
            }
            for p in &ct.phones {
                info_row(ui, Icon::Phone, &label_kind(&p.label), &p.value);
                ui.add_space(6.0);
            }
            for a in &ct.addresses {
                let text = [a.street.as_str(), a.city.as_str(), a.region.as_str(), a.country.as_str()]
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(", ");
                info_row(ui, Icon::Pin, &label_kind(&a.label), &text);
                ui.add_space(6.0);
            }
            if let Some(b) = ct.birthday {
                info_row(ui, Icon::Calendar, pick("Birthday", "Cumpleaños"), &b.format("%d/%m/%Y").to_string());
                ui.add_space(6.0);
            }
            if !ct.notes.is_empty() {
                info_row(ui, Icon::Notes, pick("Notes", "Notas"), &ct.notes);
            }
        });
    });
}

fn label_kind(l: &str) -> String {
    match l {
        "work" => pick("Work", "Trabajo").into(),
        "home" => pick("Home", "Casa").into(),
        "mobile" => pick("Mobile", "Móvil").into(),
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------------------------------------
// Notas

pub fn notes(app: &mut DeskApp, ui: &mut Ui) {
    let notes = app.session.notes();
    let zone = app.session.zone();
    let now = app.session.now();
    split::left(ui, "notes-list", PaneKind::Sidebar, 300.0, 220.0..=460.0, |ui| {
        let c = theme::colors(ui);
        ui.add_space(4.0);
        pane_title(ui, pick("Notes", "Notas"), &notes.len().to_string());
        ui.add_space(8.0);
        if notes.is_empty() {
            EmptyState::new(Icon::Notes, pick("No notes yet", "Aún no hay notas"))
                .body(pick("Write down ideas, minutes and checklists.", "Apunta ideas, minutas y listas."))
                .show(ui);
            return;
        }
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for n in &notes {
                let selected = app.pim.note.or(notes.first().map(|n| n.id)) == Some(n.id);
                let h = 64.0;
                let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::click());
                if selected {
                    ui.painter().rect_filled(rect, 8.0, c.selection);
                } else if resp.hovered() {
                    ui.painter().rect_filled(rect, 8.0, c.hover);
                }
                let inner = rect.shrink2(vec2(10.0, 8.0));
                let title_font = theme::bold_font(ui.ctx(), theme::density(ui.ctx()).body_size());
                let date = iuredesk_engine::time::list_date(n.updated, now, zone);
                let gd = ui.painter().layout_no_wrap(date, theme::small_font(ui.ctx()), c.text_muted);
                let dw = gd.size().x;
                ui.painter().galley(pos2(inner.right() - dw, inner.top() + 1.0), gd, c.text_muted);
                let mut title = n.title.clone();
                if n.pinned {
                    title = format!("{title}  ·  {}", pick("pinned", "fijada"));
                }
                let gt =
                    iurefficient_ui::widgets::text::elided(ui, &title, title_font, c.text, inner.width() - dw - 8.0);
                ui.painter().galley(inner.min, gt, c.text);
                let snippet: String = n
                    .body
                    .lines()
                    .map(|l| l.trim_start_matches(['#', '-', '*', ' ', '>']).replace('*', ""))
                    .filter(|l| !l.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ");
                let gs = iurefficient_ui::widgets::text::elided(
                    ui,
                    &snippet,
                    theme::body_font(ui.ctx()),
                    c.text_muted,
                    inner.width(),
                );
                ui.painter().galley(pos2(inner.left(), inner.top() + 24.0), gs, c.text_muted);
                iurefficient_ui::a11y::toggle(&resp, &n.title, selected);
                if resp.clicked() {
                    app.pim.note = Some(n.id);
                }
                ui.add_space(2.0);
            }
        });
    });
    let current = app.pim.note.and_then(|id| notes.iter().find(|n| n.id == id)).or(notes.first());
    split::center(ui, PaneKind::Detail, |ui| {
        let Some(n) = current else {
            EmptyState::new(Icon::Notes, pick("Select a note", "Selecciona una nota")).show(ui);
            return;
        };
        let c = theme::colors(ui);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.set_max_width(720.0);
            ui.add_space(8.0);
            ui.label(RichText::new(&n.title).font(theme::bold_font(ui.ctx(), 24.0)).color(c.text));
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!(
                        "{} {}",
                        pick("Edited", "Editada"),
                        iuredesk_engine::time::long_date(n.updated, zone)
                    ))
                    .color(c.text_muted)
                    .size(theme::density(ui.ctx()).small_size()),
                );
                for t in &n.tags {
                    badge(ui, t, BadgeKind::Neutral);
                }
            });
            ui.add_space(12.0);
            markdown(ui, &n.body);
        });
    });
}

/// Markdown sencillo: encabezados, viñetas, casillas, citas, negritas y párrafos.
fn markdown(ui: &mut Ui, text: &str) {
    let c = theme::colors(ui);
    let body = theme::density(ui.ctx()).body_size() + 1.0;
    for line in text.lines().take(5_000) {
        let t = line.trim_end();
        if t.trim().is_empty() {
            ui.add_space(6.0);
            continue;
        }
        if let Some(h) = t.strip_prefix("### ") {
            ui.add_space(6.0);
            ui.label(RichText::new(h).font(theme::bold_font(ui.ctx(), body + 2.0)).color(c.text));
        } else if let Some(h) = t.strip_prefix("## ") {
            ui.add_space(8.0);
            ui.label(RichText::new(h).font(theme::bold_font(ui.ctx(), body + 4.0)).color(c.text));
        } else if let Some(h) = t.strip_prefix("# ") {
            ui.add_space(10.0);
            ui.label(RichText::new(h).font(theme::bold_font(ui.ctx(), body + 7.0)).color(c.text));
        } else if let Some(rest) = t.strip_prefix("- [ ] ").or_else(|| t.strip_prefix("- [x] ")) {
            let done = t.starts_with("- [x]");
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                ui.painter().rect_stroke(r.shrink(1.5), 3.0, Stroke::new(1.3, c.text_muted), egui::StrokeKind::Inside);
                if done {
                    icons::paint(ui.painter(), r.shrink(2.0), Icon::Check, c.accent, c.accent);
                }
                inline(ui, rest, body, if done { c.text_muted } else { c.text });
            });
        } else if let Some(rest) = t.trim_start().strip_prefix("- ").or_else(|| t.trim_start().strip_prefix("* ")) {
            ui.horizontal(|ui| {
                ui.add_space(4.0);
                let (r, _) = ui.allocate_exact_size(vec2(10.0, body), Sense::hover());
                ui.painter().circle_filled(r.center(), 2.5, c.text_muted);
                inline(ui, rest, body, c.text);
            });
        } else if let Some(q) = t.strip_prefix("> ") {
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(3.0, body * 1.4), Sense::hover());
                ui.painter().rect_filled(r, 1.0, c.separator);
                ui.add_space(6.0);
                inline(ui, q, body, c.text_muted);
            });
        } else {
            inline(ui, t, body, c.text);
        }
    }
}

/// Texto con `**negritas**`.
fn inline(ui: &mut Ui, text: &str, size: f32, color: egui::Color32) {
    let mut job = egui::text::LayoutJob::default();
    for (i, part) in text.split("**").enumerate() {
        let font = if i % 2 == 1 { theme::bold_font(ui.ctx(), size) } else { egui::FontId::proportional(size) };
        job.append(
            part,
            0.0,
            egui::TextFormat { font_id: font, color, line_height: Some(size * 1.45), ..Default::default() },
        );
    }
    job.wrap.max_width = ui.available_width();
    ui.add(egui::Label::new(job).wrap().selectable(true));
}

// ---------------------------------------------------------------------------------------------
// Tareas

pub fn tasks(app: &mut DeskApp, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    let lists = app.session.task_lists();
    let selected = app.pim.task_list.or(lists.first().map(|l| l.id));
    split::left(ui, "task-lists", PaneKind::Sidebar, 244.0, 190.0..=360.0, |ui| {
        ui.add_space(6.0);
        section_label(ui, pick("Lists", "Listas"));
        ui.add_space(2.0);
        ui.spacing_mut().item_spacing.y = 1.0;
        for l in &lists {
            let open = app.session.tasks(l.id).iter().filter(|t| !t.is_done()).count();
            if sidebar_item(ui, Some(Icon::Tasks), &l.name, u32::try_from(open).unwrap_or(0), selected == Some(l.id))
                .clicked()
            {
                app.pim.task_list = Some(l.id);
            }
        }
    });
    split::center(ui, PaneKind::Detail, |ui| {
        let c = theme::colors(ui);
        let Some(list_id) = selected else {
            EmptyState::new(Icon::Tasks, pick("No task lists", "No hay listas de tareas"))
                .body(pick("Tasks from your accounts will appear here.", "Las tareas de tus cuentas aparecerán aquí."))
                .show(ui);
            return;
        };
        let name = lists.iter().find(|l| l.id == list_id).map(|l| l.name.clone()).unwrap_or_default();
        let all = app.session.tasks(list_id);
        let (done, open): (Vec<Task>, Vec<Task>) = all.into_iter().partition(|t| t.is_done());
        pane_title(ui, &name, &format!("{} {}", open.len(), pick("pending", "pendientes")));
        ui.add_space(8.0);
        let mut toggle: Option<iuredesk_model::TaskId> = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.set_max_width(760.0);
            if open.is_empty() {
                ui.add_space(20.0);
                EmptyState::new(Icon::Check, pick("All done", "Todo listo"))
                    .body(pick("Nothing pending in this list.", "No hay nada pendiente en esta lista."))
                    .show(ui);
            }
            for t in &open {
                if task_row(app, ui, t) {
                    toggle = Some(t.id);
                }
            }
            if !done.is_empty() {
                ui.add_space(10.0);
                let label = format!("{} ({})", pick("Completed", "Completadas"), done.len());
                let r = ui.add(egui::Button::new(RichText::new(label).color(c.text_muted)).frame(false));
                if r.clicked() {
                    app.pim.show_done = !app.pim.show_done;
                }
                if app.pim.show_done {
                    for t in &done {
                        if task_row(app, ui, t) {
                            toggle = Some(t.id);
                        }
                    }
                }
            }
        });
        if let Some(id) = toggle {
            match app.session.toggle_task(id, &lists) {
                Ok(out) => app.outcome(&ctx, out),
                Err(e) => app.notice(iuredesk_engine::Notice::error(e.text())),
            }
        }
    });
}

/// Una tarea con su casilla. Devuelve `true` si se pulsó la casilla.
fn task_row(app: &DeskApp, ui: &mut Ui, t: &Task) -> bool {
    let c = theme::colors(ui);
    let zone = app.session.zone();
    let today = app.session.today();
    let d = theme::density(ui.ctx());
    let h = d.row_height().max(30.0) + 6.0;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::hover());
    let box_r = Rect::from_center_size(pos2(rect.left() + 14.0, rect.center().y), vec2(18.0, 18.0));
    let resp = ui.interact(box_r.expand(4.0), ui.scope_id().with(("task", t.id.get())), Sense::click());
    let done = t.is_done();
    let ring = if resp.hovered() { c.accent } else { c.text_muted };
    if done {
        ui.painter().circle_filled(box_r.center(), 9.0, c.accent);
        icons::paint(ui.painter(), box_r.shrink(3.0), Icon::Check, c.on_accent, c.on_accent);
    } else {
        ui.painter().circle_stroke(box_r.center(), 8.5, Stroke::new(1.5, ring));
    }
    iurefficient_ui::a11y::toggle(&resp, &t.summary, done);
    let x = box_r.right() + 12.0;
    let mut right = rect.right() - 8.0;
    if let Some(due) = &t.due {
        let dd = zone.local(due.to_utc()).date();
        let (txt, col) = if done {
            (dd.format("%d/%m").to_string(), c.text_muted)
        } else if dd < today {
            (format!("{} · {}", pick("Overdue", "Vencida"), dd.format("%d/%m")), c.danger)
        } else if dd == today {
            (pick("Today", "Hoy").to_string(), c.accent)
        } else if Some(dd) == today.succ_opt() {
            (pick("Tomorrow", "Mañana").to_string(), c.text_muted)
        } else {
            (format!("{} {}", iuredesk_engine::time::weekday_short(dd), dd.format("%d/%m")), c.text_muted)
        };
        let g = ui.painter().layout_no_wrap(txt, theme::small_font(ui.ctx()), col);
        let w = g.size().x;
        ui.painter().galley(pos2(right - w, rect.center().y - g.size().y / 2.0), g, col);
        right -= w + 14.0;
    }
    if !done && (1..=4).contains(&t.priority) {
        let fr = Rect::from_center_size(pos2(right - 7.0, rect.center().y), vec2(14.0, 14.0));
        icons::paint(ui.painter(), fr, Icon::Flag, c.danger, c.danger);
        right -= 22.0;
    }
    let font = theme::body_font(ui.ctx());
    let ink = if done { c.text_muted } else { c.text };
    let g = iurefficient_ui::widgets::text::elided(ui, &t.summary, font, ink, (right - x).max(0.0));
    let gy = rect.center().y - g.size().y / 2.0;
    let gw = g.size().x;
    ui.painter().galley(pos2(x, gy), g, ink);
    if done {
        ui.painter().hline(x..=(x + gw), rect.center().y, Stroke::new(1.0, c.text_muted));
    }
    ui.painter().hline((rect.left() + 34.0)..=rect.right(), rect.bottom() - 0.5, Stroke::new(1.0, c.separator));
    resp.clicked()
}

// ---------------------------------------------------------------------------------------------
// Asistente
