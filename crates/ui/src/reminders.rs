//! Recordatorios mientras la app está abierta: una notificación del escritorio la primera vez
//! (`notify-rust` en Linux; en otros sistemas, solo la tarjeta) y una tarjeta discreta abajo a la
//! derecha con «Posponer» y «Descartar» hasta que se atienden.

use egui::{Align, Context, Layout, RichText, Sense, vec2};
use iuredesk_engine::calendar::reminders::SNOOZE_CHOICES;
use iurefficient_ui::{Icon, icons, pick, theme};

use crate::app::DeskApp;

/// Cuadro de la app: avisa lo nuevo, dibuja la tarjeta y programa el próximo repintado.
pub(crate) fn show(app: &mut DeskApp, ctx: &Context) {
    let fresh = app.session.take_new_reminders();
    if app.session.prefs.notify_reminders
        && let Some(n) = app.notifier()
    {
        let zone = app.session.zone();
        for r in &fresh {
            let when = if r.all_day {
                pick("All day", "Todo el día").to_string()
            } else {
                format!("{}–{}", zone.local(r.start).format("%H:%M"), zone.local(r.end).format("%H:%M"))
            };
            let body = if r.location.is_empty() { when } else { format!("{when} · {}", r.location) };
            n(&r.summary, &body);
        }
    }
    if let Some(d) = app.session.next_reminder_in() {
        ctx.request_repaint_after(d.max(std::time::Duration::from_secs(1)));
    } else {
        ctx.request_repaint_after(std::time::Duration::from_secs(60));
    }
    let due = app.session.due_reminders();
    if due.is_empty() || app.session.needs_onboarding() {
        return;
    }
    let now = app.session.now();
    let zone = app.session.zone();
    let screen = ctx.content_rect();
    let w: f32 = 320.0;
    let mut snooze: Option<(iuredesk_engine::calendar::reminders::ReminderKey, i64)> = None;
    let mut dismiss: Vec<iuredesk_engine::calendar::reminders::ReminderKey> = Vec::new();
    let mut join: Option<String> = None;
    egui::Area::new(egui::Id::unique("reminders"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::RIGHT_BOTTOM, vec2(-16.0, -16.0))
        .show(ctx, |ui| {
            let c = theme::colors(ui);
            egui::Frame::popup(ui.style()).corner_radius(10.0).inner_margin(12.0).show(ui, |ui| {
                ui.set_width(w.min(screen.width() - 40.0));
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                    icons::paint(ui.painter(), r, Icon::Bell, c.accent, c.accent);
                    ui.label(
                        RichText::new(pick("Reminders", "Recordatorios"))
                            .font(theme::bold_font(ui.ctx(), 13.5))
                            .color(c.text),
                    );
                    if due.len() > 1 {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.link(pick("Dismiss all", "Descartar todos")).clicked() {
                                dismiss.extend(due.iter().map(|d| d.key));
                            }
                        });
                    }
                });
                for r in due.iter().take(4) {
                    ui.add_space(6.0);
                    ui.label(RichText::new(&r.summary).font(theme::medium_font(ui.ctx(), 13.5)).color(c.text));
                    let mins = (r.start - now).num_minutes();
                    let when = if r.all_day {
                        pick("Today, all day", "Hoy, todo el día").to_string()
                    } else if mins > 0 {
                        format!("{} {} min · {}", pick("In", "En"), mins, zone.local(r.start).format("%H:%M"))
                    } else {
                        format!(
                            "{} · {}–{}",
                            pick("Now", "Ahora"),
                            zone.local(r.start).format("%H:%M"),
                            zone.local(r.end).format("%H:%M")
                        )
                    };
                    let detail = if r.location.is_empty() { when } else { format!("{when} · {}", r.location) };
                    ui.label(RichText::new(detail).color(c.text_muted).size(12.0));
                    ui.horizontal(|ui| {
                        let combo = egui::ComboBox::from_id_salt(("snooze", r.key.event, r.key.start))
                            .selected_text(pick("Snooze", "Posponer"))
                            .width(110.0)
                            .show_ui(ui, |ui| {
                                for m in SNOOZE_CHOICES {
                                    let label = if m >= 60 { format!("{} h", m / 60) } else { format!("{m} min") };
                                    if ui.selectable_label(false, label).clicked() {
                                        snooze = Some((r.key, m));
                                    }
                                }
                            });
                        crate::widgets::named(&combo.response, egui::Role::ComboBox, pick("Snooze", "Posponer"));
                        if ui.button(pick("Dismiss", "Descartar")).clicked() {
                            dismiss.push(r.key);
                        }
                    });
                    let description =
                        app.session.calendar_event(r.key.event).map(|e| e.description).unwrap_or_default();
                    if let Some(j) = iuredesk_engine::today::meeting_link(&[&r.location, &description])
                        && crate::meeting::join_button(ui, &j, true)
                    {
                        join = Some(j.url.clone());
                    }
                }
                if due.len() > 4 {
                    ui.label(RichText::new(format!("+{} {}", due.len() - 4, pick("more", "más"))).color(c.text_muted));
                }
            });
        });
    if let Some((k, m)) = snooze {
        app.session.snooze_reminder(k, m);
    }
    for k in dismiss {
        app.session.dismiss_reminder(k);
    }
    if let Some(u) = join {
        crate::meeting::join(app, ctx, &u);
    }
}
