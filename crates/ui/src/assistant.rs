//! Asistente ✦ en la interfaz (F4.2): botones del panel de lectura («✦ Resumir», respuestas
//! rápidas, «✦ Analizar» con «Crear evento/tarea/lead/ticket»), el panel ✦ del redactor
//! (`Ctrl+J`: reescribir y redactar respuesta), la página del módulo (chat, triage de hoy,
//! minuta, documento para IureEditor), el diálogo «Se enviará…» antes de cualquier proveedor
//! externo y la sección «Asistente» de Ajustes.
//!
//! «La IA propone, el usuario confirma»: nada se crea ni se envía sin un clic en los diálogos que
//! ya existen (editor de eventos de F2.4, diálogos de Iurefficient de F3.1, redactor).

use std::collections::HashSet;

use egui::{Align, Context, Layout, RichText, Stroke, Ui, vec2};
use iuredesk_engine::assist::{AiKey, AiState, AiTask, AiValue, Assist, CopilotProbe, LONG_THREAD_CHARS, TriageRow};
use iuredesk_engine::iure::{LeadForm, TaskForm, TicketForm};
use iuredesk_engine::iuredesk_assistant::schema::{Importance, LeadOrTicket, ProposedEvent, ProposedTask, ReplyKind};
use iuredesk_engine::iuredesk_assistant::{AssistantConfig, Lang, Length, ProviderConfig, ProviderKind, Tone};
use iuredesk_engine::{Notice, Text};
use iuredesk_model::MessageId;
use iurefficient_ui::{
    BadgeKind, Banner, EmptyState, Icon, PaneKind, Severity, badge, icons, pick, primary_button, section_label, split,
    theme,
};

use crate::app::DeskApp;
use crate::rich_edit::{CharStyle, RichDoc};
use crate::widgets::{hairline, pane_title, tool_button};

/// Pestaña de la página del asistente.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Today,
    Chat,
    Minutes,
    Document,
}

/// Estado de la interfaz del asistente.
pub struct AiUi {
    pub hub: Assist,
    pub tab: Tab,
    remember: bool,
    chat_input: String,
    minutes_title: String,
    minutes_text: String,
    doc_purpose: String,
    /// Mensajes para los que ya se pidieron respuestas automáticas.
    auto_done: HashSet<MessageId>,
    // Ajustes.
    openai_url: String,
    openai_model: String,
    anthropic_model: String,
    key_input: String,
    settings_error: Option<String>,
    /// Demostración pendiente (capturas): se lanza cuando hay mensaje enfocado.
    demo: Option<String>,
}

impl AiUi {
    pub fn new(session: &iuredesk_engine::Session) -> Self {
        let path = if session.is_sample() { None } else { AssistantConfig::default_path() };
        let hub = Assist::new(session, path);
        let (mut openai_url, mut openai_model) = ("http://localhost:11434/v1".to_string(), "llama3.1".to_string());
        let mut anthropic_model = iuredesk_engine::iuredesk_assistant::config::DEFAULT_ANTHROPIC_MODEL.to_string();
        for p in &hub.config().providers {
            match &p.kind {
                ProviderKind::OpenAiCompatible { base_url, model } => {
                    openai_url = base_url.clone();
                    openai_model = model.clone();
                }
                ProviderKind::Anthropic { model } => anthropic_model = model.clone(),
                _ => {}
            }
        }
        AiUi {
            hub,
            tab: Tab::Today,
            remember: false,
            chat_input: String::new(),
            minutes_title: String::new(),
            minutes_text: String::new(),
            doc_purpose: String::new(),
            auto_done: HashSet::new(),
            openai_url,
            openai_model,
            anthropic_model,
            key_input: String::new(),
            settings_error: None,
            demo: None,
        }
    }

    /// El diálogo «Se enviará…» está abierto.
    pub fn modal(&self) -> bool {
        self.hub.consent().is_some()
    }

    /// Pon la transcripción de una reunión (de IureTranscribe) en la pestaña «Minuta».
    pub fn set_transcript(&mut self, title: &str, text: &str) {
        self.tab = Tab::Minutes;
        self.minutes_title = title.to_string();
        self.minutes_text = text.chars().take(150_000).collect();
    }
}

/// Estado ✦ de una ventana de redacción.
#[derive(Default)]
pub struct ComposeAi {
    pub open: bool,
    tone: Tone,
    length: Length,
    lang: Option<Lang>,
    instructions: String,
    /// Mensaje al que se responde (para «Redactar respuesta»).
    pub reply_to: Option<MessageId>,
    /// Lo pedido en este cuadro (lo despacha [`compose_pump`]).
    want: Option<Want>,
    /// Texto que se reescribe: (inicio, fin) en caracteres.
    range: Option<(usize, usize)>,
    /// Texto que se pondrá arriba al abrir (respuestas rápidas).
    pub insert_top: Option<String>,
    /// Estado del trabajo ✦ (copiado cada cuadro por [`compose_sync`]).
    view: Option<(AiState, String)>,
}

enum Want {
    Rewrite,
    Draft,
    Dictate,
    Cancel,
    Apply,
}

fn lang_now() -> Lang {
    if iuredesk_engine::lang::spanish() { Lang::Es } else { Lang::En }
}

fn notice_text(app: &mut DeskApp, t: Text) {
    app.notice(Notice::error(t));
}

fn request(app: &mut DeskApp, task: AiTask) {
    if let Err(t) = app.ai.hub.request(&mut app.session, task) {
        notice_text(app, t);
    }
}

/// Atiende un comando `ai.*` / `suite.*` / `app.import`.
pub fn command(app: &mut DeskApp, ctx: &Context, id: &str) {
    let focused = app.session.focused();
    match id {
        "ai.summarize" => {
            if let Some(m) = focused {
                request(app, AiTask::Summarize(m));
            }
        }
        "ai.replies" => {
            if let Some(m) = focused {
                request(app, AiTask::Replies(m));
            }
        }
        "ai.analyze" => {
            if let Some(m) = focused {
                request(app, AiTask::Analyze(m));
            }
        }
        "ai.compose" => {
            if app.composers.is_empty() {
                app.run(ctx, "mail.new");
            }
            if let Some(w) = app.composers.last_mut() {
                w.ai.open = true;
            }
        }
        "ai.triage" => {
            app.ai.tab = Tab::Today;
            request(app, AiTask::Triage);
        }
        "ai.minutes" => app.ai.tab = Tab::Minutes,
        "ai.chat" => app.ai.tab = Tab::Chat,
        "ai.document" => {
            let mut msgs: Vec<MessageId> = app.session.selection().to_vec();
            if msgs.is_empty()
                && let Some(m) = focused
            {
                msgs.push(m);
            }
            app.session.module = iuredesk_engine::Module::Assistant;
            app.ai.tab = Tab::Document;
            let purpose = app.ai.doc_purpose.clone();
            request(app, AiTask::Document { msgs, purpose });
        }
        "app.import" => app.import.open(),
        "notes.open_editor" => crate::pim::open_note_in_editor(app),
        "suite.read_aloud" => crate::suite_ui::read_aloud(app),
        "suite.connect_agents" => app.suite.hub.run(iuredesk_engine::suite::SuiteAction::RegisterAgents),
        _ => {}
    }
}

/// Cada cuadro: resultados, consentimiento y respuestas automáticas.
pub fn frame(app: &mut DeskApp, ctx: &Context) {
    app.ai.hub.poll();
    run_demo(app, ctx);
    if app.ai.hub.prefs().auto_replies
        && let Some(m) = app.session.focused()
        && app.session.module == iuredesk_engine::Module::Mail
        && app.ai.auto_done.insert(m)
        && app.ai.hub.job(AiKey::Replies(m)).is_none()
    {
        // Sin preguntar no: si el proveedor es externo, igual aparece el diálogo.
        let _ = app.ai.hub.request(&mut app.session, AiTask::Replies(m));
    }
    consent_dialog(app, ctx);
}

/// «Se enviará: asunto, cuerpo… a Anthropic.»
fn consent_dialog(app: &mut DeskApp, ctx: &Context) {
    let Some(c) = app.ai.hub.consent() else { return };
    let task = c.task.label();
    let provider = c.provider.clone();
    let scope = c.scope.clone();
    let mut decision: Option<bool> = None;
    let mut remember = app.ai.remember;
    let resp = egui::Modal::new(egui::Id::unique("ai-datascope")).show(ctx, |ui| {
        ui.set_width(480.0);
        let col = theme::colors(ui);
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(20.0, 20.0), egui::Sense::hover());
            icons::paint(ui.painter(), r, Icon::Sparkle, col.accent, col.accent);
            ui.label(RichText::new(task.get()).font(theme::bold_font(ui.ctx(), 17.0)).color(col.text));
        });
        ui.add_space(6.0);
        let es = iuredesk_engine::lang::spanish();
        let fields: Vec<&str> = scope.fields.keys().map(|f| f.label(es)).collect();
        ui.add(
            egui::Label::new(
                RichText::new(format!(
                    "{} {} {} {}.",
                    pick("Will be sent:", "Se enviará:"),
                    fields.join(", ").to_lowercase(),
                    pick("to", "a"),
                    provider
                ))
                .color(col.text),
            )
            .wrap(),
        );
        ui.add_space(4.0);
        let mut facts = vec![format!(
            "{} {} · {} {}",
            scope.items.max(1),
            pick("item(s)", "elemento(s)"),
            scope.total_chars,
            pick("characters", "caracteres")
        )];
        if scope.redacted {
            facts.push(pick("emails and phone numbers masked", "correos y teléfonos enmascarados").into());
        }
        if scope.truncated {
            facts.push(pick("long bodies trimmed", "cuerpos largos recortados").into());
        }
        ui.label(RichText::new(facts.join(" · ")).color(col.text_muted).size(theme::density(ui.ctx()).small_size()));
        ui.add_space(6.0);
        egui::CollapsingHeader::new(pick("See exactly what is sent", "Ver exactamente lo que se envía"))
            .id_salt("ai-payload")
            .show(ui, |ui| {
                egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
                    let mut text: String = scope.payload.chars().take(20_000).collect();
                    ui.add(
                        egui::TextEdit::multiline(&mut text)
                            .font(egui::TextStyle::Monospace)
                            .desired_width(f32::INFINITY)
                            .interactive(false),
                    );
                });
            });
        ui.add_space(6.0);
        ui.checkbox(
            &mut remember,
            pick("Don't ask again for this provider", "No volver a preguntar para este proveedor"),
        );
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if primary_button(ui, pick("Send", "Enviar")).clicked() {
                decision = Some(true);
            }
            if ui.button(pick("Cancel", "Cancelar")).clicked() {
                decision = Some(false);
            }
        });
    });
    app.ai.remember = remember;
    if resp.should_close() && decision.is_none() {
        decision = Some(false);
    }
    match decision {
        Some(true) => {
            app.ai.hub.confirm(remember);
            app.ai.remember = false;
        }
        Some(false) => {
            app.ai.hub.decline();
            app.ai.remember = false;
        }
        None => {}
    }
}

// ---------------------------------------------------------------------------------------------
// Panel de lectura

/// Barra ✦ bajo las acciones del mensaje.
pub fn reading_bar(app: &mut DeskApp, ui: &mut Ui, id: MessageId) {
    let long = app.session.thread_chars(id) >= LONG_THREAD_CHARS;
    let mut fire: Option<&str> = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        if long
            && tool_button(
                ui,
                Icon::Sparkle,
                pick("Summarize", "Resumir"),
                pick("Summarize the conversation", "Resumir la conversación"),
                true,
            )
            .clicked()
        {
            fire = Some("ai.summarize");
        }
        if tool_button(
            ui,
            Icon::Sparkle,
            pick("Analyze", "Analizar"),
            pick("Dates, commitments, lead or ticket", "Fechas, compromisos, lead o ticket"),
            true,
        )
        .clicked()
        {
            fire = Some("ai.analyze");
        }
        if !app.ai.hub.prefs().auto_replies
            && app.ai.hub.job(AiKey::Replies(id)).is_none()
            && tool_button(
                ui,
                Icon::Reply,
                pick("Quick replies", "Respuestas rápidas"),
                pick("Suggest three replies", "Sugerir tres respuestas"),
                true,
            )
            .clicked()
        {
            fire = Some("ai.replies");
        }
        if tool_button(
            ui,
            Icon::Comment,
            pick("Read aloud", "Leer en voz alta"),
            pick("Read the message aloud with IureVoz", "Leer el mensaje en voz alta con IureVoz"),
            true,
        )
        .clicked()
        {
            fire = Some("suite.read_aloud");
        }
    });
    if let Some(cmd) = fire {
        let ctx = ui.ctx().clone();
        command(app, &ctx, cmd);
    }
}

fn card(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    let c = theme::colors(ui);
    egui::Frame::new()
        .fill(c.surface)
        .stroke(Stroke::new(1.0, c.accent.gamma_multiply(0.45)))
        .corner_radius(8)
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
    ui.add_space(6.0);
}

/// Cabecera de una tarjeta ✦ con «Cancelar»/«Cerrar». Devuelve `true` si se cerró.
fn card_head(app: &mut DeskApp, ui: &mut Ui, key: AiKey, title: &str) -> bool {
    let c = theme::colors(ui);
    let running = app.ai.hub.job(key).is_some_and(|j| j.state == AiState::Running);
    let provider = app.ai.hub.job(key).map(|j| j.provider.clone()).unwrap_or_default();
    let mut closed = false;
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), egui::Sense::hover());
        icons::paint(ui.painter(), r, Icon::Sparkle, c.accent, c.accent);
        ui.label(RichText::new(title).strong().color(c.text));
        ui.label(RichText::new(&provider).color(c.text_muted).size(theme::density(ui.ctx()).small_size()));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if running {
                if ui.small_button(pick("Cancel", "Cancelar")).clicked() {
                    app.ai.hub.cancel(key);
                }
                ui.add(egui::Spinner::new().size(12.0));
            } else if icons::icon_button(ui, Icon::Close, &format!("{} {title}", pick("Close", "Cerrar")), true)
                .clicked()
            {
                closed = true;
            }
        });
    });
    if closed {
        app.ai.hub.dismiss(key);
    }
    closed
}

/// Texto en curso, error o cancelación de un trabajo. `true` si ya hay resultado.
fn job_status(app: &mut DeskApp, ui: &mut Ui, key: AiKey, retry: Option<AiTask>) -> Option<AiValue> {
    let c = theme::colors(ui);
    let state = app.ai.hub.job(key).map(|j| j.state.clone())?;
    match state {
        AiState::Running => {
            let s = app.ai.hub.streamed(key);
            let shown = if s.trim().is_empty() {
                pick("Thinking…", "Pensando…").to_string()
            } else {
                // Mientras llega, los últimos caracteres del flujo (JSON en crudo o texto).
                let tail: String = s.chars().rev().take(280).collect::<Vec<_>>().into_iter().rev().collect();
                format!("… {tail}")
            };
            ui.add(egui::Label::new(RichText::new(shown).color(c.text_muted)).wrap());
            None
        }
        AiState::Failed(t) => {
            let r = Banner::new(Severity::Error, t.get()).action(pick("Retry", "Reintentar")).show(ui);
            if r.action
                && let Some(task) = retry
            {
                request(app, task);
            }
            None
        }
        AiState::Cancelled => {
            ui.label(RichText::new(pick("Cancelled.", "Cancelado.")).color(c.text_muted));
            None
        }
        AiState::Done(v) => Some(v),
    }
}

fn bullets(ui: &mut Ui, title: &str, items: &[String]) {
    if items.is_empty() {
        return;
    }
    let c = theme::colors(ui);
    ui.label(RichText::new(title).strong().color(c.text_muted).size(theme::density(ui.ctx()).small_size()));
    for i in items {
        ui.add(egui::Label::new(RichText::new(format!("• {i}")).color(c.text)).wrap());
    }
}

/// Tarjetas ✦ del mensaje: resumen, respuestas y análisis.
pub fn reading_cards(app: &mut DeskApp, ui: &mut Ui, id: MessageId) {
    if app.ai.hub.job(AiKey::Summary(id)).is_some() {
        card(ui, |ui| {
            if card_head(app, ui, AiKey::Summary(id), pick("Summary", "Resumen")) {
                return;
            }
            if let Some(AiValue::Summary(s)) = job_status(app, ui, AiKey::Summary(id), Some(AiTask::Summarize(id))) {
                let c = theme::colors(ui);
                ui.add(egui::Label::new(RichText::new(&s.summary).color(c.text)).wrap());
                ui.add_space(4.0);
                bullets(ui, pick("Key points", "Puntos clave"), &s.key_points);
                bullets(ui, pick("Decisions", "Acuerdos"), &s.decisions);
                bullets(ui, pick("Open questions", "Preguntas abiertas"), &s.open_questions);
                bullets(ui, pick("Next steps", "Siguientes pasos"), &s.next_steps);
            }
        });
    }
    if app.ai.hub.job(AiKey::Analysis(id)).is_some() {
        card(ui, |ui| {
            if card_head(app, ui, AiKey::Analysis(id), pick("Analysis", "Análisis")) {
                return;
            }
            if let Some(AiValue::Analysis(a)) = job_status(app, ui, AiKey::Analysis(id), Some(AiTask::Analyze(id))) {
                analysis(app, ui, id, &a);
            }
        });
    }
}

/// Respuestas rápidas bajo el mensaje.
pub fn reply_chips(app: &mut DeskApp, ui: &mut Ui, id: MessageId) {
    if app.ai.hub.job(AiKey::Replies(id)).is_none() {
        return;
    }
    card(ui, |ui| {
        if card_head(app, ui, AiKey::Replies(id), pick("Quick replies", "Respuestas rápidas")) {
            return;
        }
        if let Some(AiValue::Replies(r)) = job_status(app, ui, AiKey::Replies(id), Some(AiTask::Replies(id))) {
            let c = theme::colors(ui);
            let mut picked: Option<String> = None;
            ui.horizontal_wrapped(|ui| {
                for s in r.suggestions.iter().take(3) {
                    let tag = match s.kind {
                        ReplyKind::Short => pick("Short", "Corta"),
                        ReplyKind::Detailed => pick("Detailed", "Detallada"),
                        _ => pick("Neutral", "Neutra"),
                    };
                    let first: String =
                        s.body.lines().find(|l| !l.trim().is_empty()).unwrap_or("").chars().take(60).collect();
                    let resp = ui
                        .add(
                            egui::Button::new(RichText::new(format!("{tag}: {first}…")).color(c.text))
                                .min_size(vec2(0.0, 26.0)),
                        )
                        .on_hover_text(&s.body);
                    if resp.clicked() {
                        picked = Some(s.body.clone());
                    }
                }
            });
            ui.label(
                RichText::new(pick(
                    "Opens a reply you can edit before sending.",
                    "Abre una respuesta que puedes editar antes de enviar.",
                ))
                .color(c.text_muted)
                .size(theme::density(ui.ctx()).small_size()),
            );
            if let Some(body) = picked {
                open_reply_with(app, ui.ctx(), id, body);
            }
        }
    });
}

fn open_reply_with(app: &mut DeskApp, ctx: &Context, id: MessageId, body: String) {
    let before = app.composers.len();
    app.run(ctx, "mail.reply");
    if app.composers.len() > before
        && let Some(w) = app.composers.last_mut()
    {
        w.ai.reply_to = Some(id);
        w.ai.insert_top = Some(body);
    }
}

fn analysis(app: &mut DeskApp, ui: &mut Ui, id: MessageId, a: &iuredesk_engine::assist::Analysis) {
    let c = theme::colors(ui);
    let ex = &a.extraction;
    ui.horizontal_wrapped(|ui| {
        let (u, k) = match ex.urgency.level() {
            3 => (pick("Critical", "Crítico"), BadgeKind::Danger),
            2 => (pick("Urgent", "Urgente"), BadgeKind::Warning),
            0 => (pick("Not urgent", "Sin prisa"), BadgeKind::Neutral),
            _ => (pick("Normal", "Normal"), BadgeKind::Neutral),
        };
        badge(ui, u, k);
        if let Some(cl) = &a.classification {
            let (t, k) = match cl.category {
                LeadOrTicket::SalesOpportunity => {
                    (pick("Sales opportunity", "Oportunidad de venta"), BadgeKind::Success)
                }
                LeadOrTicket::SupportRequest => (pick("Support request", "Solicitud de soporte"), BadgeKind::Accent),
                LeadOrTicket::Both => (pick("Lead and ticket", "Lead y ticket"), BadgeKind::Accent),
                LeadOrTicket::Neither => (pick("Neither lead nor ticket", "Ni lead ni ticket"), BadgeKind::Neutral),
            };
            badge(ui, t, k);
        }
    });
    if !ex.summary.is_empty() {
        ui.add(egui::Label::new(RichText::new(&ex.summary).color(c.text)).wrap());
    }
    let mut make_event: Option<ProposedEvent> = None;
    let mut make_task: Option<ProposedTask> = None;
    for e in ex.events.iter().take(5) {
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(14.0, 14.0), egui::Sense::hover());
            icons::paint(ui.painter(), r, Icon::Calendar, c.text_muted, c.accent);
            ui.label(RichText::new(format!("{} — {}", e.title, e.start.replace('T', " "))).color(c.text));
            if ui.small_button(pick("Create event", "Crear evento")).clicked() {
                make_event = Some(e.clone());
            }
        });
    }
    for t in ex.tasks.iter().take(5) {
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(14.0, 14.0), egui::Sense::hover());
            icons::paint(ui.painter(), r, Icon::Tasks, c.text_muted, c.accent);
            let due = t.due_date.as_deref().map(|d| format!(" ({d})")).unwrap_or_default();
            ui.label(RichText::new(format!("{}{due}", t.title)).color(c.text));
            if ui.small_button(pick("Create task", "Crear tarea")).clicked() {
                make_task = Some(t.clone());
            }
        });
    }
    let mut lead = false;
    let mut ticket = false;
    if let Some(cl) = &a.classification {
        if !cl.reason.is_empty() {
            ui.label(RichText::new(&cl.reason).color(c.text_muted).size(theme::density(ui.ctx()).small_size()));
        }
        ui.horizontal(|ui| {
            if matches!(cl.category, LeadOrTicket::SalesOpportunity | LeadOrTicket::Both)
                && ui.button(pick("Create lead…", "Crear lead…")).clicked()
            {
                lead = true;
            }
            if matches!(cl.category, LeadOrTicket::SupportRequest | LeadOrTicket::Both)
                && ui.button(pick("Create ticket…", "Crear ticket…")).clicked()
            {
                ticket = true;
            }
        });
    }
    if let Some(e) = make_event {
        create_event(app, &e);
    }
    if let Some(t) = make_task {
        create_task(app, id, &t);
    }
    if lead {
        let mut form = app.session.iure_sender_lead(id);
        if let Some(l) = a.classification.as_ref().and_then(|c| c.lead.clone()) {
            form = LeadForm {
                name: l.name,
                company: l.company_name.unwrap_or(form.company),
                email: l.email.unwrap_or(form.email),
                phone: l.phone.unwrap_or_default(),
                position: l.position.unwrap_or_default(),
                notes: l.notes.unwrap_or(form.notes),
            };
        }
        iure_dialog(app, crate::iure::Dialog::Lead { msg: id, form: Some(form), picked: 0 });
    }
    if ticket {
        let mut form = app.session.iure_ticket_form(id);
        if let Some(t) = a.classification.as_ref().and_then(|c| c.ticket.clone()) {
            form = TicketForm {
                title: t.title,
                description: t.description.unwrap_or(form.description),
                priority: t.priority.as_str().to_string(),
                ..form
            };
        }
        iure_dialog(app, crate::iure::Dialog::Ticket { msg: id, form });
    }
}

fn iure_dialog(app: &mut DeskApp, d: crate::iure::Dialog) {
    if app.session.iure_status().is_ready() || app.session.is_sample() {
        app.iure.dialog = Some(d);
    } else {
        app.notice(Notice::info(
            "Connect Iurefficient in Settings to create leads and tickets.",
            "Conecta Iurefficient en Ajustes para crear leads y tickets.",
        ));
    }
}

/// «Crear evento»: abre el editor de eventos de F2.4 con la propuesta (el usuario guarda).
fn create_event(app: &mut DeskApp, e: &ProposedEvent) {
    use iuredesk_engine::iuredesk_assistant::schema::{When, parse_when};
    let start = parse_when(&e.start);
    let end = e.end.as_deref().and_then(parse_when);
    let (s, en, all_day) = match (start, end) {
        (Some(When::DateTime(a)), Some(When::DateTime(b))) => (a, b, false),
        (Some(When::DateTime(a)), _) => (a, a + chrono::Duration::hours(1), false),
        (Some(When::Date(a)), Some(When::Date(b))) => {
            (a.and_time(chrono::NaiveTime::MIN), b.and_time(chrono::NaiveTime::MIN), true)
        }
        (Some(When::Date(a)), _) => (a.and_time(chrono::NaiveTime::MIN), a.and_time(chrono::NaiveTime::MIN), true),
        _ => return,
    };
    match app.session.new_event_draft(s, en, all_day) {
        Some(mut d) => {
            d.title = e.title.clone();
            d.location = e.location.clone().unwrap_or_default();
            if let Some(src) = &e.source_text {
                d.description = src.clone();
            }
            crate::event_editor::open(app, d);
        }
        None => app
            .notice(Notice::info("Add an account with a calendar first.", "Primero agrega una cuenta con calendario.")),
    }
}

/// «Crear tarea»: en Iurefficient (diálogo de F3.1) si hay sesión; si no, tarea local.
fn create_task(app: &mut DeskApp, id: MessageId, t: &ProposedTask) {
    let due = t.due_date.as_deref().and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok());
    if app.session.iure_status().is_ready() || app.session.is_sample() {
        let base = app.session.iure_task_form(id);
        let form = TaskForm { title: t.title.clone(), due: due.or(base.due), ..base };
        let due_text = form.due.map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default();
        app.iure.dialog = Some(crate::iure::Dialog::Task { msg: id, form, due_text });
        return;
    }
    match app.session.ai_create_task(&t.title, due, t.source_text.as_deref().unwrap_or("")) {
        Ok(iuredesk_engine::Outcome::Notice(n)) => app.notice(n),
        Ok(_) => {}
        Err(e) => app.notice(Notice::error(e.text())),
    }
}

// ---------------------------------------------------------------------------------------------
// Redactor

/// Botones ✦ y «Dictar» en la barra de formato del redactor.
pub fn compose_buttons(ui: &mut Ui, ai: &mut ComposeAi, window: u64) {
    if icons::icon_button(ui, Icon::Sparkle, pick("Assistant (Ctrl+J)", "Asistente (Ctrl+J)"), true).clicked() {
        ai.open = !ai.open;
    }
    if ui.button(pick("Dictate", "Dictar")).on_hover_text(pick("Dictate with IureVoz", "Dictar con IureVoz")).clicked()
    {
        ai.want = Some(Want::Dictate);
    }
    // Ctrl+J dentro del redactor (los atajos de la app no se disparan mientras se escribe).
    let mine = ui.memory(|m| {
        m.has_focus(egui::Id::unique(("compose-body", window)))
            || m.has_focus(egui::Id::unique(("compose-subject", window)))
    });
    if mine && ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::J)) {
        ai.open = !ai.open;
    }
}

/// Panel ✦ del redactor (tono, longitud, idioma; reescribir; redactar respuesta).
pub fn compose_panel(ui: &mut Ui, ai: &mut ComposeAi, editor: &crate::rich_edit::EditorState) {
    if !ai.open {
        return;
    }
    let state = ai.view.clone();
    let c = theme::colors(ui);
    egui::Frame::new()
        .fill(c.surface_alt)
        .stroke(Stroke::new(1.0, c.accent.gamma_multiply(0.45)))
        .corner_radius(6)
        .inner_margin(egui::Margin::symmetric(8, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(14.0, 14.0), egui::Sense::hover());
                icons::paint(ui.painter(), r, Icon::Sparkle, c.accent, c.accent);
                let l = ui.label(RichText::new(pick("Tone", "Tono")).color(c.text_muted));
                egui::ComboBox::from_id_salt("ai-tone")
                    .selected_text(tone_label(ai.tone))
                    .show_ui(ui, |ui| {
                        for t in [Tone::Neutral, Tone::Formal, Tone::Friendly, Tone::Firm] {
                            ui.selectable_value(&mut ai.tone, t, tone_label(t));
                        }
                    })
                    .response
                    .labelled_by(l.id);
                let l = ui.label(RichText::new(pick("Length", "Longitud")).color(c.text_muted));
                egui::ComboBox::from_id_salt("ai-length")
                    .selected_text(length_label(ai.length))
                    .show_ui(ui, |ui| {
                        for x in [Length::Same, Length::Shorter, Length::Longer] {
                            ui.selectable_value(&mut ai.length, x, length_label(x));
                        }
                    })
                    .response
                    .labelled_by(l.id);
                let l = ui.label(RichText::new(pick("Language", "Idioma")).color(c.text_muted));
                let mut lang = ai.lang.unwrap_or_else(lang_now);
                egui::ComboBox::from_id_salt("ai-lang")
                    .selected_text(if lang == Lang::Es { "Español" } else { "English" })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut lang, Lang::Es, "Español");
                        ui.selectable_value(&mut lang, Lang::En, "English");
                    })
                    .response
                    .labelled_by(l.id);
                ai.lang = Some(lang);
            });
            let l = ui.label(
                RichText::new(pick("Extra instructions (optional)", "Indicaciones (opcional)")).color(c.text_muted),
            );
            ui.add(
                egui::TextEdit::singleline(&mut ai.instructions)
                    .desired_width(f32::INFINITY)
                    .hint_text(pick("e.g. accept the meeting on Tuesday", "p. ej. acepta la reunión del martes")),
            )
            .labelled_by(l.id);
            let running = matches!(state, Some((AiState::Running, _)));
            ui.horizontal(|ui| {
                let (a, b) = editor.sel;
                let label = if a != b {
                    pick("Rewrite selection", "Reescribir selección")
                } else {
                    pick("Rewrite", "Reescribir")
                };
                if ui.add_enabled(!running, egui::Button::new(label)).clicked() {
                    ai.want = Some(Want::Rewrite);
                }
                if ui.add_enabled(!running, egui::Button::new(pick("Draft reply", "Redactar respuesta"))).clicked() {
                    ai.want = Some(Want::Draft);
                }
                if running {
                    ui.add(egui::Spinner::new().size(12.0));
                    if ui.button(pick("Cancel", "Cancelar")).clicked() {
                        ai.want = Some(Want::Cancel);
                    }
                }
            });
            match state {
                Some((AiState::Running, s)) if !s.is_empty() => {
                    let tail: String = s.chars().rev().take(200).collect::<Vec<_>>().into_iter().rev().collect();
                    ui.label(RichText::new(format!("… {tail}")).color(c.text_muted));
                }
                Some((AiState::Done(AiValue::Text(t)), _)) => {
                    egui::ScrollArea::vertical().max_height(120.0).id_salt("ai-proposal").show(ui, |ui| {
                        ui.add(egui::Label::new(RichText::new(&t).color(c.text)).wrap());
                    });
                    ui.horizontal(|ui| {
                        if primary_button(ui, pick("Use this text", "Usar este texto")).clicked() {
                            ai.want = Some(Want::Apply);
                        }
                        if ui.button(pick("Discard", "Descartar")).clicked() {
                            ai.want = Some(Want::Cancel);
                        }
                    });
                }
                Some((AiState::Failed(t), _)) => {
                    ui.label(RichText::new(t.get()).color(c.danger));
                }
                _ => {}
            }
        });
    ui.add_space(4.0);
}

fn tone_label(t: Tone) -> &'static str {
    match t {
        Tone::Formal => pick("Formal", "Formal"),
        Tone::Neutral => pick("Neutral", "Neutro"),
        Tone::Friendly => pick("Friendly", "Cercano"),
        Tone::Firm => pick("Firm", "Firme"),
    }
}

fn length_label(l: Length) -> &'static str {
    match l {
        Length::Shorter => pick("Shorter", "Más corto"),
        Length::Same => pick("Same", "Igual"),
        Length::Longer => pick("Longer", "Más largo"),
    }
}

/// Lo propio del texto del redactor (antes de la cita y de la firma), en caracteres.
fn own_text_end(text: &str) -> usize {
    let mut pos = 0usize;
    for line in text.split('\n') {
        let t = line.trim_start();
        if t.starts_with('>') || line == "-- " || line == "--" {
            break;
        }
        pos = pos.saturating_add(line.chars().count()).saturating_add(1);
    }
    pos.min(text.chars().count())
}

/// Copia a cada redactor el estado de su trabajo ✦ (el formulario no ve la aplicación).
pub fn compose_sync(app: &mut DeskApp) {
    let hub = &app.ai.hub;
    for w in &mut app.composers {
        let key = AiKey::Compose(w.id);
        w.ai.view = hub.job(key).map(|j| (j.state.clone(), hub.streamed(key)));
    }
}

/// Despacha lo pedido en los paneles ✦ de los redactores (con acceso a la sesión).
pub fn compose_pump(app: &mut DeskApp) {
    let n = app.composers.len();
    for i in 0..n {
        let (window, want, insert) = match app.composers.get_mut(i) {
            Some(w) => (w.id, w.ai.want.take(), w.ai.insert_top.take()),
            None => continue,
        };
        if let Some(text) = insert.filter(|t| !t.trim().is_empty())
            && let Some(w) = app.composers.get_mut(i)
        {
            let mut doc = w.editor.doc.clone();
            doc.insert(0, &format!("{}\n\n", text.trim_end()), CharStyle::default());
            w.editor.set_doc(doc);
        }
        let Some(want) = want else { continue };
        let key = AiKey::Compose(window);
        match want {
            Want::Cancel => app.ai.hub.dismiss(key),
            Want::Dictate => app.suite.hub.run(iuredesk_engine::suite::SuiteAction::Dictate { window }),
            Want::Rewrite => {
                let Some(w) = app.composers.get_mut(i) else { continue };
                let (a, b) = w.editor.sel;
                let (from, to) = if a != b { (a.min(b), a.max(b)) } else { (0, own_text_end(&w.editor.doc.text)) };
                let text: String = w.editor.doc.text.chars().skip(from).take(to.saturating_sub(from)).collect();
                w.ai.range = Some((from, to));
                let task = AiTask::Rewrite {
                    window,
                    text,
                    tone: w.ai.tone,
                    length: w.ai.length,
                    lang: w.ai.lang.unwrap_or_else(lang_now),
                };
                request(app, task);
            }
            Want::Draft => {
                let Some(w) = app.composers.get_mut(i) else { continue };
                let Some(msg) = w.ai.reply_to.or_else(|| app.session.focused()) else {
                    app.notice(Notice::info(
                        "Open a message to reply to first.",
                        "Primero abre el mensaje al que respondes.",
                    ));
                    continue;
                };
                w.ai.range = Some((0, 0));
                let task = AiTask::DraftReply { window, msg, tone: w.ai.tone, instructions: w.ai.instructions.clone() };
                request(app, task);
            }
            Want::Apply => {
                let text = match app.ai.hub.job(key).map(|j| &j.state) {
                    Some(AiState::Done(AiValue::Text(t))) => t.clone(),
                    _ => continue,
                };
                if let Some(w) = app.composers.get_mut(i) {
                    let (from, to) = w.ai.range.take().unwrap_or((0, 0));
                    let mut doc: RichDoc = w.editor.doc.clone();
                    let to = to.min(doc.text.chars().count());
                    let from = from.min(to);
                    doc.remove(from, to);
                    let sep = if to == from { "\n\n" } else { "\n" };
                    doc.insert(from, &format!("{}{sep}", text.trim_end()), CharStyle::default());
                    w.editor.set_doc(doc);
                }
                app.ai.hub.dismiss(key);
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Página del módulo

pub fn page(app: &mut DeskApp, ui: &mut Ui) {
    split::center(ui, PaneKind::Detail, |ui| {
        let c = theme::colors(ui);
        ui.horizontal(|ui| {
            pane_title(ui, pick("Assistant", "Asistente"), &app.ai.hub.provider_label());
        });
        ui.label(
            RichText::new(pick(
                "The AI proposes; you confirm. Nothing is sent or created without your click.",
                "La IA propone; tú confirmas. Nada se envía ni se crea sin tu clic.",
            ))
            .color(c.text_muted),
        );
        ui.add_space(6.0);
        let mut tab = app.ai.tab;
        iurefficient_ui::segmented(
            ui,
            &mut tab,
            &[
                (Tab::Today, pick("Today", "Hoy")),
                (Tab::Chat, pick("Chat", "Chat")),
                (Tab::Minutes, pick("Minutes", "Minuta")),
                (Tab::Document, pick("Document", "Documento")),
            ],
        );
        app.ai.tab = tab;
        ui.add_space(8.0);
        hairline(ui);
        ui.add_space(8.0);
        egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("ai-page").show(ui, |ui| {
            ui.set_max_width(820.0);
            match app.ai.tab {
                Tab::Today => today(app, ui),
                Tab::Chat => chat(app, ui),
                Tab::Minutes => minutes(app, ui),
                Tab::Document => document(app, ui),
            }
        });
    });
}

fn today(app: &mut DeskApp, ui: &mut Ui) {
    let c = theme::colors(ui);
    if primary_button(ui, pick("What needs my attention today?", "¿Qué necesito atender hoy?")).clicked() {
        request(app, AiTask::Triage);
    }
    ui.add_space(8.0);
    if app.ai.hub.job(AiKey::Triage).is_none() {
        EmptyState::new(Icon::Sparkle, pick("Your day, in order", "Tu día, en orden"))
            .body(pick(
                "The assistant reads your unread inbox (subject, sender and a short excerpt) and ranks it with a one-line reason.",
                "El asistente lee tu bandeja sin leer (asunto, remitente y un extracto) y la ordena con una razón de una línea.",
            ))
            .show(ui);
        return;
    }
    if card_head(app, ui, AiKey::Triage, pick("Today's priorities", "Prioridades de hoy")) {
        return;
    }
    let Some(AiValue::Triage(rows)) = job_status(app, ui, AiKey::Triage, Some(AiTask::Triage)) else { return };
    let mut open: Option<MessageId> = None;
    for (k, r) in rows.iter().enumerate() {
        if triage_row(ui, k, r) {
            open = Some(r.id);
        }
    }
    if rows.is_empty() {
        ui.label(RichText::new(pick("Nothing pending.", "Nada pendiente.")).color(c.text_muted));
    }
    if let Some(id) = open {
        app.session.select_folder(iuredesk_engine::FolderSel::Unified(iuredesk_model::FolderRole::Inbox));
        app.session.request_focus(id);
    }
}

/// «Mi día»: pide la priorización de la bandeja (con el consentimiento y las reglas de
/// DataScope de siempre; nunca sola).
pub(crate) fn request_triage(app: &mut DeskApp) {
    request(app, AiTask::Triage);
}

/// ¿Se está priorizando ahora?
pub(crate) fn triage_running(app: &DeskApp) -> bool {
    app.ai.hub.job(AiKey::Triage).is_some_and(|j| j.state == AiState::Running)
}

/// El resultado de la priorización, compacto (hasta `max` filas). Devuelve el mensaje a abrir.
pub(crate) fn triage_compact(app: &mut DeskApp, ui: &mut Ui, max: usize) -> Option<MessageId> {
    app.ai.hub.job(AiKey::Triage)?;
    let Some(AiValue::Triage(rows)) = job_status(app, ui, AiKey::Triage, Some(AiTask::Triage)) else { return None };
    let mut open = None;
    for (k, r) in rows.iter().take(max).enumerate() {
        if triage_row(ui, k, r) {
            open = Some(r.id);
        }
    }
    if rows.is_empty() {
        let c = theme::colors(ui);
        ui.label(RichText::new(pick("Nothing pending.", "Nada pendiente.")).color(c.text_muted));
    }
    open
}

fn triage_row(ui: &mut Ui, k: usize, r: &TriageRow) -> bool {
    let c = theme::colors(ui);
    let mut clicked = false;
    egui::Frame::new()
        .fill(c.surface)
        .stroke(Stroke::new(1.0, c.separator))
        .corner_radius(6)
        .inner_margin(egui::Margin::symmetric(10, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.add_sized(
                    vec2(22.0, 18.0),
                    egui::Label::new(RichText::new(format!("{}.", k + 1)).color(c.text_muted)),
                );
                let (t, kind) = match r.importance {
                    Importance::High => (pick("High", "Alta"), BadgeKind::Danger),
                    Importance::Medium => (pick("Medium", "Media"), BadgeKind::Warning),
                    Importance::Low => (pick("Low", "Baja"), BadgeKind::Neutral),
                };
                badge(ui, t, kind);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    ui.add(egui::Label::new(RichText::new(&r.subject).strong().color(c.text)).truncate());
                    let line =
                        if r.reason.is_empty() { r.from.clone() } else { format!("{} — {}", r.from, r.reason) };
                    ui.add(egui::Label::new(RichText::new(line).color(c.text_muted)).truncate());
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button(pick("Open", "Abrir")).on_hover_text(&r.subject).clicked() {
                        clicked = true;
                    }
                });
            });
        });
    ui.add_space(4.0);
    clicked
}

fn chat(app: &mut DeskApp, ui: &mut Ui) {
    let c = theme::colors(ui);
    let lines = app.ai.hub.chat().to_vec();
    if lines.is_empty() {
        ui.label(
            RichText::new(pick(
                "Ask anything about your mail, agenda or a draft.",
                "Pregunta lo que quieras sobre tu correo, tu agenda o un borrador.",
            ))
            .color(c.text_muted),
        );
    }
    for l in &lines {
        let (fill, who) = if l.mine {
            (c.accent.gamma_multiply(0.15), pick("You", "Tú"))
        } else {
            (c.surface, pick("Assistant", "Asistente"))
        };
        egui::Frame::new().fill(fill).corner_radius(8).inner_margin(egui::Margin::symmetric(10, 6)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(who).strong().color(c.text_muted).size(theme::density(ui.ctx()).small_size()));
            ui.add(egui::Label::new(RichText::new(&l.text).color(c.text)).selectable(true).wrap());
        });
        ui.add_space(4.0);
    }
    let running = app.ai.hub.job(AiKey::Chat).is_some_and(|j| j.state == AiState::Running);
    if running {
        let s = app.ai.hub.streamed(AiKey::Chat);
        egui::Frame::new().fill(c.surface).corner_radius(8).inner_margin(egui::Margin::symmetric(10, 6)).show(
            ui,
            |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(12.0));
                    if ui.small_button(pick("Stop", "Detener")).clicked() {
                        app.ai.hub.cancel(AiKey::Chat);
                    }
                });
                if !s.is_empty() {
                    ui.add(egui::Label::new(RichText::new(&s).color(c.text)).wrap());
                }
            },
        );
    } else if let Some(AiState::Failed(t)) = app.ai.hub.job(AiKey::Chat).map(|j| j.state.clone()) {
        ui.label(RichText::new(t.get()).color(c.danger));
    }
    ui.add_space(6.0);
    let mut send = false;
    ui.horizontal(|ui| {
        let r = ui.add(
            egui::TextEdit::singleline(&mut app.ai.chat_input)
                .desired_width(ui.available_width() - 160.0)
                .hint_text(pick("Write to the assistant", "Escribe al asistente")),
        );
        crate::widgets::named(&r, egui::Role::TextInput, pick("Message to the assistant", "Mensaje al asistente"));
        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            send = true;
        }
        if ui.add_enabled(!running, egui::Button::new(pick("Send", "Enviar"))).clicked() {
            send = true;
        }
        if ui.button(pick("New chat", "Nueva conversación")).clicked() {
            app.ai.hub.clear_chat();
        }
    });
    if send && !running {
        let text = std::mem::take(&mut app.ai.chat_input);
        request(app, AiTask::Chat(text));
    }
}

fn minutes(app: &mut DeskApp, ui: &mut Ui) {
    let c = theme::colors(ui);
    ui.label(
        RichText::new(pick(
            "Load a recording (transcribed by IureTranscribe) or a transcript file, or paste the text.",
            "Carga una grabación (la transcribe IureTranscribe) o un archivo de transcripción, o pega el texto.",
        ))
        .color(c.text_muted),
    );
    ui.horizontal(|ui| {
        if ui.button(pick("Load recording or transcript…", "Cargar grabación o transcripción…")).clicked() {
            crate::suite_ui::pick_transcript(app, &app.ai.minutes_title.clone());
        }
        if let Some(p) = app.suite.hub.transcribing() {
            ui.add(egui::ProgressBar::new(f32::from(p.percent) / 100.0).desired_width(160.0).text(format!(
                "{} {}%",
                pick("Transcribing", "Transcribiendo"),
                p.percent
            )));
        }
    });
    let l = ui.label(RichText::new(pick("Meeting title", "Título de la reunión")).color(c.text_muted));
    ui.add(egui::TextEdit::singleline(&mut app.ai.minutes_title).desired_width(f32::INFINITY)).labelled_by(l.id);
    let l = ui.label(RichText::new(pick("Transcript", "Transcripción")).color(c.text_muted));
    ui.add(
        egui::TextEdit::multiline(&mut app.ai.minutes_text)
            .desired_rows(8)
            .desired_width(f32::INFINITY)
            .hint_text(pick("Paste the transcript here", "Pega aquí la transcripción")),
    )
    .labelled_by(l.id);
    if primary_button(ui, pick("Write the minutes", "Redactar la minuta")).clicked() {
        let title = Some(app.ai.minutes_title.clone()).filter(|t| !t.trim().is_empty());
        request(app, AiTask::Minutes { transcript: app.ai.minutes_text.clone(), title });
    }
    ui.add_space(8.0);
    if app.ai.hub.job(AiKey::Minutes).is_none() || card_head(app, ui, AiKey::Minutes, pick("Minutes", "Minuta")) {
        return;
    }
    let Some(AiValue::Minutes(m)) = job_status(app, ui, AiKey::Minutes, None) else { return };
    let es = iuredesk_engine::lang::spanish();
    let md = m.to_markdown(es);
    egui::Frame::new().fill(c.surface).corner_radius(6).inner_margin(egui::Margin::same(10)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        md_view(ui, &md);
    });
    let mut tasks: Vec<ProposedTask> = Vec::new();
    ui.horizontal_wrapped(|ui| {
        if ui.button(pick("Save in Notes", "Guardar en Notas")).clicked() {
            let title = if m.title.is_empty() { pick("Minutes", "Minuta").to_string() } else { m.title.clone() };
            match app.session.ai_save_note(&title, &md) {
                Ok(iuredesk_engine::Outcome::Notice(n)) => app.notice(n),
                Ok(_) => {}
                Err(e) => app.notice(Notice::error(e.text())),
            }
        }
        if ui.button(pick("Open in IureEditor", "Abrir en IureEditor")).clicked() {
            let name = if m.title.is_empty() { "Minuta".to_string() } else { m.title.clone() };
            app.suite.hub.run(iuredesk_engine::suite::SuiteAction::NewDocument { name, markdown: md.clone() });
        }
        if ui.button(pick("Draft email to attendees", "Borrador para los asistentes")).clicked() {
            let mut cd = app.session.compose_new(None);
            cd.draft.subject = if m.title.is_empty() { pick("Minutes", "Minuta").into() } else { m.title.clone() };
            cd.draft.body = iuredesk_engine::iuredesk_compose::RichBody::from_markdown(&md);
            app.open_composer(cd);
        }
        if !m.action_items.is_empty() && ui.button(pick("Create the tasks", "Crear las tareas")).clicked() {
            tasks = m.action_items.clone();
        }
    });
    if !tasks.is_empty() {
        let mut n = 0;
        for t in tasks.iter().take(20) {
            let due = t.due_date.as_deref().and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok());
            if app.session.ai_create_task(&t.title, due, t.assignee_hint.as_deref().unwrap_or("")).is_ok() {
                n += 1;
            }
        }
        app.notice(Notice::success(format!("{n} tasks created"), format!("{n} tareas creadas")));
    }
}

fn document(app: &mut DeskApp, ui: &mut Ui) {
    let c = theme::colors(ui);
    let n = app.session.selection().len().max(usize::from(app.session.focused().is_some()));
    ui.label(
        RichText::new(format!(
            "{} {n} {}",
            pick(
                "Creates a document outline in Markdown from the",
                "Crea el esquema de un documento en Markdown con los"
            ),
            pick(
                "selected message(s), to keep writing in IureEditor.",
                "correos seleccionados, para seguir escribiendo en IureEditor."
            )
        ))
        .color(c.text_muted),
    );
    let l = ui.label(RichText::new(pick("What is it for?", "¿Para qué es?")).color(c.text_muted));
    ui.add(egui::TextEdit::singleline(&mut app.ai.doc_purpose).desired_width(f32::INFINITY).hint_text(pick(
        "e.g. memo to the client with the agreed points",
        "p. ej. memorando al cliente con lo acordado",
    )))
    .labelled_by(l.id);
    if primary_button(ui, pick("Create document", "Crear documento")).clicked() {
        let ctx = ui.ctx().clone();
        command(app, &ctx, "ai.document");
    }
    ui.add_space(8.0);
    if app.ai.hub.job(AiKey::Document).is_none() || card_head(app, ui, AiKey::Document, pick("Document", "Documento")) {
        return;
    }
    let Some(AiValue::Outline(o)) = job_status(app, ui, AiKey::Document, None) else { return };
    egui::Frame::new().fill(c.surface).corner_radius(6).inner_margin(egui::Margin::same(10)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        md_view(ui, &o.markdown);
    });
    if primary_button(ui, pick("Open in IureEditor", "Abrir en IureEditor")).clicked() {
        let name = if o.title.is_empty() { "Documento".to_string() } else { o.title.clone() };
        app.suite.hub.run(iuredesk_engine::suite::SuiteAction::NewDocument { name, markdown: o.markdown.clone() });
    }
}

// ---------------------------------------------------------------------------------------------
// Ajustes → Asistente

fn upsert(cfg: &mut AssistantConfig, p: ProviderConfig) {
    match cfg.providers.iter_mut().find(|x| x.id == p.id) {
        Some(x) => {
            *x = ProviderConfig {
                redact: x.redact,
                timeout_secs: x.timeout_secs,
                max_output_tokens: x.max_output_tokens,
                ..p
            }
        }
        None => cfg.providers.push(p),
    }
}

pub fn settings_section(app: &mut DeskApp, ui: &mut Ui) {
    let c = theme::colors(ui);
    section_label(ui, pick("Assistant", "Asistente"));
    let mut cfg = app.ai.hub.config().clone();
    let mut changed = false;
    if ui.checkbox(&mut cfg.enabled, pick("Turn on the assistant", "Activar el asistente")).changed() {
        changed = true;
    }
    let current = cfg.default_provider().map(|p| p.id.clone()).unwrap_or_default();
    let iure_ready = app.session.iure_status().is_ready();
    let copilot_ok = *app.ai.hub.copilot() == CopilotProbe::Available;
    let mut choice = current.clone();
    let l = ui.label(RichText::new(pick("Provider", "Proveedor")).color(c.text_muted));
    egui::ComboBox::from_id_salt("ai-provider")
        .selected_text(match choice.as_str() {
            "iurefficient" => "Iurefficient".to_string(),
            "copilot" => "Microsoft 365 Copilot".to_string(),
            "openai" => pick(
                "OpenAI-compatible (OpenAI, OpenRouter, Ollama…)",
                "Compatible con OpenAI (OpenAI, OpenRouter, Ollama…)",
            )
            .to_string(),
            "anthropic" => "Anthropic".to_string(),
            other => other.to_string(),
        })
        .show_ui(ui, |ui| {
            let iure_label = if iure_ready {
                "Iurefficient".to_string()
            } else {
                format!("Iurefficient ({})", pick("not connected", "sin conectar"))
            };
            ui.selectable_value(&mut choice, "iurefficient".to_string(), iure_label);
            if copilot_ok {
                ui.selectable_value(&mut choice, "copilot".to_string(), "Microsoft 365 Copilot");
            }
            ui.selectable_value(
                &mut choice,
                "openai".to_string(),
                pick(
                    "OpenAI-compatible (OpenAI, OpenRouter, Ollama…)",
                    "Compatible con OpenAI (OpenAI, OpenRouter, Ollama…)",
                ),
            );
            ui.selectable_value(&mut choice, "anthropic".to_string(), "Anthropic");
        })
        .response
        .labelled_by(l.id);
    if choice != current {
        match choice.as_str() {
            "copilot" => upsert(&mut cfg, ProviderConfig::new("copilot", ProviderKind::Copilot)),
            "openai" => upsert(
                &mut cfg,
                ProviderConfig::new(
                    "openai",
                    ProviderKind::OpenAiCompatible {
                        base_url: app.ai.openai_url.clone(),
                        model: app.ai.openai_model.clone(),
                    },
                ),
            ),
            "anthropic" => upsert(
                &mut cfg,
                ProviderConfig::new("anthropic", ProviderKind::Anthropic { model: app.ai.anthropic_model.clone() }),
            ),
            _ => {}
        }
        cfg.default_provider = Some(choice.clone());
        changed = true;
    }
    if !copilot_ok {
        ui.horizontal(|ui| {
            let probing = *app.ai.hub.copilot() == CopilotProbe::Probing;
            if ui
                .add_enabled(
                    !probing,
                    egui::Button::new(pick("Check Microsoft 365 Copilot", "Comprobar Microsoft 365 Copilot")),
                )
                .clicked()
            {
                app.ai.hub.probe_copilot(&app.session);
            }
            if let CopilotProbe::Unavailable(why) = app.ai.hub.copilot() {
                ui.label(
                    RichText::new(format!("{}: {why}", pick("Not available", "No disponible")))
                        .color(c.text_muted)
                        .size(theme::density(ui.ctx()).small_size()),
                );
            }
        });
    }
    match choice.as_str() {
        "openai" => {
            ui.horizontal(|ui| {
                let l = ui.label(RichText::new(pick("Base URL", "URL base")).color(c.text_muted));
                let r =
                    ui.add(egui::TextEdit::singleline(&mut app.ai.openai_url).desired_width(260.0)).labelled_by(l.id);
                let l = ui.label(RichText::new(pick("Model", "Modelo")).color(c.text_muted));
                let r2 =
                    ui.add(egui::TextEdit::singleline(&mut app.ai.openai_model).desired_width(140.0)).labelled_by(l.id);
                if r.lost_focus() || r2.lost_focus() {
                    upsert(
                        &mut cfg,
                        ProviderConfig::new(
                            "openai",
                            ProviderKind::OpenAiCompatible {
                                base_url: app.ai.openai_url.trim().to_string(),
                                model: app.ai.openai_model.trim().to_string(),
                            },
                        ),
                    );
                    changed = true;
                }
            });
        }
        "anthropic" => {
            ui.horizontal(|ui| {
                let l = ui.label(RichText::new(pick("Model", "Modelo")).color(c.text_muted));
                let r = ui
                    .add(egui::TextEdit::singleline(&mut app.ai.anthropic_model).desired_width(200.0))
                    .labelled_by(l.id);
                if r.lost_focus() {
                    upsert(
                        &mut cfg,
                        ProviderConfig::new(
                            "anthropic",
                            ProviderKind::Anthropic { model: app.ai.anthropic_model.trim().to_string() },
                        ),
                    );
                    changed = true;
                }
            });
        }
        _ => {}
    }
    if matches!(choice.as_str(), "openai" | "anthropic") {
        let has = app.ai.hub.has_api_key(&app.session, &choice);
        ui.horizontal(|ui| {
            let l = ui.label(RichText::new(pick("API key", "Clave de API")).color(c.text_muted));
            let hint = if has { pick("saved in the keyring", "guardada en el llavero") } else { "sk-…" };
            crate::widgets::secret_field(ui, &mut app.ai.key_input, "ai-key", hint, 240.0).labelled_by(l.id);
            if ui.button(pick("Save key", "Guardar clave")).clicked() {
                // Primero la configuración (la clave se guarda por id de proveedor).
                if changed {
                    if let Err(t) = app.ai.hub.set_config(cfg.clone()) {
                        app.ai.settings_error = Some(t.get().to_string());
                    }
                    changed = false;
                }
                let key = std::mem::take(&mut app.ai.key_input);
                match app.ai.hub.set_api_key(&app.session, &choice, &key) {
                    Ok(()) => app.notice(Notice::success("Key saved in the keyring", "Clave guardada en el llavero")),
                    Err(t) => app.ai.settings_error = Some(t.get().to_string()),
                }
            }
        });
    }
    let mut redact = cfg.default_provider().and_then(|p| p.redact).unwrap_or(true);
    if ui
        .checkbox(
            &mut redact,
            pick(
                "Mask emails and phone numbers before sending to external providers",
                "Enmascarar correos y teléfonos antes de enviar a proveedores externos",
            ),
        )
        .changed()
    {
        let id = cfg.default_provider().map(|p| p.id.clone());
        if let Some(p) = id.and_then(|id| cfg.providers.iter_mut().find(|p| p.id == id)) {
            p.redact = Some(redact);
            changed = true;
        }
    }
    let mut prefs = app.ai.hub.prefs().clone();
    if ui
        .checkbox(
            &mut prefs.auto_replies,
            pick("Suggest quick replies when opening a message", "Sugerir respuestas rápidas al abrir un mensaje"),
        )
        .changed()
    {
        app.ai.hub.set_prefs(prefs.clone());
    }
    if !prefs.no_ask.is_empty()
        && ui
            .button(pick(
                "Ask again before sending to any provider",
                "Volver a preguntar antes de enviar a cualquier proveedor",
            ))
            .clicked()
    {
        prefs.no_ask.clear();
        app.ai.hub.set_prefs(prefs);
    }
    if changed && let Err(t) = app.ai.hub.set_config(cfg) {
        app.ai.settings_error = Some(t.get().to_string());
    }
    ui.horizontal(|ui| {
        if ui.button(pick("Test the connection", "Probar la conexión")).clicked() {
            app.ai.settings_error = None;
            if let Err(t) = app.ai.hub.request(&mut app.session, AiTask::Test) {
                app.ai.settings_error = Some(t.get().to_string());
            }
        }
        match app.ai.hub.job(AiKey::Test).map(|j| j.state.clone()) {
            Some(AiState::Running) => {
                ui.add(egui::Spinner::new().size(12.0));
            }
            Some(AiState::Done(_)) => {
                ui.label(RichText::new(pick("Works ✓", "Funciona ✓")).color(c.success));
            }
            Some(AiState::Failed(t)) => {
                ui.add(egui::Label::new(RichText::new(t.get()).color(c.danger)).wrap());
            }
            _ => {}
        }
    });
    if let Some(e) = &app.ai.settings_error {
        ui.add(egui::Label::new(RichText::new(e).color(c.danger)).wrap());
    }
    ui.add_space(14.0);
}

// ---------------------------------------------------------------------------------------------
// Demostraciones (capturas y pruebas)

fn run_demo(app: &mut DeskApp, ctx: &Context) {
    let Some(what) = app.ai.demo.clone() else { return };
    let focused = app.session.focused();
    let needs_msg = matches!(what.as_str(), "reading" | "datascope" | "compose");
    if needs_msg && focused.is_none() {
        return;
    }
    app.ai.demo = None;
    match what.as_str() {
        "reading" => {
            command(app, ctx, "ai.summarize");
            command(app, ctx, "ai.analyze");
            command(app, ctx, "ai.replies");
        }
        "datascope" => command(app, ctx, "ai.summarize"),
        "compose" => {
            if let Some(m) = focused {
                open_reply_with(app, ctx, m, String::new());
                if let Some(w) = app.composers.last_mut() {
                    w.ai.open = true;
                    w.ai.want = Some(Want::Draft);
                }
            }
        }
        "triage" => command(app, ctx, "ai.triage"),
        "chat" => {
            app.ai.tab = Tab::Chat;
            request(app, AiTask::Chat(pick("What do I have today?", "¿Qué tengo hoy?").into()));
        }
        "minutes" => {
            app.ai.set_transcript(
                pick("Case review", "Revisión del caso 241/2026"),
                pick(
                    "[00:00:05] Laura: Let's review the case. [00:01:10] Carmen: The evidence was filed. [00:03:40] Laura: Then we file the appeal; I'll draft it.",
                    "[00:00:05] Laura: Revisemos el caso. [00:01:10] Carmen: Ya se presentaron las pruebas. [00:03:40] Laura: Entonces presentamos la apelación; yo la redacto.",
                ),
            );
            let title = Some(app.ai.minutes_title.clone());
            request(app, AiTask::Minutes { transcript: app.ai.minutes_text.clone(), title });
        }
        _ => {}
    }
}

impl DeskApp {
    /// Demostración del asistente para capturas: `reading`, `datascope`, `compose`, `triage`,
    /// `chat`, `minutes`.
    pub fn demo_assistant(&mut self, what: &str) {
        if what == "datascope" {
            let hub = std::mem::replace(&mut self.ai.hub, Assist::new(&self.session, None));
            self.ai.hub =
                hub.with_factory(iuredesk_engine::assist::external_sample_factory("Anthropic · claude-sonnet-5-5"));
        }
        if matches!(what, "triage" | "chat" | "minutes") {
            self.session.module = iuredesk_engine::Module::Assistant;
        }
        self.ai.demo = Some(what.to_string());
    }

    /// Usa este proveedor para todo (pruebas).
    pub fn set_ai_factory(&mut self, f: iuredesk_engine::assist::ProviderFactory) {
        let hub = std::mem::replace(&mut self.ai.hub, Assist::new(&self.session, None));
        self.ai.hub = hub.with_factory(f);
    }

    /// ¿Está a la vista el diálogo «Se enviará…»? (pruebas)
    pub fn ai_consent_open(&self) -> bool {
        self.ai.hub.consent().is_some()
    }

    /// Estado de un trabajo del asistente (pruebas).
    pub fn ai_state(&self, key: AiKey) -> Option<AiState> {
        self.ai.hub.job(key).map(|j| j.state.clone())
    }
}

/// Markdown sencillo de las propuestas (títulos, viñetas, casillas, negritas) sin mostrar marcas.
fn md_view(ui: &mut Ui, md: &str) {
    let c = theme::colors(ui);
    let ctx = ui.ctx().clone();
    let body = theme::density(&ctx).body_size();
    ui.spacing_mut().item_spacing.y = 3.0;
    for line in md.lines().take(400) {
        let t = line.trim_end();
        if t.trim().is_empty() {
            ui.add_space(2.0);
            continue;
        }
        let clean = |s: &str| s.replace("**", "");
        let rt = if let Some(h) = t.strip_prefix("# ") {
            RichText::new(clean(h)).font(theme::bold_font(&ctx, body + 5.0)).color(c.text)
        } else if let Some(h) = t.strip_prefix("## ") {
            RichText::new(clean(h)).font(theme::bold_font(&ctx, body + 2.0)).color(c.text)
        } else if let Some(h) = t.strip_prefix("### ") {
            RichText::new(clean(h)).font(theme::bold_font(&ctx, body)).color(c.text)
        } else if let Some(x) = t.strip_prefix("- [ ] ").or_else(|| t.strip_prefix("- [x] ")) {
            RichText::new(format!("  \u{2022} {}", clean(x))).color(c.text)
        } else if let Some(x) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
            RichText::new(format!("  \u{2022} {}", clean(x))).color(c.text)
        } else {
            RichText::new(clean(t)).color(c.text)
        };
        ui.add(egui::Label::new(rt).selectable(true).wrap());
    }
}
