//! «La IA propone, el usuario confirma.» Las tareas devuelven propuestas; crear algo en
//! Iurefficient es un paso aparte que exige una [`Approved`] (sólo la construye
//! [`ProposedAction::approve`], que la UI llama tras el clic del usuario).
//!
//! Con connect v0.7.1 se usan los auxiliares REST genéricos de `Session` (`get_json`,
//! `post_json`, `me`) con los mismos cuerpos que las funciones de `connect::pim` (rama
//! `iuredesk`): cuando se publique connect 0.8.0 se cambian por `crm_create_lead`,
//! `create_ticket`, `create_task` y `create_event` sin tocar la interfaz de este módulo.

use chrono::{Duration, NaiveDateTime, TimeZone};
use chrono_tz::Tz;
use iurefficient_connect::rest::Session;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::error::{AiError, Result};
use crate::providers::iurefficient::map_connect_error;
use crate::schema::{LeadPayload, ProposedEvent, ProposedTask, TicketPayload, When, parse_when};

/// Algo que el asistente propone hacer en Iurefficient.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ProposedAction {
    /// `POST /api/plugins/crm/leads` (plugin `iur-crm`).
    CreateLead { lead: LeadPayload },
    /// `POST /api/plugins/tickets` (plugin `iur-tickets`). `notify_requester: false` (por
    /// omisión) no manda `requester_email`, para que la instancia no conteste por correo.
    CreateTicket { ticket: TicketPayload, notify_requester: bool },
    /// `POST /api/tasks` (tarea personal asignada al usuario).
    CreateTask { task: ProposedTask, case_id: Option<String> },
    /// `POST /api/calendar-events`.
    CreateEvent { event: ProposedEvent, case_id: Option<String> },
}

impl ProposedAction {
    /// El usuario la confirmó (después de verla y, si quiso, editarla).
    pub fn approve(self) -> Approved {
        Approved(self)
    }

    /// Descripción corta para el botón de confirmación.
    pub fn describe(&self, spanish: bool) -> String {
        let l = |en: &str, es: &str| if spanish { es.to_string() } else { en.to_string() };
        match self {
            ProposedAction::CreateLead { lead } => format!("{} «{}»", l("Create lead", "Crear lead"), lead.name),
            ProposedAction::CreateTicket { ticket, .. } => {
                format!("{} «{}»", l("Create ticket", "Crear ticket"), ticket.title)
            }
            ProposedAction::CreateTask { task, .. } => format!("{} «{}»", l("Create task", "Crear tarea"), task.title),
            ProposedAction::CreateEvent { event, .. } => {
                format!("{} «{}» ({})", l("Create event", "Crear evento"), event.title, event.start)
            }
        }
    }
}

/// Acción confirmada por el usuario.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Approved(ProposedAction);

impl Approved {
    pub fn action(&self) -> &ProposedAction {
        &self.0
    }
}

/// Lo creado.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionOutcome {
    /// `lead`, `ticket`, `task`, `event`.
    pub entity: String,
    pub id: String,
}

fn put(m: &mut Map<String, Value>, k: &str, v: &Option<String>) {
    if let Some(x) = v.as_deref().filter(|x| !x.trim().is_empty()) {
        m.insert(k.into(), json!(x));
    }
}

fn id_of(v: &Value, keys: &[&str]) -> String {
    for k in keys {
        let x = v.pointer(k).unwrap_or(&Value::Null);
        match x {
            Value::String(s) if !s.is_empty() => return s.clone(),
            Value::Number(n) => return n.to_string(),
            _ => {}
        }
    }
    String::new()
}

/// Cuerpo de un lead (como `connect::pim::crm_create_lead`).
pub fn lead_body(l: &LeadPayload) -> Value {
    let mut m = Map::new();
    m.insert("name".into(), json!(l.name));
    m.insert("source".into(), json!("manual"));
    put(&mut m, "first_name", &l.first_name);
    put(&mut m, "last_name", &l.last_name);
    put(&mut m, "company_name", &l.company_name);
    put(&mut m, "email", &l.email);
    put(&mut m, "phone", &l.phone);
    put(&mut m, "position", &l.position);
    put(&mut m, "notes", &l.notes);
    Value::Object(m)
}

/// Cuerpo de un ticket (como `connect::pim::create_ticket`, `source: "api"`).
pub fn ticket_body(t: &TicketPayload, notify_requester: bool) -> Value {
    let mut m = Map::new();
    m.insert("title".into(), json!(t.title));
    m.insert("source".into(), json!("api"));
    m.insert("priority".into(), json!(t.priority.as_str()));
    put(&mut m, "description", &t.description);
    put(&mut m, "category", &t.category);
    put(&mut m, "requester_name", &t.requester_name);
    if notify_requester {
        put(&mut m, "requester_email", &t.requester_email);
    }
    if !t.tags.is_empty() {
        m.insert("tags".into(), json!(t.tags));
    }
    Value::Object(m)
}

/// Cuerpo de una tarea personal.
pub fn task_body(t: &ProposedTask, case_id: Option<&str>, assigned_to_id: Option<&str>) -> Value {
    let mut m = Map::new();
    m.insert("title".into(), json!(t.title));
    let mut desc = Vec::new();
    if let Some(a) = &t.assignee_hint {
        desc.push(format!("→ {a}"));
    }
    if let Some(s) = &t.source_text {
        desc.push(format!("«{s}»"));
    }
    if !desc.is_empty() {
        m.insert("description".into(), json!(desc.join("\n")));
    }
    put(&mut m, "due_date", &t.due_date);
    put(&mut m, "case_id", &case_id.map(str::to_string));
    put(&mut m, "assigned_to_id", &assigned_to_id.map(str::to_string));
    Value::Object(m)
}

fn utc(tz: Tz, dt: NaiveDateTime) -> Result<String> {
    tz.from_local_datetime(&dt)
        .earliest()
        .map(|d| d.with_timezone(&chrono::Utc).format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .ok_or_else(|| AiError::Action(format!("hora inexistente en {tz}: {dt}")))
}

/// Cuerpo de un evento: la hora de pared propuesta se convierte a UTC en la zona del usuario.
pub fn event_body(e: &ProposedEvent, case_id: Option<&str>, tz: Tz) -> Result<Value> {
    let start = parse_when(&e.start).ok_or_else(|| AiError::Action(format!("fecha no válida: {}", e.start)))?;
    let end = e.end.as_deref().and_then(parse_when);
    let (s, en, all_day) = match (start, end) {
        (When::Date(d), end) => {
            let last = match end {
                Some(When::Date(x)) if x >= d => x,
                _ => d,
            };
            (format!("{}T00:00:00Z", d.format("%Y-%m-%d")), format!("{}T23:59:59Z", last.format("%Y-%m-%d")), true)
        }
        (When::DateTime(a), end) => {
            let b = match end {
                Some(When::DateTime(b)) if b > a => b,
                _ => a.checked_add_signed(Duration::hours(1)).unwrap_or(a),
            };
            (utc(tz, a)?, utc(tz, b)?, false)
        }
    };
    let mut m = Map::new();
    m.insert("title".into(), json!(e.title));
    m.insert("start_datetime".into(), json!(s));
    m.insert("end_datetime".into(), json!(en));
    m.insert("all_day".into(), json!(all_day));
    m.insert("event_type".into(), json!("other"));
    put(&mut m, "location", &e.location);
    put(&mut m, "description", &e.source_text.as_ref().map(|s| format!("«{s}»")));
    put(&mut m, "case_id", &case_id.map(str::to_string));
    Ok(Value::Object(m))
}

/// ¿Está el plugin habilitado para el usuario? (`GET /api/plugins`).
async fn plugin_available(sess: &Session, id: &str) -> Result<bool> {
    let v = sess.get_json("/api/plugins").await.map_err(|e| map_connect_error(&e))?;
    Ok(v.get("plugins")
        .and_then(Value::as_array)
        .is_some_and(|a| a.iter().any(|p| p.get("id").and_then(Value::as_str) == Some(id))))
}

/// Ejecuta una acción confirmada en Iurefficient. Sin sesión, o sin el plugin, devuelve
/// [`AiError::NotAvailable`].
pub async fn execute(sess: Option<&Session>, approved: Approved, tz: Tz) -> Result<ActionOutcome> {
    let sess = sess.ok_or_else(|| AiError::NotAvailable("no hay sesión de Iurefficient".into()))?;
    let out = |entity: &str, id: String| -> Result<ActionOutcome> {
        if id.is_empty() {
            return Err(AiError::Action(format!("la instancia no devolvió el id del {entity}")));
        }
        Ok(ActionOutcome { entity: entity.into(), id })
    };
    match approved.0 {
        ProposedAction::CreateLead { lead } => {
            let lead = lead.normalized().ok_or_else(|| AiError::Action("el lead necesita nombre".into()))?;
            if !plugin_available(sess, "iur-crm").await? {
                return Err(AiError::NotAvailable("el CRM no está habilitado en la instancia".into()));
            }
            let v = sess.post_json("/api/plugins/crm/leads", &lead_body(&lead)).await.map_err(|e| {
                if e.to_string().contains("409") || e.to_string().contains("duplicate_email") {
                    AiError::Action("ya existe un lead con ese correo".into())
                } else {
                    map_connect_error(&e)
                }
            })?;
            out("lead", id_of(&v, &["/lead/id", "/id", "/item/id"]))
        }
        ProposedAction::CreateTicket { ticket, notify_requester } => {
            let ticket = ticket.normalized().ok_or_else(|| AiError::Action("el ticket necesita título".into()))?;
            if !plugin_available(sess, "iur-tickets").await? {
                return Err(AiError::NotAvailable("los tickets no están habilitados en la instancia".into()));
            }
            let v = sess
                .post_json("/api/plugins/tickets", &ticket_body(&ticket, notify_requester))
                .await
                .map_err(|e| map_connect_error(&e))?;
            out("ticket", id_of(&v, &["/ticket/id", "/id"]))
        }
        ProposedAction::CreateTask { task, case_id } => {
            let task = task.normalized().ok_or_else(|| AiError::Action("la tarea necesita título".into()))?;
            let me = sess.me().await.map_err(|e| map_connect_error(&e))?;
            let my_id = match &me.id {
                Value::String(s) if !s.is_empty() => Some(s.clone()),
                Value::Number(n) => Some(n.to_string()),
                _ => None,
            };
            let v = sess
                .post_json("/api/tasks", &task_body(&task, case_id.as_deref(), my_id.as_deref()))
                .await
                .map_err(|e| map_connect_error(&e))?;
            out("task", id_of(&v, &["/task/id", "/id"]))
        }
        ProposedAction::CreateEvent { event, case_id } => {
            let event =
                event.normalized().ok_or_else(|| AiError::Action("el evento necesita título y fecha".into()))?;
            let body = event_body(&event, case_id.as_deref(), tz)?;
            let v = sess.post_json("/api/calendar-events", &body).await.map_err(|e| map_connect_error(&e))?;
            out("event", id_of(&v, &["/event/id", "/id"]))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::TicketPriority;

    #[test]
    fn ticket_never_sends_requester_email_unless_asked() {
        let t = TicketPayload {
            title: "No imprime".into(),
            description: Some("La impresora".into()),
            priority: TicketPriority::High,
            category: None,
            requester_name: Some("Ana".into()),
            requester_email: Some("ana@cliente.mx".into()),
            tags: vec!["correo".into()],
        };
        let b = ticket_body(&t, false);
        assert!(b.get("requester_email").is_none());
        assert_eq!(b["priority"], "high");
        assert_eq!(b["source"], "api");
        assert_eq!(ticket_body(&t, true)["requester_email"], "ana@cliente.mx");
    }

    #[test]
    fn event_times_are_converted_to_utc_in_the_user_zone() {
        let e = ProposedEvent {
            title: "Audiencia".into(),
            start: "2026-10-27T16:00".into(),
            end: None,
            all_day: false,
            location: Some("Juzgado 3".into()),
            source_text: None,
        };
        let b = event_body(&e, Some("c1"), chrono_tz::America::Mexico_City).unwrap();
        assert_eq!(b["start_datetime"], "2026-10-27T22:00:00Z");
        assert_eq!(b["end_datetime"], "2026-10-27T23:00:00Z");
        assert_eq!(b["all_day"], false);
        assert_eq!(b["case_id"], "c1");
        let day = ProposedEvent { start: "2026-11-02".into(), ..e };
        let b = event_body(&day, None, chrono_tz::UTC).unwrap();
        assert_eq!(b["all_day"], true);
        assert!(b.get("case_id").is_none());
    }

    #[test]
    fn lead_and_task_bodies_use_iurefficient_field_names() {
        let l = LeadPayload { name: "Ana Ruiz".into(), company_name: Some("Cliente SA".into()), ..Default::default() };
        let b = lead_body(&l);
        assert_eq!(b["company_name"], "Cliente SA");
        assert_eq!(b["source"], "manual");
        assert!(b.get("email").is_none());
        let t = ProposedTask {
            title: "Enviar contrato".into(),
            due_date: Some("2026-10-30".into()),
            assignee_hint: None,
            source_text: Some("te lo mando el jueves".into()),
        };
        let b = task_body(&t, None, Some("7"));
        assert_eq!(b["due_date"], "2026-10-30");
        assert_eq!(b["assigned_to_id"], "7");
        assert!(b["description"].as_str().unwrap().contains("jueves"));
    }

    #[tokio::test]
    async fn without_session_it_is_not_available() {
        let a = ProposedAction::CreateLead { lead: LeadPayload { name: "x".into(), ..Default::default() } };
        assert!(a.describe(true).contains("Crear lead"));
        let r = execute(None, a.approve(), chrono_tz::UTC).await;
        assert!(matches!(r, Err(AiError::NotAvailable(_))));
    }
}
