//! «Mi día» (F6.1): la pantalla con la que abre IureDesk. Saludo y fecha, cifras del día, y
//! tarjetas: agenda (línea de «ahora», la próxima cita con su cuenta atrás y «Unirse»), tareas de
//! todas las fuentes con su insignia, correo (importantes, «Necesita respuesta», marcados y la
//! priorización del asistente bajo demanda), Iurefficient y recordatorios (cumpleaños).
//!
//! El resumen lo calcula el motor en segundo plano ([`iuredesk_engine::today`]); aquí solo se
//! pinta. Todo se puede pulsar y lleva al elemento.

use std::collections::HashSet;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use egui::{Align, Color32, CornerRadius, Layout, Margin, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};
use iuredesk_engine::calendar::Occurrence;
use iuredesk_engine::iure::FeedItem;
use iuredesk_engine::today::{self, AgendaItem, DaySummary, DayTask, MailItem, NextState, TaskSource};
use iuredesk_engine::{FolderSel, Module, Notice};
use iuredesk_model::{ContactId, FolderId, TaskId, TaskListId};
use iurefficient_ui::{Icon, PaneKind, icons, pick, split, theme};

use crate::app::DeskApp;

/// Ancho máximo del contenido.
const MAX_WIDTH: f32 = 1180.0;
/// Separación entre tarjetas.
const GAP: f32 = 16.0;
/// Por debajo de este ancho, una sola columna.
const TWO_COLUMNS: f32 = 720.0;
/// Color de la insignia «Iurefficient» (el del calendario de la instancia).
const IURE_COLOR: u32 = 0x0F766E;
/// Filas por grupo antes de «+N más».
const ROWS: usize = 5;

/// Estado de la pantalla.
#[derive(Default)]
pub struct TodayState {
    /// Tareas recién terminadas aquí (se ven tachadas hasta que llega el resumen nuevo).
    done: HashSet<TaskId>,
    /// El resumen que se veía al terminarlas.
    seen: Option<Arc<DaySummary>>,
}

/// Qué se pulsó.
enum Act {
    Event(Box<Occurrence>),
    Join(String),
    Calendar,
    Task(TaskListId),
    Complete(Box<DayTask>),
    Tasks,
    Message(Box<MailItem>),
    Inbox(Option<FolderId>),
    Flagged,
    Triage,
    TriageOpen(iuredesk_model::MessageId),
    Iure(Box<FeedItem>),
    IureWeb,
    Contact(ContactId),
}

/// La pantalla.
pub fn show(app: &mut DeskApp, ui: &mut Ui) {
    let summary = app.session.day_summary();
    let now = app.session.now();
    if let (Some(s), Some(old)) = (&summary, &app.today.seen)
        && !Arc::ptr_eq(s, old)
    {
        app.today.done.clear();
        app.today.seen = None;
    }
    // La línea de «ahora» y la cuenta atrás avanzan cada minuto.
    let secs = 60 - now.timestamp().rem_euclid(60);
    ui.ctx().request_repaint_after(std::time::Duration::from_secs(u64::try_from(secs.max(1)).unwrap_or(60)));
    let mut acts: Vec<Act> = Vec::new();
    split::center(ui, PaneKind::Content, |ui| {
        let c = theme::colors(ui);
        ui.painter().rect_filled(ui.max_rect(), 0.0, c.bg);
        egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("today-page").show(ui, |ui| {
            let full = ui.available_width();
            let pad = ((full - MAX_WIDTH) / 2.0).max(if full < 760.0 { 16.0 } else { 28.0 });
            egui::Frame::new().inner_margin(Margin { left: pad as i8, right: pad as i8, top: 22, bottom: 28 }).show(
                ui,
                |ui| {
                    let w = ui.available_width();
                    ui.set_width(w);
                    let Some(s) = summary.as_ref() else {
                        loading(ui);
                        return;
                    };
                    header(app, ui, s, now, &mut acts);
                    ui.add_space(18.0);
                    columns(app, ui, s, now, w, &mut acts);
                },
            );
        });
    });
    for a in acts {
        apply(app, ui.ctx(), a);
    }
}

fn loading(ui: &mut Ui) {
    let c = theme::colors(ui);
    ui.add_space(40.0);
    ui.vertical_centered(|ui| {
        ui.add(egui::Spinner::new().size(18.0));
        ui.add_space(8.0);
        ui.label(RichText::new(pick("Getting your day ready…", "Preparando tu día…")).color(c.text_muted));
    });
}

// ---------------------------------------------------------------------------------------------
// Cabecera

fn header(app: &DeskApp, ui: &mut Ui, s: &DaySummary, now: DateTime<Utc>, acts: &mut Vec<Act>) {
    let c = theme::colors(ui);
    let ctx = ui.ctx().clone();
    let zone = app.session.zone();
    let hour = chrono::Timelike::hour(&zone.local(now));
    let hello = today::greeting(hour);
    let title = if s.user.is_empty() { hello.get().to_string() } else { format!("{}, {}", hello.get(), s.user) };
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(30.0, 30.0), Sense::hover());
        icons::paint(ui.painter(), r, Icon::Sun, c.warning, c.warning);
        ui.add_space(4.0);
        ui.add(egui::Label::new(RichText::new(title).font(theme::semibold_font(&ctx, 26.0)).color(c.text)).truncate());
    });
    ui.add_space(2.0);
    ui.label(RichText::new(capitalize(&today::long_day(s.date))).size(15.0).color(c.text_muted));
    ui.add_space(14.0);

    // Cifras del día.
    let timed = s.events.iter().filter(|e| !e.occ.all_day && e.occ.end > now).count();
    let due_today = s.tasks.today.len();
    let overdue = s.tasks.overdue.len();
    let important = s.important_unread();
    let reply = s.needs_reply.len();
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
        let events_label = match timed {
            0 => pick("No more events today", "Sin más citas hoy").to_string(),
            1 => pick("1 event left", "Queda 1 cita").to_string(),
            n => format!("{n} {}", pick("events left", "citas pendientes")),
        };
        if chip(ui, Icon::Calendar, &events_label, None).clicked() {
            acts.push(Act::Calendar);
        }
        let tasks_label = match due_today {
            0 => pick("No tasks due today", "Sin tareas para hoy").to_string(),
            1 => pick("1 task for today", "1 tarea para hoy").to_string(),
            n => format!("{n} {}", pick("tasks for today", "tareas para hoy")),
        };
        if chip(ui, Icon::Tasks, &tasks_label, None).clicked() {
            acts.push(Act::Tasks);
        }
        if overdue > 0 {
            let l = if overdue == 1 {
                pick("1 overdue", "1 vencida").to_string()
            } else {
                format!("{overdue} {}", pick("overdue", "vencidas"))
            };
            if chip(ui, Icon::Alert, &l, Some(c.danger)).clicked() {
                acts.push(Act::Tasks);
            }
        }
        let mail_label = match important {
            0 => format!("{} {}", s.unread(), pick("unread", "sin leer")),
            1 => pick("1 important unread", "1 importante sin leer").to_string(),
            n => format!("{n} {}", pick("important unread", "importantes sin leer")),
        };
        if chip(ui, Icon::Mail, &mail_label, None).clicked() {
            acts.push(Act::Inbox(None));
        }
        if reply > 0 {
            let l = if reply == 1 {
                pick("1 awaiting your reply", "1 espera tu respuesta").to_string()
            } else {
                format!("{reply} {}", pick("awaiting your reply", "esperan tu respuesta"))
            };
            if chip(ui, Icon::Reply, &l, None).clicked() {
                acts.push(Act::Inbox(None));
            }
        }
        if let Some(i) = s.iure.as_ref().filter(|i| i.unread > 0) {
            let l = format!("{} {}", i.unread, pick("Iurefficient notices", "avisos de Iurefficient"));
            if chip(ui, Icon::Bell, &l, None).clicked() {
                acts.push(Act::IureWeb);
            }
        }
    });
}

/// Píldora con icono y texto; `tint` = color de alerta (borde y texto).
fn chip(ui: &mut Ui, ic: Icon, text: &str, tint: Option<Color32>) -> egui::Response {
    let c = theme::colors(ui);
    let font = theme::medium_font(ui.ctx(), theme::density(ui.ctx()).body_size());
    let ink = tint.unwrap_or(c.text);
    let g = ui.painter().layout_no_wrap(text.to_string(), font, ink);
    let h = 30.0;
    let w = g.size().x + 16.0 + 18.0 + 8.0 + 14.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(w, h), Sense::click());
    let fill = if resp.hovered() { c.hover } else { c.surface };
    let border = tint.map_or(c.separator, |t| t.gamma_multiply(0.55));
    ui.painter().rect(rect, CornerRadius::same(15), fill, Stroke::new(1.0, border), egui::StrokeKind::Inside);
    let ir = Rect::from_center_size(pos2(rect.left() + 14.0 + 8.0, rect.center().y), vec2(16.0, 16.0));
    icons::paint(ui.painter(), ir, ic, tint.unwrap_or(c.text_muted), tint.unwrap_or(c.accent));
    ui.painter().galley(pos2(ir.right() + 8.0, rect.center().y - g.size().y / 2.0), g, ink);
    iurefficient_ui::a11y::button(&resp, text);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

// ---------------------------------------------------------------------------------------------
// Columnas

fn columns(app: &mut DeskApp, ui: &mut Ui, s: &DaySummary, now: DateTime<Utc>, w: f32, acts: &mut Vec<Act>) {
    if w < TWO_COLUMNS {
        agenda_card(app, ui, s, now, acts);
        tasks_card(app, ui, s, acts);
        mail_card(app, ui, s, now, acts);
        iure_card(ui, s, now, acts);
        reminders_card(ui, s, acts);
        return;
    }
    let left = ((w - GAP) * if w > 1000.0 { 0.56 } else { 0.52 }).floor();
    let right = (w - GAP - left).floor();
    let spacing = ui.spacing().item_spacing;
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.allocate_ui_with_layout(vec2(left, 0.0), Layout::top_down(Align::Min), |ui| {
            ui.set_width(left);
            ui.spacing_mut().item_spacing = spacing;
            agenda_card(app, ui, s, now, acts);
            mail_card(app, ui, s, now, acts);
        });
        ui.add_space(GAP);
        ui.allocate_ui_with_layout(vec2(right, 0.0), Layout::top_down(Align::Min), |ui| {
            ui.set_width(right);
            ui.spacing_mut().item_spacing = spacing;
            tasks_card(app, ui, s, acts);
            iure_card(ui, s, now, acts);
            reminders_card(ui, s, acts);
        });
    });
}

/// Tarjeta con cabecera (icono, título, cifra) y una acción a la derecha. Devuelve si se pulsó la
/// acción.
fn card(ui: &mut Ui, ic: Icon, title: &str, detail: &str, action: Option<&str>, add: impl FnOnce(&mut Ui)) -> bool {
    let c = theme::colors(ui);
    let ctx = ui.ctx().clone();
    let mut clicked = false;
    egui::Frame::new()
        .fill(c.surface)
        .stroke(Stroke::new(1.0, c.separator))
        .corner_radius(12)
        .inner_margin(Margin { left: 16, right: 16, top: 14, bottom: 14 })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let (r, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
                icons::paint(ui.painter(), r, ic, c.accent, c.accent);
                ui.label(RichText::new(title).font(theme::semibold_font(&ctx, 15.0)).color(c.text));
                if !detail.is_empty() {
                    ui.label(RichText::new(detail).color(c.text_muted));
                }
                if let Some(a) = action {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if link(ui, a).clicked() {
                            clicked = true;
                        }
                    });
                }
            });
            ui.add_space(10.0);
            add(ui);
        });
    ui.add_space(GAP);
    clicked
}

/// Enlace discreto «Abrir calendario ›».
fn link(ui: &mut Ui, text: &str) -> egui::Response {
    let c = theme::colors(ui);
    let r = ui.add(
        egui::Button::new(RichText::new(format!("{text} ›")).color(c.accent)).frame(false).min_size(vec2(0.0, 22.0)),
    );
    if r.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    r
}

/// Fila pulsable con fondo al pasar el ratón. Devuelve su rectángulo y la respuesta.
fn row(ui: &mut Ui, h: f32, label: &str) -> (Rect, egui::Response) {
    let c = theme::colors(ui);
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect.expand2(vec2(6.0, 0.0)), 6.0, c.hover);
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    iurefficient_ui::a11y::button(&resp, label);
    (rect, resp)
}

fn sub_label(ui: &mut Ui, text: &str, count: usize, ink: Color32) {
    ui.add_space(6.0);
    let font = theme::semibold_font(ui.ctx(), theme::density(ui.ctx()).small_size());
    let t = if count > 0 { format!("{}  {count}", text.to_uppercase()) } else { text.to_uppercase() };
    ui.label(RichText::new(t).font(font).color(ink));
    ui.add_space(2.0);
}

fn muted(ui: &mut Ui, text: &str) {
    let c = theme::colors(ui);
    ui.add(egui::Label::new(RichText::new(text).color(c.text_muted)).wrap());
}

/// Estado vacío amable dentro de una tarjeta.
fn quiet(ui: &mut Ui, ic: Icon, title: &str, body: &str) {
    let c = theme::colors(ui);
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::hover());
        icons::paint(ui.painter(), r, ic, c.text_muted, c.warning);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            ui.label(RichText::new(title).strong().color(c.text));
            ui.add(egui::Label::new(RichText::new(body).color(c.text_muted)).wrap());
        });
    });
}

/// Texto en una línea recortado con «…».
fn text_at(ui: &Ui, p: egui::Pos2, s: &str, font: egui::FontId, ink: Color32, max: f32) -> f32 {
    let g = iurefficient_ui::widgets::text::elided(ui, s, font, ink, max.max(0.0));
    let w = g.size().x;
    ui.painter().galley(p, g, ink);
    w
}

fn capitalize(s: &str) -> String {
    let mut ch = s.chars();
    match ch.next() {
        Some(f) => f.to_uppercase().chain(ch).collect(),
        None => String::new(),
    }
}

fn hm(app: &DeskApp, t: DateTime<Utc>) -> String {
    app.session.zone().local(t).format("%H:%M").to_string()
}

// ---------------------------------------------------------------------------------------------
// Agenda

fn agenda_card(app: &DeskApp, ui: &mut Ui, s: &DaySummary, now: DateTime<Utc>, acts: &mut Vec<Act>) {
    let n = s.events.len();
    let detail = if n == 0 { String::new() } else { n.to_string() };
    let open =
        card(ui, Icon::Calendar, pick("Agenda", "Agenda"), &detail, Some(pick("Calendar", "Calendario")), |ui| {
            let c = theme::colors(ui);
            if s.events.is_empty() {
                quiet(
                    ui,
                    Icon::Sun,
                    pick("A clear day", "Día despejado"),
                    pick(
                        "No events today. Time for deep work.",
                        "No tienes citas hoy. Buen momento para concentrarte.",
                    ),
                );
            }
            let next = s.next_event(now);
            if let Some((i, st)) = next
                && let Some(e) = s.events.get(i)
            {
                next_box(app, ui, e, st, acts);
                ui.add_space(10.0);
            } else if !s.events.is_empty() {
                muted(ui, pick("That's all for today.", "Eso es todo por hoy."));
                ui.add_space(6.0);
            }
            // Días completos, en una línea.
            let all_day: Vec<&AgendaItem> = s.events.iter().filter(|e| e.occ.all_day).collect();
            for e in &all_day {
                let (rect, resp) = row(ui, 26.0, &e.occ.summary);
                let pill = Rect::from_min_size(pos2(rect.left(), rect.center().y - 10.0), vec2(rect.width(), 20.0));
                ui.painter().rect_filled(pill, 5.0, crate::widgets::rgb(e.occ.color).gamma_multiply(0.16));
                let font = theme::small_font(ui.ctx());
                let label = format!("{} · {}", pick("All day", "Todo el día"), e.occ.summary);
                text_at(ui, pos2(pill.left() + 8.0, pill.center().y - 7.5), &label, font, c.text, pill.width() - 16.0);
                if resp.clicked() {
                    acts.push(Act::Event(Box::new(e.occ.clone())));
                }
            }
            // La línea de tiempo.
            let timed: Vec<&AgendaItem> = s.events.iter().filter(|e| !e.occ.all_day).collect();
            let mut drew_now = false;
            for e in &timed {
                if !drew_now && e.occ.start > now && s.date == app.session.today() {
                    now_line(app, ui, now);
                    drew_now = true;
                }
                if timeline_row(app, ui, e, now) {
                    acts.push(Act::Event(Box::new(e.occ.clone())));
                }
            }
            if !drew_now && !timed.is_empty() && s.date == app.session.today() {
                now_line(app, ui, now);
            }
            // Mañana.
            if let Some(t) = &s.tomorrow {
                ui.add_space(8.0);
                crate::widgets::hairline(ui);
                ui.add_space(6.0);
                let when =
                    if t.occ.all_day { pick("all day", "todo el día").to_string() } else { hm(app, t.occ.start) };
                let label = format!("{} · {when} · {}", pick("Tomorrow", "Mañana"), t.occ.summary);
                let (rect, resp) = row(ui, 24.0, &label);
                let (ir, x) = (
                    Rect::from_min_size(pos2(rect.left(), rect.center().y - 7.0), vec2(14.0, 14.0)),
                    rect.left() + 22.0,
                );
                icons::paint(ui.painter(), ir, Icon::ChevronRight, c.text_muted, c.text_muted);
                text_at(
                    ui,
                    pos2(x, rect.center().y - 8.0),
                    &label,
                    theme::body_font(ui.ctx()),
                    c.text_muted,
                    rect.right() - x,
                );
                if resp.clicked() {
                    acts.push(Act::Event(Box::new(t.occ.clone())));
                }
            }
        });
    if open {
        acts.push(Act::Calendar);
    }
}

/// La próxima cita, destacada.
fn next_box(app: &DeskApp, ui: &mut Ui, e: &AgendaItem, st: NextState, acts: &mut Vec<Act>) {
    let c = theme::colors(ui);
    let ctx = ui.ctx().clone();
    let now_on = matches!(st, NextState::Now { .. });
    let tint = if now_on { c.success } else { c.accent };
    // La tarjeta registra su clic *antes* de dibujar el contenido: así el botón «Unirse», que
    // queda encima, se lleva su propio clic y no abre además el evento (UX-2).
    let card = ui.scope_builder(egui::UiBuilder::new().sense(Sense::click()), |ui| {
        // Textos no seleccionables: si no, el título se quedaría con el clic de la tarjeta.
        ui.style_mut().interaction.selectable_labels = false;
        egui::Frame::new()
            .fill(tint.gamma_multiply(if c.dark { 0.16 } else { 0.08 }))
            .stroke(Stroke::new(1.0, tint.gamma_multiply(0.35)))
            .corner_radius(10)
            .inner_margin(Margin::symmetric(14, 12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 3.0;
                        let head = if now_on { pick("NOW", "AHORA") } else { pick("NEXT", "SIGUIENTE") };
                        let label = st.label();
                        let rest = match st {
                            NextState::Now { .. } => {
                                label.get().split_once('·').map_or("", |(_, r)| r).trim().to_string()
                            }
                            NextState::Soon { .. } => label.get().to_string(),
                        };
                        ui.label(
                            RichText::new(format!("{head} · {}", rest.to_uppercase()))
                                .font(theme::semibold_font(&ctx, theme::density(&ctx).small_size()))
                                .color(tint),
                        );
                        let w = (ui.available_width() - if e.join.is_some() { 150.0 } else { 0.0 }).max(80.0);
                        let g = iurefficient_ui::widgets::text::elided(
                            ui,
                            &e.occ.summary,
                            theme::semibold_font(&ctx, 17.0),
                            c.text,
                            w,
                        );
                        ui.label(egui::WidgetText::Galley(g));
                        let mut meta = format!("{} – {}", hm(app, e.occ.start), hm(app, e.occ.end));
                        if !e.occ.location.trim().is_empty() && !e.occ.location.starts_with("https://") {
                            meta.push_str(&format!(" · {}", e.occ.location.trim()));
                        }
                        if !e.calendar.is_empty() {
                            meta.push_str(&format!(" · {}", e.calendar));
                        }
                        let g =
                            iurefficient_ui::widgets::text::elided(ui, &meta, theme::body_font(&ctx), c.text_muted, w);
                        ui.label(egui::WidgetText::Galley(g));
                    });
                    if let Some(j) = &e.join {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if crate::meeting::join_button(ui, j, true) {
                                acts.push(Act::Join(j.url.clone()));
                            }
                        });
                    }
                });
            });
    });
    let resp = card.response;
    iurefficient_ui::a11y::button(&resp, &e.occ.summary);
    if resp.clicked() && !acts.iter().any(|a| matches!(a, Act::Join(_))) {
        acts.push(Act::Event(Box::new(e.occ.clone())));
    }
}

/// Una cita de la línea de tiempo: hora, barra de color, título y lugar. Devuelve si se pulsó.
fn timeline_row(app: &DeskApp, ui: &mut Ui, e: &AgendaItem, now: DateTime<Utc>) -> bool {
    let c = theme::colors(ui);
    let ctx = ui.ctx().clone();
    let past = e.occ.end <= now;
    let ongoing = e.occ.start <= now && now < e.occ.end;
    let h = 42.0;
    let (rect, resp) = row(ui, h, &e.occ.summary);
    if ongoing {
        ui.painter().rect_filled(rect.expand2(vec2(6.0, 0.0)), 6.0, c.success.gamma_multiply(0.08));
    }
    let ink = if past { c.text_muted } else { c.text };
    let tab = theme::tabular_font(&ctx, theme::density(&ctx).body_size());
    text_at(ui, pos2(rect.left(), rect.top() + 5.0), &hm(app, e.occ.start), tab.clone(), ink, 48.0);
    let small = theme::tabular_font(&ctx, theme::density(&ctx).small_size());
    text_at(ui, pos2(rect.left(), rect.top() + 23.0), &hm(app, e.occ.end), small, c.text_muted, 48.0);
    let bar = Rect::from_min_size(pos2(rect.left() + 52.0, rect.top() + 6.0), vec2(3.0, h - 12.0));
    let col = crate::widgets::rgb(e.occ.color);
    ui.painter().rect_filled(bar, 1.5, if past { col.gamma_multiply(0.45) } else { col });
    let x = bar.right() + 10.0;
    let mut right = rect.right();
    if let Some(j) = &e.join {
        let g = ui.painter().layout_no_wrap(j.provider.to_string(), theme::small_font(&ctx), c.accent);
        let w = g.size().x;
        let ir = Rect::from_min_size(pos2(right - w - 18.0, rect.top() + 6.0), vec2(14.0, 14.0));
        icons::paint(ui.painter(), ir, Icon::Video, c.accent, c.accent);
        ui.painter().galley(pos2(right - w, rect.top() + 6.0), g, c.accent);
        right -= w + 26.0;
    }
    if e.iurefficient {
        right -= tag(ui, pos2(right, rect.top() + 5.0), "Iurefficient", crate::widgets::rgb(IURE_COLOR)) + 6.0;
    }
    let title_font =
        if past { theme::body_font(&ctx) } else { theme::medium_font(&ctx, theme::density(&ctx).body_size()) };
    text_at(ui, pos2(x, rect.top() + 4.0), &e.occ.summary, title_font, ink, right - x);
    let mut meta = Vec::new();
    if !e.occ.location.trim().is_empty() && !e.occ.location.starts_with("https://") {
        meta.push(e.occ.location.trim().to_string());
    }
    if !e.calendar.is_empty() && !e.iurefficient {
        meta.push(e.calendar.clone());
    }
    if !meta.is_empty() {
        text_at(
            ui,
            pos2(x, rect.top() + 23.0),
            &meta.join(" · "),
            theme::small_font(&ctx),
            c.text_muted,
            rect.right() - x,
        );
    }
    resp.clicked()
}

/// Insignia pequeña alineada a la derecha en `right_top`; devuelve su ancho.
fn tag(ui: &Ui, right_top: egui::Pos2, text: &str, col: Color32) -> f32 {
    let c = theme::colors(ui);
    let font = theme::medium_font(ui.ctx(), (theme::density(ui.ctx()).small_size() - 0.5).max(9.0));
    let ink = if c.dark { col.gamma_multiply(1.0).linear_multiply(1.0) } else { col };
    let ink = if c.dark { crate::widgets::mix(ink, Color32::WHITE, 0.45) } else { ink };
    let g = ui.painter().layout_no_wrap(text.to_string(), font, ink);
    let w = g.size().x + 12.0;
    let r = Rect::from_min_size(pos2(right_top.x - w, right_top.y), vec2(w, g.size().y + 4.0));
    ui.painter().rect_filled(r, 5.0, col.gamma_multiply(if c.dark { 0.28 } else { 0.12 }));
    ui.painter().galley(pos2(r.left() + 6.0, r.top() + 2.0), g, ink);
    w
}

/// La línea de «ahora» entre citas.
fn now_line(app: &DeskApp, ui: &mut Ui, now: DateTime<Utc>) {
    let c = theme::colors(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 14.0), Sense::hover());
    let y = rect.center().y;
    let red = c.danger;
    let label = hm(app, now);
    let font = theme::semibold_font(ui.ctx(), theme::density(ui.ctx()).small_size());
    let g = ui.painter().layout_no_wrap(label, font, red);
    ui.painter().galley(pos2(rect.left(), y - g.size().y / 2.0), g, red);
    ui.painter().circle_filled(pos2(rect.left() + 53.5, y), 4.0, red);
    ui.painter().hline((rect.left() + 53.5)..=rect.right(), y, Stroke::new(1.5, red));
}

// ---------------------------------------------------------------------------------------------
// Tareas

fn tasks_card(app: &mut DeskApp, ui: &mut Ui, s: &DaySummary, acts: &mut Vec<Act>) {
    let t = &s.tasks;
    let open = t.overdue.len() + t.today.len();
    let detail = if open == 0 { String::new() } else { open.to_string() };
    let go = card(ui, Icon::Tasks, pick("Tasks", "Tareas"), &detail, Some(pick("All tasks", "Todas")), |ui| {
        let c = theme::colors(ui);
        if t.is_empty() {
            quiet(
                ui,
                Icon::Check,
                pick("Nothing pending", "Nada pendiente"),
                pick("No tasks due this week. Enjoy it.", "No hay tareas para esta semana. Disfrútalo."),
            );
            return;
        }
        for (name, list, ink) in [
            (pick("Overdue", "Vencidas"), &t.overdue, c.danger),
            (pick("Today", "Hoy"), &t.today, c.accent),
            (pick("Next 7 days", "Próximos 7 días"), &t.upcoming, c.text_muted),
        ] {
            if list.is_empty() {
                continue;
            }
            sub_label(ui, name, list.len(), ink);
            for dt in list.iter().take(ROWS) {
                task_row(app, ui, dt, s.date, acts);
            }
            if list.len() > ROWS && link(ui, &format!("+{} {}", list.len() - ROWS, pick("more", "más"))).clicked() {
                acts.push(Act::Tasks);
            }
        }
    });
    if go {
        acts.push(Act::Tasks);
    }
}

fn task_row(app: &DeskApp, ui: &mut Ui, dt: &DayTask, today: chrono::NaiveDate, acts: &mut Vec<Act>) {
    let c = theme::colors(ui);
    let ctx = ui.ctx().clone();
    let done = app.today.done.contains(&dt.task.id);
    let (rect, resp) = row(ui, 34.0, &dt.task.summary);
    // La casilla tiene su propia respuesta (encima de la fila).
    let box_r = Rect::from_center_size(pos2(rect.left() + 10.0, rect.center().y), vec2(18.0, 18.0));
    let check = ui.interact(box_r.expand(4.0), ui.scope_id().with(("today-task", dt.task.id.get())), Sense::click());
    let label = format!("{} {}", pick("Complete", "Terminar"), dt.task.summary);
    iurefficient_ui::a11y::toggle(&check, &label, done);
    if done {
        ui.painter().circle_filled(box_r.center(), 9.0, c.accent);
        icons::paint(ui.painter(), box_r.shrink(3.0), Icon::Check, c.on_accent, c.on_accent);
    } else {
        let ring = if check.hovered() {
            c.accent
        } else if dt.overdue_days > 0 {
            c.danger
        } else {
            c.text_muted
        };
        ui.painter().circle_stroke(box_r.center(), 8.5, Stroke::new(1.5, ring));
    }
    let x = box_r.right() + 10.0;
    let mut right = rect.right();
    // Vencimiento.
    if let Some(due) = dt.due {
        let (txt, col) = if dt.overdue_days == 1 {
            (pick("yesterday", "ayer").to_string(), c.danger)
        } else if dt.overdue_days > 1 {
            (
                format!("{} {} {}", pick("", "hace"), dt.overdue_days, pick("days ago", "días")).trim().to_string(),
                c.danger,
            )
        } else if due == today {
            (pick("today", "hoy").to_string(), c.accent)
        } else if Some(due) == today.succ_opt() {
            (pick("tomorrow", "mañana").to_string(), c.text_muted)
        } else {
            (format!("{} {}", iuredesk_engine::time::weekday_short(due), due.format("%d")), c.text_muted)
        };
        let g = ui.painter().layout_no_wrap(txt, theme::small_font(&ctx), col);
        let w = g.size().x;
        ui.painter().galley(pos2(right - w, rect.center().y - g.size().y / 2.0), g, col);
        right -= w + 10.0;
    }
    // Origen.
    let col = match dt.source {
        TaskSource::Iurefficient => crate::widgets::rgb(IURE_COLOR),
        _ => c.text_muted,
    };
    let tw = tag(ui, pos2(right, rect.center().y - 8.0), &dt.badge, col);
    right -= tw + 10.0;
    if !done && (1..=4).contains(&dt.task.priority) {
        let fr = Rect::from_center_size(pos2(right - 6.0, rect.center().y), vec2(12.0, 12.0));
        icons::paint(ui.painter(), fr, Icon::Flag, c.danger, c.danger);
        right -= 18.0;
    }
    let ink = if done { c.text_muted } else { c.text };
    let w = text_at(ui, pos2(x, rect.center().y - 9.0), &dt.task.summary, theme::body_font(&ctx), ink, right - x);
    if done {
        ui.painter().hline(x..=(x + w), rect.center().y, Stroke::new(1.0, c.text_muted));
    }
    let tip = match &dt.source {
        TaskSource::Iurefficient => "Iurefficient".to_string(),
        TaskSource::Account { email } if !email.is_empty() => format!("{} · {email}", dt.list_name),
        _ => dt.list_name.clone(),
    };
    if check.clicked() {
        if !done {
            acts.push(Act::Complete(Box::new(dt.clone())));
        }
    } else if resp.on_hover_text(tip).clicked() {
        acts.push(Act::Task(dt.list));
    }
}

// ---------------------------------------------------------------------------------------------
// Correo

fn mail_card(app: &mut DeskApp, ui: &mut Ui, s: &DaySummary, now: DateTime<Utc>, acts: &mut Vec<Act>) {
    let unread = s.unread();
    let detail = if unread == 0 { String::new() } else { format!("{unread} {}", pick("unread", "sin leer")) };
    let go = card(ui, Icon::Mail, pick("Mail", "Correo"), &detail, Some(pick("Inbox", "Bandeja")), |ui| {
        let c = theme::colors(ui);
        let ctx = ui.ctx().clone();
        if s.accounts.is_empty() {
            muted(ui, pick("No mail accounts yet.", "Aún no hay cuentas de correo."));
        }
        for a in &s.accounts {
            let label = format!("{} · {} {}", a.email, a.unread, pick("unread", "sin leer"));
            let (rect, resp) = row(ui, 36.0, &label);
            iurefficient_ui::widgets::avatar::paint_avatar(
                ui.painter(),
                pos2(rect.left() + 13.0, rect.center().y),
                26.0,
                &a.name,
                &a.email,
            );
            let x = rect.left() + 36.0;
            let mut right = rect.right();
            if a.important > 0 {
                let t = if a.important == 1 {
                    pick("1 important", "1 importante").to_string()
                } else {
                    format!("{} {}", a.important, pick("important", "importantes"))
                };
                right -= tag(ui, pos2(right, rect.center().y - 8.0), &t, c.warning) + 10.0;
            }
            let t = format!("{} {}", a.unread, pick("unread", "sin leer"));
            let g = ui.painter().layout_no_wrap(t, theme::small_font(&ctx), c.text_muted);
            let gw = g.size().x;
            ui.painter().galley(pos2(right - gw, rect.center().y - g.size().y / 2.0), g, c.text_muted);
            right -= gw + 12.0;
            text_at(ui, pos2(x, rect.center().y - 9.0), &a.email, theme::body_font(&ctx), c.text, right - x);
            if resp.clicked() {
                acts.push(Act::Inbox(a.inbox));
            }
        }
        if !s.needs_reply.is_empty() {
            ui.add_space(4.0);
            sub_label(ui, pick("Needs a reply", "Necesita respuesta"), s.needs_reply.len(), c.text_muted);
            for m in s.needs_reply.iter().take(4) {
                let waited = now - m.date;
                let ago = if waited.num_days() >= 1 {
                    format!("{} {} {}", pick("", "hace"), waited.num_days(), pick("d ago", "d")).trim().to_string()
                } else {
                    format!("{} {} h", pick("", "hace"), waited.num_hours()).trim().to_string()
                };
                if mail_row(ui, m, &ago, Icon::Reply) {
                    acts.push(Act::Message(Box::new(m.clone())));
                }
            }
        }
        if !s.flagged.is_empty() {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                sub_label(ui, pick("Flagged", "Marcados"), s.flagged.len(), c.text_muted);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if s.flagged.len() > 3 && link(ui, pick("See all", "Ver todos")).clicked() {
                        acts.push(Act::Flagged);
                    }
                });
            });
            for m in s.flagged.iter().take(3) {
                let when = iuredesk_engine::time::list_date(m.date, now, app.session.zone());
                if mail_row(ui, m, &when, Icon::Flag) {
                    acts.push(Act::Message(Box::new(m.clone())));
                }
            }
        }
        // Priorización con el asistente: solo bajo demanda (consentimiento y DataScope de siempre).
        ui.add_space(10.0);
        crate::widgets::hairline(ui);
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let running = crate::assistant::triage_running(app);
            let label = pick("What needs my attention today?", "¿Qué necesito atender hoy?");
            if chip(ui, Icon::Sparkle, label, None).clicked() && !running {
                acts.push(Act::Triage);
            }
            if running {
                ui.add(egui::Spinner::new().size(14.0));
            }
        });
        ui.add(
            egui::Label::new(
                RichText::new(pick(
                    "The assistant reads subject, sender and a short excerpt of your unread mail, only when you ask.",
                    "El asistente lee asunto, remitente y un extracto de tu correo sin leer, solo cuando lo pides.",
                ))
                .size(theme::density(&ctx).small_size())
                .color(c.text_muted),
            )
            .wrap(),
        );
        ui.add_space(4.0);
        if let Some(id) = crate::assistant::triage_compact(app, ui, 3) {
            acts.push(Act::TriageOpen(id));
        }
    });
    if go {
        acts.push(Act::Inbox(None));
    }
}

/// Fila de un correo: icono, remitente en negrita, asunto y hora. Devuelve si se pulsó.
fn mail_row(ui: &mut Ui, m: &MailItem, when: &str, ic: Icon) -> bool {
    let c = theme::colors(ui);
    let ctx = ui.ctx().clone();
    let label = format!("{} — {}", m.from, m.subject);
    let (rect, resp) = row(ui, 30.0, &label);
    let ir = Rect::from_center_size(pos2(rect.left() + 8.0, rect.center().y), vec2(14.0, 14.0));
    let tint = if ic == Icon::Flag { c.danger } else { c.text_muted };
    icons::paint(ui.painter(), ir, ic, tint, tint);
    let g = ui.painter().layout_no_wrap(when.to_string(), theme::small_font(&ctx), c.text_muted);
    let gw = g.size().x;
    ui.painter().galley(pos2(rect.right() - gw, rect.center().y - g.size().y / 2.0), g, c.text_muted);
    let x = rect.left() + 24.0;
    let max = rect.right() - gw - 12.0 - x;
    let name_w = (max * 0.38).min(170.0);
    let y = rect.center().y - 9.0;
    let w =
        text_at(ui, pos2(x, y), &m.from, theme::medium_font(&ctx, theme::density(&ctx).body_size()), c.text, name_w);
    text_at(ui, pos2(x + w + 8.0, y), &m.subject, theme::body_font(&ctx), c.text_muted, max - w - 8.0);
    resp.clicked()
}

// ---------------------------------------------------------------------------------------------
// Iurefficient

fn iure_card(ui: &mut Ui, s: &DaySummary, now: DateTime<Utc>, acts: &mut Vec<Act>) {
    let Some(iu) = &s.iure else { return };
    let iure_events = s.events.iter().filter(|e| e.iurefficient && (e.occ.all_day || e.occ.end > now)).count();
    let iure_tasks = [&s.tasks.overdue, &s.tasks.today]
        .iter()
        .flat_map(|l| l.iter())
        .filter(|t| t.source == TaskSource::Iurefficient)
        .count();
    let go = card(ui, Icon::Bell, "Iurefficient", &iu.host, Some(pick("Open", "Abrir")), |ui| {
        let c = theme::colors(ui);
        let ctx = ui.ctx().clone();
        if iure_events + iure_tasks > 0 {
            let ev = match iure_events {
                1 => pick("1 event today", "1 cita hoy").to_string(),
                n => format!("{n} {}", pick("events today", "citas hoy")),
            };
            let ta = match iure_tasks {
                1 => pick("1 task due or overdue", "1 tarea para hoy o vencida").to_string(),
                n => format!("{n} {}", pick("tasks due or overdue", "tareas para hoy o vencidas")),
            };
            let t = format!("{ev} · {ta}");
            muted(ui, &t);
            ui.add_space(4.0);
        }
        if iu.items.is_empty() {
            quiet(ui, Icon::Check, pick("All caught up", "Todo al día"), pick("No new notices.", "Sin avisos nuevos."));
            return;
        }
        for it in iu.items.iter().take(4) {
            let (rect, resp) = row(ui, if it.message.is_some() { 40.0 } else { 26.0 }, &it.title);
            let ir = Rect::from_min_size(pos2(rect.left(), rect.top() + 5.0), vec2(16.0, 16.0));
            let ic = if it.mention { Icon::Comment } else { Icon::Bell };
            icons::paint(ui.painter(), ir, ic, if it.read { c.text_muted } else { c.accent }, c.accent);
            let x = rect.left() + 24.0;
            let font = if it.read {
                theme::body_font(&ctx)
            } else {
                theme::medium_font(&ctx, theme::density(&ctx).body_size())
            };
            text_at(
                ui,
                pos2(x, rect.top() + 3.0),
                &it.title,
                font,
                if it.read { c.text_muted } else { c.text },
                rect.right() - x,
            );
            if let Some(m) = &it.message {
                text_at(ui, pos2(x, rect.top() + 21.0), m, theme::small_font(&ctx), c.text_muted, rect.right() - x);
            }
            if !it.read {
                ui.painter().circle_filled(pos2(rect.right() - 4.0, rect.top() + 12.0), 3.5, c.accent);
            }
            if resp.clicked() {
                acts.push(Act::Iure(Box::new(it.clone())));
            }
        }
    });
    if go {
        acts.push(Act::IureWeb);
    }
}

// ---------------------------------------------------------------------------------------------
// Recordatorios

fn reminders_card(ui: &mut Ui, s: &DaySummary, acts: &mut Vec<Act>) {
    card(ui, Icon::Gift, pick("Reminders", "Recordatorios"), "", None, |ui| {
        let c = theme::colors(ui);
        let ctx = ui.ctx().clone();
        if s.birthdays.is_empty() {
            muted(ui, pick("No birthdays in the next two weeks.", "Sin cumpleaños en las próximas dos semanas."));
            return;
        }
        for b in s.birthdays.iter().take(5) {
            let label = format!("{} {}", pick("Birthday:", "Cumpleaños de"), b.name);
            let (rect, resp) = row(ui, 30.0, &label);
            let ir = Rect::from_center_size(pos2(rect.left() + 8.0, rect.center().y), vec2(16.0, 16.0));
            icons::paint(ui.painter(), ir, Icon::Gift, if b.in_days == 0 { c.danger } else { c.text_muted }, c.danger);
            let when = match b.in_days {
                0 => pick("today", "hoy").to_string(),
                1 => pick("tomorrow", "mañana").to_string(),
                n => format!("{} {n} {}", pick("in", "en"), pick("days", "días")),
            };
            let when = match b.turns {
                Some(t) => format!("{when} · {} {t}", pick("turns", "cumple")),
                None => when,
            };
            let col = if b.in_days == 0 { c.danger } else { c.text_muted };
            let g = ui.painter().layout_no_wrap(when, theme::small_font(&ctx), col);
            let gw = g.size().x;
            ui.painter().galley(pos2(rect.right() - gw, rect.center().y - g.size().y / 2.0), g, col);
            let x = rect.left() + 24.0;
            let font = if b.in_days == 0 {
                theme::medium_font(&ctx, theme::density(&ctx).body_size())
            } else {
                theme::body_font(&ctx)
            };
            text_at(ui, pos2(x, rect.center().y - 9.0), &label, font, c.text, rect.right() - gw - 12.0 - x);
            if resp.clicked() {
                acts.push(Act::Contact(b.contact));
            }
        }
    });
}

// ---------------------------------------------------------------------------------------------
// Acciones

fn apply(app: &mut DeskApp, ctx: &egui::Context, a: Act) {
    match a {
        Act::Event(o) => {
            let day = app.session.zone().local(o.start).date();
            app.session.cal.anchor = Some(day);
            app.session.module = Module::Calendar;
            crate::event_editor::open_occurrence(app, &o);
        }
        Act::Join(url) => crate::meeting::join(app, ctx, &url),
        Act::Calendar => {
            app.session.cal.anchor = None;
            app.session.module = Module::Calendar;
        }
        Act::Task(list) => {
            app.pim.task_list = Some(list);
            app.session.module = Module::Tasks;
        }
        Act::Tasks => app.session.module = Module::Tasks,
        Act::Complete(dt) => complete(app, ctx, &dt),
        Act::Message(m) => {
            app.session.select_folder(FolderSel::Folder(m.folder));
            app.session.request_focus(m.id);
        }
        Act::Inbox(f) => match f {
            Some(f) => app.session.select_folder(FolderSel::Folder(f)),
            None => app.session.select_folder(FolderSel::Unified(iuredesk_model::FolderRole::Inbox)),
        },
        Act::Flagged => app.session.select_folder(FolderSel::Flagged),
        Act::Triage => crate::assistant::request_triage(app),
        Act::TriageOpen(id) => {
            app.session.select_folder(FolderSel::Unified(iuredesk_model::FolderRole::Inbox));
            app.session.request_focus(id);
        }
        Act::Iure(it) => {
            app.session.iure_mark_read(it.mention, &it.id);
            if let (Some(l), Some(base)) = (it.link.clone(), app.session.iure_web_url()) {
                let url = if l.starts_with("https://") { l } else { format!("{}{}", base.trim_end_matches('/'), l) };
                crate::open_url(&url);
            } else {
                app.iure.bell_open = true;
            }
        }
        Act::IureWeb => match app.session.iure_web_url() {
            Some(u) => {
                crate::open_url(&u);
            }
            None => app.iure.bell_open = true,
        },
        Act::Contact(id) => {
            app.pim.contact = Some(id);
            app.session.module = Module::Contacts;
        }
    }
}

/// Terminar una tarea: local, CalDAV y To Do por la sesión (con deshacer); las de Iurefficient
/// solo si hay sesión, si no se abre la web (nunca en silencio).
fn complete(app: &mut DeskApp, ctx: &egui::Context, dt: &DayTask) {
    if dt.source == TaskSource::Iurefficient && !app.session.iure_can_write() {
        match app.session.iure_web_url() {
            Some(u) => {
                crate::open_url(&u);
                app.notice(Notice::info(
                    "Iurefficient is not connected: complete the task there (opened in your browser).",
                    "Iurefficient no está conectado: termina la tarea allí (se abrió en tu navegador).",
                ));
            }
            None => app.notice(Notice::error(iuredesk_engine::Text::new(
                "Iurefficient is not connected, so this task can't be completed from here. Sign in under Settings → Iurefficient.",
                "Iurefficient no está conectado y esta tarea no se puede terminar desde aquí. Inicia sesión en Ajustes → Iurefficient.",
            ))),
        }
        return;
    }
    let lists = app.session.task_lists();
    match app.session.toggle_task(dt.task.id, &lists) {
        Ok(out) => {
            app.today.done.insert(dt.task.id);
            app.today.seen = app.session.day_summary();
            app.outcome(ctx, out);
        }
        Err(e) => app.notice(Notice::error(e.text())),
    }
}
