//! Iurefficient en la interfaz (F3.1): tarjetas del panel contextual (persona/cliente,
//! lead/oportunidad, caso, ticket, actividades, «Vinculado a…»), acciones con confirmación
//! («la IA propone, el usuario confirma»), diálogos (vincular, crear lead/ticket/tarea), campana
//! de avisos del riel y la sección de Ajustes con el inicio de sesión (y TOTP).

use chrono::{Datelike, Duration, NaiveDate, Weekday};
use egui::{Context, RichText, Stroke, Ui, vec2};
use iuredesk_engine::iure::{
    DealCard, IureStatus, LeadForm, LinkOptions, LinkTarget, PersonKind, TargetKind, TaskForm, TicketForm,
};
use iuredesk_engine::{IurePanel, LeadSuggestions, Notice};
use iuredesk_model::MessageId;
use iurefficient_ui::{BadgeKind, Banner, Icon, Severity, avatar, badge, pick, section_label, theme};

use crate::app::DeskApp;

/// Diálogo abierto.
pub enum Dialog {
    Link { msg: MessageId, query: String, picked: Option<LinkTarget>, opts: LinkOptions },
    Lead { msg: MessageId, form: Option<LeadForm>, picked: usize },
    Ticket { msg: MessageId, form: TicketForm },
    Task { msg: MessageId, form: TaskForm, due_text: String },
}

/// Estado de la interfaz de Iurefficient.
#[derive(Default)]
pub struct IureUi {
    pub dialog: Option<Dialog>,
    pub bell_open: bool,
    login_open: bool,
    domain: String,
    email: String,
    password: String,
    totp: String,
}

/// Lo que pide el panel.
enum Act {
    Link,
    Lead,
    Ticket,
    Task,
    ReplyVia(String),
    Unlink(String, String),
    Retry,
    OpenWeb,
}

fn card(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    let c = theme::colors(ui);
    egui::Frame::new()
        .fill(c.surface)
        .stroke(Stroke::new(1.0, c.separator))
        .corner_radius(8)
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
    ui.add_space(6.0);
}

fn small(ui: &Ui, s: impl Into<String>) -> RichText {
    let c = theme::colors(ui);
    RichText::new(s.into()).size(theme::density(ui.ctx()).small_size()).color(c.text_muted)
}

/// Importe con separador de miles (sin moneda: la instancia no la da).
pub fn money(a: f64) -> String {
    let n = a.abs().round().min(1e15) as u64;
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    format!("{}${out}", if a < 0.0 { "-" } else { "" })
}

fn status_label(s: &str) -> (&'static str, BadgeKind) {
    match s {
        "open" => (pick("Open", "Abierto"), BadgeKind::Accent),
        "in_progress" => (pick("In progress", "En curso"), BadgeKind::Accent),
        "waiting" => (pick("Waiting", "En espera"), BadgeKind::Warning),
        "resolved" => (pick("Resolved", "Resuelto"), BadgeKind::Success),
        "closed" => (pick("Closed", "Cerrado"), BadgeKind::Neutral),
        _ => ("—", BadgeKind::Neutral),
    }
}

fn activity_label(k: &str) -> &'static str {
    match k {
        "call" => pick("Call", "Llamada"),
        "meeting" => pick("Meeting", "Reunión"),
        "email" => pick("Email", "Correo"),
        "whatsapp" => "WhatsApp",
        "sms" => "SMS",
        "note" => pick("Note", "Nota"),
        "task" => pick("Task", "Tarea"),
        _ => pick("Activity", "Actividad"),
    }
}

fn kind_label(k: &str) -> String {
    TargetKind::from_key(k).map(|t| t.label().get().to_string()).unwrap_or_else(|| k.to_string())
}

fn deal_line(d: &DealCard) -> String {
    let mut parts = Vec::new();
    if let Some(s) = &d.stage {
        parts.push(s.clone());
    }
    if let Some(a) = d.amount {
        parts.push(money(a));
    }
    if parts.is_empty()
        && let Some(o) = &d.organization
    {
        parts.push(o.clone());
    }
    parts.join(" · ")
}

/// Tarjetas de Iurefficient del mensaje (dentro del panel contextual o en línea).
pub fn panel(app: &mut DeskApp, ui: &mut Ui, id: MessageId, p: &IurePanel) {
    let c = theme::colors(ui);
    let mut act: Option<Act> = None;
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(12.0, 12.0), egui::Sense::hover());
        ui.painter().circle_filled(r.center(), 5.0, c.brand);
        ui.label(RichText::new("Iurefficient").font(theme::bold_font(ui.ctx(), 13.0)).color(c.text));
        if p.loading {
            ui.add(egui::Spinner::new().size(12.0));
        }
    });
    ui.add_space(4.0);
    match &p.status {
        IureStatus::TermsRequired { .. } => {
            let t = p.status.label();
            if Banner::new(Severity::Warning, t.get()).action(pick("Open Iurefficient", "Abrir Iurefficient")).show(ui).action {
                act = Some(Act::OpenWeb);
            }
        }
        IureStatus::Expired { .. } | IureStatus::Offline { .. } => {
            let t = p.status.label();
            if Banner::new(Severity::Info, t.get()).action(pick("Retry", "Reintentar")).show(ui).action {
                act = Some(Act::Retry);
            }
        }
        _ => {
            if let Some(e) = &p.error {
                ui.label(small(ui, e.get()));
            }
        }
    }
    // Vinculado a…
    if !p.links.is_empty() {
        card(ui, |ui| {
            ui.label(RichText::new(pick("Linked to", "Vinculado a")).strong().color(c.text));
            for l in p.links.iter().rev().take(6) {
                ui.horizontal(|ui| {
                    badge(ui, &kind_label(&l.kind), BadgeKind::Success);
                    ui.add(egui::Label::new(RichText::new(&l.label).color(c.text)).truncate());
                });
                let how = match l.how.as_str() {
                    "document" => pick("copy saved as document", "copia guardada como documento"),
                    "activity" => pick("logged as activity", "registrado como actividad"),
                    "comment" => pick("added as comment", "agregado como comentario"),
                    _ => pick("created from this email", "creado desde este correo"),
                };
                ui.horizontal(|ui| {
                    ui.label(small(ui, format!("{how} · {}", l.created.format("%d/%m %H:%M"))));
                    let r = ui.small_button("×").on_hover_text(pick("Forget this link here", "Olvidar este vínculo aquí"));
                    iurefficient_ui::a11y::button(&r, pick("Forget link", "Olvidar vínculo"));
                    if r.clicked() {
                        act = Some(Act::Unlink(l.kind.clone(), l.id.clone()));
                    }
                });
            }
        });
    }
    if let Some(ctx) = &p.context {
        for person in ctx.people.iter().take(2) {
            card(ui, |ui| {
                ui.horizontal(|ui| {
                    avatar(ui, &person.name, &person.email, 32.0);
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            let k = match person.kind {
                                PersonKind::Lead => pick("Person", "Persona"),
                                PersonKind::Client => pick("Client", "Cliente"),
                            };
                            badge(ui, k, BadgeKind::Neutral);
                            ui.add(egui::Label::new(RichText::new(&person.name).strong().color(c.text)).truncate());
                        });
                        if !person.subtitle.is_empty() {
                            ui.add(egui::Label::new(small(ui, &person.subtitle)).truncate());
                        }
                    });
                });
                if let Some(ph) = &person.phone {
                    crate::widgets::info_row(ui, Icon::Phone, "", ph);
                }
            });
        }
        for d in ctx.deals.iter().take(3) {
            card(ui, |ui| {
                ui.horizontal(|ui| {
                    let (k, kind) = match d.kind {
                        TargetKind::Opportunity => (pick("Opportunity", "Oportunidad"), BadgeKind::Success),
                        _ => ("Lead", BadgeKind::Accent),
                    };
                    badge(ui, k, kind);
                    ui.add(egui::Label::new(RichText::new(&d.name).strong().color(c.text)).truncate());
                });
                let line = deal_line(d);
                if !line.is_empty() {
                    ui.label(small(ui, line));
                }
                if d.kind == TargetKind::Opportunity {
                    let r = ui
                        .small_button(pick("Reply via the opportunity's address", "Responder vía la dirección de la oportunidad"))
                        .on_hover_text(pick(
                            "Reply with the CRM sales mailbox in Reply-To and Bcc so Iurefficient files the thread in the opportunity.",
                            "Responde con el buzón de ventas del CRM en Reply-To y CCO para que Iurefficient archive el hilo en la oportunidad.",
                        ));
                    if r.clicked() {
                        act = Some(Act::ReplyVia(d.id.clone()));
                    }
                }
            });
        }
        for case in ctx.cases.iter().take(3) {
            card(ui, |ui| {
                ui.horizontal(|ui| {
                    badge(ui, pick("Case", "Caso"), BadgeKind::Accent);
                    ui.add(egui::Label::new(RichText::new(&case.title).strong().color(c.text)).truncate());
                });
                if let Some(s) = &case.subtitle {
                    ui.label(small(ui, s));
                }
            });
        }
        for t in ctx.tickets.iter().take(3) {
            card(ui, |ui| {
                ui.horizontal(|ui| {
                    let (s, k) = status_label(&t.status);
                    badge(ui, "Ticket", BadgeKind::Neutral);
                    ui.label(RichText::new(&t.number).strong().color(c.text));
                    badge(ui, s, k);
                });
                ui.add(egui::Label::new(small(ui, &t.title)).truncate());
            });
        }
        if !ctx.activities.is_empty() {
            card(ui, |ui| {
                ui.label(RichText::new(pick("Recent CRM activity", "Actividad reciente del CRM")).strong().color(c.text));
                for a in ctx.activities.iter().take(4) {
                    let when = a.when.as_deref().and_then(|w| w.get(..10)).unwrap_or("");
                    ui.add(
                        egui::Label::new(small(ui, format!("{} · {} · {when}", activity_label(&a.kind), a.subject)))
                            .truncate(),
                    );
                }
            });
        }
        if ctx.is_empty() && p.links.is_empty() && !p.loading {
            ui.label(small(
                ui,
                pick(
                    "Iurefficient doesn't know this sender yet.",
                    "Iurefficient aún no conoce a este remitente.",
                ),
            ));
            ui.add_space(4.0);
        }
    }
    // Acciones: todas abren un diálogo para confirmar.
    let ready = p.status.is_ready() && p.linkable;
    ui.add_space(2.0);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
        let mut b = |ui: &mut Ui, label: &str, a: Act| {
            if ui.add_enabled(ready, egui::Button::new(label)).clicked() {
                act = Some(a);
            }
        };
        b(ui, pick("Link to…", "Vincular a…"), Act::Link);
        b(ui, pick("Create lead", "Crear lead"), Act::Lead);
        b(ui, pick("Create ticket", "Crear ticket"), Act::Ticket);
        b(ui, pick("Create task", "Crear tarea"), Act::Task);
    });
    if app.session.iure_busy() {
        ui.horizontal(|ui| {
            ui.add(egui::Spinner::new().size(12.0));
            ui.label(small(ui, pick("Sending to Iurefficient…", "Enviando a Iurefficient…")));
        });
    }
    let Some(a) = act else { return };
    match a {
        Act::Link => {
            app.iure.dialog = Some(Dialog::Link { msg: id, query: String::new(), picked: None, opts: LinkOptions::default() })
        }
        Act::Lead => {
            app.session.iure_suggest_leads(id);
            app.iure.dialog = Some(Dialog::Lead { msg: id, form: None, picked: 0 });
        }
        Act::Ticket => {
            let form = app.session.iure_ticket_form(id);
            app.iure.dialog = Some(Dialog::Ticket { msg: id, form });
        }
        Act::Task => {
            let form = app.session.iure_task_form(id);
            let due_text = form.due.map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default();
            app.iure.dialog = Some(Dialog::Task { msg: id, form, due_text });
        }
        Act::ReplyVia(opp) => app.session.iure_reply_via(id, &opp),
        Act::Unlink(k, t) => app.session.iure_unlink(id, &k, &t),
        Act::Retry => app.session.iure_retry(),
        Act::OpenWeb => {
            if let Some(u) = app.session.iure_web_url() {
                crate::open_url(&u);
            }
        }
    }
}

fn field(ui: &mut Ui, label: &str, value: &mut String) {
    let c = theme::colors(ui);
    let l = ui.label(RichText::new(label).color(c.text_muted));
    ui.add(egui::TextEdit::singleline(value).desired_width(f32::INFINITY)).labelled_by(l.id);
}

fn next_monday(d: NaiveDate) -> NaiveDate {
    let mut x = d + Duration::days(1);
    for _ in 0..7 {
        if x.weekday() == Weekday::Mon {
            break;
        }
        x += Duration::days(1);
    }
    x
}

/// Diálogos de Iurefficient (uno a la vez).
pub fn dialogs(app: &mut DeskApp, ctx: &Context) {
    let Some(mut d) = app.iure.dialog.take() else { return };
    let mut open = true;
    let mut close = false;
    let mut err: Option<iuredesk_engine::Text> = None;
    let today = app.session.today();
    let title = match &d {
        Dialog::Link { .. } => pick("Link to…", "Vincular a…"),
        Dialog::Lead { .. } => pick("Create lead", "Crear lead"),
        Dialog::Ticket { .. } => pick("Create ticket", "Crear ticket"),
        Dialog::Task { .. } => pick("Create task", "Crear tarea"),
    };
    egui::Window::new(title)
        .id(egui::Id::new("iure-dialog"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(440.0)
        .anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0))
        .show(ctx, |ui| {
            let c = theme::colors(ui);
            match &mut d {
                Dialog::Link { msg, query, picked, opts } => {
                    let l = ui.label(RichText::new(pick("Search a case, client, lead, opportunity or ticket", "Busca un caso, cliente, lead, oportunidad o ticket")).color(c.text_muted));
                    let r = ui.add(egui::TextEdit::singleline(query).desired_width(f32::INFINITY).hint_text("241/2026, Acme, TK-…")).labelled_by(l.id);
                    if r.changed() {
                        app.session.iure_search_targets(query);
                    }
                    let (_, found, busy) = app.session.iure_targets();
                    let found = found.to_vec();
                    if busy {
                        ui.add(egui::Spinner::new());
                    }
                    egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                        for t in &found {
                            let sel = picked.as_ref().is_some_and(|p| p.id == t.id && p.kind == t.kind);
                            let text = format!("{} · {}", t.kind.label().get(), t.label);
                            let r = ui.selectable_label(sel, text);
                            if let Some(dt) = &t.detail {
                                ui.label(small(ui, dt));
                            }
                            if r.clicked() {
                                *picked = Some(t.clone());
                            }
                        }
                        if found.is_empty() && query.trim().chars().count() >= 2 && !busy {
                            ui.label(small(ui, pick("Nothing found.", "No se encontró nada.")));
                        }
                    });
                    if let Some(t) = picked.clone() {
                        ui.separator();
                        ui.label(RichText::new(t.kind.link_effect().get()).color(c.text));
                        match t.kind {
                            TargetKind::Ticket => {
                                ui.checkbox(&mut opts.ticket_public, pick("Public comment", "Comentario público"));
                                if opts.ticket_public {
                                    Banner::new(
                                        Severity::Warning,
                                        pick(
                                            "A public comment is emailed by Iurefficient to the ticket's requester.",
                                            "Un comentario público hace que Iurefficient lo envíe por correo al solicitante del ticket.",
                                        ),
                                    )
                                    .show(ui);
                                } else {
                                    ui.label(small(ui, pick("Internal: only your team sees it.", "Interno: solo lo ve tu equipo.")));
                                }
                                ui.checkbox(&mut opts.attach_copy, pick("Also attach a text copy", "Adjuntar también una copia en texto"));
                            }
                            TargetKind::Lead | TargetKind::Opportunity => {
                                ui.checkbox(&mut opts.attach_copy, pick("Also attach a text copy", "Adjuntar también una copia en texto"));
                            }
                            _ => {}
                        }
                        ui.label(small(ui, pick(
                            "Sent to your Iurefficient instance: sender, recipients, date, subject and the text of the message.",
                            "Se envía a tu instancia de Iurefficient: remitente, destinatarios, fecha, asunto y el texto del mensaje.",
                        )));
                        ui.horizontal(|ui| {
                            if ui.button(pick("Link", "Vincular")).clicked() {
                                match app.session.iure_link(*msg, t.clone(), *opts) {
                                    Ok(()) => close = true,
                                    Err(e) => err = Some(e),
                                }
                            }
                            if ui.button(pick("Cancel", "Cancelar")).clicked() {
                                close = true;
                            }
                        });
                    }
                }
                Dialog::Lead { msg, form, picked } => {
                    let sugg = app.session.iure_lead_suggestions(*msg).cloned();
                    match (&sugg, form.is_none()) {
                        (Some(LeadSuggestions::Loading) | None, true) => {
                            ui.horizontal(|ui| {
                                ui.add(egui::Spinner::new());
                                ui.label(pick("Iurefficient is reading the signature…", "Iurefficient está leyendo la firma…"));
                            });
                        }
                        (Some(LeadSuggestions::Failed(t)), true) => {
                            ui.label(RichText::new(t.get()).color(c.danger));
                            *form = Some(app.session.iure_sender_lead(*msg));
                        }
                        (Some(LeadSuggestions::Ready(v)), true) => {
                            *form = v.first().cloned().or_else(|| Some(app.session.iure_sender_lead(*msg)));
                        }
                        _ => {}
                    }
                    if let (Some(LeadSuggestions::Ready(v)), Some(f)) = (&sugg, form.as_mut())
                        && v.len() > 1
                    {
                        ui.label(small(ui, pick("Proposals (AI proposes, you confirm):", "Propuestas (la IA propone, tú confirmas):")));
                        for (i, s) in v.iter().enumerate() {
                            if ui.radio(*picked == i, format!("{} — {}", s.name, s.email)).clicked() {
                                *picked = i;
                                *f = s.clone();
                            }
                        }
                    }
                    if let Some(f) = form.as_mut() {
                        field(ui, pick("Name", "Nombre"), &mut f.name);
                        field(ui, pick("Company", "Empresa"), &mut f.company);
                        field(ui, pick("Email", "Correo"), &mut f.email);
                        field(ui, pick("Phone", "Teléfono"), &mut f.phone);
                        field(ui, pick("Position", "Puesto"), &mut f.position);
                        field(ui, pick("Notes", "Notas"), &mut f.notes);
                        ui.horizontal(|ui| {
                            if ui.button(pick("Create lead", "Crear lead")).clicked() {
                                match app.session.iure_create_lead(*msg, f.clone()) {
                                    Ok(()) => close = true,
                                    Err(e) => err = Some(e),
                                }
                            }
                            if ui.button(pick("Cancel", "Cancelar")).clicked() {
                                close = true;
                            }
                        });
                    }
                }
                Dialog::Ticket { msg, form } => {
                    field(ui, pick("Title", "Título"), &mut form.title);
                    let l = ui.label(RichText::new(pick("Description", "Descripción")).color(c.text_muted));
                    ui.add(egui::TextEdit::multiline(&mut form.description).desired_rows(5).desired_width(f32::INFINITY)).labelled_by(l.id);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(pick("Priority", "Prioridad")).color(c.text_muted));
                        for (k, en, es) in [("low", "Low", "Baja"), ("medium", "Medium", "Media"), ("high", "High", "Alta"), ("urgent", "Urgent", "Urgente")] {
                            if ui.radio(form.priority == k, pick(en, es)).clicked() {
                                form.priority = k.into();
                            }
                        }
                    });
                    ui.label(small(ui, format!("{} {} <{}>", pick("Requester:", "Solicitante:"), form.requester_name, form.requester_email)));
                    ui.horizontal(|ui| {
                        if ui.button(pick("Create ticket", "Crear ticket")).clicked() {
                            match app.session.iure_create_ticket(*msg, form.clone()) {
                                Ok(()) => close = true,
                                Err(e) => err = Some(e),
                            }
                        }
                        if ui.button(pick("Cancel", "Cancelar")).clicked() {
                            close = true;
                        }
                    });
                }
                Dialog::Task { msg, form, due_text } => {
                    field(ui, pick("Title", "Título"), &mut form.title);
                    let l = ui.label(RichText::new(pick("Notes", "Notas")).color(c.text_muted));
                    ui.add(egui::TextEdit::multiline(&mut form.notes).desired_rows(3).desired_width(f32::INFINITY)).labelled_by(l.id);
                    ui.label(RichText::new(pick("Due", "Vence")).color(c.text_muted));
                    ui.horizontal_wrapped(|ui| {
                        let opts = [
                            (pick("Today", "Hoy"), Some(today)),
                            (pick("Tomorrow", "Mañana"), Some(today + Duration::days(1))),
                            (pick("In 2 days", "En 2 días"), Some(today + Duration::days(2))),
                            (pick("Next Monday", "El lunes"), Some(next_monday(today))),
                            (pick("In a week", "En una semana"), Some(today + Duration::days(7))),
                            (pick("No date", "Sin fecha"), None),
                        ];
                        for (label, day) in opts {
                            if ui.selectable_label(form.due == day, label).clicked() {
                                form.due = day;
                                *due_text = day.map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default();
                            }
                        }
                    });
                    let l = ui.label(small(ui, pick("Or a date (YYYY-MM-DD)", "O una fecha (AAAA-MM-DD)")));
                    let r = ui.add(egui::TextEdit::singleline(due_text).desired_width(120.0)).labelled_by(l.id);
                    if r.changed() {
                        form.due = NaiveDate::parse_from_str(due_text.trim(), "%Y-%m-%d").ok();
                    }
                    if !due_text.trim().is_empty() && form.due.is_none() {
                        ui.label(RichText::new(pick("That date is not valid.", "Esa fecha no es válida.")).color(c.danger));
                    }
                    if form.case_id.is_some() {
                        let mut keep = true;
                        ui.checkbox(&mut keep, pick("In the case of this email", "En el caso de este correo"));
                        if !keep {
                            form.case_id = None;
                        }
                    }
                    ui.horizontal(|ui| {
                        if ui.button(pick("Create task", "Crear tarea")).clicked() {
                            match app.session.iure_create_task(*msg, form.clone()) {
                                Ok(()) => close = true,
                                Err(e) => err = Some(e),
                            }
                        }
                        if ui.button(pick("Cancel", "Cancelar")).clicked() {
                            close = true;
                        }
                    });
                }
            }
        });
    if let Some(e) = err {
        app.notice(Notice::error(e));
    }
    if open && !close {
        app.iure.dialog = Some(d);
    }
}

/// Campana de avisos del riel.
pub fn bell(app: &mut DeskApp, ctx: &Context) {
    if !app.iure.bell_open {
        return;
    }
    let mut open = true;
    let mut mark: Option<(bool, String, Option<String>)> = None;
    let mut all = false;
    let feed = app.session.iure_feed().clone();
    egui::Window::new(pick("Iurefficient notifications", "Avisos de Iurefficient"))
        .id(egui::Id::new("iure-bell"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(340.0)
        .anchor(egui::Align2::LEFT_BOTTOM, vec2(64.0, -12.0))
        .show(ctx, |ui| {
            let c = theme::colors(ui);
            if feed.items.is_empty() {
                ui.label(RichText::new(pick("Nothing new.", "Nada nuevo.")).color(c.text_muted));
            }
            egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                for i in &feed.items {
                    let r = ui
                        .horizontal(|ui| {
                            let (dot, _) = ui.allocate_exact_size(vec2(8.0, 8.0), egui::Sense::hover());
                            if !i.read {
                                ui.painter().circle_filled(dot.center(), 3.5, c.accent);
                            }
                            ui.vertical(|ui| {
                                let t = RichText::new(&i.title).color(c.text);
                                ui.label(if i.read { t } else { t.strong() });
                                if let Some(m) = &i.message {
                                    ui.add(egui::Label::new(small(ui, m)).wrap());
                                }
                            });
                        })
                        .response
                        .interact(egui::Sense::click());
                    iurefficient_ui::a11y::button(&r, &i.title);
                    if r.clicked() {
                        mark = Some((i.mention, i.id.clone(), i.link.clone()));
                    }
                    ui.add_space(4.0);
                }
            });
            if feed.unread > 0 && ui.button(pick("Mark all as read", "Marcar todo como leído")).clicked() {
                all = true;
            }
        });
    if let Some((mention, id, link)) = mark {
        app.session.iure_mark_read(mention, &id);
        if let (Some(l), Some(base)) = (link, app.session.iure_web_url()) {
            let url = if l.starts_with("https://") { l } else { format!("{}{}", base.trim_end_matches('/'), l) };
            crate::open_url(&url);
        }
    }
    if all {
        app.session.iure_mark_all_read();
    }
    if !open {
        app.iure.bell_open = false;
    }
}

/// Avisos sin leer en el panel «Hoy» (hasta dos).
pub fn today_items(app: &mut DeskApp, ui: &mut Ui) {
    let items: Vec<_> = app.session.iure_feed().items.iter().filter(|i| !i.read).take(2).cloned().collect();
    if items.is_empty() {
        return;
    }
    let c = theme::colors(ui);
    ui.add_space(4.0);
    for i in items {
        let r = ui
            .horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let (r, _) = ui.allocate_exact_size(vec2(14.0, 14.0), egui::Sense::hover());
                iurefficient_ui::icons::paint(ui.painter(), r, Icon::Bell, c.accent, c.accent);
                let w = ui.available_width();
                let g = iurefficient_ui::widgets::text::elided(ui, &i.title, theme::body_font(ui.ctx()), c.text, w);
                ui.label(egui::WidgetText::Galley(g));
            })
            .response
            .interact(egui::Sense::click());
        iurefficient_ui::a11y::button(&r, &i.title);
        if r.clicked() {
            app.iure.bell_open = true;
        }
    }
}

/// Sección «Iurefficient» de Ajustes.
pub fn settings_section(app: &mut DeskApp, ui: &mut Ui) {
    let c = theme::colors(ui);
    section_label(ui, "Iurefficient");
    let st = app.session.iure_status().clone();
    let (sev, show_login) = match &st {
        IureStatus::Ready { .. } => (Severity::Success, false),
        IureStatus::TermsRequired { .. } => (Severity::Warning, false),
        IureStatus::Offline { .. } | IureStatus::Connecting => (Severity::Info, false),
        IureStatus::Expired { .. } | IureStatus::NotConfigured | IureStatus::Disabled => (Severity::Info, true),
    };
    let label = st.label();
    let mut b = Banner::new(sev, label.get());
    if matches!(st, IureStatus::TermsRequired { .. }) {
        b = b.action(pick("Open Iurefficient", "Abrir Iurefficient"));
    } else if matches!(st, IureStatus::Offline { .. }) {
        b = b.action(pick("Retry", "Reintentar"));
    }
    if b.show(ui).action {
        if matches!(st, IureStatus::TermsRequired { .. }) {
            if let Some(u) = app.session.iure_web_url() {
                crate::open_url(&u);
            }
        } else {
            app.session.iure_retry();
        }
    }
    if matches!(st, IureStatus::TermsRequired { .. }) && ui.button(pick("I accepted them, retry", "Ya los acepté, reintentar")).clicked() {
        app.session.iure_retry();
    }
    ui.add_space(4.0);
    if show_login && !app.iure.login_open && ui.button(pick("Sign in…", "Iniciar sesión…")).clicked() {
        app.iure.login_open = true;
        let (d, e) = app.session.iure_login_hint();
        app.iure.domain = d;
        app.iure.email = e;
        app.session.iure_login_reset();
    }
    if app.iure.login_open && !matches!(st, IureStatus::Ready { .. }) {
        let ls = app.session.iure_login_state().clone();
        egui::Frame::new()
            .fill(c.surface_alt)
            .stroke(Stroke::new(1.0, c.separator))
            .corner_radius(8)
            .inner_margin(egui::Margin::same(10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                if ls.needs_totp {
                    let l = ui.label(pick("Two-step verification code", "Código de verificación en dos pasos"));
                    let r = ui.add(egui::TextEdit::singleline(&mut app.iure.totp).desired_width(140.0)).labelled_by(l.id);
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if (ui.add_enabled(!ls.busy, egui::Button::new(pick("Verify", "Verificar"))).clicked() || enter)
                        && !app.iure.totp.trim().is_empty()
                    {
                        let code = std::mem::take(&mut app.iure.totp);
                        app.session.iure_verify_totp(&code);
                    }
                } else {
                    field(ui, pick("Instance", "Instancia"), &mut app.iure.domain);
                    field(ui, pick("Email", "Correo"), &mut app.iure.email);
                    let l = ui.label(RichText::new(pick("Password", "Contraseña")).color(c.text_muted));
                    ui.add(egui::TextEdit::singleline(&mut app.iure.password).password(true).desired_width(f32::INFINITY)).labelled_by(l.id);
                    ui.label(small(ui, pick(
                        "The session (never the password) is kept in the system keyring and shared with the other Iurefficient apps.",
                        "La sesión (nunca la contraseña) se guarda en el llavero del sistema y se comparte con las demás apps de Iurefficient.",
                    )));
                    let ok = !ls.busy && !app.iure.domain.trim().is_empty() && !app.iure.email.trim().is_empty() && !app.iure.password.is_empty();
                    ui.horizontal(|ui| {
                        if ui.add_enabled(ok, egui::Button::new(pick("Sign in", "Iniciar sesión"))).clicked() {
                            let pw = std::mem::take(&mut app.iure.password);
                            let (d, e) = (app.iure.domain.clone(), app.iure.email.clone());
                            app.session.iure_login(&d, &e, &pw);
                        }
                        if ui.button(pick("Cancel", "Cancelar")).clicked() {
                            app.iure.login_open = false;
                            app.iure.password.clear();
                            app.session.iure_login_reset();
                        }
                    });
                }
                if ls.busy {
                    ui.add(egui::Spinner::new());
                }
                if let Some(e) = &ls.error {
                    ui.label(RichText::new(e.get()).color(c.danger));
                }
            });
    }
    if matches!(st, IureStatus::Ready { .. }) {
        app.iure.login_open = false;
        ui.horizontal(|ui| {
            ui.label(RichText::new(pick("Check notifications every", "Consultar avisos cada")).color(c.text));
            let mut mins = app.session.prefs.iure_poll_secs / 60;
            if ui.add(egui::DragValue::new(&mut mins).range(1..=60).suffix(" min")).changed() {
                app.session.prefs.iure_poll_secs = mins.clamp(1, 60) * 60;
                app.session.save_prefs();
            }
        });
    }
    ui.add_space(14.0);
}
