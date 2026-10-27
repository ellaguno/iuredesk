//! Crear y editar eventos: aviso de «crear rápido» tras arrastrar en la rejilla (título, hora,
//! calendario, «Más opciones»), el editor completo (lugar, todo el día, inicio/fin con zona,
//! repetición, recordatorios, invitados con autocompletado y obligatorio/opcional, asistente de
//! programación con disponibilidad y huecos sugeridos, calendario, ocupado/disponible, privado,
//! descripción con vista previa Markdown) y la pregunta de las series («Solo este evento / Este y
//! los siguientes / Toda la serie»).

use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, Weekday};
use egui::{Align, Color32, Context, Layout, Pos2, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};
use iuredesk_engine::SeriesScope;
use iuredesk_engine::calendar::Occurrence;
use iuredesk_engine::calendar::draft::{COMMON_ZONES, EventDraft, REMINDER_CHOICES, reminder_label};
use iuredesk_engine::calendar::drag::DAY_MIN;
use iuredesk_engine::calendar::invite::partstat_label;
use iuredesk_engine::calendar::repeat::{Freq, MonthlyBy, Preset, Repeat, RepeatEnd, weekday_letter};
use iuredesk_engine::calendar::schedule::{Availability, suggest};
use iuredesk_engine::time::{month_name, weekday_short};
use iuredesk_engine::{Notice, Outcome};
use iuredesk_model::{AttendeeRole, EventTime, Mailbox};
use iurefficient_ui::{Icon, icons, pick, primary_button, section_label, theme};

use crate::app::DeskApp;
use crate::calendar::tz_label;
use crate::widgets::{mix, rgb};

/// Aviso de crear rápido.
pub(crate) struct Quick {
    draft: EventDraft,
    pos: Pos2,
    focused: bool,
}

/// Editor completo.
pub(crate) struct Full {
    draft: EventDraft,
    occ: Option<Occurrence>,
    start_text: String,
    end_text: String,
    until_text: String,
    attendee: String,
    optional: bool,
    preview: bool,
    focused: bool,
    read_only: bool,
}

/// Acción que espera «¿solo este evento?».
pub(crate) enum Pending {
    Save(Box<EventDraft>),
    Reschedule { occ: Box<Occurrence>, start: EventTime, end: EventTime },
    Delete(Box<Occurrence>),
}

/// Estado del editor.
#[derive(Default)]
pub struct EditorState {
    pub(crate) quick: Option<Quick>,
    pub(crate) full: Option<Full>,
    pub(crate) ask: Option<Pending>,
}

impl Full {
    fn new(draft: EventDraft, occ: Option<Occurrence>, read_only: bool) -> Full {
        let until_text = match draft.repeat.end {
            RepeatEnd::Until(d) => d.format("%Y-%m-%d").to_string(),
            _ => String::new(),
        };
        Full {
            start_text: draft.start_date.format("%Y-%m-%d").to_string(),
            end_text: draft.end_date.format("%Y-%m-%d").to_string(),
            until_text,
            draft,
            occ,
            attendee: String::new(),
            optional: false,
            preview: false,
            focused: false,
            read_only,
        }
    }
}

// ------------------------------------------------------------------ entradas

/// Abre el editor completo con un formulario (comando `cal.new_event`, «Más opciones»).
pub(crate) fn open(app: &mut DeskApp, draft: EventDraft) {
    app.calendar.editor.quick = None;
    let ro = !app.session.can_edit(draft.calendar);
    app.calendar.editor.full = Some(Full::new(draft, None, ro));
}

/// Abre una aparición (clic en un evento).
pub(crate) fn open_occurrence(app: &mut DeskApp, o: &Occurrence) {
    if let Some(d) = app.session.draft_for(o) {
        app.calendar.editor.quick = None;
        let ro = !app.session.can_edit(d.calendar);
        app.calendar.editor.full = Some(Full::new(d, Some(o.clone()), ro));
    }
}

/// Aviso de crear rápido para `[start, end)` (horas locales) junto a `pos`.
pub(crate) fn quick_create(app: &mut DeskApp, start: NaiveDateTime, end: NaiveDateTime, all_day: bool, pos: Pos2) {
    match app.session.new_event_draft(start, end, all_day) {
        Some(draft) => app.calendar.editor.quick = Some(Quick { draft, pos, focused: false }),
        None => app
            .notice(Notice::info("Add an account with a calendar first.", "Primero agrega una cuenta con calendario.")),
    }
}

/// Mover o estirar (arrastre): pregunta el alcance si el evento se repite.
pub(crate) fn reschedule(app: &mut DeskApp, o: &Occurrence, start: EventTime, end: EventTime) {
    if o.recurring {
        app.calendar.editor.ask = Some(Pending::Reschedule { occ: Box::new(o.clone()), start, end });
        return;
    }
    let r = app.session.reschedule(o, start, end, SeriesScope::All);
    finish(app, r);
}

fn finish(app: &mut DeskApp, r: iuredesk_engine::Result<Outcome>) -> bool {
    match r {
        Ok(Outcome::Notice(n)) if n.kind == iuredesk_engine::NoticeKind::Error => {
            app.notice(n);
            false
        }
        Ok(out) => {
            if let Outcome::Notice(n) = out {
                app.notice(n);
            }
            true
        }
        Err(e) => {
            app.notice(Notice::error(e.text()));
            false
        }
    }
}

fn save(app: &mut DeskApp, draft: EventDraft) -> bool {
    if draft.occurrence.is_some() {
        app.calendar.editor.ask = Some(Pending::Save(Box::new(draft)));
        return true;
    }
    let r = app.session.save_event_draft(&draft, SeriesScope::All);
    finish(app, r)
}

/// Dibuja lo que esté abierto (aviso, editor, pregunta). Se llama una vez por cuadro.
pub(crate) fn show(app: &mut DeskApp, ctx: &Context) {
    quick_popover(app, ctx);
    full_editor(app, ctx);
    scope_question(app, ctx);
}

// ------------------------------------------------------------------ textos

fn date_label(d: NaiveDate) -> String {
    let m: String = month_name(d.month()).chars().take(3).collect();
    format!("{} {} {m}", weekday_short(d), d.day())
}

fn hm(min: i64) -> String {
    let min = min.clamp(0, DAY_MIN);
    format!("{:02}:{:02}", min / 60, min % 60)
}

fn when_label(d: &EventDraft) -> String {
    if d.all_day {
        if d.end_date > d.start_date {
            format!("{} – {} · {}", date_label(d.start_date), date_label(d.end_date), pick("all day", "todo el día"))
        } else {
            format!("{} · {}", date_label(d.start_date), pick("all day", "todo el día"))
        }
    } else if d.end_date == d.start_date || (d.end_min == DAY_MIN && d.end_date == d.start_date) {
        format!("{} · {}–{}", date_label(d.start_date), hm(d.start_min), hm(d.end_min))
    } else {
        format!("{} {} – {} {}", date_label(d.start_date), hm(d.start_min), date_label(d.end_date), hm(d.end_min))
    }
}

fn duration_label(min: i64) -> String {
    if min < 60 {
        format!("{min} min")
    } else if min % 60 == 0 {
        format!("{} h", min / 60)
    } else {
        format!("{} h {} min", min / 60, min % 60)
    }
}

fn calendar_combo(app: &DeskApp, ui: &mut Ui, id: &str, draft: &mut EventDraft, width: f32) {
    let cals = app.session.writable_calendars();
    let current = cals.iter().find(|c| c.id == draft.calendar);
    let text = current.map(|c| c.name.clone()).unwrap_or_else(|| pick("Calendar", "Calendario").to_string());
    let dot = current.and_then(|c| c.color).unwrap_or(0x3B82F6);
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
        ui.painter().circle_filled(r.center(), 5.0, rgb(dot));
        let combo = egui::ComboBox::from_id_salt(id).width(width).selected_text(text).show_ui(ui, |ui| {
            for c in &cals {
                ui.selectable_value(&mut draft.calendar, c.id, &c.name);
            }
        });
        crate::widgets::named(&combo.response, egui::Role::ComboBox, pick("Calendar", "Calendario"));
    });
}

// ------------------------------------------------------------------ crear rápido

fn quick_popover(app: &mut DeskApp, ctx: &Context) {
    let Some(mut q) = app.calendar.editor.quick.take() else { return };
    let screen = ctx.content_rect();
    let w = 300.0;
    let pos = pos2(
        (q.pos.x + 8.0).min(screen.right() - w - 12.0).max(screen.left() + 8.0),
        q.pos.y.clamp(60.0, screen.bottom() - 220.0),
    );
    let mut keep = true;
    let mut more = false;
    let mut do_save = false;
    egui::Area::new(egui::Id::unique("quick-create")).order(egui::Order::Foreground).fixed_pos(pos).show(ctx, |ui| {
        let c = theme::colors(ui);
        egui::Frame::popup(ui.style()).corner_radius(10.0).inner_margin(14.0).show(ui, |ui| {
            ui.set_width(w);
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(pick("New event", "Nuevo evento"))
                        .font(theme::bold_font(ui.ctx(), 14.0))
                        .color(c.text),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if icons::icon_button(ui, Icon::Close, pick("Close", "Cerrar"), true).clicked() {
                        keep = false;
                    }
                });
            });
            ui.add_space(6.0);
            let t = ui.add(
                egui::TextEdit::singleline(&mut q.draft.title)
                    .hint_text(pick("Add a title", "Añade un título"))
                    .font(theme::medium_font(ui.ctx(), 15.0))
                    .desired_width(f32::INFINITY),
            );
            if !q.focused {
                t.request_focus();
                q.focused = true;
            }
            if t.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                do_save = true;
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                icons::paint(ui.painter(), r, Icon::Clock, c.text_muted, c.accent);
                ui.label(RichText::new(when_label(&q.draft)).color(c.text));
            });
            ui.add_space(4.0);
            calendar_combo(app, ui, "quick-cal", &mut q.draft, 200.0);
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button(pick("More options", "Más opciones")).clicked() {
                    more = true;
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if primary_button(ui, pick("Save", "Guardar")).clicked() {
                        do_save = true;
                    }
                });
            });
        });
    });
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        keep = false;
    }
    if more {
        open(app, q.draft);
        return;
    }
    if do_save {
        if save(app, q.draft) {
            return;
        }
        return;
    }
    if keep {
        app.calendar.editor.quick = Some(q);
    }
}

// ------------------------------------------------------------------ editor completo

fn full_editor(app: &mut DeskApp, ctx: &Context) {
    let Some(mut f) = app.calendar.editor.full.take() else { return };
    let mut open_flag = true;
    let mut action: Option<&'static str> = None;
    let title = if f.draft.id.is_some() { pick("Event", "Evento") } else { pick("New event", "Nuevo evento") };
    egui::Window::new(title)
        .id(egui::Id::unique("event-editor"))
        .open(&mut open_flag)
        .collapsible(false)
        .resizable(true)
        .default_width(620.0)
        .default_height(640.0)
        .max_height(ctx.content_rect().height() - 40.0)
        .default_pos(ctx.content_rect().center() - vec2(310.0, 320.0))
        .show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, true])
                .max_height(ctx.content_rect().height() - 150.0)
                .show(ui, |ui| {
                    ui.add_enabled_ui(!f.read_only, |ui| editor_body(app, ui, &mut f));
                });
            ui.separator();
            ui.horizontal(|ui| {
                if f.draft.id.is_some() && !f.read_only {
                    let c = theme::colors(ui);
                    if ui.button(RichText::new(pick("Delete", "Eliminar")).color(c.danger)).clicked() {
                        action = Some("delete");
                    }
                }
                if f.read_only {
                    ui.label(
                        RichText::new(pick("Read-only calendar", "Calendario de solo lectura"))
                            .color(theme::colors(ui).text_muted),
                    );
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if !f.read_only && primary_button(ui, pick("Save", "Guardar")).clicked() {
                        action = Some("save");
                    }
                    if ui.button(pick("Cancel", "Cancelar")).clicked() {
                        action = Some("cancel");
                    }
                });
            });
        });
    if !open_flag {
        action = Some("cancel");
    }
    match action {
        Some("cancel") => {}
        Some("save") => {
            if f.draft.occurrence.is_some() {
                // Se pregunta el alcance; el editor sigue abierto hasta guardar.
                app.calendar.editor.ask = Some(Pending::Save(Box::new(f.draft.clone())));
                app.calendar.editor.full = Some(f);
            } else if !save(app, f.draft.clone()) {
                app.calendar.editor.full = Some(f);
            }
        }
        Some("delete") => {
            if let Some(o) = f.occ.clone() {
                if o.recurring {
                    app.calendar.editor.ask = Some(Pending::Delete(Box::new(o)));
                } else {
                    let r = app.session.delete_occurrence(&o, SeriesScope::All);
                    finish(app, r);
                }
            }
        }
        _ => app.calendar.editor.full = Some(f),
    }
}

fn row_icon(ui: &mut Ui, ic: Icon) {
    let c = theme::colors(ui);
    let (r, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
    icons::paint(ui.painter(), r, ic, c.text_muted, c.accent);
}

fn time_combo(ui: &mut Ui, id: (&str, u8), min: &mut i64, end: bool) {
    let mut v = *min;
    let combo = egui::ComboBox::from_id_salt(id).width(74.0).selected_text(hm(v)).height(280.0).show_ui(ui, |ui| {
        let from = if end { 15 } else { 0 };
        let to = if end { DAY_MIN } else { DAY_MIN - 15 };
        let mut m = from;
        while m <= to {
            ui.selectable_value(&mut v, m, hm(m));
            m += 15;
        }
    });
    crate::widgets::named(&combo.response, egui::Role::ComboBox, pick("Time", "Hora"));
    *min = v;
}

fn date_field(ui: &mut Ui, text: &mut String, value: &mut NaiveDate) -> bool {
    let r = ui.add(egui::TextEdit::singleline(text).desired_width(96.0).hint_text(pick("YYYY-MM-DD", "AAAA-MM-DD")));
    crate::widgets::named(&r, egui::Role::TextInput, pick("Date", "Fecha"));
    if r.changed()
        && let Ok(d) = NaiveDate::parse_from_str(text.trim(), "%Y-%m-%d")
    {
        *value = d;
        return true;
    }
    if r.lost_focus() {
        *text = value.format("%Y-%m-%d").to_string();
    }
    false
}

fn editor_body(app: &mut DeskApp, ui: &mut Ui, f: &mut Full) {
    let c = theme::colors(ui);
    let zone = app.session.zone();
    ui.spacing_mut().item_spacing.y = 8.0;
    let t = ui.add(
        egui::TextEdit::singleline(&mut f.draft.title)
            .hint_text(pick("Add a title", "Añade un título"))
            .font(theme::bold_font(ui.ctx(), 18.0))
            .desired_width(f32::INFINITY),
    );
    crate::widgets::named(&t, egui::Role::TextInput, pick("Title", "Título"));
    if !f.focused {
        if f.draft.id.is_none() {
            t.request_focus();
        }
        f.focused = true;
    }
    ui.horizontal(|ui| {
        row_icon(ui, Icon::Pin);
        let r = ui.add(
            egui::TextEdit::singleline(&mut f.draft.location)
                .hint_text(pick("Add a location or video link", "Añade un lugar o enlace de videollamada"))
                .desired_width(f32::INFINITY),
        );
        crate::widgets::named(&r, egui::Role::TextInput, pick("Location", "Lugar"));
    });

    // Cuándo.
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        row_icon(ui, Icon::Clock);
        ui.checkbox(&mut f.draft.all_day, pick("All day", "Todo el día"));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if !f.draft.all_day {
                let current = f.draft.tz.clone();
                let mut chosen = current.clone();
                let combo = egui::ComboBox::from_id_salt("ev-tz")
                    .width(170.0)
                    .selected_text(
                        current
                            .as_deref()
                            .map(tz_label)
                            .unwrap_or_else(|| pick("My time zone", "Mi zona horaria").into()),
                    )
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut chosen, None, pick("My time zone", "Mi zona horaria"));
                        if let Some(cur) = &current
                            && !COMMON_ZONES.contains(&cur.as_str())
                        {
                            ui.selectable_value(&mut chosen, Some(cur.clone()), tz_label(cur));
                        }
                        for z in COMMON_ZONES {
                            ui.selectable_value(&mut chosen, Some(z.to_string()), tz_label(z));
                        }
                    });
                crate::widgets::named(&combo.response, egui::Role::ComboBox, pick("Time zone", "Zona horaria"));
                if chosen != current {
                    // Las horas siguen siendo el mismo instante en la zona nueva.
                    let (s, e) = f.draft.times(zone);
                    f.draft.tz = chosen;
                    let tz = f.draft.zone_tz();
                    let loc = |t: &EventTime| match tz {
                        Some(z) => t.to_utc().with_timezone(&z).naive_local(),
                        None => zone.local(t.to_utc()),
                    };
                    let (ls, le) = (loc(&s), loc(&e));
                    f.draft.start_date = ls.date();
                    f.draft.start_min = iuredesk_engine::calendar::drag::minutes_of(ls);
                    f.draft.end_date = le.date();
                    f.draft.end_min = iuredesk_engine::calendar::drag::minutes_of(le);
                    f.start_text = f.draft.start_date.format("%Y-%m-%d").to_string();
                    f.end_text = f.draft.end_date.format("%Y-%m-%d").to_string();
                }
            }
        });
    });
    ui.horizontal(|ui| {
        ui.add_space(26.0);
        let mut sd = f.draft.start_date;
        if date_field(ui, &mut f.start_text, &mut sd) {
            f.draft.set_start_keep_length(sd, f.draft.start_min);
            f.end_text = f.draft.end_date.format("%Y-%m-%d").to_string();
        }
        if !f.draft.all_day {
            let mut sm = f.draft.start_min;
            time_combo(ui, ("ev-start", 0), &mut sm, false);
            if sm != f.draft.start_min {
                f.draft.set_start_keep_length(f.draft.start_date, sm);
                f.end_text = f.draft.end_date.format("%Y-%m-%d").to_string();
            }
        }
        ui.label(RichText::new("→").color(c.text_muted));
        let mut ed = f.draft.end_date;
        if date_field(ui, &mut f.end_text, &mut ed) {
            f.draft.end_date = ed;
        }
        if !f.draft.all_day {
            time_combo(ui, ("ev-end", 1), &mut f.draft.end_min, true);
            let len = f.draft.length_min();
            if len > 0 {
                ui.label(RichText::new(duration_label(len)).color(c.text_muted));
            }
        }
    });
    if let Some(p) = f.draft.problem(zone) {
        ui.horizontal(|ui| {
            ui.add_space(26.0);
            ui.label(RichText::new(p.get()).color(c.danger));
        });
    }
    repeat_editor(ui, f);

    // Recordatorios.
    ui.horizontal_wrapped(|ui| {
        row_icon(ui, Icon::Bell);
        let mut remove = None;
        for (i, m) in f.draft.reminders.iter_mut().enumerate() {
            let combo =
                egui::ComboBox::from_id_salt(("ev-rem", i)).selected_text(reminder_label(*m)).show_ui(ui, |ui| {
                    for choice in REMINDER_CHOICES {
                        ui.selectable_value(m, choice, reminder_label(choice));
                    }
                });
            crate::widgets::named(&combo.response, egui::Role::ComboBox, pick("Reminder", "Recordatorio"));
            if icons::icon_button(ui, Icon::Close, pick("Remove reminder", "Quitar recordatorio"), true).clicked() {
                remove = Some(i);
            }
        }
        if let Some(i) = remove {
            f.draft.reminders.remove(i);
        }
        if f.draft.reminders.len() < 5 && ui.button(pick("+ Reminder", "+ Recordatorio")).clicked() {
            let next = REMINDER_CHOICES.iter().copied().find(|m| !f.draft.reminders.contains(m)).unwrap_or(15);
            f.draft.reminders.push(next);
        }
        if f.draft.reminders.is_empty() {
            ui.label(RichText::new(pick("No reminders", "Sin recordatorios")).color(c.text_muted));
        }
    });

    // Invitados.
    ui.add_space(4.0);
    attendees_editor(app, ui, f);
    if !f.draft.attendees.is_empty() {
        scheduling_assistant(app, ui, f);
    }

    // Detalles.
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        row_icon(ui, Icon::Calendar);
        calendar_combo(app, ui, "ev-cal", &mut f.draft, 180.0);
        ui.add_space(8.0);
        let combo = egui::ComboBox::from_id_salt("ev-busy")
            .selected_text(if f.draft.busy {
                pick("Show as busy", "Mostrar como ocupado")
            } else {
                pick("Show as free", "Mostrar como disponible")
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut f.draft.busy, true, pick("Busy", "Ocupado"));
                ui.selectable_value(&mut f.draft.busy, false, pick("Free", "Disponible"));
            });
        crate::widgets::named(&combo.response, egui::Role::ComboBox, pick("Show as", "Mostrar como"));
        ui.add_space(8.0);
        ui.checkbox(&mut f.draft.private, pick("Private", "Privado"));
    });

    // Descripción.
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        row_icon(ui, Icon::Notes);
        section_label(ui, pick("Description", "Descripción"));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.selectable_value(&mut f.preview, true, pick("Preview", "Vista previa"));
            ui.selectable_value(&mut f.preview, false, pick("Edit", "Editar"));
        });
    });
    if f.preview {
        egui::Frame::new().fill(c.surface_alt).corner_radius(6.0).inner_margin(10.0).show(ui, |ui| {
            ui.set_width(ui.available_width());
            markdown_preview(ui, &f.draft.description);
        });
    } else {
        let r = ui.add(
            egui::TextEdit::multiline(&mut f.draft.description)
                .hint_text(pick(
                    "Notes (Markdown: **bold**, - lists, # headings)",
                    "Notas (Markdown: **negrita**, - listas, # títulos)",
                ))
                .desired_rows(4)
                .desired_width(f32::INFINITY),
        );
        crate::widgets::named(&r, egui::Role::TextInput, pick("Description", "Descripción"));
    }
}

fn repeat_editor(ui: &mut Ui, f: &mut Full) {
    let c = theme::colors(ui);
    let start = f.draft.start_date;
    let preset = f.draft.repeat.preset(start);
    ui.horizontal(|ui| {
        row_icon(ui, Icon::Sync);
        let mut p = preset;
        let combo = egui::ComboBox::from_id_salt("ev-repeat").width(220.0).selected_text(p.label()).show_ui(ui, |ui| {
            for x in Preset::ALL {
                ui.selectable_value(&mut p, x, x.label());
            }
        });
        crate::widgets::named(&combo.response, egui::Role::ComboBox, pick("Repeat", "Repetir"));
        if p != preset {
            f.draft.repeat = f.draft.repeat.from_preset(p, start);
        }
        if f.draft.repeat.freq != Freq::None {
            ui.label(RichText::new(f.draft.repeat.describe(start)).color(c.text_muted));
        }
    });
    if preset != Preset::Custom && f.draft.repeat.preset(start) != Preset::Custom {
        return;
    }
    let r: &mut Repeat = &mut f.draft.repeat;
    if r.raw.is_some() {
        return;
    }
    egui::Frame::new().fill(c.surface_alt).corner_radius(6.0).inner_margin(10.0).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(pick("Every", "Cada"));
            let mut n = r.interval.max(1);
            ui.add(egui::DragValue::new(&mut n).range(1..=99));
            r.interval = n;
            let combo = egui::ComboBox::from_id_salt("ev-freq")
                .selected_text(match r.freq {
                    Freq::Daily => pick("days", "días"),
                    Freq::Weekly => pick("weeks", "semanas"),
                    Freq::Monthly => pick("months", "meses"),
                    Freq::Yearly => pick("years", "años"),
                    Freq::None => "—",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut r.freq, Freq::Daily, pick("days", "días"));
                    ui.selectable_value(&mut r.freq, Freq::Weekly, pick("weeks", "semanas"));
                    ui.selectable_value(&mut r.freq, Freq::Monthly, pick("months", "meses"));
                    ui.selectable_value(&mut r.freq, Freq::Yearly, pick("years", "años"));
                });
            crate::widgets::named(&combo.response, egui::Role::ComboBox, pick("Frequency", "Frecuencia"));
        });
        if r.freq == Freq::Weekly {
            ui.horizontal(|ui| {
                for w in
                    [Weekday::Mon, Weekday::Tue, Weekday::Wed, Weekday::Thu, Weekday::Fri, Weekday::Sat, Weekday::Sun]
                {
                    let on = r.weekdays.contains(&w) || (r.weekdays.is_empty() && w == start.weekday());
                    if ui.selectable_label(on, weekday_letter(w)).clicked() {
                        if r.weekdays.is_empty() {
                            r.weekdays.push(start.weekday());
                        }
                        if on {
                            r.weekdays.retain(|x| *x != w);
                        } else {
                            r.weekdays.push(w);
                        }
                    }
                }
            });
        }
        if r.freq == Freq::Monthly {
            ui.horizontal(|ui| {
                ui.radio_value(
                    &mut r.monthly,
                    MonthlyBy::Date,
                    format!("{} {}", pick("On day", "El día"), start.day()),
                );
                let nth =
                    Repeat { freq: Freq::Monthly, monthly: MonthlyBy::NthWeekday, ..Repeat::none() }.describe(start);
                let nth = nth.split_once(' ').map_or(nth.clone(), |(_, rest)| {
                    rest.split_once(' ').map_or(rest.to_string(), |(_, r)| r.to_string())
                });
                ui.radio_value(&mut r.monthly, MonthlyBy::NthWeekday, nth);
            });
        }
        ui.horizontal(|ui| {
            ui.label(pick("Ends", "Termina"));
            let mut kind = match r.end {
                RepeatEnd::Never => 0,
                RepeatEnd::Count(_) => 1,
                RepeatEnd::Until(_) => 2,
            };
            ui.radio_value(&mut kind, 0, pick("never", "nunca"));
            ui.radio_value(&mut kind, 1, pick("after", "después de"));
            let mut count = if let RepeatEnd::Count(n) = r.end { n } else { 10 };
            if kind == 1 {
                ui.add(egui::DragValue::new(&mut count).range(1..=999).suffix(pick(" times", " veces")));
            }
            ui.radio_value(&mut kind, 2, pick("on", "el"));
            let mut until = if let RepeatEnd::Until(d) = r.end { d } else { start + Duration::days(90) };
            if kind == 2 {
                if f.until_text.is_empty() {
                    f.until_text = until.format("%Y-%m-%d").to_string();
                }
                date_field(ui, &mut f.until_text, &mut until);
            }
            r.end = match kind {
                1 => RepeatEnd::Count(count),
                2 => RepeatEnd::Until(until),
                _ => RepeatEnd::Never,
            };
        });
    });
}

fn attendees_editor(app: &mut DeskApp, ui: &mut Ui, f: &mut Full) {
    let c = theme::colors(ui);
    let mut add: Option<Mailbox> = None;
    ui.horizontal(|ui| {
        row_icon(ui, Icon::Person);
        let r = ui.add(
            egui::TextEdit::singleline(&mut f.attendee)
                .hint_text(pick("Invite people (name or email)", "Invitar personas (nombre o correo)"))
                .desired_width(ui.available_width() - 110.0),
        );
        crate::widgets::named(&r, egui::Role::TextInput, pick("Invite people", "Invitar personas"));
        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && f.attendee.contains('@') {
            add = Some(Mailbox { name: None, addr: f.attendee.trim().to_string() });
        }
        ui.checkbox(&mut f.optional, pick("Optional", "Opcional"));
    });
    let q = f.attendee.trim().to_lowercase();
    if q.len() >= 2 {
        let matches: Vec<Mailbox> = app
            .session
            .contacts()
            .iter()
            .filter_map(|ct| {
                let email = ct.emails.first()?.value.clone();
                let hit = ct.full_name.to_lowercase().contains(&q) || email.to_lowercase().contains(&q);
                hit.then(|| Mailbox { name: Some(ct.full_name.clone()), addr: email })
            })
            .take(5)
            .collect();
        for m in matches {
            ui.horizontal(|ui| {
                ui.add_space(26.0);
                let label = format!("{} <{}>", m.name.clone().unwrap_or_default(), m.addr);
                if ui.selectable_label(false, label).clicked() {
                    add = Some(m.clone());
                }
            });
        }
    }
    if let Some(m) = add {
        f.draft.add_attendee(m, f.optional);
        f.attendee.clear();
    }
    let mut remove = None;
    for (i, a) in f.draft.attendees.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            ui.add_space(26.0);
            let name = a.mailbox.name.clone().filter(|n| !n.is_empty()).unwrap_or_else(|| a.mailbox.addr.clone());
            iurefficient_ui::avatar(ui, &name, &a.mailbox.addr, 22.0);
            ui.label(RichText::new(&name).color(c.text));
            ui.label(RichText::new(partstat_label(&a.partstat)).color(c.text_muted).size(12.0));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if icons::icon_button(ui, Icon::Close, pick("Remove", "Quitar"), true).clicked() {
                    remove = Some(i);
                }
                let mut opt = a.role == AttendeeRole::Optional;
                let combo = egui::ComboBox::from_id_salt(("ev-role", i))
                    .width(96.0)
                    .selected_text(if opt { pick("Optional", "Opcional") } else { pick("Required", "Obligatorio") })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut opt, false, pick("Required", "Obligatorio"));
                        ui.selectable_value(&mut opt, true, pick("Optional", "Opcional"));
                    });
                crate::widgets::named(&combo.response, egui::Role::ComboBox, pick("Role", "Papel"));
                a.role = if opt { AttendeeRole::Optional } else { AttendeeRole::Required };
            });
        });
    }
    if let Some(i) = remove {
        f.draft.attendees.remove(i);
    }
}

/// Asistente de programación: mi fila y la de cada invitado, el hueco elegido y propuestas.
fn scheduling_assistant(app: &mut DeskApp, ui: &mut Ui, f: &mut Full) {
    let c = theme::colors(ui);
    let zone = app.session.zone();
    let day = f.draft.start_date;
    let emails: Vec<String> = f.draft.attendees.iter().map(|a| a.mailbox.addr.clone()).collect();
    app.session.request_free_busy(f.draft.calendar, &emails, day);
    let mine = app.session.my_availability(day, f.draft.id);
    let mut rows: Vec<(String, Availability)> = vec![(pick("Me", "Yo").to_string(), mine)];
    for a in &f.draft.attendees {
        let name = a.mailbox.name.clone().filter(|n| !n.is_empty()).unwrap_or_else(|| a.mailbox.addr.clone());
        rows.push((name, app.session.availability(&a.mailbox.addr, day)));
    }
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        ui.add_space(26.0);
        section_label(ui, &format!("{} · {}", pick("Availability", "Disponibilidad"), date_label(day)));
    });
    let (h0, h1) = (7i64, 21i64);
    let label_w = 150.0;
    let width = ui.available_width() - 30.0;
    let bar_w = (width - label_w).max(120.0);
    let row_h = 20.0;
    let (rect, _) = ui.allocate_exact_size(vec2(width + 26.0, 16.0 + row_h * rows.len() as f32), Sense::hover());
    let p = ui.painter_at(rect);
    let x0 = rect.left() + 26.0 + label_w;
    let x_of = |min: i64| x0 + bar_w * ((min - h0 * 60) as f32 / ((h1 - h0) * 60) as f32).clamp(0.0, 1.0);
    for h in h0..=h1 {
        let x = x_of(h * 60);
        p.vline(x, (rect.top() + 14.0)..=rect.bottom(), Stroke::new(1.0, mix(c.surface, c.separator, 0.6)));
        if h % 2 == 1 && h < h1 {
            p.text(
                pos2(x, rect.top() + 6.0),
                egui::Align2::CENTER_CENTER,
                format!("{h}"),
                egui::FontId::proportional(10.0),
                c.text_muted,
            );
        }
    }
    let day_start = zone.day_start(day);
    let min_of = |t: chrono::DateTime<chrono::Utc>| (t - day_start).num_minutes();
    for (i, (name, av)) in rows.iter().enumerate() {
        let y = rect.top() + 16.0 + row_h * i as f32;
        p.text(
            pos2(rect.left() + 26.0, y + row_h / 2.0),
            egui::Align2::LEFT_CENTER,
            name,
            egui::FontId::proportional(12.0),
            c.text,
        );
        let lane = Rect::from_min_max(pos2(x0, y + 3.0), pos2(x0 + bar_w, y + row_h - 3.0));
        p.rect_filled(lane, 3.0, c.surface_alt);
        match av {
            Availability::Busy(b) => {
                for (s, e, tentative) in b {
                    let r =
                        Rect::from_min_max(pos2(x_of(min_of(*s)), lane.top()), pos2(x_of(min_of(*e)), lane.bottom()));
                    if r.width() > 0.5 {
                        let col =
                            if *tentative { mix(c.surface, c.accent, 0.35) } else { mix(c.surface, c.accent, 0.75) };
                        p.rect_filled(r, 2.0, col);
                    }
                }
            }
            Availability::Unknown => {
                p.text(
                    lane.left_center() + vec2(8.0, 0.0),
                    egui::Align2::LEFT_CENTER,
                    pick("availability unknown", "disponibilidad desconocida"),
                    egui::FontId::proportional(11.0),
                    c.text_muted,
                );
            }
            Availability::Loading => {
                p.text(
                    lane.left_center() + vec2(8.0, 0.0),
                    egui::Align2::LEFT_CENTER,
                    pick("checking…", "consultando…"),
                    egui::FontId::proportional(11.0),
                    c.text_muted,
                );
            }
        }
    }
    if !f.draft.all_day {
        let r = Rect::from_min_max(
            pos2(x_of(f.draft.start_min), rect.top() + 14.0),
            pos2(x_of(f.draft.start_min + f.draft.length_min().max(15)), rect.bottom()),
        );
        p.rect_stroke(r, 3.0, Stroke::new(2.0, c.accent), egui::StrokeKind::Outside);
    }
    let people: Vec<Availability> = rows.into_iter().map(|(_, a)| a).collect();
    let len = f.draft.length_min().max(15);
    let now = app.session.now();
    let slots = suggest(&people, day, len, zone, now, 4);
    ui.horizontal_wrapped(|ui| {
        ui.add_space(26.0);
        ui.label(RichText::new(pick("Suggested:", "Sugerencias:")).color(c.text_muted));
        if slots.is_empty() {
            ui.label(
                RichText::new(pick("no common gap in the next two weeks", "ningún hueco común en dos semanas"))
                    .color(c.text_muted),
            );
        }
        for s in slots {
            let l = zone.local(s);
            let label = format!("{} {}", date_label(l.date()), l.format("%H:%M"));
            if ui.button(label).clicked() {
                f.draft.set_start_keep_length(l.date(), iuredesk_engine::calendar::drag::minutes_of(l));
                f.start_text = f.draft.start_date.format("%Y-%m-%d").to_string();
                f.end_text = f.draft.end_date.format("%Y-%m-%d").to_string();
            }
        }
    });
}

/// Vista previa sencilla de Markdown: títulos, listas y **negrita** / *cursiva*.
fn markdown_preview(ui: &mut Ui, text: &str) {
    let c = theme::colors(ui);
    if text.trim().is_empty() {
        ui.label(RichText::new(pick("No description", "Sin descripción")).color(c.text_muted));
        return;
    }
    for line in text.lines().take(400) {
        let t = line.trim_end();
        if let Some(h) = t.strip_prefix("# ").or_else(|| t.strip_prefix("## ")) {
            ui.label(RichText::new(h).font(theme::bold_font(ui.ctx(), 15.0)).color(c.text));
            continue;
        }
        let (bullet, body) = match t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
            Some(b) => (true, b),
            None => (false, t),
        };
        let mut job = egui::text::LayoutJob::default();
        if bullet {
            job.append("•  ", 0.0, egui::TextFormat::simple(theme::body_font(ui.ctx()), c.text));
        }
        let mut bold = false;
        let mut italic = false;
        let mut buf = String::new();
        let flush = |job: &mut egui::text::LayoutJob, buf: &mut String, bold: bool, italic: bool| {
            if buf.is_empty() {
                return;
            }
            let font = if bold { theme::bold_font(ui.ctx(), 14.0) } else { theme::body_font(ui.ctx()) };
            let mut fmt = egui::TextFormat::simple(font, c.text);
            fmt.italics = italic;
            job.append(buf, 0.0, fmt);
            buf.clear();
        };
        let mut chars = body.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch == '*' && chars.peek() == Some(&'*') {
                chars.next();
                flush(&mut job, &mut buf, bold, italic);
                bold = !bold;
            } else if ch == '*' || ch == '_' {
                flush(&mut job, &mut buf, bold, italic);
                italic = !italic;
            } else {
                buf.push(ch);
            }
        }
        flush(&mut job, &mut buf, bold, italic);
        job.wrap.max_width = ui.available_width();
        ui.label(job);
    }
}

// ------------------------------------------------------------------ series

fn scope_question(app: &mut DeskApp, ctx: &Context) {
    let Some(pending) = app.calendar.editor.ask.take() else { return };
    let mut choice: Option<Option<SeriesScope>> = None;
    let (title, body) = match &pending {
        Pending::Delete(_) => (
            pick("Delete a repeating event", "Eliminar un evento que se repite"),
            pick("Which events do you want to delete?", "¿Qué eventos quieres eliminar?"),
        ),
        _ => (
            pick("Change a repeating event", "Cambiar un evento que se repite"),
            pick("Which events do you want to change?", "¿Qué eventos quieres cambiar?"),
        ),
    };
    egui::Modal::new(egui::Id::unique("series-scope")).show(ctx, |ui| {
        let c = theme::colors(ui);
        ui.set_width(360.0);
        ui.label(RichText::new(title).font(theme::bold_font(ui.ctx(), 16.0)).color(c.text));
        ui.add_space(4.0);
        ui.label(RichText::new(body).color(c.text_muted));
        ui.add_space(12.0);
        let w = ui.available_width();
        for (scope, label) in [
            (SeriesScope::This, pick("Only this event", "Solo este evento")),
            (SeriesScope::ThisAndFollowing, pick("This and following events", "Este y los siguientes")),
            (SeriesScope::All, pick("All events in the series", "Toda la serie")),
        ] {
            if ui.add_sized([w, 30.0], egui::Button::new(label)).clicked() {
                choice = Some(Some(scope));
            }
        }
        ui.add_space(6.0);
        if ui.add_sized([w, 26.0], egui::Button::new(pick("Cancel", "Cancelar")).fill(Color32::TRANSPARENT)).clicked() {
            choice = Some(None);
        }
    });
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        choice = Some(None);
    }
    match choice {
        None => app.calendar.editor.ask = Some(pending),
        Some(None) => {}
        Some(Some(scope)) => {
            let r = match pending {
                Pending::Save(d) => {
                    let r = app.session.save_event_draft(&d, scope);
                    if matches!(&r, Ok(Outcome::Notice(n)) if n.kind != iuredesk_engine::NoticeKind::Error) {
                        app.calendar.editor.full = None;
                    }
                    r
                }
                Pending::Reschedule { occ, start, end } => app.session.reschedule(&occ, start, end, scope),
                Pending::Delete(o) => app.session.delete_occurrence(&o, scope),
            };
            finish(app, r);
        }
    }
}

// ------------------------------------------------------------------ pruebas y capturas

impl DeskApp {
    /// Cambia la vista del calendario (`day`, `work_week`, `week`, `month`, `agenda`).
    pub fn set_calendar_view(&mut self, key: &str) {
        if let Some(v) = iuredesk_engine::calendar::nav::CalView::from_key(key) {
            self.session.set_cal_view(v);
        }
    }

    /// Abre el editor sobre una cita de esta semana con invitados (capturas y pruebas).
    pub fn demo_event_editor(&mut self) -> bool {
        let zone = self.session.zone();
        let mon = iuredesk_engine::time::monday_of(self.session.today());
        let occ = self.session.visible_occurrences(zone.day_start(mon), zone.day_start(mon + Duration::days(5)));
        let found = occ.iter().find(|o| {
            !o.recurring
                && !o.all_day
                && self.session.calendar_event(o.id).is_some_and(|e| e.attendees.len() >= 2 && !e.location.is_empty())
        });
        match found.cloned() {
            Some(o) => {
                open_occurrence(self, &o);
                true
            }
            None => false,
        }
    }

    /// ¿Está abierto el editor de eventos?
    pub fn event_editor_open(&self) -> bool {
        self.calendar.editor.full.is_some()
    }

    /// ¿Está abierto el aviso de crear rápido?
    pub fn quick_create_open(&self) -> bool {
        self.calendar.editor.quick.is_some()
    }
}
