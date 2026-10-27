//! Tarjeta de invitación en el panel de lectura: cuándo, dónde, organizador, invitados, choques
//! con mi calendario y Aceptar / Tal vez / Rechazar (con comentario opcional). También
//! cancelaciones («Quitar de mi calendario»), contrapropuestas y respuestas de los invitados.
//!
//! El correo la llama con una línea: `crate::invitation::card(app, ui, id)`.

use chrono::{Datelike, Duration};
use egui::{Align, Layout, RichText, Sense, Ui, vec2};
use iuredesk_engine::Notice;
use iuredesk_engine::calendar::Occurrence;
use iuredesk_engine::calendar::invite::{Freshness, InviteKind, Invitation, partstat_label};
use iuredesk_engine::time::{month_name, weekday_short};
use iuredesk_model::{EventTime, MessageId, PartStat};
use iurefficient_ui::{BadgeKind, Icon, badge, icons, pick, primary_button, theme};

use crate::app::DeskApp;
use crate::widgets::mix;

/// Comentario que se está escribiendo (por mensaje).
fn comment_id(msg: MessageId) -> egui::Id {
    egui::Id::unique(("invite-comment", msg))
}

fn when(app: &DeskApp, s: &EventTime, e: &EventTime) -> String {
    let zone = app.session.zone();
    let day = |d: chrono::NaiveDate| {
        let m: String = month_name(d.month()).chars().take(3).collect();
        format!("{} {} {m}", weekday_short(d), d.day())
    };
    match (s, e) {
        (EventTime::Date(a), EventTime::Date(b)) => {
            let last = (*b - Duration::days(1)).max(*a);
            if last == *a {
                format!("{} · {}", day(*a), pick("all day", "todo el día"))
            } else {
                format!("{} – {} · {}", day(*a), day(last), pick("all day", "todo el día"))
            }
        }
        _ => {
            let (ls, le) = (zone.local(s.to_utc()), zone.local(e.to_utc()));
            let mins = (e.to_utc() - s.to_utc()).num_minutes();
            let len = if mins % 60 == 0 { format!("{} h", mins / 60) } else { format!("{mins} min") };
            if ls.date() == le.date() {
                format!("{} · {}–{} ({len})", day(ls.date()), ls.format("%H:%M"), le.format("%H:%M"))
            } else {
                format!("{} {} – {} {}", day(ls.date()), ls.format("%H:%M"), day(le.date()), le.format("%H:%M"))
            }
        }
    }
}

/// Dibuja la tarjeta si el mensaje trae una invitación.
pub(crate) fn card(app: &mut DeskApp, ui: &mut Ui, msg: MessageId) {
    let Some((inv, conflicts)) = app.session.invitation(msg) else { return };
    let c = theme::colors(ui);
    let accent = match inv.kind {
        InviteKind::Cancel => c.danger,
        _ => c.accent,
    };
    let frame = egui::Frame::new()
        .fill(mix(c.surface, accent, if ui.visuals().dark_mode { 0.10 } else { 0.05 }))
        .stroke(egui::Stroke::new(1.0, mix(c.surface, accent, 0.35)))
        .corner_radius(10.0)
        .inner_margin(egui::Margin::symmetric(16, 14));
    let mut action: Option<Action> = None;
    let resp = frame.show(ui, |ui| {
        ui.set_width(ui.available_width().min(640.0));
        ui.spacing_mut().item_spacing.y = 6.0;
        header(ui, &inv);
        ui.label(RichText::new(&inv.event.summary).font(theme::bold_font(ui.ctx(), 17.0)).color(c.text));
        let (s, e) = inv.proposed.clone().unwrap_or((inv.event.start.clone(), inv.event.end.clone()));
        line(ui, Icon::Clock, &when(app, &s, &e));
        if inv.kind == InviteKind::Counter {
            line(ui, Icon::Clock, &format!("{} {}", pick("Was:", "Antes:"), when(app, &inv.event.start, &inv.event.end)));
        }
        if inv.event.rrule.is_some() {
            let start = match &inv.event.start {
                EventTime::Date(d) => *d,
                t => app.session.zone().local(t.to_utc()).date(),
            };
            let rep = iuredesk_engine::calendar::repeat::Repeat::from_rrule(inv.event.rrule.as_deref(), start);
            line(ui, Icon::Sync, &rep.describe(start));
        }
        if !inv.event.location.is_empty() {
            line(ui, Icon::Pin, &inv.event.location);
        }
        if let Some(o) = &inv.organizer {
            line(ui, Icon::Person, &format!("{} {}", pick("Organizer:", "Organiza:"), o.display()));
        }
        attendees(ui, &inv);
        if matches!(inv.kind, InviteKind::Request | InviteKind::Counter) {
            conflicts_line(app, ui, &conflicts);
        }
        if let Some(cm) = &inv.comment {
            line(ui, Icon::Comment, &format!("«{cm}»"));
        }
        ui.add_space(4.0);
        action = actions(app, ui, msg, &inv);
    });
    iurefficient_ui::a11y::area(&resp.response, pick("Invitation", "Invitación"));
    ui.add_space(10.0);
    if let Some(a) = action {
        run(app, ui, msg, a, &inv);
    }
}

#[derive(Clone, Debug)]
enum Action {
    Respond(PartStat),
    Add,
    Remove,
    AcceptCounter,
    ApplyReply,
    Show,
}

fn header(ui: &mut Ui, inv: &Invitation) {
    let c = theme::colors(ui);
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
        icons::paint(ui.painter(), r, Icon::Calendar, c.text_muted, c.accent);
        let t = match inv.kind {
            InviteKind::Request if inv.freshness == Freshness::Update => pick("Updated invitation", "Invitación actualizada"),
            InviteKind::Request => pick("Invitation", "Invitación"),
            InviteKind::Cancel => pick("Event cancelled", "Evento cancelado"),
            InviteKind::Counter => pick("New time proposed", "Proponen otro horario"),
            InviteKind::Reply => pick("Reply to your event", "Respuesta a tu evento"),
            InviteKind::Publish => pick("Event", "Evento"),
        };
        ui.label(RichText::new(t).color(c.text_muted).size(12.5));
        if inv.single_occurrence {
            badge(ui, pick("one occurrence", "una repetición"), BadgeKind::Neutral);
        }
        if inv.freshness == Freshness::Outdated {
            badge(ui, pick("outdated", "desactualizada"), BadgeKind::Warning);
        }
    });
}

fn line(ui: &mut Ui, ic: Icon, text: &str) {
    let c = theme::colors(ui);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
        icons::paint(ui.painter(), r, ic, c.text_muted, c.accent);
        ui.add(egui::Label::new(RichText::new(text).color(c.text)).wrap());
    });
}

fn attendees(ui: &mut Ui, inv: &Invitation) {
    let c = theme::colors(ui);
    if inv.attendees.is_empty() {
        return;
    }
    let count = |p: PartStat| inv.attendees.iter().filter(|a| a.partstat == p).count();
    let mut parts = Vec::new();
    for (p, en, es) in [
        (PartStat::Accepted, "accepted", "aceptaron"),
        (PartStat::Tentative, "maybe", "tal vez"),
        (PartStat::Declined, "declined", "rechazaron"),
        (PartStat::NeedsAction, "no response", "sin respuesta"),
    ] {
        let n = count(p);
        if n > 0 {
            parts.push(format!("{n} {}", pick(en, es)));
        }
    }
    let head = format!("{} {} · {}", inv.attendees.len(), pick("guests", "invitados"), parts.join(", "));
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
        icons::paint(ui.painter(), r, Icon::Contacts, c.text_muted, c.accent);
        egui::CollapsingHeader::new(RichText::new(head).color(c.text)).id_salt(("inv-att", &inv.uid)).show(ui, |ui| {
            for a in inv.attendees.iter().take(50) {
                ui.horizontal(|ui| {
                    let name = a.mailbox.name.clone().filter(|n| !n.is_empty()).unwrap_or_else(|| a.mailbox.addr.clone());
                    iurefficient_ui::avatar(ui, &name, &a.mailbox.addr, 20.0);
                    ui.label(RichText::new(name).color(c.text));
                    let optional = a.role == iuredesk_model::AttendeeRole::Optional;
                    let tag = if optional {
                        format!("{} · {}", partstat_label(&a.partstat), pick("optional", "opcional"))
                    } else {
                        partstat_label(&a.partstat).to_string()
                    };
                    ui.label(RichText::new(tag).color(c.text_muted).size(12.0));
                });
            }
        });
    });
}

fn conflicts_line(app: &DeskApp, ui: &mut Ui, conflicts: &[Occurrence]) {
    let c = theme::colors(ui);
    let zone = app.session.zone();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
        if conflicts.is_empty() {
            icons::paint(ui.painter(), r, Icon::Check, c.success, c.success);
            ui.label(RichText::new(pick("No conflicts in your calendar", "Sin choques en tu calendario")).color(c.success));
        } else {
            icons::paint(ui.painter(), r, Icon::Warning, c.warning, c.warning);
            let list: Vec<String> = conflicts
                .iter()
                .map(|o| format!("{} ({}–{})", o.summary, zone.local(o.start).format("%H:%M"), zone.local(o.end).format("%H:%M")))
                .collect();
            ui.add(
                egui::Label::new(
                    RichText::new(format!("{} {}", pick("Conflicts with:", "Choca con:"), list.join(", "))).color(c.warning),
                )
                .wrap(),
            );
        }
    });
}

fn actions(app: &mut DeskApp, ui: &mut Ui, msg: MessageId, inv: &Invitation) -> Option<Action> {
    let c = theme::colors(ui);
    let mut out = None;
    match inv.kind {
        InviteKind::Request if inv.me.is_some() || inv.attendees.is_empty() => {
            if let Some(st) = &inv.my_status
                && *st != PartStat::NeedsAction
            {
                ui.label(
                    RichText::new(format!("{} {}", pick("Your answer:", "Tu respuesta:"), partstat_label(st)))
                        .color(c.text_muted),
                );
            }
            let id = comment_id(msg);
            let mut comment: Option<String> = ui.ctx().data(|d| d.get_temp::<Option<String>>(id)).flatten();
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let chosen = inv.my_status.clone().unwrap_or_default();
                let mark = |p: PartStat, label: &str| if chosen == p { format!("✓ {label}") } else { label.to_string() };
                if primary_button(ui, &mark(PartStat::Accepted, pick("Accept", "Aceptar"))).clicked() {
                    out = Some(Action::Respond(PartStat::Accepted));
                }
                if ui.button(mark(PartStat::Tentative, pick("Maybe", "Tal vez"))).clicked() {
                    out = Some(Action::Respond(PartStat::Tentative));
                }
                if ui.button(mark(PartStat::Declined, pick("Decline", "Rechazar"))).clicked() {
                    out = Some(Action::Respond(PartStat::Declined));
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if comment.is_none() && ui.link(pick("Add a comment", "Agregar comentario")).clicked() {
                        comment = Some(String::new());
                    }
                    if inv.existing.is_some() && ui.link(pick("Show in calendar", "Ver en el calendario")).clicked() {
                        out = Some(Action::Show);
                    }
                });
            });
            if let Some(text) = comment.as_mut() {
                let r = ui.add(
                    egui::TextEdit::singleline(text)
                        .hint_text(pick("Comment for the organizer (optional)", "Comentario para quien organiza (opcional)"))
                        .desired_width(f32::INFINITY),
                );
                iurefficient_ui::a11y::label(&r, pick("Comment", "Comentario"));
            }
            ui.ctx().data_mut(|d| d.insert_temp(id, comment));
        }
        InviteKind::Request | InviteKind::Publish => {
            if inv.existing.is_none() {
                if primary_button(ui, pick("Add to calendar", "Agregar al calendario")).clicked() {
                    out = Some(Action::Add);
                }
            } else if ui.link(pick("Show in calendar", "Ver en el calendario")).clicked() {
                out = Some(Action::Show);
            }
        }
        InviteKind::Cancel => {
            if inv.existing.is_some() {
                if primary_button(ui, pick("Remove from my calendar", "Quitar de mi calendario")).clicked() {
                    out = Some(Action::Remove);
                }
            } else {
                ui.label(RichText::new(pick("It is not in your calendar.", "No está en tu calendario.")).color(c.text_muted));
            }
        }
        InviteKind::Counter => {
            if inv.i_organize && inv.existing.is_some() {
                if primary_button(ui, pick("Accept new time", "Aceptar el nuevo horario")).clicked() {
                    out = Some(Action::AcceptCounter);
                }
            } else {
                ui.label(RichText::new(pick("Only the organizer can change the time.", "Solo quien organiza puede cambiar el horario.")).color(c.text_muted));
            }
        }
        InviteKind::Reply => {
            if let Some((who, ps)) = &inv.reply {
                ui.label(RichText::new(format!("{} — {}", who.display(), partstat_label(ps))).color(c.text));
            }
            if inv.existing.is_some() && ui.button(pick("Update my event", "Actualizar mi evento")).clicked() {
                out = Some(Action::ApplyReply);
            }
        }
    }
    let _ = app;
    out
}

fn run(app: &mut DeskApp, ui: &mut Ui, msg: MessageId, a: Action, inv: &Invitation) {
    let r = match a {
        Action::Respond(p) => {
            let id = comment_id(msg);
            let comment: Option<String> = ui.ctx().data(|d| d.get_temp::<Option<String>>(id)).flatten();
            let r = app.session.respond_invitation(msg, p, comment.as_deref());
            if r.is_ok() {
                ui.ctx().data_mut(|d| d.remove::<Option<String>>(id));
            }
            r
        }
        Action::Add => app.session.add_invitation_to_calendar(msg),
        Action::Remove => app.session.apply_invitation_cancel(msg),
        Action::AcceptCounter => app.session.accept_counter(msg),
        Action::ApplyReply => app.session.apply_invitation_reply(msg),
        Action::Show => {
            let d = match &inv.event.start {
                EventTime::Date(d) => *d,
                t => app.session.zone().local(t.to_utc()).date(),
            };
            app.session.cal.anchor = Some(d);
            app.session.module = iuredesk_engine::Module::Calendar;
            return;
        }
    };
    match r {
        Ok(iuredesk_engine::Outcome::Notice(n)) => app.notice(n),
        Ok(_) => {}
        Err(e) => app.notice(Notice::error(e.text())),
    }
}
