//! Iurefficient desde IureDesk (F3.1): lo que se consulta y se hace en la instancia, sin estado
//! de la sesión ni egui. Todo pasa por `iurefficient-connect` (`connect::pim`, `connect::api`).
//!
//! - [`lookup`]: contexto de un correo (personas y leads del CRM por remitente y dominio,
//!   oportunidades, clientes, casos por número en el asunto, tickets del solicitante y actividades
//!   recientes) para el panel contextual.
//! - [`search_targets`] y [`link`]: «Vincular a…» con lo que la instancia permite **hoy**
//!   (`docs/iurefficient-api.md` §3.1): actividad `email` en el lead u oportunidad, documento
//!   `.txt` en el caso o cliente (`.eml` no se admite), comentario **interno** en el ticket.
//! - [`create_lead`], [`create_ticket`], [`create_task`], [`reply_address`]: acciones del panel.
//! - [`fetch_feed`], [`fetch_agenda`]: avisos/menciones y agenda/tareas propias.
//!
//! Los datos de la instancia son hostiles: textos acotados, listas con tope y tiempo máximo por
//! llamada. Una sección que falla con un error del servidor (p. ej. plugin no instalado: 404) se
//! deja vacía; sin red, sesión caducada o términos sin aceptar cortan la consulta.
//!
//! [`sample`] da las mismas respuestas sin red para el buzón de ejemplo y las capturas.

use std::future::Future;
use std::time::Duration;

use chrono::{DateTime, NaiveDate, Utc};
use iuredesk_store::EmailLink;
use iurefficient_connect::api::{self, CrmKind, UploadOptions};
use iurefficient_connect::pim;
use iurefficient_connect::rest::{ApiError, Session};

use crate::lang::Text;

/// Tiempo máximo de cada llamada a la instancia.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(15);
/// Texto máximo del correo que sale hacia la instancia (actividad, documento, comentario).
pub const MAX_EMAIL_TEXT: usize = 20_000;
/// Elementos máximos por sección de la tarjeta.
const MAX_ITEMS: usize = 5;

/// Dominios de correo gratuito: no identifican a una organización.
const FREE_MAIL: &[&str] = &[
    "gmail.com",
    "googlemail.com",
    "hotmail.com",
    "outlook.com",
    "live.com",
    "yahoo.com",
    "yahoo.com.mx",
    "icloud.com",
    "me.com",
    "aol.com",
    "proton.me",
    "protonmail.com",
    "gmx.com",
    "msn.com",
    "prodigy.net.mx",
];

/// Estado de la conexión con Iurefficient tal como lo ve la interfaz.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum IureStatus {
    /// Iurefficient no se usa en esta sesión (pruebas, sin arrancar).
    #[default]
    Disabled,
    /// Restaurando la sesión compartida de la suite.
    Connecting,
    /// Ninguna app de la suite inició sesión: hay que iniciarla.
    NotConfigured,
    /// Lista.
    Ready { host: String, email: String, name: Option<String> },
    /// La sesión caducó: volver a iniciar sesión.
    Expired { host: String, email: String },
    /// Sin conexión con la instancia (se reintenta).
    Offline { host: String, email: String, reason: String },
    /// Hay que aceptar los términos vigentes en la web.
    TermsRequired { host: String, email: String },
}

impl IureStatus {
    /// `true` si se puede consultar la instancia.
    pub fn is_ready(&self) -> bool {
        matches!(self, IureStatus::Ready { .. })
    }

    /// Instancia (dominio) si se conoce.
    pub fn host(&self) -> Option<&str> {
        match self {
            IureStatus::Ready { host, .. }
            | IureStatus::Expired { host, .. }
            | IureStatus::Offline { host, .. }
            | IureStatus::TermsRequired { host, .. } => Some(host),
            _ => None,
        }
    }

    /// Correo del usuario si se conoce.
    pub fn email(&self) -> Option<&str> {
        match self {
            IureStatus::Ready { email, .. }
            | IureStatus::Expired { email, .. }
            | IureStatus::Offline { email, .. }
            | IureStatus::TermsRequired { email, .. } => Some(email),
            _ => None,
        }
    }

    /// Texto del estado en lenguaje humano.
    pub fn label(&self) -> Text {
        match self {
            IureStatus::Disabled => Text::new("Not in use", "Sin usar"),
            IureStatus::Connecting => Text::new("Connecting…", "Conectando…"),
            IureStatus::NotConfigured => Text::new(
                "Not signed in. Sign in once and every Iurefficient app on this computer uses it.",
                "Sin iniciar sesión. Inicia sesión una vez y todas las apps de Iurefficient de este equipo la usan.",
            ),
            IureStatus::Ready { host, email, name } => {
                let who = name.clone().unwrap_or_else(|| email.clone());
                Text::new(format!("Connected as {who} · {host}"), format!("Conectado como {who} · {host}"))
            }
            IureStatus::Expired { host, .. } => Text::new(
                format!("The session on {host} expired. Sign in again."),
                format!("La sesión en {host} caducó. Vuelve a iniciar sesión."),
            ),
            IureStatus::Offline { host, .. } => Text::new(
                format!("Could not reach {host}. Check your internet; it will retry by itself."),
                format!("No se pudo conectar con {host}. Revisa tu internet; se reintentará solo."),
            ),
            IureStatus::TermsRequired { host, .. } => Text::new(
                format!(
                    "{host} asks you to accept its updated terms. Open Iurefficient in your browser, accept them and come back."
                ),
                format!(
                    "{host} te pide aceptar sus términos actualizados. Abre Iurefficient en el navegador, acéptalos y vuelve."
                ),
            ),
        }
    }
}

/// Por qué falló una llamada.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IureError {
    /// Sin red o la instancia no responde.
    Offline(String),
    /// La sesión ya no vale.
    Expired,
    /// Términos sin aceptar (403 `terms_acceptance_required`).
    Terms,
    /// El servidor respondió con un error (permiso, dato no válido, plugin ausente…).
    Server(Text),
}

impl IureError {
    /// Mensaje para el usuario.
    pub fn text(&self) -> Text {
        match self {
            IureError::Offline(_) => Text::new(
                "Iurefficient could not be reached. Check your internet and try again.",
                "No se pudo conectar con Iurefficient. Revisa tu internet e inténtalo de nuevo.",
            ),
            IureError::Expired => Text::new(
                "Your Iurefficient session expired. Sign in again in Settings → Iurefficient.",
                "Tu sesión de Iurefficient caducó. Vuelve a iniciarla en Ajustes → Iurefficient.",
            ),
            IureError::Terms => Text::new(
                "Iurefficient asks you to accept its updated terms. Open it in your browser and accept them.",
                "Iurefficient te pide aceptar sus términos actualizados. Ábrelo en el navegador y acéptalos.",
            ),
            IureError::Server(t) => Text::new(
                format!("Iurefficient could not do it: {}", t.en),
                format!("Iurefficient no pudo hacerlo: {}", t.es),
            ),
        }
    }

    /// `true` si cambia el estado de la conexión (y no solo esta acción).
    pub fn affects_status(&self) -> bool {
        !matches!(self, IureError::Server(_))
    }
}

/// Clasifica un error de `connect`.
pub fn classify(e: &anyhow::Error) -> IureError {
    if iuredesk_accounts::iurefficient::is_terms(e) {
        return IureError::Terms;
    }
    for c in e.chain() {
        if let Some(a) = c.downcast_ref::<ApiError>() {
            if a.status == 401 {
                return IureError::Expired;
            }
            let m = clip(&a.message, 300);
            return IureError::Server(Text::new(m.clone(), m));
        }
        if let Some(r) = c.downcast_ref::<reqwest::Error>()
            && (r.is_connect() || r.is_timeout() || r.is_request())
        {
            return IureError::Offline(clip(&r.to_string(), 300));
        }
    }
    let m = clip(&format!("{e:#}"), 300);
    if m.contains("sesión caducó") || m.contains("session expired") || m.contains("Sesión no válida") {
        return IureError::Expired;
    }
    IureError::Server(Text::new(m.clone(), m))
}

/// Ejecuta una llamada con tiempo máximo y clasifica su error.
pub async fn call<T>(f: impl Future<Output = anyhow::Result<T>>) -> Result<T, IureError> {
    match tokio::time::timeout(CALL_TIMEOUT, f).await {
        Err(_) => Err(IureError::Offline("tiempo de espera agotado".into())),
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(classify(&e)),
    }
}

/// Como [`call`], pero un error del servidor deja la sección vacía (`None`).
async fn soft<T>(f: impl Future<Output = anyhow::Result<T>>) -> Result<Option<T>, IureError> {
    match call(f).await {
        Ok(v) => Ok(Some(v)),
        Err(IureError::Server(_)) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Recorta a `max` caracteres (no bytes) y quita espacios de los extremos.
pub fn clip(s: &str, max: usize) -> String {
    let t = s.trim();
    if t.chars().count() <= max {
        return t.to_string();
    }
    let mut out: String = t.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

// ---------------------------------------------------------------------------------------------
// Contexto de un correo

/// Lo que se busca para un correo.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContextQuery {
    /// Dirección del remitente (o del otro participante si el correo es mío).
    pub email: String,
    /// Nombre visible.
    pub name: String,
    /// Organización del contacto local, si la hay.
    pub org: Option<String>,
    /// Asunto.
    pub subject: String,
    /// Números de caso del asunto («241/2026»).
    pub case_refs: Vec<String>,
}

impl ContextQuery {
    /// Clave de caché.
    pub fn key(&self) -> String {
        format!("{}|{}", self.email.to_lowercase(), self.case_refs.join(","))
    }

    /// Dominio del remitente si identifica a una organización (no correo gratuito).
    pub fn org_domain(&self) -> Option<String> {
        let d = self.email.rsplit_once('@')?.1.trim().to_lowercase();
        if d.is_empty() || !d.contains('.') || FREE_MAIL.contains(&d.as_str()) {
            return None;
        }
        Some(d)
    }
}

/// Tipo de persona.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PersonKind {
    /// Lead del CRM.
    Lead,
    /// Cliente.
    Client,
}

/// Persona o cliente de la instancia.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersonCard {
    pub kind: PersonKind,
    pub id: String,
    pub name: String,
    /// Puesto · empresa.
    pub subtitle: String,
    pub email: String,
    pub phone: Option<String>,
}

/// Lead u oportunidad.
#[derive(Clone, Debug, PartialEq)]
pub struct DealCard {
    pub kind: TargetKind,
    pub id: String,
    pub name: String,
    pub organization: Option<String>,
    pub stage: Option<String>,
    pub status: Option<String>,
    pub amount: Option<f64>,
}

/// Caso (expediente).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaseCard {
    pub id: String,
    pub title: String,
    pub subtitle: Option<String>,
}

/// Ticket.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TicketCard {
    pub id: String,
    pub number: String,
    pub title: String,
    /// open | in_progress | waiting | resolved | closed
    pub status: String,
    pub priority: String,
}

/// Actividad reciente del CRM.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActivityCard {
    /// call | meeting | email | whatsapp | sms | note | task
    pub kind: String,
    pub subject: String,
    /// Fecha ISO (`completed_at`, `scheduled_at` o `created_at`).
    pub when: Option<String>,
}

/// Lo que la instancia sabe de un correo.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct IureContext {
    pub people: Vec<PersonCard>,
    pub deals: Vec<DealCard>,
    pub cases: Vec<CaseCard>,
    pub tickets: Vec<TicketCard>,
    pub activities: Vec<ActivityCard>,
}

impl IureContext {
    /// `true` si la instancia no sabe nada.
    pub fn is_empty(&self) -> bool {
        self.people.is_empty()
            && self.deals.is_empty()
            && self.cases.is_empty()
            && self.tickets.is_empty()
            && self.activities.is_empty()
    }
}

fn person_from_lead(p: &pim::CrmPerson) -> PersonCard {
    let company = p.organization_name.clone().or_else(|| p.company_name.clone()).unwrap_or_default();
    let subtitle = [p.position.clone().unwrap_or_default(), company]
        .into_iter()
        .filter(|s| !s.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    PersonCard {
        kind: PersonKind::Lead,
        id: p.id.clone(),
        name: clip(if p.name.trim().is_empty() { p.company_name.as_deref().unwrap_or("") } else { &p.name }, 120),
        subtitle: clip(&subtitle, 160),
        email: p.emails.first().map(|e| clip(e, 200)).unwrap_or_default(),
        phone: p.phones.first().map(|x| clip(x, 60)),
    }
}

fn deal_from_lead(p: &pim::CrmPerson) -> DealCard {
    DealCard {
        kind: TargetKind::Lead,
        id: p.id.clone(),
        name: clip(if p.name.trim().is_empty() { p.company_name.as_deref().unwrap_or("") } else { &p.name }, 120),
        organization: p.organization_name.clone().or_else(|| p.company_name.clone()).map(|s| clip(&s, 120)),
        stage: None,
        status: Some(clip(&p.status, 40)).filter(|s| !s.is_empty()),
        amount: None,
    }
}

fn deal_from_item(i: &api::CrmItem) -> DealCard {
    DealCard {
        kind: if i.kind == CrmKind::Lead { TargetKind::Lead } else { TargetKind::Opportunity },
        id: i.id.clone(),
        name: clip(&i.name, 120),
        organization: i.organization.as_deref().map(|s| clip(s, 120)),
        stage: i.stage.as_deref().map(|s| clip(s, 60)),
        status: i.status.as_deref().map(|s| clip(s, 40)),
        amount: i.amount.filter(|a| a.is_finite()),
    }
}

fn activity_from(a: &pim::CrmActivity) -> ActivityCard {
    ActivityCard {
        kind: clip(&a.activity_type, 20),
        subject: clip(a.subject.as_deref().or(a.description.as_deref()).unwrap_or(""), 140),
        when: a.completed_at.clone().or_else(|| a.scheduled_at.clone()).or_else(|| a.created_at.clone()),
    }
}

/// Consulta el contexto de un correo.
pub async fn lookup(sess: &Session, q: &ContextQuery) -> Result<IureContext, IureError> {
    let email = q.email.trim().to_lowercase();
    let mut ctx = IureContext::default();
    if email.is_empty() && q.case_refs.is_empty() {
        return Ok(ctx);
    }
    let tq = pim::TicketQuery { search: Some(email.clone()), page: 1, per_page: 10, ..Default::default() };
    let (people, hits, tickets) = tokio::join!(
        soft(pim::crm_people(sess, Some(&email), 10, 0)),
        soft(pim::search(sess, &email)),
        soft(pim::tickets(sess, &tq)),
    );
    // Leads con esa dirección exacta; si no hay, los del mismo dominio de empresa.
    let mut leads: Vec<pim::CrmPerson> = people?
        .map(|p| p.items)
        .unwrap_or_default()
        .into_iter()
        .filter(|p| p.emails.iter().any(|e| e.trim().eq_ignore_ascii_case(&email)))
        .take(MAX_ITEMS)
        .collect();
    if leads.is_empty()
        && let Some(d) = q.org_domain()
    {
        let suffix = format!("@{d}");
        leads = soft(pim::crm_people(sess, Some(&d), 10, 0))
            .await?
            .map(|p| p.items)
            .unwrap_or_default()
            .into_iter()
            .filter(|p| p.emails.iter().any(|e| e.trim().to_lowercase().ends_with(&suffix)))
            .take(3)
            .collect();
    }
    for l in &leads {
        if l.emails.iter().any(|e| e.trim().eq_ignore_ascii_case(&email)) {
            ctx.people.push(person_from_lead(l));
        }
        ctx.deals.push(deal_from_lead(l));
    }
    for h in hits?.unwrap_or_default().into_iter().take(40) {
        match h.kind.as_str() {
            "client" if ctx.people.len() < MAX_ITEMS => ctx.people.push(PersonCard {
                kind: PersonKind::Client,
                id: h.id.clone(),
                name: clip(&h.title, 120),
                subtitle: String::new(),
                email: h.subtitle.as_deref().map(|s| clip(s, 200)).unwrap_or_default(),
                phone: None,
            }),
            "case" if ctx.cases.len() < MAX_ITEMS => {
                ctx.cases.push(CaseCard { id: h.id, title: clip(&h.title, 160), subtitle: h.subtitle })
            }
            _ => {}
        }
    }
    for t in tickets?.map(|p| p.items).unwrap_or_default() {
        if ctx.tickets.len() >= MAX_ITEMS {
            break;
        }
        if t.requester_email.as_deref().is_some_and(|r| r.trim().eq_ignore_ascii_case(&email)) {
            ctx.tickets.push(TicketCard {
                id: t.id,
                number: clip(&t.ticket_number, 40),
                title: clip(&t.title, 160),
                status: clip(&t.status, 30),
                priority: clip(&t.priority, 30),
            });
        }
    }
    // Oportunidades abiertas de la organización del lead (o del contacto local).
    let org = leads
        .iter()
        .find_map(|l| l.organization_name.clone().or_else(|| l.company_name.clone()))
        .or_else(|| q.org.clone())
        .filter(|o| !o.trim().is_empty());
    if let Some(org) = org {
        let opps = soft(api::crm_list(sess, CrmKind::Opportunity, Some(&org), MAX_ITEMS as u32)).await?;
        ctx.deals.extend(opps.unwrap_or_default().iter().take(MAX_ITEMS).map(deal_from_item));
    }
    // Casos citados en el asunto.
    for r in q.case_refs.iter().take(3) {
        for h in soft(pim::search(sess, r)).await?.unwrap_or_default().into_iter().filter(|h| h.kind == "case") {
            if ctx.cases.len() < MAX_ITEMS && !ctx.cases.iter().any(|c| c.id == h.id) {
                ctx.cases.push(CaseCard { id: h.id, title: clip(&h.title, 160), subtitle: h.subtitle });
            }
        }
    }
    // Actividades recientes del primer lead y de la primera oportunidad.
    let firsts: Vec<(CrmKind, String)> = [
        ctx.deals.iter().find(|d| d.kind == TargetKind::Lead).map(|d| (CrmKind::Lead, d.id.clone())),
        ctx.deals.iter().find(|d| d.kind == TargetKind::Opportunity).map(|d| (CrmKind::Opportunity, d.id.clone())),
    ]
    .into_iter()
    .flatten()
    .collect();
    for (kind, id) in firsts {
        let acts = soft(pim::crm_activities(sess, kind, &id, MAX_ITEMS as u32)).await?.unwrap_or_default();
        ctx.activities.extend(acts.iter().map(activity_from));
    }
    ctx.activities.sort_by(|a, b| b.when.cmp(&a.when));
    ctx.activities.truncate(MAX_ITEMS);
    ctx.deals.truncate(MAX_ITEMS * 2);
    Ok(ctx)
}

// ---------------------------------------------------------------------------------------------
// Vincular

/// Tipo de entidad a la que se vincula un correo.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TargetKind {
    Case,
    Client,
    Lead,
    Opportunity,
    Ticket,
    Task,
}

impl TargetKind {
    /// Clave estable (la de [`EmailLink::kind`]).
    pub fn key(self) -> &'static str {
        match self {
            TargetKind::Case => "case",
            TargetKind::Client => "client",
            TargetKind::Lead => "lead",
            TargetKind::Opportunity => "opportunity",
            TargetKind::Ticket => "ticket",
            TargetKind::Task => "task",
        }
    }

    /// De la clave.
    pub fn from_key(k: &str) -> Option<TargetKind> {
        Some(match k {
            "case" => TargetKind::Case,
            "client" => TargetKind::Client,
            "lead" => TargetKind::Lead,
            "opportunity" => TargetKind::Opportunity,
            "ticket" => TargetKind::Ticket,
            "task" => TargetKind::Task,
            _ => return None,
        })
    }

    /// Nombre para la interfaz.
    pub fn label(self) -> Text {
        match self {
            TargetKind::Case => Text::new("Case", "Caso"),
            TargetKind::Client => Text::new("Client", "Cliente"),
            TargetKind::Lead => Text::new("Lead", "Lead"),
            TargetKind::Opportunity => Text::new("Opportunity", "Oportunidad"),
            TargetKind::Ticket => Text::new("Ticket", "Ticket"),
            TargetKind::Task => Text::new("Task", "Tarea"),
        }
    }

    /// Qué hará «Vincular» con este destino (se muestra antes de confirmar).
    pub fn link_effect(self) -> Text {
        match self {
            TargetKind::Case => Text::new(
                "Saves a copy of the email (text) as a document of the case.",
                "Guarda una copia del correo (texto) como documento del caso.",
            ),
            TargetKind::Client => Text::new(
                "Saves a copy of the email (text) as a document of the client.",
                "Guarda una copia del correo (texto) como documento del cliente.",
            ),
            TargetKind::Lead | TargetKind::Opportunity => Text::new(
                "Logs an «email» activity with the summary of the message.",
                "Registra una actividad «correo» con el resumen del mensaje.",
            ),
            TargetKind::Ticket => {
                Text::new("Adds the email to the ticket as a comment.", "Agrega el correo al ticket como comentario.")
            }
            TargetKind::Task => Text::new("Links the email to the task.", "Vincula el correo con la tarea."),
        }
    }
}

/// Un destino posible de «Vincular a…».
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkTarget {
    pub kind: TargetKind,
    pub id: String,
    pub label: String,
    pub detail: Option<String>,
}

/// Busca casos, clientes, leads, oportunidades y tickets para el selector.
pub async fn search_targets(sess: &Session, q: &str) -> Result<Vec<LinkTarget>, IureError> {
    let q = q.trim();
    if q.chars().count() < 2 {
        return Ok(Vec::new());
    }
    let tq = pim::TicketQuery { search: Some(q.to_string()), page: 1, per_page: 8, ..Default::default() };
    let (hits, leads, opps, tickets) = tokio::join!(
        soft(pim::search(sess, q)),
        soft(api::crm_list(sess, CrmKind::Lead, Some(q), 8)),
        soft(api::crm_list(sess, CrmKind::Opportunity, Some(q), 8)),
        soft(pim::tickets(sess, &tq)),
    );
    let mut out = Vec::new();
    for h in hits?.unwrap_or_default() {
        let kind = match h.kind.as_str() {
            "case" => TargetKind::Case,
            "client" => TargetKind::Client,
            _ => continue,
        };
        out.push(LinkTarget { kind, id: h.id, label: clip(&h.title, 160), detail: h.subtitle.map(|s| clip(&s, 160)) });
    }
    for i in opps?.unwrap_or_default().iter().chain(leads?.unwrap_or_default().iter()) {
        let d = deal_from_item(i);
        let detail = [d.organization.clone(), d.stage.clone()].into_iter().flatten().collect::<Vec<_>>().join(" · ");
        out.push(LinkTarget { kind: d.kind, id: d.id, label: d.name, detail: Some(detail).filter(|s| !s.is_empty()) });
    }
    for t in tickets?.map(|p| p.items).unwrap_or_default() {
        out.push(LinkTarget {
            kind: TargetKind::Ticket,
            label: clip(&format!("{} · {}", t.ticket_number, t.title), 160),
            id: t.id,
            detail: t.requester_email.map(|s| clip(&s, 160)),
        });
    }
    out.truncate(40);
    Ok(out)
}

/// El correo que se vincula, ya en texto.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EmailDigest {
    pub message_id: String,
    pub from: String,
    pub to: String,
    pub date: DateTime<Utc>,
    pub subject: String,
    /// Texto del cuerpo (acotado a [`MAX_EMAIL_TEXT`]).
    pub text: String,
}

impl EmailDigest {
    /// Cabeceras en texto.
    pub fn headers(&self) -> String {
        format!(
            "De / From: {}\nPara / To: {}\nFecha / Date: {}\nAsunto / Subject: {}\nMessage-ID: <{}>",
            self.from,
            self.to,
            self.date.format("%Y-%m-%d %H:%M UTC"),
            self.subject,
            self.message_id
        )
    }

    /// Copia del correo en texto (lo que se sube como documento).
    pub fn as_text(&self) -> String {
        format!("{}\n\n{}\n", self.headers(), clip(&self.text, MAX_EMAIL_TEXT))
    }

    /// Resumen corto (actividad, comentario).
    pub fn summary(&self, max: usize) -> String {
        format!("{}\n\n{}", self.headers(), clip(&self.text, max))
    }

    /// Nombre del archivo `.txt` (sin caracteres raros).
    pub fn file_name(&self) -> String {
        let base: String = self
            .subject
            .chars()
            .map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' { c } else { '_' })
            .take(80)
            .collect();
        let base = base.trim();
        format!("{} {}.txt", self.date.format("%Y-%m-%d"), if base.is_empty() { "correo" } else { base })
    }
}

/// Opciones de «Vincular».
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LinkOptions {
    /// Ticket: comentario público (la instancia lo reenvía por correo al solicitante).
    pub ticket_public: bool,
    /// Lead, oportunidad o ticket: también subir la copia en texto.
    pub attach_copy: bool,
}

fn upload_opts(e: &EmailDigest) -> UploadOptions {
    UploadOptions {
        title: Some(clip(&format!("Correo: {}", e.subject), 200)),
        document_type: Some("other".into()),
        description: Some(clip(&e.headers(), 900)),
        tags: vec!["email".into()],
        ..Default::default()
    }
}

/// Vincula el correo con un destino haciendo lo que la instancia admite hoy (§3.1). Devuelve los
/// vínculos que hay que recordar localmente.
pub async fn link(
    sess: &Session,
    target: &LinkTarget,
    e: &EmailDigest,
    opts: LinkOptions,
    now: DateTime<Utc>,
) -> Result<Vec<EmailLink>, IureError> {
    let mk = |how: &str, remote: Option<String>| EmailLink {
        kind: target.kind.key().into(),
        id: target.id.clone(),
        label: target.label.clone(),
        how: how.into(),
        remote_ref: remote,
        created: now,
    };
    let mut out = Vec::new();
    match target.kind {
        TargetKind::Case | TargetKind::Client => {
            let mut o = upload_opts(e);
            if target.kind == TargetKind::Case {
                o.case_id = Some(target.id.clone());
            } else {
                o.client_id = Some(target.id.clone());
            }
            let doc = call(api::upload_document_bytes(sess, e.as_text().into_bytes(), &e.file_name(), &o)).await?;
            out.push(mk("document", Some(doc.id)));
        }
        TargetKind::Lead | TargetKind::Opportunity => {
            let kind = if target.kind == TargetKind::Lead { CrmKind::Lead } else { CrmKind::Opportunity };
            let subject = clip(&e.subject, 200);
            let desc = e.summary(4_000);
            let when = e.date.to_rfc3339();
            let id = call(api::crm_activity(sess, kind, &target.id, "email", &subject, Some(&desc), None, Some(&when)))
                .await?;
            out.push(mk("activity", Some(id).filter(|i| !i.is_empty())));
            if opts.attach_copy {
                let doc = call(api::crm_attach_bytes(sess, kind, &target.id, e.as_text().into_bytes(), &e.file_name()))
                    .await?;
                out.push(mk("document", Some(doc.id)));
            }
        }
        TargetKind::Ticket => {
            let c = call(pim::ticket_comment(sess, &target.id, &e.summary(6_000), !opts.ticket_public)).await?;
            out.push(mk("comment", Some(c.id).filter(|i| !i.is_empty())));
            if opts.attach_copy {
                let doc =
                    call(api::upload_document_bytes(sess, e.as_text().into_bytes(), &e.file_name(), &upload_opts(e)))
                        .await?;
                call(pim::ticket_attach_document(sess, &target.id, &doc.id, Some("Correo"))).await?;
                out.push(mk("document", Some(doc.id)));
            }
        }
        TargetKind::Task => out.push(mk("created", None)),
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Crear desde el correo

/// Datos editables de un lead nuevo.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LeadForm {
    pub name: String,
    pub company: String,
    pub email: String,
    pub phone: String,
    pub position: String,
    pub notes: String,
}

impl LeadForm {
    fn from_person(p: &pim::CrmPerson) -> LeadForm {
        LeadForm {
            name: clip(&p.name, 200),
            company: clip(p.company_name.as_deref().or(p.organization_name.as_deref()).unwrap_or(""), 200),
            email: p.emails.first().map(|e| clip(e, 200)).unwrap_or_default(),
            phone: p.phones.first().map(|e| clip(e, 60)).unwrap_or_default(),
            position: clip(p.position.as_deref().unwrap_or(""), 120),
            notes: String::new(),
        }
    }
}

/// Propuestas de lead para el correo (`/extract` con `persist:false`): la IA propone y el usuario
/// confirma. Si la IA no da nada, el remitente.
pub async fn suggest_leads(sess: &Session, e: &EmailDigest, sender: &LeadForm) -> Result<Vec<LeadForm>, IureError> {
    let text = clip(&format!("{}\n\n{}", e.headers(), e.text), 8_000);
    let mut out: Vec<LeadForm> = match call(pim::crm_extract_leads(sess, &text)).await {
        Ok(v) => v.iter().take(5).map(LeadForm::from_person).collect(),
        // Sin IA (cuota, plugin): basta con el remitente.
        Err(IureError::Server(_)) => Vec::new(),
        Err(err) => return Err(err),
    };
    for f in &mut out {
        if f.notes.is_empty() {
            f.notes = clip(&format!("Desde el correo «{}»", e.subject), 300);
        }
    }
    if !out.iter().any(|f| f.email.eq_ignore_ascii_case(&sender.email)) {
        out.push(sender.clone());
    }
    Ok(out)
}

/// Crea el lead y registra el correo como su primera actividad.
pub async fn create_lead(
    sess: &Session,
    f: &LeadForm,
    e: &EmailDigest,
    now: DateTime<Utc>,
) -> Result<Vec<EmailLink>, IureError> {
    let some = |s: &str| Some(clip(s, 300)).filter(|x| !x.is_empty());
    let lead = call(pim::crm_create_lead(
        sess,
        &pim::NewLead {
            name: clip(&f.name, 200),
            company_name: some(&f.company),
            email: some(&f.email),
            phone: some(&f.phone),
            position: some(&f.position),
            notes: some(&f.notes),
            ..Default::default()
        },
    ))
    .await?;
    let target = LinkTarget { kind: TargetKind::Lead, id: lead.id.clone(), label: clip(&f.name, 200), detail: None };
    let mut links = vec![EmailLink {
        kind: "lead".into(),
        id: lead.id,
        label: target.label.clone(),
        how: "created".into(),
        remote_ref: None,
        created: now,
    }];
    // La actividad es un extra: si falla, el lead ya existe.
    if let Ok(mut more) = link(sess, &target, e, LinkOptions::default(), now).await {
        links.append(&mut more);
    }
    Ok(links)
}

/// Datos de un ticket nuevo.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TicketForm {
    pub title: String,
    pub description: String,
    /// low | medium | high | urgent
    pub priority: String,
    pub requester_name: String,
    pub requester_email: String,
}

/// Crea un ticket desde el correo.
pub async fn create_ticket(sess: &Session, f: &TicketForm, now: DateTime<Utc>) -> Result<Vec<EmailLink>, IureError> {
    let some = |s: &str| Some(clip(s, 300)).filter(|x| !x.is_empty());
    let t = call(pim::create_ticket(
        sess,
        &pim::NewTicket {
            title: clip(&f.title, 200),
            description: Some(clip(&f.description, MAX_EMAIL_TEXT)).filter(|x| !x.is_empty()),
            priority: some(&f.priority),
            requester_name: some(&f.requester_name),
            requester_email: some(&f.requester_email),
            tags: vec!["email".into()],
            ..Default::default()
        },
    ))
    .await?;
    let label =
        if t.ticket_number.is_empty() { clip(&f.title, 200) } else { format!("{} · {}", t.ticket_number, f.title) };
    Ok(vec![EmailLink {
        kind: "ticket".into(),
        id: t.id,
        label: clip(&label, 200),
        how: "created".into(),
        remote_ref: None,
        created: now,
    }])
}

/// Datos de una tarea nueva.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TaskForm {
    pub title: String,
    pub notes: String,
    pub due: Option<NaiveDate>,
    /// Caso al que pertenece (vacío = tarea personal).
    pub case_id: Option<String>,
}

/// Crea una tarea (personal o del caso) desde el correo.
pub async fn create_task(sess: &Session, f: &TaskForm, now: DateTime<Utc>) -> Result<Vec<EmailLink>, IureError> {
    let t = call(pim::create_task(
        sess,
        &pim::TaskInput {
            title: Some(clip(&f.title, 200)),
            description: Some(clip(&f.notes, MAX_EMAIL_TEXT)).filter(|x| !x.is_empty()),
            case_id: f.case_id.clone().filter(|c| !c.is_empty()),
            due_date: f.due.map(|d| d.format("%Y-%m-%d").to_string()),
            ..Default::default()
        },
    ))
    .await?;
    Ok(vec![EmailLink {
        kind: "task".into(),
        id: t.id,
        label: clip(&f.title, 200),
        how: "created".into(),
        remote_ref: None,
        created: now,
    }])
}

/// Dirección de respuesta de una oportunidad (`Reply-To` + `Bcc` del buzón de ventas).
pub async fn reply_address(sess: &Session, opportunity: &str) -> Result<Option<pim::CrmReplyAddress>, IureError> {
    call(pim::crm_reply_address(sess, opportunity)).await
}

// ---------------------------------------------------------------------------------------------
// Avisos y menciones

/// Un aviso o una mención.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeedItem {
    /// `true` = mención (`/mentions`), `false` = aviso (`/notifications`).
    pub mention: bool,
    pub id: String,
    pub title: String,
    pub message: Option<String>,
    /// Ruta en la web (`/cases/…`), si la hay.
    pub link: Option<String>,
    pub read: bool,
    pub created_at: Option<String>,
}

/// Avisos y menciones.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Feed {
    pub items: Vec<FeedItem>,
    pub unread: u64,
}

/// Avisos (los 30 últimos) y menciones sin leer.
pub async fn fetch_feed(sess: &Session) -> Result<Feed, IureError> {
    let (notes, unread) = call(pim::notifications(sess, false, 30)).await?;
    // El plugin de colaboración puede no estar: sin menciones.
    let mentions = soft(pim::mentions(sess, true, 20)).await?.unwrap_or_default();
    let mut items: Vec<FeedItem> = mentions
        .iter()
        .map(|m| FeedItem {
            mention: true,
            id: m.id.clone(),
            title: clip(
                &format!(
                    "{} {}",
                    m.mentioned_by_name.as_deref().unwrap_or("?"),
                    crate::lang::pick("mentioned you", "te mencionó")
                ),
                160,
            ),
            message: Some(clip(&format!("{} {}", m.target_type, m.target_id), 160)),
            link: None,
            read: m.is_read,
            created_at: m.created_at.clone(),
        })
        .collect();
    items.extend(notes.iter().map(|n| FeedItem {
        mention: false,
        id: n.id.clone(),
        title: clip(&n.title, 160),
        message: n.message.as_deref().map(|m| clip(m, 300)),
        link: n.link.clone().filter(|l| l.starts_with('/') || l.starts_with("https://")),
        read: n.is_read,
        created_at: n.created_at.clone(),
    }));
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    items.truncate(50);
    let unread_mentions = items.iter().filter(|i| i.mention && !i.read).count() as u64;
    Ok(Feed { items, unread: unread.min(9_999).saturating_add(unread_mentions) })
}

/// Marca leído un aviso o una mención.
pub async fn mark_read(sess: &Session, mention: bool, id: &str) -> Result<(), IureError> {
    if mention {
        call(pim::mark_mention_read(sess, id)).await
    } else {
        call(pim::mark_notification_read(sess, id)).await
    }
}

// ---------------------------------------------------------------------------------------------
// Agenda y tareas propias

/// Eventos y tareas abiertas del usuario en la instancia.
#[derive(Clone, Debug, Default)]
pub struct Agenda {
    pub events: Vec<pim::CalendarEvent>,
    pub tasks: Vec<pim::Task>,
}

/// Agenda del rango y tareas abiertas (necesita el id del usuario: `GET /api/auth/me`).
pub async fn fetch_agenda(
    sess: &Session,
    user_id: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Agenda, IureError> {
    let (f, t) = (from.to_rfc3339(), to.to_rfc3339());
    let mut events = call(pim::events(sess, Some(&f), Some(&t), None)).await?;
    events.truncate(2_000);
    let mut tasks = soft(pim::my_open_tasks(sess, user_id, 500)).await?.unwrap_or_default();
    tasks.truncate(500);
    Ok(Agenda { events, tasks })
}

/// Id del usuario de la sesión.
pub async fn user_id(sess: &Session) -> Result<String, IureError> {
    let u = call(sess.me()).await?;
    let id = match &u.id {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        _ => String::new(),
    };
    if id.is_empty() {
        return Err(IureError::Server(Text::new("unknown user", "usuario desconocido")));
    }
    Ok(id)
}

// ---------------------------------------------------------------------------------------------
// Buzón de ejemplo

/// Respuestas sin red para el buzón de ejemplo y las capturas (deterministas).
pub mod sample {
    use super::*;

    /// Organizaciones del buzón de ejemplo con oportunidad abierta.
    const DEALS: &[(&str, &str, f64)] = &[
        ("Acme Corp.", "Propuesta", 120_000.0),
        ("Northwind Ltd.", "Negociación", 48_500.0),
        ("Constructora Altamira", "Calificado", 310_000.0),
    ];

    fn hash(s: &str) -> u64 {
        s.bytes().fold(1469598103934665603u64, |h, b| (h ^ u64::from(b)).wrapping_mul(1099511628211))
    }

    /// Contexto de ejemplo para un correo.
    pub fn context(q: &ContextQuery) -> IureContext {
        let mut ctx = IureContext::default();
        if q.email.is_empty() {
            return ctx;
        }
        let h = hash(&q.email.to_lowercase());
        let deal = q.org.as_deref().and_then(|o| DEALS.iter().find(|(n, _, _)| *n == o));
        let known = deal.is_some() || !q.case_refs.is_empty() || h.is_multiple_of(3);
        if known {
            let subtitle = match &q.org {
                Some(o) if !o.is_empty() => format!("Directora jurídica · {o}"),
                _ => "Cliente".to_string(),
            };
            ctx.people.push(PersonCard {
                kind: if deal.is_some() { PersonKind::Lead } else { PersonKind::Client },
                id: format!("p-{}", h % 1000),
                name: q.name.clone(),
                subtitle,
                email: q.email.clone(),
                phone: Some(format!("+52 55 5{:03} {:04}", h % 1000, (h / 1000) % 10_000)),
            });
        }
        if let Some((org, stage, amount)) = deal {
            ctx.deals.push(DealCard {
                kind: TargetKind::Opportunity,
                id: format!("o-{}", hash(org) % 1000),
                name: format!("{org} — servicios 2027"),
                organization: Some((*org).to_string()),
                stage: Some((*stage).to_string()),
                status: Some("open".into()),
                amount: Some(*amount),
            });
            ctx.activities.push(ActivityCard {
                kind: "call".into(),
                subject: "Llamada de seguimiento de la propuesta".into(),
                when: Some("2026-10-19T16:00:00Z".into()),
            });
            ctx.activities.push(ActivityCard {
                kind: "email".into(),
                subject: "Envío de la propuesta económica".into(),
                when: Some("2026-10-14T10:30:00Z".into()),
            });
        }
        for r in q.case_refs.iter().take(3) {
            ctx.cases.push(CaseCard {
                id: format!("c-{r}"),
                title: format!("{r} · Arrendamiento local comercial"),
                subtitle: q.org.clone().or_else(|| Some(q.name.clone())),
            });
        }
        if h % 4 == 1 || (deal.is_none() && known && q.case_refs.is_empty()) {
            ctx.tickets.push(TicketCard {
                id: format!("t-{}", h % 1000),
                number: format!("TK-{}", 1000 + h % 900),
                title: "Acceso al portal de clientes".into(),
                status: if h.is_multiple_of(2) { "open".into() } else { "waiting".into() },
                priority: "medium".into(),
            });
        }
        ctx
    }

    /// Destinos de ejemplo para «Vincular a…».
    pub fn targets(q: &str) -> Vec<LinkTarget> {
        let q = q.trim().to_lowercase();
        if q.chars().count() < 2 {
            return Vec::new();
        }
        let all = [
            (TargetKind::Case, "c-241/2026", "241/2026 · Arrendamiento local comercial", "Constructora Altamira"),
            (TargetKind::Case, "c-1452/2026", "1452/2026 · Juicio ejecutivo mercantil", "Northwind Ltd."),
            (TargetKind::Client, "cl-7", "Constructora Altamira S.A. de C.V.", "contacto@altamira.example.mx"),
            (TargetKind::Opportunity, "o-1", "Acme Corp. — servicios 2027", "Acme Corp. · Propuesta"),
            (TargetKind::Lead, "l-3", "Carmen Brown", "Northwind Ltd."),
            (TargetKind::Ticket, "t-12", "TK-1012 · Acceso al portal de clientes", "carmen.brown@example.net"),
        ];
        all.iter()
            .filter(|(_, _, l, d)| l.to_lowercase().contains(&q) || d.to_lowercase().contains(&q))
            .map(|(k, id, l, d)| LinkTarget {
                kind: *k,
                id: (*id).into(),
                label: (*l).into(),
                detail: Some((*d).into()),
            })
            .collect()
    }

    /// Avisos de ejemplo.
    pub fn feed() -> Feed {
        let items = vec![
            FeedItem {
                mention: true,
                id: "m1".into(),
                title: "Pablo Ruiz te mencionó".into(),
                message: Some("«@Laura ¿revisas el escrito del caso 241/2026?»".into()),
                link: None,
                read: false,
                created_at: Some("2026-10-21T10:05:00Z".into()),
            },
            FeedItem {
                mention: false,
                id: "n1".into(),
                title: "Nuevo plazo: contestación de demanda".into(),
                message: Some("Caso 1452/2026 · vence el 28 de octubre".into()),
                link: Some("/cases/1452".into()),
                read: false,
                created_at: Some("2026-10-21T09:12:00Z".into()),
            },
            FeedItem {
                mention: false,
                id: "n2".into(),
                title: "Ticket TK-1012 en espera".into(),
                message: Some("El solicitante respondió".into()),
                link: None,
                read: true,
                created_at: Some("2026-10-20T17:40:00Z".into()),
            },
        ];
        let unread = items.iter().filter(|i| !i.read).count() as u64;
        Feed { items, unread }
    }

    /// Agenda de ejemplo alrededor de `now`.
    pub fn agenda(now: DateTime<Utc>) -> Agenda {
        let day = |d: i64, h: u32| {
            let date = now.date_naive() + chrono::Duration::days(d);
            date.and_hms_opt(h, 0, 0).map(|t| t.and_utc().to_rfc3339()).unwrap_or_default()
        };
        let ev = |id: &str, title: &str, d: i64, h: u32, kind: &str| pim::CalendarEvent {
            id: id.into(),
            title: title.into(),
            start: day(d, h),
            end: day(d, h + 1),
            event_type: kind.into(),
            location: Some("Iurefficient".into()),
            case_title: Some("241/2026".into()),
            ..Default::default()
        };
        let due = |d: i64| (now.date_naive() + chrono::Duration::days(d)).format("%Y-%m-%d").to_string();
        Agenda {
            events: vec![
                pim::CalendarEvent {
                    meeting_url: Some("https://teams.microsoft.com/l/meetup-join/19%3ameeting_demo241".into()),
                    location: Some("Videollamada".into()),
                    ..ev("ie-3", "Videollamada con Constructora Altamira", 0, 16, "meeting")
                },
                ev("ie-1", "Audiencia de pruebas (Iurefficient)", 1, 15, "court"),
                ev("ie-2", "Reunión con Altamira (Iurefficient)", 2, 17, "meeting"),
            ],
            tasks: vec![
                pim::Task {
                    id: "it-1".into(),
                    title: "Revisar escrito de contestación".into(),
                    status: "pending".into(),
                    due_date: Some(due(2)),
                    case_title: Some("1452/2026".into()),
                    ..Default::default()
                },
                pim::Task {
                    id: "it-3".into(),
                    title: "Presentar ofrecimiento de pruebas".into(),
                    status: "pending".into(),
                    priority: Some("high".into()),
                    due_date: Some(due(0)),
                    case_title: Some("241/2026".into()),
                    ..Default::default()
                },
                pim::Task {
                    id: "it-4".into(),
                    title: "Llamar a la contraparte".into(),
                    status: "pending".into(),
                    due_date: Some(due(-1)),
                    case_title: Some("1452/2026".into()),
                    ..Default::default()
                },
                pim::Task {
                    id: "it-2".into(),
                    title: "Enviar cotización a Acme".into(),
                    status: "in_progress".into(),
                    due_date: Some(due(5)),
                    ..Default::default()
                },
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_mail_is_not_an_organization() {
        let q = |e: &str| ContextQuery { email: e.into(), ..Default::default() };
        assert_eq!(q("ana@gmail.com").org_domain(), None);
        assert_eq!(q("ana@Altamira.example.MX").org_domain().as_deref(), Some("altamira.example.mx"));
        assert_eq!(q("sin-arroba").org_domain(), None);
        assert_eq!(q("x@localhost").org_domain(), None);
    }

    #[test]
    fn digest_text_is_bounded_and_named() {
        let e = EmailDigest {
            message_id: "abc@x".into(),
            from: "Ana <ana@x.mx>".into(),
            to: "yo@x.mx".into(),
            date: DateTime::<Utc>::default(),
            subject: "Re: Contrato/arrendamiento: v2?".into(),
            text: "a".repeat(MAX_EMAIL_TEXT * 2),
        };
        assert!(e.as_text().chars().count() < MAX_EMAIL_TEXT + 400);
        assert!(e.as_text().contains("Message-ID: <abc@x>"));
        let n = e.file_name();
        assert!(n.ends_with(".txt") && !n.contains('/') && !n.contains(':'), "{n}");
    }

    #[test]
    fn clip_counts_chars() {
        assert_eq!(clip("  ñandú  ", 10), "ñandú");
        assert_eq!(clip("ñandú", 3), "ña…");
        assert_eq!(clip("", 0), "");
    }

    #[test]
    fn errors_are_classified() {
        let terms: anyhow::Error = ApiError::new(403, Some("terms_acceptance_required"), "x").into();
        assert_eq!(classify(&terms), IureError::Terms);
        let expired: anyhow::Error = ApiError::new(401, None, "x").into();
        assert_eq!(classify(&expired), IureError::Expired);
        let other: anyhow::Error = ApiError::new(404, Some("no_plugin"), "x").into();
        assert!(matches!(classify(&other), IureError::Server(_)));
    }

    #[test]
    fn sample_is_deterministic_and_bounded() {
        let q = ContextQuery {
            email: "carmen.brown@example.net".into(),
            name: "Carmen Brown".into(),
            org: Some("Acme Corp.".into()),
            subject: "caso 241/2026".into(),
            case_refs: vec!["241/2026".into()],
        };
        let a = sample::context(&q);
        assert_eq!(a, sample::context(&q));
        assert!(!a.people.is_empty() && !a.deals.is_empty() && !a.cases.is_empty());
        assert!(sample::targets("x").is_empty());
        assert!(!sample::targets("altamira").is_empty());
        assert!(sample::feed().unread >= 1);
    }
}
