//! Calendario: semana laboral, semana y mes; fila de días completos, eventos que se solapan en
//! columnas y la línea de «ahora».

use chrono::{Datelike, Duration, NaiveDate, Timelike};
use egui::{Align, Align2, Color32, FontId, Layout, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};
use iuredesk_engine::calendar::{Occurrence, layout_columns};
use iuredesk_engine::time::{monday_of, month_name, weekday_short};
use iurefficient_ui::{Icon, PaneKind, icons, pick, section_label, segmented, split, theme};

use crate::app::DeskApp;
use crate::widgets::{hairline, mix, rgb};

/// Vista del calendario.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CalView {
    #[default]
    WorkWeek,
    Week,
    Month,
}

/// Estado del calendario.
#[derive(Default)]
pub struct CalendarState {
    pub view: CalView,
    /// Día de referencia (`None` = hoy).
    pub anchor: Option<NaiveDate>,
    scrolled: bool,
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
    let anchor = app.calendar.anchor.unwrap_or(today);
    split::left(ui, "cal-side", PaneKind::Sidebar, 244.0, 200.0..=360.0, |ui| sidebar(app, ui, anchor, today));
    split::top_bar(ui, "cal-top", |ui| top_bar(app, ui, anchor, today));
    split::center(ui, PaneKind::Content, |ui| match app.calendar.view {
        CalView::Month => month_view(app, ui, anchor, today),
        v => week_view(app, ui, anchor, today, if v == CalView::WorkWeek { 5 } else { 7 }),
    });
}

fn sidebar(app: &mut DeskApp, ui: &mut Ui, anchor: NaiveDate, today: NaiveDate) {
    let c = theme::colors(ui);
    ui.add_space(6.0);
    // Mini mes.
    let first = NaiveDate::from_ymd_opt(anchor.year(), anchor.month(), 1).unwrap_or(anchor);
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!("{} {}", capitalize(month_name(anchor.month())), anchor.year()))
                .strong()
                .color(c.text),
        );
    });
    ui.add_space(4.0);
    let start = monday_of(first);
    let cell = ((ui.available_width() - 4.0) / 7.0).floor().clamp(20.0, 36.0);
    let (rect, _) = ui.allocate_exact_size(vec2(cell * 7.0, cell * 7.0), Sense::hover());
    let p = ui.painter_at(rect);
    for (i, d) in ["L", "M", "X", "J", "V", "S", "D"].iter().enumerate() {
        let en = ["M", "T", "W", "T", "F", "S", "S"];
        let label = pick(en.get(i).copied().unwrap_or(""), d);
        p.text(
            pos2(rect.left() + cell * (i as f32 + 0.5), rect.top() + cell * 0.5),
            Align2::CENTER_CENTER,
            label,
            FontId::proportional(11.0),
            c.text_muted,
        );
    }
    for k in 0..42i64 {
        let day = start + Duration::days(k);
        let (col, row) = ((k % 7) as f32, (k / 7) as f32 + 1.0);
        let r = Rect::from_min_size(pos2(rect.left() + col * cell, rect.top() + row * cell), vec2(cell, cell));
        let resp = ui.interact(r, ui.scope_id().with(("mini", k)), Sense::click());
        let in_month = day.month() == anchor.month();
        let is_today = day == today;
        let selected = day == anchor;
        if is_today {
            p.circle_filled(r.center(), cell * 0.4, c.accent);
        } else if selected {
            p.circle_filled(r.center(), cell * 0.4, c.selection);
        } else if resp.hovered() {
            p.circle_filled(r.center(), cell * 0.4, c.hover);
        }
        let ink = if is_today {
            c.on_accent
        } else if in_month {
            c.text
        } else {
            c.text_muted
        };
        p.text(r.center(), Align2::CENTER_CENTER, day.day().to_string(), FontId::proportional(12.0), ink);
        iurefficient_ui::a11y::button(&resp, &day.format("%Y-%m-%d").to_string());
        if resp.clicked() {
            app.calendar.anchor = Some(day);
        }
    }
    ui.add_space(14.0);
    section_label(ui, pick("Calendars", "Calendarios"));
    ui.add_space(4.0);
    let labels = app.session.account_labels().to_vec();
    for cal in app.session.calendars().to_vec() {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let (r, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
            ui.painter().rect_filled(r, 3.0, rgb(cal.color.unwrap_or(0x3B82F6)));
            ui.label(RichText::new(&cal.name).color(c.text));
            if let Some(l) = labels.iter().find(|l| l.id == cal.account) {
                ui.add(
                    egui::Label::new(
                        RichText::new(&l.email).color(c.text_muted).size(theme::density(ui.ctx()).small_size()),
                    )
                    .truncate(),
                );
            }
        });
    }
    if app.session.calendars().is_empty() {
        ui.label(RichText::new(pick("No calendars yet.", "Aún no hay calendarios.")).color(c.text_muted));
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
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        if ui.button(pick("Today", "Hoy")).clicked() {
            app.calendar.anchor = Some(today);
            app.calendar.scrolled = false;
        }
        let step = match app.calendar.view {
            CalView::Month => 30,
            _ => 7,
        };
        let shift = |d: NaiveDate, fwd: bool| -> NaiveDate {
            if app.calendar.view == CalView::Month {
                let m = if fwd {
                    d.checked_add_months(chrono::Months::new(1))
                } else {
                    d.checked_sub_months(chrono::Months::new(1))
                };
                m.unwrap_or(d)
            } else if fwd {
                d + Duration::days(step)
            } else {
                d - Duration::days(step)
            }
        };
        if icons::icon_button(ui, Icon::ChevronLeft, pick("Previous", "Anterior"), true).clicked() {
            app.calendar.anchor = Some(shift(anchor, false));
        }
        if icons::icon_button(ui, Icon::ChevronRight, pick("Next", "Siguiente"), true).clicked() {
            app.calendar.anchor = Some(shift(anchor, true));
        }
        let title = match app.calendar.view {
            CalView::Month => format!("{} {}", capitalize(month_name(anchor.month())), anchor.year()),
            v => {
                let mon = monday_of(anchor);
                let end = mon + Duration::days(if v == CalView::WorkWeek { 4 } else { 6 });
                if mon.month() == end.month() {
                    if iuredesk_engine::lang::spanish() {
                        format!("{}–{} de {} de {}", mon.day(), end.day(), month_name(mon.month()), mon.year())
                    } else {
                        format!("{} {}–{}, {}", capitalize(month_name(mon.month())), mon.day(), end.day(), mon.year())
                    }
                } else {
                    let a: String = month_name(mon.month()).chars().take(3).collect();
                    let b: String = month_name(end.month()).chars().take(3).collect();
                    format!("{} {a} – {} {b} {}", mon.day(), end.day(), end.year())
                }
            }
        };
        ui.add_space(6.0);
        ui.label(RichText::new(title).font(theme::bold_font(ui.ctx(), 17.0)).color(c.text));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            segmented(
                ui,
                &mut app.calendar.view,
                &[
                    (CalView::WorkWeek, pick("Work week", "Semana laboral")),
                    (CalView::Week, pick("Week", "Semana")),
                    (CalView::Month, pick("Month", "Mes")),
                ],
            );
        });
    });
}

fn week_view(app: &mut DeskApp, ui: &mut Ui, anchor: NaiveDate, today: NaiveDate, days: i64) {
    let c = theme::colors(ui);
    let zone = app.session.zone();
    let now = app.session.now();
    let mon = monday_of(anchor);
    let from = zone.day_start(mon);
    let to = zone.day_start(mon + Duration::days(days));
    let occ: Vec<Occurrence> = app.session.occurrences(from, to).to_vec();
    let gutter = 66.0;
    let avail = ui.available_rect_before_wrap();
    let col_w = ((avail.width() - gutter - 8.0) / days as f32).max(40.0);
    let hh = hour_height(ui);

    // Encabezado de días.
    let (head, _) = ui.allocate_exact_size(vec2(avail.width(), 46.0), Sense::hover());
    let p = ui.painter_at(head);
    for d in 0..days {
        let day = mon + Duration::days(d);
        let x = head.left() + gutter + col_w * d as f32;
        let is_today = day == today;
        p.text(
            pos2(x + 10.0, head.top() + 12.0),
            Align2::LEFT_CENTER,
            weekday_short(day).to_uppercase(),
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

    // Fila de días completos.
    let all_day: Vec<&Occurrence> = occ.iter().filter(|o| o.all_day).collect();
    let lanes = if all_day.is_empty() { 0 } else { all_day.len().min(3) };
    let ad_h = 8.0 + 22.0 * lanes.max(1) as f32;
    let (ad, _) = ui.allocate_exact_size(vec2(avail.width(), ad_h), Sense::hover());
    let p = ui.painter_at(ad);
    p.hline(ad.x_range(), ad.top(), Stroke::new(1.0, c.separator));
    p.text(
        pos2(ad.left() + gutter - 8.0, ad.center().y),
        Align2::RIGHT_CENTER,
        pick("all day", "todo el día"),
        FontId::proportional(10.5),
        c.text_muted,
    );
    for (lane, o) in all_day.iter().take(3).enumerate() {
        let Some((d0, d1)) = o.days else { continue };
        let s = (d0 - mon).num_days().clamp(0, days);
        let e = (d1 - mon).num_days().clamp(0, days);
        if e <= s {
            continue;
        }
        let r = Rect::from_min_max(
            pos2(ad.left() + gutter + col_w * s as f32 + 2.0, ad.top() + 4.0 + 22.0 * lane as f32),
            pos2(ad.left() + gutter + col_w * e as f32 - 3.0, ad.top() + 4.0 + 22.0 * lane as f32 + 19.0),
        );
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
    }
    hairline(ui);

    // Rejilla de horas.
    let scroll = egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("week-scroll");
    let scroll = if app.calendar.scrolled { scroll } else { scroll.vertical_scroll_offset(hh * 7.5) };
    app.calendar.scrolled = true;
    scroll.show(ui, |ui| {
        let (grid, _) = ui.allocate_exact_size(vec2(ui.available_width(), hh * 24.0 + 8.0), Sense::hover());
        let p = ui.painter_at(grid);
        let top = grid.top() + 4.0;
        // Fondo de hoy y fines de semana.
        for d in 0..days {
            let day = mon + Duration::days(d);
            let x = grid.left() + gutter + col_w * d as f32;
            let r = Rect::from_min_max(pos2(x, top), pos2(x + col_w, top + hh * 24.0));
            if day == today {
                p.rect_filled(r, 0.0, mix(c.surface, c.accent, 0.04));
            } else if day.weekday().num_days_from_monday() >= 5 {
                p.rect_filled(r, 0.0, c.surface_alt);
            }
            p.vline(x, r.y_range(), Stroke::new(1.0, c.separator));
        }
        for h in 0..24 {
            let y = top + hh * h as f32;
            p.hline((grid.left() + gutter - 4.0)..=grid.right(), y, Stroke::new(1.0, c.separator));
            let half = Stroke::new(1.0, mix(c.surface, c.separator, 0.45));
            p.hline((grid.left() + gutter)..=grid.right(), y + hh / 2.0, half);
            if h > 0 {
                p.text(
                    pos2(grid.left() + gutter - 8.0, y),
                    Align2::RIGHT_CENTER,
                    format!("{h:02}:00"),
                    FontId::proportional(11.0),
                    c.text_muted,
                );
            }
        }
        // Eventos con hora, por día y en columnas si se solapan.
        for d in 0..days {
            let day = mon + Duration::days(d);
            let x = grid.left() + gutter + col_w * d as f32;
            let mut todays: Vec<&Occurrence> =
                occ.iter().filter(|o| !o.all_day && zone.local(o.start).date() == day).collect();
            todays.sort_by_key(|o| o.start);
            let spans: Vec<(i64, i64)> = todays
                .iter()
                .map(|o| {
                    let s = zone.local(o.start);
                    let e = zone.local(o.end);
                    let sm = i64::from(s.hour() * 60 + s.minute());
                    let em = if e.date() > day { 24 * 60 } else { i64::from(e.hour() * 60 + e.minute()) };
                    (sm, em.max(sm + 20))
                })
                .collect();
            let cols = layout_columns(&spans);
            for ((o, (sm, em)), (ci, n)) in todays.iter().zip(spans.iter()).zip(cols.iter()) {
                let w = (col_w - 6.0) / *n as f32;
                let r = Rect::from_min_max(
                    pos2(x + 3.0 + w * *ci as f32, top + hh * (*sm as f32 / 60.0) + 1.0),
                    pos2(x + 3.0 + w * (*ci as f32 + 1.0) - 2.0, top + hh * (*em as f32 / 60.0) - 1.0),
                );
                event_block(ui, &p, r, o, zone);
            }
        }
        // Línea de ahora.
        let ln = zone.local(now);
        let nd = ln.date();
        if nd >= mon && nd < mon + Duration::days(days) {
            let d = (nd - mon).num_days();
            let x = grid.left() + gutter + col_w * d as f32;
            let y = top + hh * ((ln.hour() * 60 + ln.minute()) as f32 / 60.0);
            p.hline(x..=(x + col_w), y, Stroke::new(2.0, c.danger));
            p.circle_filled(pos2(x, y), 4.5, c.danger);
        }
    });
}

fn event_block(ui: &Ui, p: &egui::Painter, r: Rect, o: &Occurrence, zone: iuredesk_engine::Zone) {
    let c = theme::colors(ui);
    let col = rgb(o.color);
    let dark = ui.visuals().dark_mode;
    let fill = mix(c.surface, col, if dark { 0.32 } else { 0.18 });
    p.rect_filled(r, 5.0, fill);
    if o.tentative {
        p.rect_stroke(r, 5.0, Stroke::new(1.0, col), egui::StrokeKind::Inside);
    }
    p.rect_filled(Rect::from_min_max(r.min, pos2(r.left() + 3.5, r.bottom())), 2.0, col);
    let inner = r.shrink2(vec2(7.0, 3.0));
    let inner = Rect::from_min_max(pos2(inner.left() + 2.0, inner.top()), inner.max);
    let clip = p.with_clip_rect(r.shrink(1.0));
    let title_font = theme::bold_font(ui.ctx(), 12.5);
    let ink = if dark { c.text } else { mix(col, Color32::BLACK, 0.55) };
    let g = clip.layout(o.summary.clone(), title_font, ink, inner.width());
    let title_h = g.size().y;
    clip.galley(inner.min, g, ink);
    let time = format!("{}–{}", zone.local(o.start).format("%H:%M"), zone.local(o.end).format("%H:%M"));
    let mut y = inner.top() + title_h + 1.0;
    if y + 14.0 <= inner.bottom() {
        clip.text(pos2(inner.left(), y), Align2::LEFT_TOP, time, FontId::proportional(11.0), c.text_muted);
        y += 14.0;
    }
    if !o.location.is_empty() && y + 14.0 <= inner.bottom() {
        clip.text(pos2(inner.left(), y), Align2::LEFT_TOP, &o.location, FontId::proportional(11.0), c.text_muted);
    }
}

fn month_view(app: &mut DeskApp, ui: &mut Ui, anchor: NaiveDate, today: NaiveDate) {
    let c = theme::colors(ui);
    let zone = app.session.zone();
    let first = NaiveDate::from_ymd_opt(anchor.year(), anchor.month(), 1).unwrap_or(anchor);
    let start = monday_of(first);
    let from = zone.day_start(start);
    let to = zone.day_start(start + Duration::days(42));
    let occ: Vec<Occurrence> = app.session.occurrences(from, to).to_vec();
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
