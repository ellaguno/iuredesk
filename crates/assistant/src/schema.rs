//! Resultados de las tareas: tipos con `serde` (tolerantes a campos ausentes y a valores
//! enumerados desconocidos) y validación que normaliza, recorta o rechaza.
//!
//! Los nombres de campo de [`LeadPayload`] y [`TicketPayload`] son los de Iurefficient
//! (`docs/iurefficient-api.md` §1.3, §1.4), para pasarlos tal cual a [`crate::actions`].

use std::collections::HashSet;

use chrono::{NaiveDate, NaiveDateTime};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// Tope de un campo corto (títulos, razones).
const SHORT: usize = 300;
/// Tope de un campo de texto largo (cuerpos, resúmenes).
const LONG: usize = 20_000;
/// Tope del Markdown de un esquema de documento.
const DOC: usize = 100_000;
/// Tope de elementos de una lista.
const MAX_ITEMS: usize = 50;

/// Contexto de validación (lo que se sabe de la entrada).
#[derive(Debug, Clone, Default)]
pub struct ValidateCtx {
    /// Ids de los correos enviados (el triage no puede inventar otros).
    pub known_ids: Vec<String>,
}

/// Un resultado de tarea: se deserializa y luego se valida.
pub trait TaskOutput: DeserializeOwned + Serialize + Send + Sized + 'static {
    /// Normaliza (recorta, quita vacíos, corrige) o rechaza con el motivo.
    fn validate(self, ctx: &ValidateCtx) -> Result<Self, String>;
}

fn clip(s: &str, max: usize) -> String {
    let t = s.trim();
    if t.chars().count() <= max { t.to_string() } else { t.chars().take(max).collect::<String>() + "…" }
}

fn clip_opt(s: Option<String>, max: usize) -> Option<String> {
    s.map(|x| clip(&x, max)).filter(|x| !x.is_empty() && x != "null")
}

fn clip_list(v: Vec<String>, max: usize) -> Vec<String> {
    v.into_iter().map(|s| clip(&s, max)).filter(|s| !s.is_empty()).take(MAX_ITEMS).collect()
}

/// Fecha (`AAAA-MM-DD`) o fecha y hora sin zona (`AAAA-MM-DDTHH:MM[:SS]`); RFC 3339 se acepta y
/// se queda con la hora de pared.
pub fn parse_when(s: &str) -> Option<When> {
    let s = s.trim();
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Some(When::Date(d));
    }
    for f in ["%Y-%m-%dT%H:%M", "%Y-%m-%dT%H:%M:%S", "%Y-%m-%d %H:%M", "%Y-%m-%d %H:%M:%S"] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(s, f) {
            return Some(When::DateTime(dt));
        }
    }
    chrono::DateTime::parse_from_rfc3339(s).ok().map(|d| When::DateTime(d.naive_local()))
}

/// Momento propuesto (siempre hora de pared en la zona del usuario).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
    Date(NaiveDate),
    DateTime(NaiveDateTime),
}

impl When {
    /// Forma canónica.
    pub fn canonical(self) -> String {
        match self {
            When::Date(d) => d.format("%Y-%m-%d").to_string(),
            When::DateTime(dt) => dt.format("%Y-%m-%dT%H:%M").to_string(),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Respuestas propuestas
// ---------------------------------------------------------------------------------------------

/// Variante de respuesta.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplyKind {
    Short,
    Neutral,
    Detailed,
    #[serde(other)]
    Unknown,
}

/// Una respuesta propuesta.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplySuggestion {
    #[serde(default = "unknown_kind")]
    pub kind: ReplyKind,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub body: String,
}

fn unknown_kind() -> ReplyKind {
    ReplyKind::Unknown
}

/// Tres respuestas: corta, neutra y detallada.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftReplies {
    #[serde(default)]
    pub suggestions: Vec<ReplySuggestion>,
}

impl TaskOutput for DraftReplies {
    fn validate(self, _: &ValidateCtx) -> Result<Self, String> {
        let mut items: Vec<ReplySuggestion> = self
            .suggestions
            .into_iter()
            .map(|s| ReplySuggestion { kind: s.kind, subject: clip_opt(s.subject, SHORT), body: clip(&s.body, LONG) })
            .filter(|s| !s.body.is_empty())
            .take(MAX_ITEMS)
            .collect();
        let kinds: HashSet<ReplyKind> = items.iter().map(|s| s.kind).collect();
        let complete = [ReplyKind::Short, ReplyKind::Neutral, ReplyKind::Detailed].iter().all(|k| kinds.contains(k));
        if !complete {
            // Reparación: tres textos sin etiquetas útiles → por longitud.
            if items.len() != 3 {
                return Err(format!("se esperaban 3 respuestas (corta, neutra, detallada) y llegaron {}", items.len()));
            }
            items.sort_by_key(|s| s.body.chars().count());
            for (s, k) in items.iter_mut().zip([ReplyKind::Short, ReplyKind::Neutral, ReplyKind::Detailed]) {
                s.kind = k;
            }
        }
        let mut out = Vec::with_capacity(3);
        for k in [ReplyKind::Short, ReplyKind::Neutral, ReplyKind::Detailed] {
            if let Some(s) = items.iter().find(|s| s.kind == k) {
                out.push(s.clone());
            }
        }
        Ok(DraftReplies { suggestions: out })
    }
}

/// Texto reescrito.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rewritten {
    #[serde(default)]
    pub text: String,
}

impl TaskOutput for Rewritten {
    fn validate(self, _: &ValidateCtx) -> Result<Self, String> {
        let text = clip(&self.text, LONG);
        if text.is_empty() {
            return Err("texto vacío".into());
        }
        Ok(Rewritten { text })
    }
}

/// Resumen de un hilo.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ThreadSummary {
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub key_points: Vec<String>,
    #[serde(default)]
    pub decisions: Vec<String>,
    #[serde(default)]
    pub open_questions: Vec<String>,
    #[serde(default)]
    pub next_steps: Vec<String>,
}

impl TaskOutput for ThreadSummary {
    fn validate(self, _: &ValidateCtx) -> Result<Self, String> {
        let summary = clip(&self.summary, LONG);
        if summary.is_empty() {
            return Err("resumen vacío".into());
        }
        Ok(ThreadSummary {
            summary,
            key_points: clip_list(self.key_points, SHORT * 2),
            decisions: clip_list(self.decisions, SHORT * 2),
            open_questions: clip_list(self.open_questions, SHORT * 2),
            next_steps: clip_list(self.next_steps, SHORT * 2),
        })
    }
}

// ---------------------------------------------------------------------------------------------
// Análisis de un correo
// ---------------------------------------------------------------------------------------------

/// Intención del correo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Intent {
    SupportRequest,
    SalesInquiry,
    Scheduling,
    LegalMatter,
    Information,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sentiment {
    Positive,
    Negative,
    #[serde(other)]
    Neutral,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Urgency {
    Low,
    High,
    Critical,
    /// También para valores desconocidos (`serde(other)` debe ir al final).
    #[serde(other)]
    Normal,
}

impl Urgency {
    /// 0 (baja) … 3 (crítica), para ordenar.
    pub fn level(self) -> u8 {
        match self {
            Urgency::Low => 0,
            Urgency::Normal => 1,
            Urgency::High => 2,
            Urgency::Critical => 3,
        }
    }
}

/// Evento propuesto (para el calendario propio o el de Iurefficient).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposedEvent {
    #[serde(default)]
    pub title: String,
    /// `AAAA-MM-DD` o `AAAA-MM-DDTHH:MM`, hora de pared del usuario.
    #[serde(default)]
    pub start: String,
    #[serde(default)]
    pub end: Option<String>,
    #[serde(default)]
    pub all_day: bool,
    #[serde(default)]
    pub location: Option<String>,
    #[serde(default)]
    pub source_text: Option<String>,
}

impl ProposedEvent {
    /// Normaliza; `None` si no tiene título o fecha válida.
    pub fn normalized(self) -> Option<Self> {
        let title = clip(&self.title, SHORT);
        let start = parse_when(&self.start)?;
        if title.is_empty() {
            return None;
        }
        let end = self.end.as_deref().and_then(parse_when).filter(|e| match (start, *e) {
            (When::DateTime(a), When::DateTime(b)) => b > a,
            (When::Date(a), When::Date(b)) => b >= a,
            _ => false,
        });
        Some(ProposedEvent {
            title,
            start: start.canonical(),
            end: end.map(When::canonical),
            all_day: matches!(start, When::Date(_)),
            location: clip_opt(self.location, SHORT),
            source_text: clip_opt(self.source_text, SHORT),
        })
    }
}

/// Compromiso → tarea propuesta.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposedTask {
    #[serde(default)]
    pub title: String,
    /// `AAAA-MM-DD` (fecha de pared, como `tasks.due_date` de Iurefficient).
    #[serde(default)]
    pub due_date: Option<String>,
    #[serde(default)]
    pub assignee_hint: Option<String>,
    #[serde(default)]
    pub source_text: Option<String>,
}

impl ProposedTask {
    pub fn normalized(self) -> Option<Self> {
        let title = clip(&self.title, SHORT);
        if title.is_empty() {
            return None;
        }
        let due_date = self.due_date.as_deref().and_then(parse_when).map(|w| match w {
            When::Date(d) => d.format("%Y-%m-%d").to_string(),
            When::DateTime(dt) => dt.date().format("%Y-%m-%d").to_string(),
        });
        Some(ProposedTask {
            title,
            due_date,
            assignee_hint: clip_opt(self.assignee_hint, SHORT),
            source_text: clip_opt(self.source_text, SHORT),
        })
    }
}

/// Monto mencionado.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Amount {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub value: Option<f64>,
    #[serde(default)]
    pub currency: Option<String>,
}

/// Persona mencionada.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonMention {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub phone: Option<String>,
    #[serde(default)]
    pub organization: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
}

/// Análisis de un correo.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Extraction {
    #[serde(default = "other_intent")]
    pub intent: Intent,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub events: Vec<ProposedEvent>,
    #[serde(default)]
    pub tasks: Vec<ProposedTask>,
    #[serde(default)]
    pub amounts: Vec<Amount>,
    #[serde(default)]
    pub people: Vec<PersonMention>,
    #[serde(default)]
    pub organizations: Vec<String>,
    #[serde(default = "neutral")]
    pub sentiment: Sentiment,
    #[serde(default = "normal")]
    pub urgency: Urgency,
}

fn other_intent() -> Intent {
    Intent::Other
}
fn neutral() -> Sentiment {
    Sentiment::Neutral
}
fn normal() -> Urgency {
    Urgency::Normal
}

impl TaskOutput for Extraction {
    fn validate(self, _: &ValidateCtx) -> Result<Self, String> {
        Ok(Extraction {
            intent: self.intent,
            summary: clip(&self.summary, SHORT * 3),
            events: self.events.into_iter().filter_map(ProposedEvent::normalized).take(MAX_ITEMS).collect(),
            tasks: self.tasks.into_iter().filter_map(ProposedTask::normalized).take(MAX_ITEMS).collect(),
            amounts: self
                .amounts
                .into_iter()
                .map(|a| Amount {
                    text: clip(&a.text, SHORT),
                    value: a.value.filter(|v| v.is_finite()),
                    currency: clip_opt(a.currency, 8).map(|c| c.to_uppercase()),
                })
                .filter(|a| !a.text.is_empty())
                .take(MAX_ITEMS)
                .collect(),
            people: self
                .people
                .into_iter()
                .map(|p| PersonMention {
                    name: clip(&p.name, SHORT),
                    email: clip_opt(p.email, SHORT).filter(|e| e.contains('@')),
                    phone: clip_opt(p.phone, 40),
                    organization: clip_opt(p.organization, SHORT),
                    role: clip_opt(p.role, SHORT),
                })
                .filter(|p| !p.name.is_empty() || p.email.is_some())
                .take(MAX_ITEMS)
                .collect(),
            organizations: clip_list(self.organizations, SHORT),
            sentiment: self.sentiment,
            urgency: self.urgency,
        })
    }
}

// ---------------------------------------------------------------------------------------------
// ¿Lead o ticket?
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeadOrTicket {
    SalesOpportunity,
    SupportRequest,
    Both,
    #[serde(other)]
    Neither,
}

/// Lead propuesto: campos de `POST /api/plugins/crm/leads`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LeadPayload {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub first_name: Option<String>,
    #[serde(default)]
    pub last_name: Option<String>,
    #[serde(default)]
    pub company_name: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub phone: Option<String>,
    #[serde(default)]
    pub position: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
}

impl LeadPayload {
    pub fn normalized(self) -> Option<Self> {
        let name = clip(&self.name, SHORT);
        if name.is_empty() {
            return None;
        }
        Some(LeadPayload {
            name,
            first_name: clip_opt(self.first_name, SHORT),
            last_name: clip_opt(self.last_name, SHORT),
            company_name: clip_opt(self.company_name, SHORT),
            email: clip_opt(self.email, SHORT).filter(|e| e.contains('@')),
            phone: clip_opt(self.phone, 40),
            position: clip_opt(self.position, SHORT),
            notes: clip_opt(self.notes, LONG / 4),
        })
    }
}

/// Prioridad de un ticket (valores de Iurefficient).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TicketPriority {
    Low,
    High,
    Urgent,
    #[serde(other)]
    Medium,
}

impl TicketPriority {
    pub fn as_str(self) -> &'static str {
        match self {
            TicketPriority::Low => "low",
            TicketPriority::Medium => "medium",
            TicketPriority::High => "high",
            TicketPriority::Urgent => "urgent",
        }
    }
}

/// Ticket propuesto: campos de `POST /api/plugins/tickets`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketPayload {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default = "medium")]
    pub priority: TicketPriority,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub requester_name: Option<String>,
    /// Ojo: con este campo la instancia contesta por correo al solicitante cada comentario
    /// público. [`crate::actions`] sólo lo manda si el usuario lo pide explícitamente.
    #[serde(default)]
    pub requester_email: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

fn medium() -> TicketPriority {
    TicketPriority::Medium
}

impl TicketPayload {
    pub fn normalized(self) -> Option<Self> {
        let title = clip(&self.title, SHORT);
        if title.is_empty() {
            return None;
        }
        Some(TicketPayload {
            title,
            description: clip_opt(self.description, LONG),
            priority: self.priority,
            category: clip_opt(self.category, SHORT),
            requester_name: clip_opt(self.requester_name, SHORT),
            requester_email: clip_opt(self.requester_email, SHORT).filter(|e| e.contains('@')),
            tags: clip_list(self.tags, 40).into_iter().take(10).collect(),
        })
    }
}

/// Clasificación lead/ticket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LeadTicketClassification {
    #[serde(default = "neither")]
    pub category: LeadOrTicket,
    #[serde(default)]
    pub confidence: f64,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub lead: Option<LeadPayload>,
    #[serde(default)]
    pub ticket: Option<TicketPayload>,
}

fn neither() -> LeadOrTicket {
    LeadOrTicket::Neither
}

impl TaskOutput for LeadTicketClassification {
    fn validate(self, _: &ValidateCtx) -> Result<Self, String> {
        let confidence = if self.confidence.is_finite() { self.confidence.clamp(0.0, 1.0) } else { 0.0 };
        let wants_lead = matches!(self.category, LeadOrTicket::SalesOpportunity | LeadOrTicket::Both);
        let wants_ticket = matches!(self.category, LeadOrTicket::SupportRequest | LeadOrTicket::Both);
        Ok(LeadTicketClassification {
            category: self.category,
            confidence,
            reason: clip(&self.reason, SHORT),
            // Sólo se propone lo que corresponde a la categoría.
            lead: self.lead.filter(|_| wants_lead).and_then(LeadPayload::normalized),
            ticket: self.ticket.filter(|_| wants_ticket).and_then(TicketPayload::normalized),
        })
    }
}

// ---------------------------------------------------------------------------------------------
// Minuta
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MinutesTopic {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub notes: String,
}

/// Minuta de reunión.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeetingMinutes {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub date: Option<String>,
    #[serde(default)]
    pub attendees: Vec<String>,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub topics: Vec<MinutesTopic>,
    #[serde(default)]
    pub decisions: Vec<String>,
    #[serde(default)]
    pub action_items: Vec<ProposedTask>,
    #[serde(default)]
    pub next_meeting: Option<ProposedEvent>,
}

impl TaskOutput for MeetingMinutes {
    fn validate(self, _: &ValidateCtx) -> Result<Self, String> {
        let summary = clip(&self.summary, LONG);
        if summary.is_empty() {
            return Err("minuta sin resumen".into());
        }
        Ok(MeetingMinutes {
            title: clip(&self.title, SHORT),
            date: self.date.as_deref().and_then(parse_when).map(|w| match w {
                When::Date(d) => d.format("%Y-%m-%d").to_string(),
                When::DateTime(dt) => dt.date().format("%Y-%m-%d").to_string(),
            }),
            attendees: clip_list(self.attendees, SHORT),
            summary,
            topics: self
                .topics
                .into_iter()
                .map(|t| MinutesTopic { title: clip(&t.title, SHORT), notes: clip(&t.notes, LONG / 4) })
                .filter(|t| !t.title.is_empty())
                .take(MAX_ITEMS)
                .collect(),
            decisions: clip_list(self.decisions, SHORT * 2),
            action_items: self.action_items.into_iter().filter_map(ProposedTask::normalized).take(MAX_ITEMS).collect(),
            next_meeting: self.next_meeting.and_then(ProposedEvent::normalized),
        })
    }
}

impl MeetingMinutes {
    /// La minuta en Markdown (para IureEditor o un documento del caso).
    pub fn to_markdown(&self, spanish: bool) -> String {
        let l = |en: &'static str, es: &'static str| if spanish { es } else { en };
        let mut md = format!("# {}\n\n", if self.title.is_empty() { l("Minutes", "Minuta") } else { &self.title });
        if let Some(d) = &self.date {
            md.push_str(&format!("**{}:** {d}\n\n", l("Date", "Fecha")));
        }
        if !self.attendees.is_empty() {
            md.push_str(&format!("**{}:** {}\n\n", l("Attendees", "Asistentes"), self.attendees.join(", ")));
        }
        md.push_str(&format!("## {}\n\n{}\n\n", l("Summary", "Resumen"), self.summary));
        for t in &self.topics {
            md.push_str(&format!("### {}\n\n{}\n\n", t.title, t.notes));
        }
        if !self.decisions.is_empty() {
            md.push_str(&format!("## {}\n\n", l("Decisions", "Acuerdos")));
            for d in &self.decisions {
                md.push_str(&format!("- {d}\n"));
            }
            md.push('\n');
        }
        if !self.action_items.is_empty() {
            md.push_str(&format!("## {}\n\n", l("Action items", "Compromisos")));
            for a in &self.action_items {
                let who = a.assignee_hint.as_deref().map(|w| format!(" — {w}")).unwrap_or_default();
                let due = a.due_date.as_deref().map(|d| format!(" ({d})")).unwrap_or_default();
                md.push_str(&format!("- [ ] {}{who}{due}\n", a.title));
            }
            md.push('\n');
        }
        md
    }
}

// ---------------------------------------------------------------------------------------------
// Triage
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Importance {
    High,
    Low,
    #[serde(other)]
    Medium,
}

impl Importance {
    /// 0 (alta) … 2 (baja), para ordenar.
    pub fn rank(self) -> u8 {
        match self {
            Importance::High => 0,
            Importance::Medium => 1,
            Importance::Low => 2,
        }
    }
}

/// Un correo en el orden del día.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriageItem {
    #[serde(default)]
    pub id: String,
    #[serde(default = "medium_importance")]
    pub importance: Importance,
    #[serde(default)]
    pub reason: String,
}

fn medium_importance() -> Importance {
    Importance::Medium
}

/// Orden de los no leídos.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Triage {
    #[serde(default)]
    pub items: Vec<TriageItem>,
}

impl TaskOutput for Triage {
    fn validate(self, ctx: &ValidateCtx) -> Result<Self, String> {
        let known: HashSet<&str> = ctx.known_ids.iter().map(String::as_str).collect();
        let mut seen = HashSet::new();
        let mut items: Vec<TriageItem> = self
            .items
            .into_iter()
            .map(|i| TriageItem { id: i.id.trim().to_string(), importance: i.importance, reason: clip(&i.reason, 160) })
            .filter(|i| known.contains(i.id.as_str()) && seen.insert(i.id.clone()))
            .collect();
        if items.is_empty() && !ctx.known_ids.is_empty() {
            return Err("ningún id del triage coincide con los correos enviados".into());
        }
        // Orden estable por importancia (respeta el orden del modelo dentro de cada nivel).
        items.sort_by_key(|i| i.importance.rank());
        // Los que el modelo omitió van al final, sin razón inventada.
        for id in &ctx.known_ids {
            if !seen.contains(id) {
                items.push(TriageItem { id: id.clone(), importance: Importance::Low, reason: String::new() });
            }
        }
        Ok(Triage { items })
    }
}

// ---------------------------------------------------------------------------------------------
// Esquema de documento
// ---------------------------------------------------------------------------------------------

/// Esquema de documento en Markdown para IureEditor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentOutline {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub sections: Vec<String>,
    #[serde(default)]
    pub markdown: String,
}

impl TaskOutput for DocumentOutline {
    fn validate(self, _: &ValidateCtx) -> Result<Self, String> {
        let title = clip(&self.title, SHORT);
        let mut markdown = clip(&self.markdown, DOC);
        if markdown.is_empty() {
            return Err("esquema vacío".into());
        }
        if !markdown.starts_with("# ") {
            let t = if title.is_empty() { "Documento" } else { title.as_str() };
            markdown = format!("# {t}\n\n{markdown}");
        }
        let sections = if self.sections.is_empty() {
            markdown.lines().filter_map(|l| l.strip_prefix("## ")).map(|s| clip(s, SHORT)).take(MAX_ITEMS).collect()
        } else {
            clip_list(self.sections, SHORT)
        };
        Ok(DocumentOutline { title, sections, markdown })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn v<T: TaskOutput>(j: serde_json::Value, ctx: &ValidateCtx) -> Result<T, String> {
        serde_json::from_value::<T>(j).map_err(|e| e.to_string())?.validate(ctx)
    }

    #[test]
    fn draft_replies_need_three_and_are_ordered() {
        let ok: DraftReplies = v(
            json!({"suggestions": [
                {"kind": "detailed", "body": "Largo"}, {"kind": "short", "body": "Sí."}, {"kind": "neutral", "body": "Claro"}]}),
            &ValidateCtx::default(),
        )
        .unwrap();
        let kinds: Vec<_> = ok.suggestions.iter().map(|s| s.kind).collect();
        assert_eq!(kinds, [ReplyKind::Short, ReplyKind::Neutral, ReplyKind::Detailed]);

        // Reparación: sin etiquetas, por longitud.
        let fixed: DraftReplies = v(
            json!({"suggestions": [{"body": "mediana respuesta"}, {"kind": "x", "body": "ok"}, {"body": "una respuesta mucho más larga"}]}),
            &ValidateCtx::default(),
        )
        .unwrap();
        assert_eq!(fixed.suggestions[0].body, "ok");
        assert_eq!(fixed.suggestions[2].kind, ReplyKind::Detailed);

        assert!(
            v::<DraftReplies>(json!({"suggestions": [{"kind": "short", "body": "a"}]}), &ValidateCtx::default())
                .is_err()
        );
        assert!(v::<DraftReplies>(json!({"suggestions": "nope"}), &ValidateCtx::default()).is_err());
    }

    #[test]
    fn extraction_drops_bad_dates_and_unknown_enums_fall_back() {
        let e: Extraction = v(
            json!({
                "intent": "teleport", "summary": " Pide cita ",
                "events": [
                    {"title": "Audiencia", "start": "2026-10-27T16:00", "end": "2026-10-27T15:00"},
                    {"title": "Mal", "start": "el martes"},
                    {"title": "Entrega", "start": "2026-11-02"}],
                "tasks": [{"title": "Enviar contrato", "due_date": "mañana"}, {"title": ""}],
                "amounts": [{"text": "$12,000", "value": 12000, "currency": "mxn"}],
                "people": [{"name": "Ana", "email": "no-es-correo"}],
                "sentiment": "angry", "urgency": "HIGH"
            }),
            &ValidateCtx::default(),
        )
        .unwrap();
        assert_eq!(e.intent, Intent::Other);
        assert_eq!(e.summary, "Pide cita");
        assert_eq!(e.events.len(), 2);
        assert_eq!(e.events[0].end, None);
        assert!(!e.events[0].all_day && e.events[1].all_day);
        assert_eq!(e.tasks.len(), 1);
        assert_eq!(e.tasks[0].due_date, None);
        assert_eq!(e.amounts[0].currency.as_deref(), Some("MXN"));
        assert_eq!(e.people[0].email, None);
        assert_eq!(e.sentiment, Sentiment::Neutral);
        assert_eq!(e.urgency, Urgency::Normal);
    }

    #[test]
    fn classification_only_keeps_matching_payloads() {
        let c: LeadTicketClassification = v(
            json!({"category": "sales_opportunity", "confidence": 7.0, "reason": "quiere cotización",
                   "lead": {"name": "Ana Ruiz", "company_name": "Cliente SA", "email": "ana@cliente.mx"},
                   "ticket": {"title": "no debería estar"}}),
            &ValidateCtx::default(),
        )
        .unwrap();
        assert_eq!(c.confidence, 1.0);
        assert_eq!(c.lead.as_ref().unwrap().company_name.as_deref(), Some("Cliente SA"));
        assert!(c.ticket.is_none());
        let c: LeadTicketClassification =
            v(json!({"category": "support_request", "confidence": "alta"}), &ValidateCtx::default()).unwrap_or_else(
                |_| LeadTicketClassification {
                    category: LeadOrTicket::Neither,
                    confidence: 0.0,
                    reason: String::new(),
                    lead: None,
                    ticket: None,
                },
            );
        assert!(c.ticket.is_none());
    }

    #[test]
    fn triage_keeps_only_known_ids_and_appends_missing() {
        let ctx = ValidateCtx { known_ids: vec!["a".into(), "b".into(), "c".into()] };
        let t: Triage = v(
            json!({"items": [
                {"id": "b", "importance": "low", "reason": "boletín"},
                {"id": "zz", "importance": "high", "reason": "inventado"},
                {"id": "a", "importance": "high", "reason": "plazo hoy"},
                {"id": "a", "importance": "low", "reason": "repetido"}]}),
            &ctx,
        )
        .unwrap();
        let ids: Vec<_> = t.items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["a", "b", "c"]);
        assert_eq!(t.items[0].reason, "plazo hoy");
        assert!(v::<Triage>(json!({"items": [{"id": "zz"}]}), &ctx).is_err());
    }

    #[test]
    fn outline_gets_a_title_and_sections() {
        let o: DocumentOutline =
            v(json!({"title": "Contrato", "markdown": "## Antecedentes\n- x\n## Pendientes"}), &ValidateCtx::default())
                .unwrap();
        assert!(o.markdown.starts_with("# Contrato\n"));
        assert_eq!(o.sections, ["Antecedentes", "Pendientes"]);
        assert!(v::<DocumentOutline>(json!({"title": "x"}), &ValidateCtx::default()).is_err());
    }

    #[test]
    fn minutes_markdown() {
        let m: MeetingMinutes = v(
            json!({"title": "Revisión", "date": "2026-10-26T10:00", "attendees": ["Ana", "Luis"], "summary": "Se revisó.",
                   "decisions": ["Firmar"], "action_items": [{"title": "Enviar borrador", "assignee_hint": "Ana", "due_date": "2026-10-30"}]}),
            &ValidateCtx::default(),
        )
        .unwrap();
        assert_eq!(m.date.as_deref(), Some("2026-10-26"));
        let md = m.to_markdown(true);
        assert!(md.starts_with("# Revisión\n"));
        assert!(md.contains("- [ ] Enviar borrador — Ana (2026-10-30)"));
    }
}
