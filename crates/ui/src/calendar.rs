//! Calendario: día, semana laboral, semana, mes y agenda; mini mes para navegar, calendarios por
//! cuenta con color y casilla de mostrar/ocultar, segunda zona horaria, fila de días completos,
//! eventos que se solapan en columnas, la línea de «ahora» y arrastrar para crear, mover y
//! estirar (pasos de 15 minutos). El editor y el aviso de crear rápido viven en
//! [`crate::event_editor`].

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveDateTime, Timelike, Utc};
use egui::{Align, Align2, Color32, CursorIcon, FontId, Layout, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};
use iuredesk_engine::calendar::drag::{
    DAY_MIN, at_minutes, column_at, create_span, day_span, move_days, move_times, resize_end, y_to_minutes,
};
use iuredesk_engine::calendar::nav::{CalView, day_title, title, visible_range};
use iuredesk_engine::calendar::{Occurrence, layout_columns};
use iuredesk_engine::time::{month_name, weekday_short};
use iuredesk_model::EventTime;
use iurefficient_ui::{EmptyState, Icon, PaneKind, icons, pick, section_label, segmented, split, theme};

use crate::app::DeskApp;
use crate::event_editor::{self, EditorState};
use crate::widgets::{hairline, mix, rgb};

/// Arrastre en curso sobre la rejilla.
#[derive(Clone, Debug)]
pub(crate) enum Drag {
    /// Crear: columna y minuto donde se pulsó.
    Create { col: i64, press_min: f32 },
    /// Mover: la aparición y a cuántos minutos de su inicio se agarró.
    Move { occ: Box<Occurrence>, grab_min: f32, moved: bool },
    /// Estirar el borde de abajo.
    Resize { occ: Box<Occurrence> },
    /// Crear en la fila de días completos.
    AllDayCreate { col: i64 },
    /// Mover un evento de día completo.
    AllDayMove { occ: Box<Occurrence>, grab_col: i64 },
}

/// Estado del calendario en la interfaz.
#[derive(Default)]
pub struct CalendarState {
    /// Cuadros en que todavía se fija el desplazamiento inicial (07:30).
    scroll_frames: u8,
    scrolled: bool,
    pub(crate) drag: Option<Drag>,
    /// Editor, crear rápido y preguntas («¿solo este evento?»).
    pub(crate) editor: EditorState,
}

/// Altura de una hora según la densidad.
fn hour_height(ui: &Ui) -> f32 {
    match theme::density(ui.ctx()) {
        iurefficient_ui::Density::Comfortable => 52.0,
        iurefficient_ui::Density::Compact => 44.0,
        iurefficient_ui::Density::Dense => 36.0,
    }
}

pub fn show(app: &mut DeskApp, ui: &mut Ui) {
    let today = app.session.today();
    let anchor = app.session.cal_anchor();
    split::left(ui, "cal-side", PaneKind::Sidebar, 244.0, 200.0..=360.0, |ui| sidebar(app, ui, anchor, today));
    split::top_bar(ui, "cal-top", |ui| top_bar(app, ui, anchor, today));
    split::center(ui, PaneKind::Content, |ui| match app.session.cal_view() {
        CalView::Month => month_view(app, ui, anchor, today),
        CalView::Agenda => agenda_view(app, ui, anchor, today),
        v => {
            let (start, days) = visible_range(v, anchor);
            week_view(app, ui, start, days, today);
        }
    });
}

fn sidebar(app: &mut DeskApp, ui: &mut Ui, anchor: NaiveDate, today: NaiveDate) {
    let c = theme::colors(ui);
    ui.add_space(6.0);
    if crate::widgets::wide_primary(ui, Icon::Plus, pick("New event", "Nuevo evento")).clicked() {
        let ctx = ui.ctx().clone();
        app.run(&ctx, "cal.new_event");
    }
    ui.add_space(12.0);
    // Mini mes con flechas propias (no mueve la vista hasta elegir un día).
    let shown = ui.ctx().data(|d| d.get_temp::<NaiveDate>(egui::Id::unique("mini-month"))).unwrap_or(anchor);
    let shown = if ui.ctx().data(|d| d.get_temp::<NaiveDate>(egui::Id::unique("mini-anchor"))) != Some(anchor) {
        ui.ctx().data_mut(|d| d.insert_temp(egui::Id::unique("mini-anchor"), anchor));
        anchor
    } else {
        shown
    };
    let mut month = shown;
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!("{} {}", capitalize(month_name(month.month())), month.year())).strong().color(c.text),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if icons::icon_button(ui, Icon::ChevronRight, pick("Next month", "Mes siguiente"), true).clicked() {
                month = month.checked_add_months(chrono::Months::new(1)).unwrap_or(month);
            }
            if icons::icon_button(ui, Icon::ChevronLeft, pick("Previous month", "Mes anterior"), true).clicked() {
                month = month.checked_sub_months(chrono::Months::new(1)).unwrap_or(month);
            }
        });
    });
    ui.ctx().data_mut(|d| d.insert_temp(egui::Id::unique("mini-month"), month));
    ui.add_space(2.0);
    let first = NaiveDate::from_ymd_opt(month.year(), month.month(), 1).unwrap_or(month);
    let start = iuredesk_engine::time::monday_of(first);
    let (vis_start, vis_days) = visible_range(app.session.cal_view(), anchor);
    let cell = ((ui.available_width() - 4.0) / 7.0).floor().clamp(20.0, 34.0);
    let (rect, _) = ui.allocate_exact_size(vec2(cell * 7.0, cell * 7.0), Sense::hover());
    let p = ui.painter_at(rect);
    for i in 0..7usize {
        let en = ["M", "T", "W", "T", "F", "S", "S"];
        let es = ["L", "M", "X", "J", "V", "S", "D"];
        p.text(
            pos2(rect.left() + cell * (i as f32 + 0.5), rect.top() + cell * 0.5),
            Align2::CENTER_CENTER,
            pick(en.get(i).copied().unwrap_or(""), es.get(i).copied().unwrap_or("")),
            FontId::proportional(11.0),
            c.text_muted,
        );
    }
    for k in 0..42i64 {
        let day = start + Duration::days(k);
        let (col, row) = ((k % 7) as f32, (k / 7) as f32 + 1.0);
        let r = Rect::from_min_size(pos2(rect.left() + col * cell, rect.top() + row * cell), vec2(cell, cell));
        let resp = ui.interact(r, ui.scope_id().with(("mini", k)), Sense::click());
        let in_view = vis_days <= 7 && day >= vis_start && day < vis_start + Duration::days(vis_days);
        if in_view {
            p.rect_filled(r.shrink2(vec2(0.0, 3.0)), 0.0, mix(c.surface, c.accent, 0.08));
        }
        let is_today = day == today;
        if is_today {
            p.circle_filled(r.center(), cell * 0.4, c.accent);
        } else if day == anchor {
            p.circle_filled(r.center(), cell * 0.4, c.selection);
        } else if resp.hovered() {
            p.circle_filled(r.center(), cell * 0.4, c.hover);
        }
        let ink = if is_today {
            c.on_accent
        } else if day.month() == month.month() {
            c.text
        } else {
            c.text_muted
        };
        p.text(r.center(), Align2::CENTER_CENTER, day.day().to_string(), FontId::proportional(12.0), ink);
        iurefficient_ui::a11y::button(&resp, &day.format("%Y-%m-%d").to_string());
        if resp.clicked() {
            app.session.cal.anchor = Some(day);
        }
    }
    ui.add_space(14.0);
    calendar_list(app, ui);
    ui.add_space(14.0);
    section_label(ui, pick("Second time zone", "Segunda zona horaria"));
    ui.add_space(2.0);
    let current = app.session.prefs.secondary_tz.clone();
    let mut chosen = current.clone();
    let combo = egui::ComboBox::from_id_salt("cal-second-tz")
        .width(ui.available_width() - 8.0)
        .selected_text(current.as_deref().map(tz_label).unwrap_or_else(|| pick("None", "Ninguna").to_string()))
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut chosen, None, pick("None", "Ninguna"));
            for z in iuredesk_engine::calendar::draft::COMMON_ZONES {
                ui.selectable_value(&mut chosen, Some(z.to_string()), tz_label(z));
            }
        });
    crate::widgets::named(&combo.response, egui::Role::ComboBox, pick("Second time zone", "Segunda zona horaria"));
    if chosen != current {
        app.session.prefs.secondary_tz = chosen;
        app.session.save_prefs();
    }
}

/// «Mexico City» de «America/Mexico_City».
pub(crate) fn tz_label(name: &str) -> String {
    name.rsplit('/').next().unwrap_or(name).replace('_', " ")
}

/// Calendarios agrupados por cuenta, con su color como casilla de mostrar/ocultar.
fn calendar_list(app: &mut DeskApp, ui: &mut Ui) {
    let c = theme::colors(ui);
    let labels = app.session.account_labels().to_vec();
    let cals = app.session.calendars().to_vec();
    if cals.is_empty() {
        section_label(ui, pick("Calendars", "Calendarios"));
        ui.label(RichText::new(pick("No calendars yet.", "Aún no hay calendarios.")).color(c.text_muted));
        return;
    }
    let mut accounts: Vec<iuredesk_model::AccountId> = Vec::new();
    for cal in &cals {
        if !accounts.contains(&cal.account) {
            accounts.push(cal.account);
        }
    }
    for acct in accounts {
        let head = labels
            .iter()
            .find(|l| l.id == acct)
            .map_or_else(|| pick("Calendars", "Calendarios").to_string(), |l| l.email.clone());
        section_label(ui, &head);
        ui.add_space(2.0);
        for cal in cals.iter().filter(|x| x.account == acct) {
            let hidden = app.session.calendar_hidden(cal.id);
            let resp = ui
                .horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    let (r, resp) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::click());
                    let col = rgb(cal.color.unwrap_or(0x3B82F6));
                    if hidden {
                        ui.painter().rect_stroke(r.shrink(0.5), 3.0, Stroke::new(1.5, col), egui::StrokeKind::Inside);
                    } else {
                        ui.painter().rect_filled(r, 3.0, col);
                        // Palomita.
                        let a = pos2(r.left() + 3.0, r.center().y);
                        let b = pos2(r.left() + 6.0, r.bottom() - 3.5);
                        let d = pos2(r.right() - 3.0, r.top() + 3.5);
                        ui.painter().line_segment([a, b], Stroke::new(1.6, Color32::WHITE));
                        ui.painter().line_segment([b, d], Stroke::new(1.6, Color32::WHITE));
                    }
                    // Sin emoji: las fuentes de la suite no lo traen; el candado es un icono dibujado.
                    let l = ui.add(
                        egui::Label::new(RichText::new(&cal.name).color(if hidden { c.text_muted } else { c.text }))
                            .sense(Sense::click()),
                    );
                    if cal.read_only {
                        let (r, lock) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), Sense::hover());
                        icons::paint(ui.painter(), r, Icon::Lock, c.text_muted, c.text_muted);
                        lock.on_hover_text(pick("Read-only", "Solo lectura"));
                    }
                    resp | l
                })
                .inner;
            iurefficient_ui::a11y::toggle(&resp, &cal.name, !hidden);
            if resp.clicked() {
                app.session.set_calendar_hidden(cal.id, !hidden);
            }
        }
        ui.add_space(8.0);
    }
}

fn capitalize(s: &str) -> String {
    let mut ch = s.chars();
    match ch.next() {
        Some(f) => f.to_uppercase().collect::<String>() + ch.as_str(),
        None => String::new(),
    }
}

fn top_bar(app: &mut DeskApp, ui: &mut Ui, anchor: NaiveDate, today: NaiveDate) {
    let c = theme::colors(ui);
    let ctx = ui.ctx().clone();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let _ = today;
        if ui.button(pick("Today", "Hoy")).clicked() {
            app.run(&ctx, "cal.today");
            app.calendar.scrolled = false;
            app.calendar.scroll_frames = 0;
        }
        if icons::icon_button(ui, Icon::ChevronLeft, pick("Previous", "Anterior"), true).clicked() {
            app.run(&ctx, "cal.prev");
        }
        if icons::icon_button(ui, Icon::ChevronRight, pick("Next", "Siguiente"), true).clicked() {
            app.run(&ctx, "cal.next");
        }
        ui.add_space(6.0);
        let view = app.session.cal_view();
        ui.label(RichText::new(title(view, anchor)).font(theme::bold_font(ui.ctx(), 17.0)).color(c.text));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let mut v = view;
            segmented(ui, &mut v, &CalView::ALL.map(|x| (x, x.label())));
            if v != view {
                app.run(&ctx, &format!("cal.view.{}", v.key()));
            }
        });
    });
}

/// Hora local de un instante en la zona del usuario.
fn local(app: &DeskApp, t: DateTime<Utc>) -> NaiveDateTime {
    app.session.zone().local(t)
}

fn week_view(app: &mut DeskApp, ui: &mut Ui, mon: NaiveDate, days: i64, today: NaiveDate) {
    let c = theme::colors(ui);
    let zone = app.session.zone();
    let now = app.session.now();
    let from = zone.day_start(mon);
    let to = zone.day_start(mon + Duration::days(days));
    let occ: Vec<Occurrence> = app.session.visible_occurrences(from, to);
    let second: Option<iuredesk_engine::chrono_tz::Tz> =
        app.session.prefs.secondary_tz.as_deref().and_then(|t| t.parse().ok());
    let gutter = if second.is_some() { 104.0 } else { 62.0 };
    let avail = ui.available_rect_before_wrap();
    let col_w = ((avail.width() - gutter - 8.0) / days as f32).max(40.0);
    let hh = hour_height(ui);
    let pointer = ui.ctx().pointer_interact_pos();
    let released = ui.input(|i| i.pointer.any_released());
    let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
    if escape {
        app.calendar.drag = None;
    }

    // Encabezado de días.
    let (head, _) = ui.allocate_exact_size(vec2(avail.width(), if days == 1 { 52.0 } else { 46.0 }), Sense::hover());
    let p = ui.painter_at(head);
    if let Some(z) = second {
        p.text(
            pos2(head.left() + 4.0, head.bottom() - 8.0),
            Align2::LEFT_CENTER,
            tz_label(z.name()),
            FontId::proportional(10.0),
            c.text_muted,
        );
    }
    for d in 0..days {
        let day = mon + Duration::days(d);
        let x = head.left() + gutter + col_w * d as f32;
        let is_today = day == today;
        let label = weekday_short(day).to_uppercase();
        p.text(
            pos2(x + 10.0, head.top() + 12.0),
            Align2::LEFT_CENTER,
            label,
            FontId::proportional(11.0),
            if is_today { c.accent } else { c.text_muted },
        );
        let num_pos = pos2(x + 10.0, head.top() + 31.0);
        if is_today {
            let g = p.layout_no_wrap(day.day().to_string(), theme::bold_font(ui.ctx(), 17.0), c.on_accent);
            let r = Rect::from_min_size(num_pos - vec2(4.0, g.size().y / 2.0 + 1.0), g.size() + vec2(8.0, 2.0));
            p.rect_filled(r.expand2(vec2(2.0, 0.0)), 8.0, c.accent);
            p.galley(pos2(num_pos.x, num_pos.y - g.size().y / 2.0), g, c.on_accent);
        } else {
            p.text(num_pos, Align2::LEFT_CENTER, day.day().to_string(), theme::bold_font(ui.ctx(), 17.0), c.text);
        }
    }

    // Fila de días completos (se puede arrastrar para crear o mover).
    let all_day: Vec<&Occurrence> = occ.iter().filter(|o| o.all_day).collect();
    let lanes = all_day.len().clamp(1, 3);
    let ad_h = 8.0 + 22.0 * lanes as f32;
    let (ad, ad_resp) = ui.allocate_exact_size(vec2(avail.width(), ad_h), Sense::click_and_drag());
    iurefficient_ui::a11y::area(&ad_resp, pick("All-day events", "Eventos de todo el día"));
    let p = ui.painter_at(ad);
    p.hline(ad.x_range(), ad.top(), Stroke::new(1.0, c.separator));
    p.text(
        pos2(ad.left() + gutter - 8.0, ad.top() + 15.0),
        Align2::RIGHT_CENTER,
        pick("all day", "todo el día"),
        FontId::proportional(10.5),
        c.text_muted,
    );
    let col_of = |x: f32| column_at(x - ad.left() - gutter, col_w, days);
    if ad_resp.drag_started()
        && let Some(pt) = pointer
        && pt.x > ad.left() + gutter
    {
        app.calendar.drag = Some(Drag::AllDayCreate { col: col_of(pt.x) });
    }
    for (lane, o) in all_day.iter().take(3).enumerate() {
        let Some((d0, d1)) = o.days else { continue };
        let (d0, d1) = match (&app.calendar.drag, pointer) {
            (Some(Drag::AllDayMove { occ, grab_col }), Some(pt)) if occ.id == o.id && occ.start == o.start => {
                move_days(d0, d1, col_of(pt.x) - grab_col)
            }
            _ => (d0, d1),
        };
        let s = (d0 - mon).num_days().clamp(0, days);
        let e = (d1 - mon).num_days().clamp(0, days);
        if e <= s {
            continue;
        }
        let r = Rect::from_min_max(
            pos2(ad.left() + gutter + col_w * s as f32 + 2.0, ad.top() + 4.0 + 22.0 * lane as f32),
            pos2(ad.left() + gutter + col_w * e as f32 - 3.0, ad.top() + 4.0 + 22.0 * lane as f32 + 19.0),
        );
        let resp = ui.interact(r, ui.scope_id().with(("ad", o.id, o.start.timestamp())), Sense::click_and_drag());
        iurefficient_ui::a11y::button(&resp, &o.summary);
        let col = rgb(o.color);
        p.rect_filled(r, 4.0, mix(c.surface, col, if ui.visuals().dark_mode { 0.35 } else { 0.22 }));
        p.rect_filled(Rect::from_min_max(r.min, pos2(r.left() + 3.0, r.bottom())), 2.0, col);
        p.with_clip_rect(r).text(
            pos2(r.left() + 8.0, r.center().y),
            Align2::LEFT_CENTER,
            &o.summary,
            FontId::proportional(12.0),
            c.text,
        );
        if resp.clicked() {
            event_editor::open_occurrence(app, o);
        }
        if resp.drag_started()
            && let Some(pt) = pointer
            && app.session.can_edit(o.calendar)
        {
            app.calendar.drag = Some(Drag::AllDayMove { occ: Box::new((*o).clone()), grab_col: col_of(pt.x) });
        }
    }
    if let (Some(Drag::AllDayCreate { col }), Some(pt)) = (&app.calendar.drag, pointer) {
        let (a, b) = (*col, col_of(pt.x));
        let (s, e) = (a.min(b), a.max(b) + 1);
        let r = Rect::from_min_max(
            pos2(ad.left() + gutter + col_w * s as f32 + 2.0, ad.top() + 4.0),
            pos2(ad.left() + gutter + col_w * e as f32 - 3.0, ad.top() + 23.0),
        );
        p.rect_filled(r, 4.0, mix(c.surface, c.accent, 0.3));
    }
    if released {
        match app.calendar.drag.clone() {
            Some(Drag::AllDayCreate { col }) => {
                app.calendar.drag = None;
                let b = pointer.map_or(col, |pt| col_of(pt.x));
                let (d0, d1) = day_span(mon + Duration::days(col), mon + Duration::days(b));
                event_editor::quick_create(app, at_minutes(d0, 0), at_minutes(d1, 0), true, ad.center_top());
            }
            Some(Drag::AllDayMove { occ, grab_col }) => {
                app.calendar.drag = None;
                if let (Some(pt), Some((d0, d1))) = (pointer, occ.days) {
                    let delta = col_of(pt.x) - grab_col;
                    if delta != 0 {
                        let (n0, n1) = move_days(d0, d1, delta);
                        event_editor::reschedule(app, &occ, EventTime::Date(n0), EventTime::Date(n1));
                    }
                }
            }
            _ => {}
        }
    }
    hairline(ui);

    // Rejilla de horas.
    let scroll = egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("week-scroll");
    // El desplazamiento inicial se repite unos cuadros: el primero aún no conoce el alto del
    // contenido y egui lo recortaría.
    let scroll = if app.calendar.scrolled { scroll } else { scroll.vertical_scroll_offset(hh * 7.5) };
    app.calendar.scroll_frames = app.calendar.scroll_frames.saturating_add(1);
    if app.calendar.scroll_frames >= 3 {
        app.calendar.scrolled = true;
    }
    scroll.show(ui, |ui| {
        let (grid, grid_resp) =
            ui.allocate_exact_size(vec2(ui.available_width(), hh * 24.0 + 8.0), Sense::click_and_drag());
        iurefficient_ui::a11y::area(&grid_resp, pick("Calendar grid", "Rejilla del calendario"));
        let p = ui.painter_at(grid);
        let top = grid.top() + 4.0;
        let cols_left = grid.left() + gutter;
        let minute_at = |y: f32| y_to_minutes(y - top, hh);
        let col_at = |x: f32| column_at(x - cols_left, col_w, days);
        // Fondo de hoy y fines de semana.
        for d in 0..days {
            let day = mon + Duration::days(d);
            let x = cols_left + col_w * d as f32;
            let r = Rect::from_min_max(pos2(x, top), pos2(x + col_w, top + hh * 24.0));
            if day == today && days > 1 {
                p.rect_filled(r, 0.0, mix(c.surface, c.accent, 0.04));
            } else if day.weekday().num_days_from_monday() >= 5 {
                p.rect_filled(r, 0.0, c.surface_alt);
            }
            p.vline(x, r.y_range(), Stroke::new(1.0, c.separator));
        }
        let second_off = second.map(|z| {
            use chrono::Offset;
            let utc_now = now;
            let a = utc_now.with_timezone(&z).offset().fix().local_minus_utc();
            let b = zone_offset(app, utc_now);
            i64::from(a - b) / 60
        });
        for h in 0..24 {
            let y = top + hh * h as f32;
            p.hline((cols_left - 4.0)..=grid.right(), y, Stroke::new(1.0, c.separator));
            let half = Stroke::new(1.0, mix(c.surface, c.separator, 0.45));
            p.hline(cols_left..=grid.right(), y + hh / 2.0, half);
            if h > 0 {
                p.text(
                    pos2(cols_left - 8.0, y),
                    Align2::RIGHT_CENTER,
                    format!("{h:02}:00"),
                    FontId::proportional(11.0),
                    c.text_muted,
                );
                if let Some(off) = second_off {
                    let m = (i64::from(h) * 60 + off).rem_euclid(DAY_MIN);
                    p.text(
                        pos2(grid.left() + 6.0, y),
                        Align2::LEFT_CENTER,
                        format!("{:02}:{:02}", m / 60, m % 60),
                        FontId::proportional(10.5),
                        mix(c.text_muted, c.surface, 0.25),
                    );
                }
            }
        }
        // Empezar a crear arrastrando sobre el fondo.
        if grid_resp.drag_started()
            && let Some(pt) = pointer
            && pt.x > cols_left
            && app.calendar.drag.is_none()
        {
            app.calendar.drag = Some(Drag::Create { col: col_at(pt.x), press_min: minute_at(pt.y) });
        }
        // Un clic en el fondo: crear rápido de 30 minutos.
        if grid_resp.clicked()
            && let Some(pt) = pointer
            && pt.x > cols_left
        {
            let day = mon + Duration::days(col_at(pt.x));
            let (s, e) = create_span(minute_at(pt.y), minute_at(pt.y));
            let anchor_pos = pos2(cols_left + col_w * (col_at(pt.x) as f32 + 1.0), top + hh * s as f32 / 60.0);
            event_editor::quick_create(app, at_minutes(day, s), at_minutes(day, e), false, anchor_pos);
        }
        // Eventos con hora, por día y en columnas si se solapan.
        let dragging_id = match &app.calendar.drag {
            Some(Drag::Move { occ, moved: true, .. }) | Some(Drag::Resize { occ }) => Some((occ.id, occ.start)),
            _ => None,
        };
        for d in 0..days {
            let day = mon + Duration::days(d);
            let x = cols_left + col_w * d as f32;
            let mut todays: Vec<&Occurrence> =
                occ.iter().filter(|o| !o.all_day && local(app, o.start).date() == day).collect();
            todays.sort_by_key(|o| o.start);
            let spans: Vec<(i64, i64)> = todays
                .iter()
                .map(|o| {
                    let s = local(app, o.start);
                    let e = local(app, o.end);
                    let sm = i64::from(s.hour() * 60 + s.minute());
                    let em = if e.date() > day { DAY_MIN } else { i64::from(e.hour() * 60 + e.minute()) };
                    (sm, em.max(sm + 20))
                })
                .collect();
            let cols = layout_columns(&spans);
            for ((o, (sm, em)), (ci, n)) in todays.iter().zip(spans.iter()).zip(cols.iter()) {
                let w = (col_w - 6.0) / *n as f32;
                let (y0, y1) = block_y(*sm, *em, hh);
                let r = Rect::from_min_max(
                    pos2(x + 3.0 + w * *ci as f32, top + y0),
                    pos2(x + 3.0 + w * (*ci as f32 + 1.0) - 2.0, top + y1),
                );
                let ghost = dragging_id == Some((o.id, o.start));
                event_block(ui, &p, r, o, zone, ghost);
                let id = ui.scope_id().with(("ev", o.id, o.start.timestamp()));
                let editable = app.session.can_edit(o.calendar);
                let handle = Rect::from_min_max(pos2(r.left(), r.bottom() - 5.0), r.max);
                let body = Rect::from_min_max(r.min, pos2(r.right(), (r.bottom() - 5.0).max(r.top() + 4.0)));
                let resp = ui.interact(body, id, Sense::click_and_drag());
                iurefficient_ui::a11y::button(&resp, &format!("{} {}", o.summary, zone.local(o.start).format("%H:%M")));
                if editable {
                    let hr = ui.interact(handle, id.with("resize"), Sense::drag());
                    if hr.hovered() || matches!(app.calendar.drag, Some(Drag::Resize { .. })) {
                        ui.ctx().set_cursor_icon(CursorIcon::ResizeVertical);
                    }
                    if hr.drag_started() {
                        app.calendar.drag = Some(Drag::Resize { occ: Box::new((*o).clone()) });
                    }
                }
                if resp.clicked() {
                    event_editor::open_occurrence(app, o);
                }
                if resp.drag_started()
                    && editable
                    && let Some(pt) = pointer
                {
                    let grab = minute_at(pt.y) - *sm as f32;
                    app.calendar.drag = Some(Drag::Move { occ: Box::new((*o).clone()), grab_min: grab, moved: false });
                }
            }
        }
        // Vista previa del arrastre.
        if let Some(pt) = pointer {
            let preview: Option<(i64, i64, i64, String, u32)> = match &mut app.calendar.drag {
                Some(Drag::Create { col, press_min }) => {
                    let (s, e) = create_span(*press_min, minute_at(pt.y));
                    Some((*col, s, e, String::new(), 0))
                }
                Some(Drag::Move { occ, grab_min, moved }) => {
                    let (s0, e0) = (zone.local(occ.start), zone.local(occ.end));
                    let delta_days = (mon + Duration::days(col_at(pt.x)) - s0.date()).num_days();
                    let delta_min = minute_at(pt.y) - *grab_min - (s0.hour() * 60 + s0.minute()) as f32;
                    let (ns, ne) = move_times(s0, e0, delta_days, delta_min);
                    if ns != s0 {
                        *moved = true;
                    }
                    let col = (ns.date() - mon).num_days();
                    let em = if ne.date() > ns.date() { DAY_MIN } else { minutes_of(ne) };
                    moved.then_some((col, minutes_of(ns), em, occ.summary.clone(), occ.color))
                }
                Some(Drag::Resize { occ }) => {
                    let (s0, e0) = (zone.local(occ.start), zone.local(occ.end));
                    let cur_end = (minute_at(pt.y) - minutes_of(e0) as f32) + 0.0;
                    let ne = resize_end(s0, e0, cur_end);
                    let col = (s0.date() - mon).num_days();
                    let em = if ne.date() > s0.date() { DAY_MIN } else { minutes_of(ne) };
                    Some((col, minutes_of(s0), em, occ.summary.clone(), occ.color))
                }
                _ => None,
            };
            if let Some((col, s, e, label, color)) = preview
                && (0..days).contains(&col)
            {
                let x = cols_left + col_w * col as f32;
                let (y0, y1) = block_y(s, e, hh);
                let r = Rect::from_min_max(pos2(x + 3.0, top + y0), pos2(x + col_w - 3.0, top + y1));
                let col_c = if color == 0 { c.accent } else { rgb(color) };
                p.rect_filled(r, 5.0, mix(c.surface, col_c, 0.35));
                p.rect_stroke(r, 5.0, Stroke::new(1.5, col_c), egui::StrokeKind::Inside);
                let t = format!(
                    "{}{:02}:{:02}–{:02}:{:02}",
                    if label.is_empty() { String::new() } else { format!("{label}  ") },
                    s / 60,
                    s % 60,
                    e / 60,
                    e % 60
                );
                p.with_clip_rect(r).text(
                    r.min + vec2(7.0, 3.0),
                    Align2::LEFT_TOP,
                    t,
                    theme::bold_font(ui.ctx(), 12.0),
                    c.text,
                );
            }
        }
        // Soltar: crear, mover o estirar.
        if released && let Some(drag) = app.calendar.drag.take() {
            match (drag, pointer) {
                (Drag::Create { col, press_min }, Some(pt)) => {
                    let day = mon + Duration::days(col);
                    let (s, e) = create_span(press_min, minute_at(pt.y));
                    let anchor_pos = pos2(cols_left + col_w * (col as f32 + 1.0), top + hh * s as f32 / 60.0);
                    event_editor::quick_create(app, at_minutes(day, s), at_minutes(day, e), false, anchor_pos);
                }
                (Drag::Move { occ, grab_min, moved: true }, Some(pt)) => {
                    let (s0, e0) = (zone.local(occ.start), zone.local(occ.end));
                    let delta_days = (mon + Duration::days(col_at(pt.x)) - s0.date()).num_days();
                    let delta_min = minute_at(pt.y) - grab_min - (s0.hour() * 60 + s0.minute()) as f32;
                    let (ns, ne) = move_times(s0, e0, delta_days, delta_min);
                    if ns != s0 {
                        let tz = |t| EventTime::DateTime { utc: zone.to_utc(t), tzid: None };
                        event_editor::reschedule(app, &occ, tz(ns), tz(ne));
                    }
                }
                (Drag::Resize { occ }, Some(pt)) => {
                    let (s0, e0) = (zone.local(occ.start), zone.local(occ.end));
                    let ne = resize_end(s0, e0, minute_at(pt.y) - minutes_of(e0) as f32);
                    if ne != e0 {
                        let tz = |t| EventTime::DateTime { utc: zone.to_utc(t), tzid: None };
                        event_editor::reschedule(app, &occ, tz(s0), tz(ne));
                    }
                }
                (other @ (Drag::AllDayCreate { .. } | Drag::AllDayMove { .. }), _) => app.calendar.drag = Some(other),
                _ => {}
            }
        }
        if app.calendar.drag.is_some() {
            ui.ctx().request_repaint();
        }
        // Línea de ahora.
        let ln = zone.local(now);
        let nd = ln.date();
        if nd >= mon && nd < mon + Duration::days(days) {
            let d = (nd - mon).num_days();
            let x = cols_left + col_w * d as f32;
            let y = top + hh * ((ln.hour() * 60 + ln.minute()) as f32 / 60.0);
            p.hline(x..=(x + col_w), y, Stroke::new(2.0, c.danger));
            p.circle_filled(pos2(x, y), 4.5, c.danger);
        }
    });
}

fn minutes_of(t: NaiveDateTime) -> i64 {
    iuredesk_engine::calendar::drag::minutes_of(t)
}

/// Desfase de la zona de la vista en un instante (segundos).
fn zone_offset(app: &DeskApp, t: DateTime<Utc>) -> i32 {
    let l = app.session.zone().local(t);
    i32::try_from((l - t.naive_utc()).num_seconds()).unwrap_or(0)
}

fn event_block(ui: &Ui, p: &egui::Painter, r: Rect, o: &Occurrence, zone: iuredesk_engine::Zone, ghost: bool) {
    let c = theme::colors(ui);
    let col = rgb(o.color);
    let dark = ui.visuals().dark_mode;
    let fill = mix(
        c.surface,
        col,
        if ghost {
            0.08
        } else if dark {
            0.32
        } else {
            0.18
        },
    );
    p.rect_filled(r, 5.0, fill);
    if o.tentative || !o.busy {
        p.rect_stroke(r, 5.0, Stroke::new(1.0, col), egui::StrokeKind::Inside);
    }
    p.rect_filled(Rect::from_min_max(r.min, pos2(r.left() + 3.5, r.bottom())), 2.0, col);
    let inner = r.shrink2(vec2(7.0, 3.0));
    let inner = Rect::from_min_max(pos2(inner.left() + 2.0, inner.top()), inner.max);
    let clip = p.with_clip_rect(r.shrink(1.0));
    let title_font = theme::bold_font(ui.ctx(), 12.5);
    let ink = if dark { c.text } else { mix(col, Color32::BLACK, 0.55) };
    let summary = o.summary.clone();
    let time = format!("{}–{}", zone.local(o.start).format("%H:%M"), zone.local(o.end).format("%H:%M"));
    // Bloque corto (≤ 30 min): título y hora en una línea, recortados con «…».
    let line_h = 16.0;
    if inner.height() < line_h * 2.0 {
        let text = format!("{summary}  {}", zone.local(o.start).format("%H:%M"));
        let mut job = egui::text::LayoutJob::simple_singleline(text, title_font, ink);
        job.wrap = egui::text::TextWrapping::truncate_at_width(inner.width());
        let g = clip.ctx().fonts_mut(|f| f.layout_job(job));
        clip.galley(pos2(inner.left(), r.center().y - g.size().y / 2.0), g, ink);
        return;
    }
    // Título en hasta dos líneas (tres si el bloque es alto), y luego la hora y el lugar.
    let max_rows = if inner.height() >= line_h * 4.0 { 3 } else { 2 };
    let mut job = egui::text::LayoutJob::simple(summary, title_font, ink, inner.width());
    job.wrap.max_rows = max_rows;
    job.wrap.break_anywhere = false;
    let g = clip.ctx().fonts_mut(|f| f.layout_job(job));
    let title_h = g.size().y;
    clip.galley(inner.min, g, ink);
    let mut y = inner.top() + title_h + 1.0;
    if y + 14.0 <= inner.bottom() {
        clip.text(pos2(inner.left(), y), Align2::LEFT_TOP, time, FontId::proportional(11.0), c.text_muted);
        y += 14.0;
    }
    if !o.location.is_empty() && y + 14.0 <= inner.bottom() {
        clip.text(pos2(inner.left(), y), Align2::LEFT_TOP, &o.location, FontId::proportional(11.0), c.text_muted);
    }
}

/// Posición vertical (arriba, abajo) de un evento de `sm` a `em` minutos del día, con `hh` píxeles
/// por hora: una hora ocupa exactamente `hh` (menos 2 px de separación); lo muy corto, al menos 20
/// minutos para que se pueda leer.
pub(crate) fn block_y(sm: i64, em: i64, hh: f32) -> (f32, f32) {
    let sm = sm.clamp(0, 24 * 60);
    let em = em.clamp(sm, 24 * 60).max((sm + 20).min(24 * 60));
    (hh * (sm as f32 / 60.0) + 1.0, hh * (em as f32 / 60.0) - 1.0)
}

fn month_view(app: &mut DeskApp, ui: &mut Ui, anchor: NaiveDate, today: NaiveDate) {
    let c = theme::colors(ui);
    let zone = app.session.zone();
    let (start, _) = visible_range(CalView::Month, anchor);
    let from = zone.day_start(start);
    let to = zone.day_start(start + Duration::days(42));
    let occ: Vec<Occurrence> = app.session.visible_occurrences(from, to);
    let rect = ui.available_rect_before_wrap();
    let head_h = 26.0;
    let cw = rect.width() / 7.0;
    let ch = ((rect.height() - head_h) / 6.0).max(60.0);
    let (_, _) = ui.allocate_exact_size(rect.size(), Sense::hover());
    let p = ui.painter_at(rect);
    for i in 0..7i64 {
        let d = start + Duration::days(i);
        p.text(
            pos2(rect.left() + cw * i as f32 + 10.0, rect.top() + head_h / 2.0),
            Align2::LEFT_CENTER,
            weekday_short(d).to_uppercase(),
            FontId::proportional(11.0),
            c.text_muted,
        );
    }
    for k in 0..42i64 {
        let day = start + Duration::days(k);
        let (col, row) = ((k % 7) as f32, (k / 7) as f32);
        let r = Rect::from_min_size(pos2(rect.left() + cw * col, rect.top() + head_h + ch * row), vec2(cw, ch));
        if day.month() != anchor.month() {
            p.rect_filled(r, 0.0, c.surface_alt);
        }
        p.rect_stroke(r, 0.0, Stroke::new(1.0, c.separator), egui::StrokeKind::Inside);
        // Doble clic en el día: evento nuevo; clic en el número: vista de día.
        let cell = ui.interact(r, ui.scope_id().with(("month-cell", k)), Sense::click());
        iurefficient_ui::a11y::button(&cell, &day.format("%Y-%m-%d").to_string());
        if cell.double_clicked() {
            event_editor::quick_create(app, at_minutes(day, 9 * 60), at_minutes(day, 10 * 60), false, r.center());
        }
        let num = day.day().to_string();
        if day == today {
            p.circle_filled(pos2(r.left() + 16.0, r.top() + 14.0), 11.0, c.accent);
            p.text(
                pos2(r.left() + 16.0, r.top() + 14.0),
                Align2::CENTER_CENTER,
                num,
                theme::bold_font(ui.ctx(), 12.0),
                c.on_accent,
            );
        } else {
            p.text(
                pos2(r.left() + 10.0, r.top() + 14.0),
                Align2::LEFT_CENTER,
                num,
                FontId::proportional(12.0),
                if day.month() == anchor.month() { c.text } else { c.text_muted },
            );
        }
        let todays: Vec<&Occurrence> = occ
            .iter()
            .filter(|o| match o.days {
                Some((d0, d1)) => day >= d0 && day < d1,
                None => zone.local(o.start).date() == day,
            })
            .collect();
        let max = (((ch - 30.0) / 18.0).floor() as usize).max(1);
        for (i, o) in todays.iter().take(max).enumerate() {
            let er = Rect::from_min_size(pos2(r.left() + 4.0, r.top() + 28.0 + 18.0 * i as f32), vec2(cw - 8.0, 16.0));
            let col = rgb(o.color);
            let clip = p.with_clip_rect(er);
            let resp = ui.interact(er, ui.scope_id().with(("month-ev", o.id, o.start.timestamp(), k)), Sense::click());
            iurefficient_ui::a11y::button(&resp, &o.summary);
            if resp.hovered() {
                clip.rect_filled(er, 3.0, c.hover);
            }
            if resp.clicked() {
                event_editor::open_occurrence(app, o);
            }
            if o.all_day {
                clip.rect_filled(er, 3.0, mix(c.surface, col, 0.25));
                clip.text(
                    pos2(er.left() + 4.0, er.center().y),
                    Align2::LEFT_CENTER,
                    &o.summary,
                    FontId::proportional(11.5),
                    c.text,
                );
            } else {
                clip.circle_filled(pos2(er.left() + 5.0, er.center().y), 3.0, col);
                let t = format!("{} {}", zone.local(o.start).format("%H:%M"), o.summary);
                clip.text(
                    pos2(er.left() + 12.0, er.center().y),
                    Align2::LEFT_CENTER,
                    t,
                    FontId::proportional(11.5),
                    c.text,
                );
            }
        }
        if todays.len() > max {
            p.text(
                pos2(r.left() + 8.0, r.bottom() - 9.0),
                Align2::LEFT_CENTER,
                format!("+{} {}", todays.len() - max, pick("more", "más")),
                FontId::proportional(11.0),
                c.text_muted,
            );
        }
    }
}

/// Agenda: lo que viene en los próximos 30 días, agrupado por día.
fn agenda_view(app: &mut DeskApp, ui: &mut Ui, anchor: NaiveDate, today: NaiveDate) {
    let c = theme::colors(ui);
    let zone = app.session.zone();
    let (start, days) = visible_range(CalView::Agenda, anchor);
    let from = zone.day_start(start);
    let to = zone.day_start(start + Duration::days(days));
    let occ = app.session.visible_occurrences(from, to);
    let now = app.session.now();
    let mut groups: Vec<(NaiveDate, Vec<&Occurrence>)> = Vec::new();
    for k in 0..days {
        let day = start + Duration::days(k);
        let list: Vec<&Occurrence> = occ
            .iter()
            .filter(|o| match o.days {
                Some((d0, d1)) => day >= d0 && day < d1,
                None => zone.local(o.start).date() == day,
            })
            .collect();
        if !list.is_empty() {
            groups.push((day, list));
        }
    }
    if groups.is_empty() {
        ui.add_space(40.0);
        EmptyState::new(Icon::Calendar, pick("Nothing in the next 30 days", "Nada en los próximos 30 días"))
            .body(pick("Drag on the week view or press New event.", "Arrastra en la semana o pulsa «Nuevo evento»."))
            .show(ui);
        return;
    }
    egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("agenda-scroll").show(ui, |ui| {
        ui.add_space(8.0);
        let width = ui.available_width().min(860.0);
        for (day, list) in groups {
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                let head = capitalize(&day_title(day));
                let ink = if day == today { c.accent } else { c.text };
                ui.label(RichText::new(head).font(theme::bold_font(ui.ctx(), 14.0)).color(ink));
                if day == today {
                    ui.label(RichText::new(pick("Today", "Hoy")).color(c.accent).size(12.0));
                }
            });
            ui.add_space(2.0);
            for o in list {
                let (r, resp) = ui.allocate_exact_size(vec2(width, 30.0), Sense::click());
                iurefficient_ui::a11y::button(&resp, &o.summary);
                let past = !o.all_day && o.end <= now;
                if resp.hovered() {
                    ui.painter().rect_filled(r.shrink2(vec2(8.0, 0.0)), 6.0, c.hover);
                }
                let p = ui.painter_at(r);
                let time = if o.all_day {
                    pick("All day", "Todo el día").to_string()
                } else {
                    format!("{} – {}", zone.local(o.start).format("%H:%M"), zone.local(o.end).format("%H:%M"))
                };
                let muted = if past { mix(c.text_muted, c.surface, 0.3) } else { c.text_muted };
                p.text(
                    pos2(r.left() + 24.0, r.center().y),
                    Align2::LEFT_CENTER,
                    time,
                    theme::tabular_font(ui.ctx(), 12.5),
                    muted,
                );
                p.circle_filled(pos2(r.left() + 146.0, r.center().y), 4.5, rgb(o.color));
                let ink = if past { c.text_muted } else { c.text };
                let g = p.layout_no_wrap(o.summary.clone(), theme::medium_font(ui.ctx(), 13.5), ink);
                let tx = r.left() + 160.0;
                let gw = g.size().x;
                p.galley(pos2(tx, r.center().y - g.size().y / 2.0), g, ink);
                if !o.location.is_empty() {
                    p.text(
                        pos2(tx + gw + 12.0, r.center().y),
                        Align2::LEFT_CENTER,
                        &o.location,
                        FontId::proportional(12.0),
                        c.text_muted,
                    );
                }
                if resp.clicked() {
                    event_editor::open_occurrence(app, o);
                }
            }
            ui.add_space(10.0);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::block_y;

    #[test]
    fn one_hour_is_one_hour_tall() {
        let hh = 44.0;
        let (a, b) = block_y(9 * 60, 10 * 60, hh);
        assert!((b - a - (hh - 2.0)).abs() < 0.01);
        let (a2, b2) = block_y(14 * 60, 15 * 60, hh);
        assert!(((b2 - a2) - (b - a)).abs() < 0.01, "igual en cualquier columna y hora");
        let (a, b) = block_y(9 * 60, 9 * 60 + 90, hh);
        assert!((b - a - (hh * 1.5 - 2.0)).abs() < 0.01);
        // Muy corto o al revés: mínimo legible, nunca negativo.
        let (a, b) = block_y(600, 600, hh);
        assert!(b > a);
        let (a, b) = block_y(23 * 60 + 50, 24 * 60, hh);
        assert!(b > a && b <= hh * 24.0);
    }
}
