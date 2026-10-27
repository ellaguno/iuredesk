//! Tareas del asistente en dos pasos, para que nada salga del equipo sin una llamada explícita:
//!
//! 1. `prepare_*` arma la petición y el [`DataScope`] (qué se enviará, ya enmascarado si
//!    corresponde) sin tocar la red. La UI lo muestra.
//! 2. [`run`] la envía al proveedor (con flujo de texto, cancelación y topes), repara o rechaza
//!    el JSON, restaura lo enmascarado y valida: devuelve una [`Proposal`] que el usuario confirma.

use std::marker::PhantomData;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::error::{AiError, Result};
use crate::input::{Addr, EmailInput, Lang};
use crate::json_repair::parse_lenient;
use crate::privacy::{DataField, DataScope, Redactor, ScopeOptions};
use crate::prompts::{PromptId, system_prompt};
use crate::provider::{AiProvider, ChatMessage, ChatRequest, Limits, StreamEvent, Usage, run_chat};
use crate::schema::{
    DocumentOutline, DraftReplies, Extraction, LeadTicketClassification, MeetingMinutes, TaskOutput, ThreadSummary,
    Triage, ValidateCtx,
};

/// Tope de la transcripción de una reunión.
const MAX_TRANSCRIPT: usize = 150_000;
/// Extracto por correo en el triage.
const TRIAGE_SNIPPET: usize = 400;
/// Correos máximos en el triage.
const TRIAGE_MAX: usize = 60;

/// A quién se enviará (para el [`DataScope`] y la redacción por omisión).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub label: String,
    pub external: bool,
}

impl Target {
    pub fn of(provider: &dyn AiProvider) -> Self {
        Self { label: provider.label(), external: provider.is_external() }
    }
}

/// Opciones comunes de las tareas.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskOptions {
    /// Idioma de las instrucciones y de los textos de salida.
    pub lang: Lang,
    pub scope: ScopeOptions,
    pub max_output_tokens: u32,
    /// Nombre y correo del usuario («Ana Pérez <ana@despacho.mx>»).
    pub me: String,
    /// Hoy (inyectable para pruebas deterministas).
    pub today: NaiveDate,
    /// Zona IANA del usuario.
    pub time_zone: String,
    /// Caso de Iurefficient para dar contexto (sólo proveedor Iurefficient).
    pub case_id: Option<String>,
}

impl Default for TaskOptions {
    fn default() -> Self {
        Self {
            lang: Lang::Es,
            scope: ScopeOptions::default(),
            max_output_tokens: 2048,
            me: String::new(),
            today: chrono::Local::now().date_naive(),
            time_zone: "UTC".into(),
            case_id: None,
        }
    }
}

/// Tono de una respuesta o reescritura.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    Formal,
    #[default]
    Neutral,
    Friendly,
    Firm,
}

impl Tone {
    fn label(self, lang: Lang) -> &'static str {
        match self {
            Tone::Formal => lang.pick("formal", "formal"),
            Tone::Neutral => lang.pick("neutral and professional", "neutro y profesional"),
            Tone::Friendly => lang.pick("warm and friendly", "cálido y cercano"),
            Tone::Firm => lang.pick("firm but polite", "firme pero cortés"),
        }
    }
}

/// Longitud de una reescritura.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Length {
    Shorter,
    #[default]
    Same,
    Longer,
}

impl Length {
    fn label(self, lang: Lang) -> &'static str {
        match self {
            Length::Shorter => lang.pick("noticeably shorter", "notablemente más corto"),
            Length::Same => lang.pick("about the same", "más o menos igual"),
            Length::Longer => lang.pick("somewhat longer and more complete", "algo más largo y completo"),
        }
    }
}

fn language_name(target: Lang, prompt: Lang) -> &'static str {
    match target {
        Lang::Es => prompt.pick("Spanish", "español"),
        Lang::En => prompt.pick("English", "inglés"),
    }
}

/// Una petición lista para enviar, con lo que saldrá del equipo.
#[derive(Debug, Clone)]
pub struct Prepared<T> {
    pub task: PromptId,
    /// Qué se enviará (mostrar antes de confirmar).
    pub scope: DataScope,
    pub request: ChatRequest,
    redactor: Option<Redactor>,
    ctx: ValidateCtx,
    _out: PhantomData<fn() -> T>,
}

/// Propuesta del asistente: el usuario decide qué hacer con ella.
#[derive(Debug, Clone, PartialEq)]
pub struct Proposal<T> {
    pub task: PromptId,
    pub value: T,
    pub provider: String,
    pub scope: DataScope,
    /// Hubo que reparar el JSON.
    pub repaired: bool,
    /// El modelo se cortó por tope de tokens.
    pub truncated: bool,
    pub usage: Option<Usage>,
}

/// Arma el texto de los correos con lo que permite el alcance.
struct Builder<'a> {
    opts: &'a TaskOptions,
    scope: DataScope,
    text: String,
}

impl<'a> Builder<'a> {
    fn new(opts: &'a TaskOptions, target: &Target) -> Self {
        let redacted = opts.scope.redact.unwrap_or(target.external);
        Self {
            opts,
            scope: DataScope {
                provider: target.label.clone(),
                external: target.external,
                redacted,
                ..Default::default()
            },
            text: String::new(),
        }
    }

    fn l(&self, en: &'static str, es: &'static str) -> &'static str {
        self.opts.lang.pick(en, es)
    }

    fn line(&mut self, field: DataField, label: &str, value: &str) {
        if value.trim().is_empty() {
            return;
        }
        self.scope.add(field, value.chars().count());
        self.text.push_str(label);
        self.text.push_str(": ");
        self.text.push_str(value.trim());
        self.text.push('\n');
    }

    fn email(&mut self, n: usize, e: &EmailInput, body_cap: usize, with_id: bool) {
        self.scope.items = self.scope.items.saturating_add(1);
        let header = if with_id {
            format!("=== {} {n} (id: {}) ===\n", self.l("Email", "Correo"), e.id)
        } else {
            format!("=== {} {n} ===\n", self.l("Email", "Correo"))
        };
        self.text.push_str(&header);
        self.line(DataField::Sender, self.l("From", "De"), &e.from.display());
        if self.opts.scope.include_recipients {
            let join = |v: &[Addr]| v.iter().map(Addr::display).collect::<Vec<_>>().join(", ");
            self.line(DataField::Recipients, self.l("To", "Para"), &join(&e.to));
            self.line(DataField::Recipients, "CC", &join(&e.cc));
        }
        if let Some(d) = &e.date {
            self.line(DataField::Date, self.l("Date", "Fecha"), d);
        }
        self.line(DataField::Subject, self.l("Subject", "Asunto"), &e.subject);
        if self.opts.scope.include_attachment_names && !e.attachments.is_empty() {
            self.line(DataField::AttachmentNames, self.l("Attachments", "Adjuntos"), &e.attachments.join(", "));
        }
        if self.opts.scope.include_body && !e.body_text.trim().is_empty() {
            let cap = body_cap.min(self.opts.scope.max_body_chars);
            let body = e.body_text.trim();
            let mut b: String = body.chars().take(cap).collect();
            if body.chars().count() > cap {
                self.scope.truncated = true;
                b.push_str(self.l("\n[…truncated]", "\n[…recortado]"));
            }
            self.scope.add(DataField::Body, b.chars().count());
            self.text.push_str("---\n");
            self.text.push_str(&b);
            self.text.push('\n');
        }
        self.text.push('\n');
    }

    fn block(&mut self, field: DataField, label: &str, value: &str, cap: usize) {
        let mut v: String = value.trim().chars().take(cap).collect();
        if value.trim().chars().count() > cap {
            self.scope.truncated = true;
            v.push_str(self.l("\n[…truncated]", "\n[…recortado]"));
        }
        self.scope.add(field, v.chars().count());
        self.text.push_str(&format!("=== {label} ===\n{v}\n"));
    }

    fn finish<T>(self, task: PromptId, vars: &[(&str, &str)], ctx: ValidateCtx) -> Prepared<T> {
        let system = system_prompt(task, self.opts.lang, vars);
        let mut scope = self.scope;
        let (system, user, redactor) = if scope.redacted {
            let mut r = Redactor::new();
            let s = r.mask(&system);
            let u = r.mask(&self.text);
            (s, u, Some(r))
        } else {
            (system, self.text, None)
        };
        scope.add(DataField::Instructions, system.chars().count());
        scope.total_chars = system.chars().count().saturating_add(user.chars().count());
        scope.payload = format!("{system}\n\n{user}");
        let request = ChatRequest {
            system,
            messages: vec![ChatMessage::user(user)],
            max_output_tokens: self.opts.max_output_tokens,
            temperature: 0.2,
            json: true,
            case_id: self.opts.case_id.clone(),
        };
        Prepared { task, scope, request, redactor, ctx, _out: PhantomData }
    }
}

/// Tres respuestas al último correo del hilo (corta, neutra, detallada).
pub fn prepare_draft_reply(
    target: &Target,
    thread: &[EmailInput],
    tone: Tone,
    reply_lang: Lang,
    instructions: &str,
    opts: &TaskOptions,
) -> Prepared<DraftReplies> {
    let mut b = Builder::new(opts, target);
    let n = thread.len();
    for (i, e) in thread.iter().enumerate() {
        // El último correo completo; los anteriores, como contexto breve.
        let cap = if i.saturating_add(1) == n { usize::MAX } else { 2_000 };
        b.email(i.saturating_add(1), e, cap, false);
    }
    if !instructions.trim().is_empty() {
        b.scope.add(DataField::UserText, instructions.chars().count());
    }
    let lang = language_name(reply_lang, opts.lang);
    let instr = if instructions.trim().is_empty() { "—" } else { instructions.trim() };
    b.finish(
        PromptId::DraftReply,
        &[("me", opts.me.as_str()), ("language", lang), ("tone", tone.label(opts.lang)), ("instructions", instr)],
        ValidateCtx::default(),
    )
}

/// Reescribe un texto del usuario.
pub fn prepare_rewrite(
    target: &Target,
    text: &str,
    tone: Tone,
    length: Length,
    out_lang: Lang,
    opts: &TaskOptions,
) -> Prepared<crate::schema::Rewritten> {
    let mut b = Builder::new(opts, target);
    let label = b.l("Text", "Texto");
    b.block(DataField::UserText, label, text, opts.scope.max_body_chars);
    b.finish(
        PromptId::Rewrite,
        &[
            ("tone", tone.label(opts.lang)),
            ("length", length.label(opts.lang)),
            ("language", language_name(out_lang, opts.lang)),
        ],
        ValidateCtx::default(),
    )
}

/// Resumen de un hilo.
pub fn prepare_summarize_thread(target: &Target, thread: &[EmailInput], opts: &TaskOptions) -> Prepared<ThreadSummary> {
    let mut b = Builder::new(opts, target);
    for (i, e) in thread.iter().enumerate() {
        b.email(i.saturating_add(1), e, usize::MAX, false);
    }
    b.finish(PromptId::SummarizeThread, &[], ValidateCtx::default())
}

/// Intención, fechas → eventos, compromisos → tareas, montos, personas, sentimiento, urgencia.
pub fn prepare_extract(target: &Target, email: &EmailInput, opts: &TaskOptions) -> Prepared<Extraction> {
    let mut b = Builder::new(opts, target);
    b.email(1, email, usize::MAX, false);
    let today = opts.today.format("%Y-%m-%d (%A)").to_string();
    b.finish(PromptId::Extract, &[("today", &today), ("time_zone", &opts.time_zone)], ValidateCtx::default())
}

/// ¿Oportunidad de venta o solicitud de soporte? Con la carga útil propuesta para Iurefficient.
pub fn prepare_classify_lead_or_ticket(
    target: &Target,
    email: &EmailInput,
    opts: &TaskOptions,
) -> Prepared<LeadTicketClassification> {
    let mut b = Builder::new(opts, target);
    b.email(1, email, usize::MAX, false);
    b.finish(PromptId::Classify, &[], ValidateCtx::default())
}

/// Minuta a partir de la transcripción (salida de IureTranscribe).
pub fn prepare_meeting_minutes(
    target: &Target,
    transcript: &str,
    meeting_title: Option<&str>,
    opts: &TaskOptions,
) -> Prepared<MeetingMinutes> {
    let mut b = Builder::new(opts, target);
    b.scope.items = 1;
    if let Some(t) = meeting_title.filter(|t| !t.trim().is_empty()) {
        let label = b.l("Meeting", "Reunión");
        b.line(DataField::Subject, label, t);
    }
    let label = b.l("Transcript", "Transcripción");
    b.block(DataField::Transcript, label, transcript, MAX_TRANSCRIPT);
    let today = opts.today.format("%Y-%m-%d").to_string();
    b.finish(PromptId::MeetingMinutes, &[("today", &today)], ValidateCtx::default())
}

/// Orden de los no leídos de hoy, con una razón de una línea.
pub fn prepare_triage_today(target: &Target, unread: &[EmailInput], opts: &TaskOptions) -> Prepared<Triage> {
    let mut b = Builder::new(opts, target);
    let mut ids = Vec::new();
    for (i, e) in unread.iter().filter(|e| !e.id.trim().is_empty()).take(TRIAGE_MAX).enumerate() {
        b.email(i.saturating_add(1), e, TRIAGE_SNIPPET, true);
        ids.push(e.id.trim().to_string());
    }
    if unread.len() > TRIAGE_MAX {
        b.scope.truncated = true;
    }
    let today = opts.today.format("%Y-%m-%d (%A)").to_string();
    b.finish(PromptId::TriageToday, &[("me", opts.me.as_str()), ("today", &today)], ValidateCtx { known_ids: ids })
}

/// Esquema de documento (Markdown para IureEditor) a partir de correos seleccionados.
pub fn prepare_document_outline(
    target: &Target,
    emails: &[EmailInput],
    purpose: &str,
    opts: &TaskOptions,
) -> Prepared<DocumentOutline> {
    let mut b = Builder::new(opts, target);
    for (i, e) in emails.iter().enumerate() {
        b.email(i.saturating_add(1), e, usize::MAX, false);
    }
    if !purpose.trim().is_empty() {
        b.scope.add(DataField::UserText, purpose.chars().count());
    }
    let p = if purpose.trim().is_empty() { "—" } else { purpose.trim() };
    b.finish(PromptId::DocumentOutline, &[("purpose", p)], ValidateCtx::default())
}

/// Claves cuyo valor es un enumerado: se normalizan (`"High"`, `"sales opportunity"` →
/// `high`, `sales_opportunity`) antes de deserializar.
const ENUM_KEYS: &[&str] = &["kind", "intent", "sentiment", "urgency", "category", "priority", "importance"];

fn normalize_enums(v: &mut Value) {
    match v {
        Value::Object(o) => {
            for (k, x) in o.iter_mut() {
                if ENUM_KEYS.contains(&k.as_str())
                    && let Value::String(s) = x
                    && s.len() <= 40
                {
                    *s = s.trim().to_lowercase().replace([' ', '-'], "_");
                } else {
                    normalize_enums(x);
                }
            }
        }
        Value::Array(a) => a.iter_mut().for_each(normalize_enums),
        _ => {}
    }
}

/// Convierte el texto del modelo en el resultado validado (sin red; útil en pruebas).
pub fn parse_output<T: TaskOutput>(raw: &str, redactor: Option<&Redactor>, ctx: &ValidateCtx) -> Result<(T, bool)> {
    let parsed = parse_lenient(raw).map_err(AiError::InvalidOutput)?;
    let mut value = parsed.value;
    if let Some(r) = redactor {
        r.restore_value(&mut value);
    }
    normalize_enums(&mut value);
    let typed: T = serde_json::from_value(value).map_err(|e| AiError::InvalidOutput(e.to_string()))?;
    let valid = typed.validate(ctx).map_err(AiError::InvalidOutput)?;
    Ok((valid, parsed.repaired))
}

/// Envía una petición preparada y devuelve la propuesta validada. Sólo se llama por una acción
/// explícita del usuario.
pub async fn run<T: TaskOutput>(
    provider: &dyn AiProvider,
    prepared: Prepared<T>,
    limits: &Limits,
    tx: Option<&UnboundedSender<StreamEvent>>,
    cancel: &CancellationToken,
) -> Result<Proposal<T>> {
    let completion = run_chat(provider, &prepared.request, limits, tx, cancel).await?;
    let (value, repaired) = parse_output::<T>(&completion.text, prepared.redactor.as_ref(), &prepared.ctx)?;
    Ok(Proposal {
        task: prepared.task,
        value,
        provider: provider.label(),
        scope: prepared.scope,
        repaired,
        truncated: completion.truncated(),
        usage: completion.usage,
    })
}

impl<T> Prepared<T> {
    /// ¿Se enmascaró algún dato?
    pub fn redacted_anything(&self) -> bool {
        self.redactor.as_ref().is_some_and(|r| !r.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{Importance, ReplyKind};

    fn email() -> EmailInput {
        EmailInput {
            id: "m1".into(),
            from: Addr::new(Some("Ana Ruiz"), "ana@cliente.mx"),
            to: vec![Addr::new(None, "yo@despacho.mx")],
            subject: "Cotización".into(),
            body_text: "Llámame al 55 1234 5678.".into(),
            attachments: vec!["contrato.pdf".into()],
            ..Default::default()
        }
    }

    fn opts() -> TaskOptions {
        TaskOptions {
            today: NaiveDate::from_ymd_opt(2026, 10, 26).unwrap(),
            me: "Yo <yo@despacho.mx>".into(),
            ..Default::default()
        }
    }

    #[test]
    fn external_targets_are_redacted_by_default_and_restored() {
        let t = Target { label: "Ext".into(), external: true };
        let p = prepare_classify_lead_or_ticket(&t, &email(), &opts());
        assert!(p.scope.redacted && p.redacted_anything());
        assert!(!p.scope.payload.contains("ana@cliente.mx"));
        assert!(!p.request.messages[0].content.contains("1234"));
        let raw = r#"{"category":"Sales Opportunity","confidence":0.8,"reason":"r","lead":{"name":"Ana","email":"[email-1]","phone":"[tel-1]"}}"#;
        let (c, _) = parse_output::<LeadTicketClassification>(raw, p.redactor.as_ref(), &p.ctx).unwrap();
        let lead = c.lead.unwrap();
        // [email-1] es el del usuario en las instrucciones ({{me}} no está aquí) o el de Ana.
        assert!(lead.email.unwrap().contains('@'));
        assert_eq!(lead.phone.as_deref(), Some("55 1234 5678"));

        let local = Target { label: "Ollama".into(), external: false };
        let p = prepare_classify_lead_or_ticket(&local, &email(), &opts());
        assert!(!p.scope.redacted);
        assert!(p.scope.payload.contains("ana@cliente.mx"));
    }

    #[test]
    fn scope_reflects_options() {
        let t = Target { label: "X".into(), external: false };
        let mut o = opts();
        o.scope.include_body = false;
        o.scope.include_recipients = false;
        o.scope.include_attachment_names = false;
        let p = prepare_extract(&t, &email(), &o);
        assert!(!p.scope.fields.contains_key(&DataField::Body));
        assert!(!p.scope.fields.contains_key(&DataField::Recipients));
        assert!(!p.scope.fields.contains_key(&DataField::AttachmentNames));
        assert!(!p.scope.payload.contains("contrato.pdf"));
        assert!(p.scope.fields.contains_key(&DataField::Subject));
        o.scope.max_body_chars = 5;
        o.scope.include_body = true;
        let p = prepare_extract(&t, &email(), &o);
        assert!(p.scope.truncated);
        assert_eq!(p.scope.total_chars, p.request.input_chars());
    }

    #[test]
    fn enum_normalization_and_output_parsing() {
        let ctx = ValidateCtx { known_ids: vec!["a".into()] };
        let (t, repaired) = parse_output::<Triage>(
            "```json\n{\"items\":[{\"id\":\"a\",\"importance\":\"High\",\"reason\":\"x\",}]}\n```",
            None,
            &ctx,
        )
        .unwrap();
        assert!(repaired);
        assert_eq!(t.items[0].importance, Importance::High);
        let (d, _) = parse_output::<DraftReplies>(
            r#"{"suggestions":[{"kind":"SHORT","body":"a"},{"kind":"Neutral","body":"b"},{"kind":"detailed","body":"c"}]}"#,
            None,
            &ValidateCtx::default(),
        )
        .unwrap();
        assert_eq!(d.suggestions[0].kind, ReplyKind::Short);
        assert!(matches!(parse_output::<Triage>("lo siento, no puedo", None, &ctx), Err(AiError::InvalidOutput(_))));
    }
}
