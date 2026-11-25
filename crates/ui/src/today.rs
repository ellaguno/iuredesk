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
use iuredesk_engine::today_layout::{CardTint, Column, TINTS, TodayCard, TodayLayout};
use iuredesk_engine::{FolderSel, Module, Notice};
use iuredesk_model::{ContactId, FolderId, TaskId, TaskListId};
use iurefficient_ui::{Icon, PaneKind, icons, pick, split, theme};

use crate::app::DeskApp;

/// Ancho máximo del contenido.
const MAX_WIDTH: f32 = 1180.0;
/// Separación entre tarjetas.
const GAP: f32 = 16.0;
/// S1.36: desde este ancho, las noticias van entre el saludo y el clima.
const NEWS_BESIDE: f32 = 900.0;
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
    /// UX-13: fondo propio.
    pub(crate) bg: crate::today_bg::BgState,
    /// UX-15: personalizando las tarjetas.
    pub(crate) editing: bool,
    /// UX-19: tarjeta que se está arrastrando.
    pub(crate) drag: Option<TodayCard>,
    /// UX-15: el clima de hoy.
    pub(crate) weather: crate::today_weather::WeatherUi,
    /// S1.36: noticias habladas (su audio sigue sonando fuera de «Mi día»).
    pub(crate) news: crate::today_news::NewsUi,
    /// S1.6: planes de IureProject que se siguen.
    pub(crate) projects: crate::today_projects::ProjectsUi,
}

/// Qué se pulsó.
enum Act {
    Event(Box<Occurrence>),
    Join(String),
    /// S1.4: grabar esta reunión con IureTranscribe.
    Record(String),
    Calendar,
    /// UX-6: abrir la tarea en el editor (sobre «Mi día»).
    Task(TaskListId, TaskId),
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
        let page = ui.max_rect();
        // UX-13: color, degradado o imagen (o el fondo del tema).
        crate::today_bg::paint(app, ui, page, c.bg);
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
                    if crate::today_bg::active(app) {
                        // Sobre un fondo propio, el encabezado va en un panel para leerse siempre.
                        let c = theme::colors(ui);
                        egui::Frame::new()
                            .fill(c.surface.gamma_multiply(0.9))
                            .corner_radius(14)
                            .inner_margin(Margin::same(16))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                header(app, ui, s, now, &mut acts);
                                crate::today_bg::credit(app, ui);
                            });
                    } else {
                        header(app, ui, s, now, &mut acts);
                    }
                    ui.add_space(18.0);
                    if app.today.editing {
                        edit_bar(app, ui);
                    }
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

fn header(app: &mut DeskApp, ui: &mut Ui, s: &DaySummary, now: DateTime<Utc>, acts: &mut Vec<Act>) {
    let c = theme::colors(ui);
    let ctx = ui.ctx().clone();
    let zone = app.session.zone();
    let hour = chrono::Timelike::hour(&zone.local(now));
    let hello = today::greeting(hour);
    let title = if s.user.is_empty() { hello.get().to_string() } else { format!("{}, {}", hello.get(), s.user) };
    let day = capitalize(&today::long_day(s.date));
    // S1.36: con sitio, las noticias van entre el saludo y el clima; si no, debajo del saludo.
    let news = crate::today_news::shown(app);
    let beside = news && ui.available_width() >= NEWS_BESIDE;
    let title_font = theme::semibold_font(&ctx, 26.0);
    let greet_w = {
        // Icono (30), su espacio (4) y los dos huecos entre elementos de la fila.
        let gaps = 34.0 + 2.0 * ui.spacing().item_spacing.x;
        let t = ui.painter().layout_no_wrap(title.clone(), title_font.clone(), c.text).size().x + gaps;
        let d = ui.painter().layout_no_wrap(day.clone(), egui::FontId::proportional(15.0), c.text).size().x;
        t.max(d) + 8.0
    };
    let greeting = |ui: &mut Ui| {
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(30.0, 30.0), Sense::hover());
            icons::paint(ui.painter(), r, Icon::Sun, c.warning, c.warning);
            ui.add_space(4.0);
            ui.add(egui::Label::new(RichText::new(title.clone()).font(title_font.clone()).color(c.text)).truncate());
        });
        ui.add_space(2.0);
        ui.add(egui::Label::new(RichText::new(day.clone()).size(15.0).color(c.text_muted)).truncate());
    };
    ui.horizontal_top(|ui| {
        // UX-15: a la derecha, «Personalizar» y el clima de hoy; el saludo (y las noticias) el resto.
        ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
            let tip = if app.today.editing {
                pick("Done customizing", "Terminar de personalizar")
            } else {
                pick("Customize this page", "Personalizar esta página")
            };
            if icons::icon_button(ui, Icon::Sliders, tip, true).clicked() {
                app.today.editing = !app.today.editing;
            }
            ui.add_space(6.0);
            crate::today_weather::widget(app, ui);
            ui.with_layout(Layout::left_to_right(Align::Min), |ui| {
                if !beside {
                    ui.vertical(greeting);
                    return;
                }
                let left = greet_w.min(ui.available_width() * 0.42);
                ui.allocate_ui_with_layout(vec2(left, 0.0), Layout::top_down(Align::Min), |ui| {
                    ui.set_width(left);
                    greeting(ui);
                });
                ui.add_space(24.0);
                let rest = ui.available_width() - 16.0;
                ui.allocate_ui_with_layout(vec2(rest, 0.0), Layout::top_down(Align::Min), |ui| {
                    ui.set_width(rest);
                    ui.spacing_mut().item_spacing.y = 2.0;
                    crate::today_news::panel(app, ui);
                });
            });
        });
    });
    if news && !beside {
        ui.add_space(8.0);
        crate::today_news::panel(app, ui);
    }
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
    let layout = app.session.prefs.today_layout.normalized();
    let mut edits: Vec<Edit> = Vec::new();
    let mut placed: Vec<Placed> = Vec::new();
    if !app.today.editing {
        app.today.drag = None;
    }
    // UX-15: si una columna queda vacía, la otra ocupa todo el ancho.
    let side_by_side = !(w < TWO_COLUMNS || layout.left.is_empty() || layout.right.is_empty());
    if !side_by_side {
        let two = w >= TWO_COLUMNS;
        for card in layout.left.iter().chain(&layout.right) {
            slot(app, ui, s, now, *card, &layout, two, acts, &mut edits, &mut placed);
        }
    } else {
        let left = ((w - GAP) * if w > 1000.0 { 0.56 } else { 0.52 }).floor();
        let right = (w - GAP - left).floor();
        let spacing = ui.spacing().item_spacing;
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            for (cards, width, gap) in [(&layout.left, left, true), (&layout.right, right, false)] {
                ui.allocate_ui_with_layout(vec2(width, 0.0), Layout::top_down(Align::Min), |ui| {
                    ui.set_width(width);
                    ui.spacing_mut().item_spacing = spacing;
                    for card in cards {
                        slot(app, ui, s, now, *card, &layout, true, acts, &mut edits, &mut placed);
                    }
                });
                if gap {
                    ui.add_space(GAP);
                }
            }
        });
    }
    // UX-19: mientras se arrastra, la tarjeta se atenúa, una raya marca dónde caerá y su nombre
    // sigue al puntero.
    let pointer = ui.ctx().pointer_latest_pos();
    if let (Some(card), Some(pos)) = (app.today.drag, pointer) {
        let c = theme::colors(ui);
        let p = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("today-drag")));
        if let Some(me) = placed.iter().find(|x| x.card == card) {
            p.rect_filled(me.rect, 12.0, c.bg.gamma_multiply(0.55));
        }
        if let Some(t) = drop_target(&placed, card, pos, side_by_side) {
            let line = Rect::from_min_max(pos2(t.x.min, t.y - 1.5), pos2(t.x.max, t.y + 1.5));
            p.rect_filled(line, 2.0, c.accent);
        }
        let font = theme::medium_font(ui.ctx(), theme::density(ui.ctx()).body_size());
        let g = p.layout_no_wrap(card.label().to_string(), font, c.text);
        let r = Rect::from_min_size(pos + vec2(14.0, 10.0), vec2(g.size().x + 44.0, 30.0));
        p.rect(r, 15.0, c.surface, Stroke::new(1.0, c.accent), egui::StrokeKind::Inside);
        let ir = Rect::from_center_size(pos2(r.left() + 18.0, r.center().y), vec2(16.0, 16.0));
        let tone = layout.tint(card).map_or(c.accent, crate::widgets::rgb);
        icons::paint(&p, ir, card_icon(card), tone, tone);
        p.galley(pos2(ir.right() + 8.0, r.center().y - g.size().y / 2.0), g, c.text);
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    }
    if !edits.is_empty() {
        let mut l = layout.clone();
        for e in edits {
            match e {
                Edit::Up(c) => l.move_card(c, true),
                Edit::Down(c) => l.move_card(c, false),
                Edit::Switch(c) => l.switch_column(c),
                Edit::Hide(c) => l.hide(c),
                Edit::Tint(c, t) => l.set_tint(c, t),
                Edit::Grab(c) => app.today.drag = Some(c),
                Edit::Drop(c) => {
                    app.today.drag = None;
                    if let Some(t) = pointer.and_then(|pos| drop_target(&placed, c, pos, side_by_side)) {
                        l.move_to(c, t.col, t.index);
                    }
                }
            }
        }
        if l != layout {
            app.session.prefs.today_layout = l;
            app.session.save_prefs();
        }
    }
}

/// UX-15: cambios al acomodo de las tarjetas.
enum Edit {
    Up(TodayCard),
    Down(TodayCard),
    Switch(TodayCard),
    Hide(TodayCard),
    /// UX-19: color (`None`: el de por omisión).
    Tint(TodayCard, Option<CardTint>),
    /// UX-19: empezó a arrastrarse.
    Grab(TodayCard),
    /// UX-19: se soltó donde está el puntero.
    Drop(TodayCard),
}

/// UX-19: dónde quedó dibujada cada tarjeta (para saber dónde cae la que se arrastra).
struct Placed {
    card: TodayCard,
    col: Column,
    rect: Rect,
}

/// UX-19: dónde caería la tarjeta: columna, posición (contada sin ella), altura y ancho de la raya.
struct DropTarget {
    col: Column,
    index: usize,
    y: f32,
    x: egui::Rangef,
}

fn drop_target(placed: &[Placed], card: TodayCard, pos: egui::Pos2, side_by_side: bool) -> Option<DropTarget> {
    let span = |v: &[&Placed]| -> Option<egui::Rangef> {
        let first = v.first()?;
        Some(v.iter().fold(first.rect.x_range(), |r, p| r.union(p.rect.x_range())))
    };
    let me = placed.iter().find(|p| p.card == card)?;
    // Las demás de la columna elegida (o de todas, en una sola columna), en orden.
    let (col, others): (Option<Column>, Vec<&Placed>) = if side_by_side {
        let of = |col: Column| placed.iter().filter(|p| p.col == col).collect::<Vec<_>>();
        let (l, r) = (of(Column::Left), of(Column::Right));
        let dist = |v: &[&Placed]| span(v).map_or(f32::INFINITY, |x| (x.center() - pos.x).abs());
        let col = if dist(&l) <= dist(&r) { Column::Left } else { Column::Right };
        let v = if col == Column::Left { l } else { r };
        (Some(col), v.into_iter().filter(|p| p.card != card).collect())
    } else {
        (None, placed.iter().filter(|p| p.card != card).collect())
    };
    let k = others.iter().filter(|p| p.rect.center().y < pos.y).count();
    let y = match (others.get(k), others.last()) {
        (Some(p), _) => p.rect.top() - GAP / 2.0,
        (None, Some(p)) => p.rect.bottom() - GAP / 2.0,
        (None, None) => me.rect.top() - GAP / 2.0,
    };
    let x = if side_by_side {
        span(&others).unwrap_or(me.rect.x_range())
    } else {
        span(&placed.iter().collect::<Vec<_>>())?
    };
    let (col, index) = match col {
        Some(col) => (col, k),
        None => {
            // Una sola columna: la izquierda va antes que la derecha.
            let lefts = others.iter().filter(|p| p.col == Column::Left).count();
            if lefts > 0 && k <= lefts { (Column::Left, k) } else { (Column::Right, k.saturating_sub(lefts)) }
        }
    };
    Some(DropTarget { col, index, y, x })
}

/// UX-19: el color de la tarjeta que se está pintando lo lee [`card`] (así no cambia la firma de
/// cada tarjeta).
fn tint_id() -> egui::Id {
    egui::Id::unique("today-card-tint")
}

fn card_icon(card: TodayCard) -> Icon {
    match card {
        TodayCard::Agenda => Icon::Calendar,
        TodayCard::Tasks => Icon::Tasks,
        TodayCard::Mail => Icon::Mail,
        TodayCard::Iurefficient => Icon::Bell,
        TodayCard::Projects => Icon::Gantt,
        TodayCard::Reminders | TodayCard::Unknown => Icon::Gift,
    }
}

/// Una tarjeta en su sitio; al personalizar, con sus controles (arrastrar, color, subir, bajar,
/// columna, quitar).
#[allow(clippy::too_many_arguments)]
fn slot(
    app: &mut DeskApp,
    ui: &mut Ui,
    s: &DaySummary,
    now: DateTime<Utc>,
    which: TodayCard,
    layout: &TodayLayout,
    two: bool,
    acts: &mut Vec<Act>,
    edits: &mut Vec<Edit>,
    placed: &mut Vec<Placed>,
) {
    let tone = layout.tint(which).map(crate::widgets::rgb);
    ui.ctx().data_mut(|d| d.insert_temp(tint_id(), tone));
    let mut rect = ui
        .scope(|ui| match which {
            TodayCard::Agenda => agenda_card(app, ui, s, now, acts),
            TodayCard::Tasks => tasks_card(app, ui, s, acts),
            TodayCard::Mail => mail_card(app, ui, s, now, acts),
            TodayCard::Iurefficient => iure_card(ui, s, now, acts),
            TodayCard::Reminders => reminders_card(ui, s, acts),
            TodayCard::Projects => crate::today_projects::card(app, ui),
            TodayCard::Unknown => {}
        })
        .response
        .rect;
    // Una tarjeta sin nada que mostrar (Iurefficient sin sesión) se ve igual para poder moverla.
    if app.today.editing && rect.height() < 4.0 {
        rect = ui
            .scope(|ui| {
                card(
                    ui,
                    card_icon(which),
                    which.label(),
                    pick("nothing to show now", "nada que mostrar ahora"),
                    None,
                    |_| {},
                )
            })
            .response
            .rect;
    }
    ui.ctx().data_mut(|d| d.remove::<Option<Color32>>(tint_id()));
    if !app.today.editing {
        return;
    }
    if let Some(col) = layout.column(which) {
        placed.push(Placed { card: which, col, rect });
    }
    let c = theme::colors(ui);
    let n = if two { 6.0 } else { 5.0 };
    let side = 30.0;
    let bar = Rect::from_min_size(
        pos2(rect.right() - 12.0 - side * n - 8.0, rect.top() + 8.0),
        vec2(side * n + 8.0, side + 4.0),
    );
    ui.painter().rect(bar, 8.0, c.surface_alt, Stroke::new(1.0, c.separator), egui::StrokeKind::Inside);
    // Encima de la tarjeta, sin ocupar sitio en la columna.
    let mut bar_ui = ui.new_child(
        egui::UiBuilder::new().max_rect(bar.shrink2(vec2(4.0, 2.0))).layout(Layout::right_to_left(Align::Center)),
    );
    {
        let ui = &mut bar_ui;
        ui.spacing_mut().item_spacing.x = 0.0;
        let name = which.label();
        if icons::icon_button(ui, Icon::Close, &format!("{} {name}", pick("Remove", "Quitar")), true).clicked() {
            edits.push(Edit::Hide(which));
        }
        if two {
            let (ic, tip) = match layout.column(which) {
                Some(Column::Left) => {
                    (Icon::ChevronRight, pick("Move to the right column", "Pasar a la columna derecha"))
                }
                _ => (Icon::ChevronLeft, pick("Move to the left column", "Pasar a la columna izquierda")),
            };
            if icons::icon_button(ui, ic, &format!("{tip}: {name}"), true).clicked() {
                edits.push(Edit::Switch(which));
            }
        }
        if icons::icon_button(
            ui,
            Icon::ChevronDown,
            &format!("{} {name}", pick("Move down", "Bajar")),
            layout.can_move(which, false),
        )
        .clicked()
        {
            edits.push(Edit::Down(which));
        }
        if icons::icon_button(
            ui,
            Icon::ChevronUp,
            &format!("{} {name}", pick("Move up", "Subir")),
            layout.can_move(which, true),
        )
        .clicked()
        {
            edits.push(Edit::Up(which));
        }
        tint_button(ui, which, tone, edits);
        grip(ui, which, app.today.drag == Some(which), edits);
    }
}

/// UX-19: asa para arrastrar la tarjeta (seis puntos).
fn grip(ui: &mut Ui, which: TodayCard, dragging: bool, edits: &mut Vec<Edit>) {
    let c = theme::colors(ui);
    let side = theme::density(ui.ctx()).icon_size() + 8.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(side, side), Sense::drag());
    let tip = format!("{} {}", pick("Drag to move", "Arrastra para mover"), which.label());
    iurefficient_ui::a11y::button(&resp, &tip);
    let hot = resp.hovered() || dragging;
    if hot {
        ui.painter().rect_filled(rect.shrink(1.0), 6.0, c.hover);
        ui.ctx().set_cursor_icon(if dragging { egui::CursorIcon::Grabbing } else { egui::CursorIcon::Grab });
    }
    let ink = if hot { c.text } else { c.text_muted };
    for (dx, dy) in [(-3.0, -5.0), (3.0, -5.0), (-3.0, 0.0), (3.0, 0.0), (-3.0, 5.0), (3.0, 5.0)] {
        ui.painter().circle_filled(rect.center() + vec2(dx, dy), 1.6, ink);
    }
    if resp.drag_started() {
        edits.push(Edit::Grab(which));
    }
    if resp.drag_stopped() {
        edits.push(Edit::Drop(which));
    }
    resp.on_hover_text(tip);
}

/// UX-19: botón con el color de la tarjeta; abre la paleta.
fn tint_button(ui: &mut Ui, which: TodayCard, tone: Option<Color32>, edits: &mut Vec<Edit>) {
    let c = theme::colors(ui);
    let side = theme::density(ui.ctx()).icon_size() + 8.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(side, side), Sense::click());
    let tip = format!("{} {}", pick("Color of", "Color de"), which.label());
    iurefficient_ui::a11y::button(&resp, &tip);
    if resp.hovered() {
        ui.painter().rect_filled(rect.shrink(1.0), 6.0, c.hover);
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    swatch(ui.painter(), rect.center(), 6.0, tone, &c);
    let resp = resp.on_hover_text(tip);
    egui::Popup::menu(&resp).show(|ui| {
        ui.label(RichText::new(pick("Card color", "Color de la tarjeta")).color(c.text_muted));
        ui.add_space(4.0);
        for row in TINTS.chunks(5) {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                for &rgb in row {
                    let col = crate::widgets::rgb(rgb);
                    let (r, sw) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::click());
                    let chosen = tone == Some(col);
                    if sw.hovered() || chosen {
                        ui.painter().circle_stroke(r.center(), 11.5, Stroke::new(2.0, col));
                    }
                    swatch(ui.painter(), r.center(), 8.0, Some(col), &c);
                    iurefficient_ui::a11y::button(&sw, &format!("#{rgb:06X}"));
                    if sw.clicked() {
                        edits.push(Edit::Tint(which, Some(CardTint::Color(rgb))));
                        ui.close();
                    }
                }
            });
        }
        ui.add_space(4.0);
        if ui.button(pick("No color", "Sin color")).clicked() {
            edits.push(Edit::Tint(which, Some(CardTint::None)));
        }
        if ui.button(pick("Default color", "Color por omisión")).clicked() {
            edits.push(Edit::Tint(which, None));
        }
    });
}

/// Muestra de un tono (punto lleno); sin color, un círculo vacío.
fn swatch(p: &egui::Painter, at: egui::Pos2, r: f32, tone: Option<Color32>, c: &iurefficient_ui::Palette) {
    match tone {
        Some(t) => {
            p.circle_filled(at, r, t);
        }
        None => {
            p.circle_stroke(at, r, Stroke::new(1.2, c.text_muted));
        }
    }
}

/// UX-19: el tono aclarado hasta un pastel sobre la superficie (o, en oscuro, un poco más vivo).
fn pastel(c: &iurefficient_ui::Palette, tone: Color32) -> Color32 {
    theme::blend(c.surface, tone.gamma_multiply(if c.dark { 0.12 } else { 0.11 }))
}

/// UX-15: barra de «Personalizar»: tarjetas quitadas para volver a ponerlas y «Listo».
fn edit_bar(app: &mut DeskApp, ui: &mut Ui) {
    let c = theme::colors(ui);
    let hidden = app.session.prefs.today_layout.normalized().hidden;
    let mut show: Option<TodayCard> = None;
    let mut weather = false;
    let mut done = false;
    egui::Frame::new()
        .fill(c.surface)
        .stroke(Stroke::new(1.0, c.accent.gamma_multiply(0.6)))
        .corner_radius(10)
        .inner_margin(Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(8.0, 6.0);
                ui.label(
                    RichText::new(pick(
                        "Drag cards by their handle, change their color or remove them.",
                        "Arrastra las tarjetas por su asa, cámbiales el color o quítalas.",
                    ))
                    .color(c.text_muted),
                );
                for h in &hidden {
                    if chip(ui, Icon::Plus, h.label(), None)
                        .on_hover_text(pick("Put it back", "Volver a ponerla"))
                        .clicked()
                    {
                        show = Some(*h);
                    }
                }
                if !app.session.prefs.weather.enabled && chip(ui, Icon::Plus, pick("Weather", "Clima"), None).clicked()
                {
                    weather = true;
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if iurefficient_ui::primary_button(ui, pick("Done", "Listo")).clicked() {
                        done = true;
                    }
                });
            });
        });
    ui.add_space(GAP);
    if let Some(card) = show {
        let mut l = app.session.prefs.today_layout.normalized();
        l.show(card);
        app.session.prefs.today_layout = l;
        app.session.save_prefs();
    }
    if weather {
        crate::today_weather::open_setup(app);
    }
    if done {
        app.today.editing = false;
    }
}

/// Tarjeta con cabecera (icono, título, cifra) y una acción a la derecha. Devuelve si se pulsó la
/// acción.
pub(crate) fn card(
    ui: &mut Ui,
    ic: Icon,
    title: &str,
    detail: &str,
    action: Option<&str>,
    add: impl FnOnce(&mut Ui),
) -> bool {
    let c = theme::colors(ui);
    let ctx = ui.ctx().clone();
    let mut clicked = false;
    // UX-19: el color suave de la tarjeta (lo pone `slot`).
    let tone = ctx.data(|d| d.get_temp::<Option<Color32>>(tint_id())).flatten();
    let fill = tone.map_or(c.surface, |t| pastel(&c, t));
    let border = tone.map_or(c.separator, |t| theme::blend(c.surface, t.gamma_multiply(0.35)));
    egui::Frame::new()
        .fill(fill)
        .stroke(Stroke::new(1.0, border))
        .corner_radius(12)
        .inner_margin(Margin { left: 16, right: 16, top: 14, bottom: 14 })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let (r, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
                let ink = tone.unwrap_or(c.accent);
                icons::paint(ui.painter(), r, ic, ink, ink);
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

pub(crate) fn muted(ui: &mut Ui, text: &str) {
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
                    let record = crate::recording_ui::can_record(app);
                    if e.join.is_some() || record {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if let Some(j) = &e.join
                                && crate::meeting::join_button(ui, j, true)
                            {
                                acts.push(Act::Join(j.url.clone()));
                            }
                            if record
                                && crate::recording_ui::record_button(ui, pick("Record", "Grabar"))
                                    .on_hover_text(pick(
                                        "Record this meeting with IureTranscribe (without opening it)",
                                        "Grabar esta reunión con IureTranscribe (sin abrirlo)",
                                    ))
                                    .clicked()
                            {
                                acts.push(Act::Record(e.occ.summary.clone()));
                            }
                        });
                    }
                });
            });
    });
    let resp = card.response;
    iurefficient_ui::a11y::button(&resp, &e.occ.summary);
    if resp.clicked() && !acts.iter().any(|a| matches!(a, Act::Join(_) | Act::Record(_))) {
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
        acts.push(Act::Task(dt.list, dt.task.id));
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
        if !crate::assistant::enabled(app) {
            return;
        }
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
        Act::Record(title) => crate::recording_ui::start(app, &title),
        Act::Calendar => {
            app.session.cal.anchor = None;
            app.session.module = Module::Calendar;
        }
        Act::Task(list, id) => {
            app.pim.task_list = Some(list);
            app.pim.task_sel = Some(id);
            crate::task_editor::open(app, id);
        }
        Act::Tasks => app.session.module = Module::Tasks,
        Act::Complete(dt) => complete(app, ctx, &dt),
        // La carpeta del mensaje y la pestaña de su categoría: en otra pestaña no se abría.
        Act::Message(m) => {
            if !app.session.reveal_message(m.id) {
                app.session.select_folder(FolderSel::Folder(m.folder));
            }
        }
        Act::Inbox(f) => match f {
            Some(f) => app.session.select_folder(FolderSel::Folder(f)),
            None => app.session.select_folder(FolderSel::Unified(iuredesk_model::FolderRole::Inbox)),
        },
        Act::Flagged => app.session.select_folder(FolderSel::Flagged),
        Act::Triage => crate::assistant::request_triage(app),
        Act::TriageOpen(id) => {
            if !app.session.reveal_message(id) {
                app.session.select_folder(FolderSel::Unified(iuredesk_model::FolderRole::Inbox));
            }
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
