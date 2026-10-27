//! Las herramientas MCP de IureDesk sobre el almacén local.
//!
//! Las mismas funciones sirven al proceso `iuredesk --mcp` (almacén abierto directamente) y al
//! puente IPC dentro de la app abierta ([`crate::bridge`]): [`dispatch`] recibe un `&Store`.
//!
//! Reglas:
//! - Nada se envía: [`crear_borrador`](dispatch) guarda en Borradores y punto.
//! - Todo argumento es hostil: tamaños acotados, fechas validadas, rangos con tope.
//! - Los nombres son en español (como los de IureVoz); las descripciones, en los dos idiomas.

use std::collections::HashMap;

use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use iuredesk_compose::{
    Draft, FsResolver, ReplyMode, ReplyOptions, RichBody, build_message, mime::BuildOptions, reply,
};
use iuredesk_model::account::Identity;
use iuredesk_model::{
    AccountId, CalendarId, Event, EventStatus, EventTime, Flags, FolderId, FolderRole, Mailbox, MessageEnvelope,
    MessageId, Note, TaskStatus, ThreadId, make_preview,
};
use iuredesk_store::Store;
use serde_json::{Value, json};

/// Nombres de las herramientas.
pub const TOOL_NAMES: &[&str] = &[
    "buscar_correo",
    "leer_hilo",
    "crear_borrador",
    "agenda",
    "crear_evento",
    "buscar_contacto",
    "crear_nota",
    "mis_tareas",
];

/// Tipo de operación que deja `crear_borrador` en la cola (la misma que `Session::save_draft`):
/// `sync` sube el borrador a la carpeta Borradores del servidor.
pub const OP_APPEND_DRAFT: &str = "append_draft";

/// Tope de caracteres de `leer_hilo`.
const MAX_THREAD_CHARS: usize = 60_000;
const DEFAULT_THREAD_CHARS: usize = 20_000;
/// Mensajes máximos de un hilo que se leen (los más recientes).
const MAX_THREAD_MESSAGES: usize = 50;
/// Rango máximo de `agenda`.
const MAX_AGENDA_DAYS: i64 = 92;
/// Apariciones máximas que devuelve `agenda`.
const MAX_AGENDA_ITEMS: usize = 500;
/// Tope de textos que escribe el asistente (cuerpo de borrador, nota).
const MAX_WRITE_CHARS: usize = 512 * 1024;
const MAX_LINE_CHARS: usize = 1_000;

/// Lo que devuelve una herramienta.
#[derive(Clone, Debug, PartialEq)]
pub enum ToolOutput {
    /// Datos estructurados.
    Data(Value),
    /// Texto plano (lo lee el modelo tal cual).
    Text(String),
}

/// Fallo de una herramienta.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ToolError {
    /// Llamada mal formada (herramienta inexistente…): error de protocolo.
    #[error("{0}")]
    Params(String),
    /// La herramienta falló: el modelo recibe el mensaje y puede corregir.
    #[error("{0}")]
    Failed(String),
}

/// Contexto de una llamada.
#[derive(Clone, Debug)]
pub struct ToolContext {
    /// «Ahora» (fijo en pruebas).
    pub now: DateTime<Utc>,
    /// Mensajes de error en español.
    pub spanish: bool,
    /// Zona por omisión (si la llamada no trae `zona`).
    pub zone: Tz,
}

impl ToolContext {
    /// Ahora, idioma del sistema y zona del sistema.
    pub fn system() -> Self {
        Self {
            now: Utc::now(),
            spanish: iurefficient_connect::lang::current() == iurefficient_connect::lang::Lang::Es,
            zone: system_zone(),
        }
    }

    fn t(&self, en: impl Into<String>, es: impl Into<String>) -> String {
        if self.spanish { es.into() } else { en.into() }
    }
}

/// `true` si la herramienta cambia el almacén (la interfaz debe refrescarse).
pub fn is_write(name: &str) -> bool {
    matches!(name, "crear_borrador" | "crear_evento" | "crear_nota")
}

/// Zona del sistema: `TZ`, luego el enlace `/etc/localtime`; si no, UTC.
pub fn system_zone() -> Tz {
    if let Ok(tz) = std::env::var("TZ")
        && let Ok(z) = tz.trim_start_matches(':').parse::<Tz>()
    {
        return z;
    }
    if let Ok(target) = std::fs::read_link("/etc/localtime") {
        let s = target.to_string_lossy();
        if let Some((_, name)) = s.split_once("zoneinfo/")
            && let Ok(z) = name.parse::<Tz>()
        {
            return z;
        }
    }
    Tz::UTC
}

/// Lista para `tools/list`.
pub fn tool_list() -> Value {
    let ro = json!({"readOnlyHint": true, "openWorldHint": false});
    let creates =
        json!({"readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false});
    let zona = json!({"type": "string", "description": "IANA time zone, e.g. America/Mexico_City (default: the computer's). · Zona IANA, p. ej. America/Mexico_City (por omisión, la del equipo)."});
    json!([
        {
            "name": "buscar_correo",
            "title": "Search mail · Buscar correo",
            "description": "Searches the user's local mailbox (all accounts). Supports words and operators from:, to:, subject:, has:attachment, is:unread. Returns id, thread_id, sender, subject, date and a preview; read a conversation with leer_hilo. · Busca en el correo local del usuario (todas las cuentas). Admite palabras y los operadores from:, to:, subject:, has:attachment, is:unread. Devuelve id, thread_id, remitente, asunto, fecha y extracto; lee una conversación con leer_hilo.",
            "inputSchema": {"type": "object", "properties": {
                "query": {"type": "string", "description": "Search text. · Texto a buscar."},
                "limit": {"type": "integer", "description": "1-50, default 20. · 1-50, por omisión 20."}
            }, "required": ["query"]},
            "annotations": ro,
        },
        {
            "name": "leer_hilo",
            "title": "Read a conversation · Leer una conversación",
            "description": "Returns a conversation as plain text (headers and body of each message, oldest first), size-capped. Pass thread_id (from buscar_correo) or message_id. · Devuelve una conversación en texto plano (cabeceras y cuerpo de cada mensaje, del más antiguo al más reciente), con tope de tamaño. Pasa thread_id (de buscar_correo) o message_id.",
            "inputSchema": {"type": "object", "properties": {
                "thread_id": {"type": ["integer", "string"]},
                "message_id": {"type": ["integer", "string"]},
                "max_chars": {"type": "integer", "description": "Default 20000, max 60000. · Por omisión 20000, máx. 60000."}
            }},
            "annotations": ro,
        },
        {
            "name": "crear_borrador",
            "title": "Create a draft · Crear un borrador",
            "description": "Saves an email draft in the Drafts folder. It is NEVER sent: the user reviews and sends it from IureDesk. With reply_to_message_id it becomes a reply (threading headers, quoted original). · Guarda un borrador de correo en Borradores. NUNCA se envía: el usuario lo revisa y lo envía desde IureDesk. Con reply_to_message_id es una respuesta (cabeceras del hilo y original citado).",
            "inputSchema": {"type": "object", "properties": {
                "to": {"type": "string", "description": "Recipients separated by commas. · Destinatarios separados por comas."},
                "cc": {"type": "string"},
                "subject": {"type": "string"},
                "body_markdown": {"type": "string", "description": "Body in Markdown. · Cuerpo en Markdown."},
                "reply_to_message_id": {"type": ["integer", "string"]},
                "account": {"type": "string", "description": "Sender account email (default: the first one, or the original's). · Correo de la cuenta remitente (por omisión, la primera o la del original)."}
            }, "required": ["body_markdown"]},
            "annotations": creates,
        },
        {
            "name": "agenda",
            "title": "Calendar · Agenda",
            "description": "Lists calendar events between two dates (recurrences expanded), max 92 days. Dates as YYYY-MM-DD or YYYY-MM-DDTHH:MM (in zona) or RFC 3339. · Lista los eventos del calendario entre dos fechas (con repeticiones), máximo 92 días. Fechas como AAAA-MM-DD o AAAA-MM-DDTHH:MM (en zona) o RFC 3339.",
            "inputSchema": {"type": "object", "properties": {
                "desde": {"type": "string", "description": "Start (default: today). · Inicio (por omisión, hoy)."},
                "hasta": {"type": "string", "description": "End, exclusive (default: start + 7 days). · Fin, exclusivo (por omisión, inicio + 7 días)."},
                "zona": zona,
            }},
            "annotations": ro,
        },
        {
            "name": "crear_evento",
            "title": "Create an event · Crear un evento",
            "description": "Creates a calendar event (saved locally and synced when online). No invitations are sent. · Crea un evento en el calendario (se guarda en local y se sincroniza al haber red). No envía invitaciones.",
            "inputSchema": {"type": "object", "properties": {
                "titulo": {"type": "string"},
                "inicio": {"type": "string", "description": "YYYY-MM-DDTHH:MM, or YYYY-MM-DD for all day. · AAAA-MM-DDTHH:MM, o AAAA-MM-DD para todo el día."},
                "fin": {"type": "string", "description": "Default: start + 1 h (or + 1 day). · Por omisión, inicio + 1 h (o + 1 día)."},
                "lugar": {"type": "string"},
                "descripcion": {"type": "string"},
                "calendario_id": {"type": "integer", "description": "Default: the first writable calendar. · Por omisión, el primer calendario con escritura."},
                "zona": zona,
            }, "required": ["titulo", "inicio"]},
            "annotations": creates,
        },
        {
            "name": "buscar_contacto",
            "title": "Find a contact · Buscar un contacto",
            "description": "Finds contacts by name, email, phone or organization. · Busca contactos por nombre, correo, teléfono u organización.",
            "inputSchema": {"type": "object", "properties": {
                "q": {"type": "string"},
                "limit": {"type": "integer", "description": "1-50, default 10."}
            }, "required": ["q"]},
            "annotations": ro,
        },
        {
            "name": "crear_nota",
            "title": "Create a note · Crear una nota",
            "description": "Creates a note (Markdown) in IureDesk. · Crea una nota (Markdown) en IureDesk.",
            "inputSchema": {"type": "object", "properties": {
                "title": {"type": "string"},
                "markdown": {"type": "string"}
            }, "required": ["title", "markdown"]},
            "annotations": creates,
        },
        {
            "name": "mis_tareas",
            "title": "My tasks · Mis tareas",
            "description": "Lists the user's tasks (pending by default), by due date. · Lista las tareas del usuario (pendientes por omisión), por fecha de vencimiento.",
            "inputSchema": {"type": "object", "properties": {
                "incluir_terminadas": {"type": "boolean"},
                "limit": {"type": "integer", "description": "1-200, default 50."}
            }},
            "annotations": ro,
        },
    ])
}

/// Ejecuta una herramienta.
pub fn dispatch(store: &Store, name: &str, args: &Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
    if !args.is_object() {
        return Err(ToolError::Params(ctx.t("arguments must be an object", "los argumentos deben ser un objeto")));
    }
    match name {
        "buscar_correo" => buscar_correo(store, args, ctx),
        "leer_hilo" => leer_hilo(store, args, ctx),
        "crear_borrador" => crear_borrador(store, args, ctx),
        "agenda" => agenda(store, args, ctx),
        "crear_evento" => crear_evento(store, args, ctx),
        "buscar_contacto" => buscar_contacto(store, args, ctx),
        "crear_nota" => crear_nota(store, args, ctx),
        "mis_tareas" => mis_tareas(store, args, ctx),
        other => {
            Err(ToolError::Params(ctx.t(format!("unknown tool: {other}"), format!("herramienta desconocida: {other}"))))
        }
    }
}

// ------------------------------------------------------------------ ayudas

fn store_err(ctx: &ToolContext, e: impl std::fmt::Display) -> ToolError {
    ToolError::Failed(
        ctx.t(format!("Could not read the local data: {e}"), format!("No se pudieron leer los datos locales: {e}")),
    )
}

fn str_arg<'a>(a: &'a Value, k: &str) -> Option<&'a str> {
    a.get(k).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

fn limit_arg(a: &Value, k: &str, default: u64, max: u64) -> usize {
    a.get(k).and_then(Value::as_u64).unwrap_or(default).clamp(1, max) as usize
}

/// Id numérico: `5`, `"5"` o `"thr:5"`.
fn id_arg(a: &Value, k: &str) -> Option<u64> {
    match a.get(k)? {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.rsplit(':').next()?.trim().parse().ok(),
        _ => None,
    }
}

fn cut(text: &str, max: usize) -> (String, bool) {
    match text.char_indices().nth(max) {
        Some((i, _)) => (text.get(..i).unwrap_or("").to_string(), true),
        None => (text.to_string(), false),
    }
}

fn mailboxes(list: &[Mailbox]) -> String {
    list.iter()
        .take(20)
        .map(|m| match &m.name {
            Some(n) if !n.is_empty() => format!("{n} <{}>", m.addr),
            _ => m.addr.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn zone_arg(a: &Value, ctx: &ToolContext) -> Result<Tz, ToolError> {
    match str_arg(a, "zona") {
        None => Ok(ctx.zone),
        Some(z) => z.parse::<Tz>().map_err(|_| {
            ToolError::Failed(ctx.t(format!("Unknown time zone: {z}"), format!("Zona horaria desconocida: {z}")))
        }),
    }
}

/// Fecha/hora del asistente. `Ok((instante, solo_fecha))`.
fn parse_time(s: &str, tz: Tz, ctx: &ToolContext) -> Result<(DateTime<Utc>, Option<NaiveDate>), ToolError> {
    let s = s.trim();
    if let Ok(d) = DateTime::parse_from_rfc3339(s) {
        return Ok((d.with_timezone(&Utc), None));
    }
    for f in ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M"] {
        if let Ok(n) = NaiveDateTime::parse_from_str(s, f) {
            return local(n, tz, ctx).map(|t| (t, None));
        }
    }
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return local(d.and_time(NaiveTime::MIN), tz, ctx).map(|t| (t, Some(d)));
    }
    Err(ToolError::Failed(ctx.t(
        format!("Unrecognized date «{s}»: use YYYY-MM-DD or YYYY-MM-DDTHH:MM."),
        format!("Fecha no reconocida «{s}»: usa AAAA-MM-DD o AAAA-MM-DDTHH:MM."),
    )))
}

fn local(n: NaiveDateTime, tz: Tz, ctx: &ToolContext) -> Result<DateTime<Utc>, ToolError> {
    // En el salto de horario de verano, la hora inexistente se corre una hora.
    tz.from_local_datetime(&n)
        .earliest()
        .or_else(|| tz.from_local_datetime(&(n + Duration::hours(1))).earliest())
        .map(|t| t.with_timezone(&Utc))
        .ok_or_else(|| ToolError::Failed(ctx.t("Invalid local time.", "Hora local no válida.")))
}

fn fmt_time(t: &EventTime, tz: Tz) -> String {
    match t {
        EventTime::Date(d) => d.format("%Y-%m-%d").to_string(),
        EventTime::DateTime { utc, .. } => utc.with_timezone(&tz).to_rfc3339(),
    }
}

// ------------------------------------------------------------------ correo

fn folder_names(store: &Store) -> HashMap<FolderId, String> {
    store.all_folders().unwrap_or_default().into_iter().map(|f| (f.id, f.name)).collect()
}

fn buscar_correo(store: &Store, a: &Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
    let q = str_arg(a, "query").ok_or_else(|| ToolError::Failed(ctx.t("Missing «query».", "Falta «query».")))?;
    let (q, _) = cut(q, MAX_LINE_CHARS);
    let limit = limit_arg(a, "limit", 20, 50);
    let found = store.search(&q, limit).map_err(|e| store_err(ctx, e))?;
    let names = folder_names(store);
    let results: Vec<Value> = found
        .iter()
        .map(|e| {
            json!({
                "id": e.id.get(),
                "thread_id": e.thread.map(ThreadId::get),
                "folder": names.get(&e.folder).cloned().unwrap_or_default(),
                "from": mailboxes(&e.from),
                "to": mailboxes(&e.to),
                "subject": e.subject,
                "date": e.date.to_rfc3339(),
                "unread": !e.flags.seen,
                "flagged": e.flags.flagged,
                "has_attachments": e.has_attachments,
                "preview": cut(&e.preview, 300).0,
            })
        })
        .collect();
    Ok(ToolOutput::Data(json!({"count": results.len(), "results": results})))
}

/// Texto del cuerpo de un correo crudo: la parte de texto, o el HTML sin etiquetas.
fn body_text(raw: &[u8]) -> Option<String> {
    let msg = mail_parser::MessageParser::default().parse(raw)?;
    if let Some(t) = msg.body_text(0) {
        return Some(t.into_owned());
    }
    msg.body_html(0).map(|h| strip_html(&h))
}

fn strip_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&nbsp;", " ").replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">")
}

fn leer_hilo(store: &Store, a: &Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
    let max = limit_arg(a, "max_chars", DEFAULT_THREAD_CHARS as u64, MAX_THREAD_CHARS as u64);
    let mut envs: Vec<MessageEnvelope> = Vec::new();
    if let Some(t) = id_arg(a, "thread_id") {
        envs = store.thread_envelopes(ThreadId(t)).map_err(|e| store_err(ctx, e))?;
    }
    // Un message_id: todo su hilo (o el mensaje solo, si no tiene).
    if envs.is_empty()
        && let Some(m) = id_arg(a, "message_id")
        && let Some(e) = store.get_envelope(MessageId(m)).map_err(|e| store_err(ctx, e))?
    {
        if let Some(t) = e.thread {
            envs = store.thread_envelopes(t).map_err(|e| store_err(ctx, e))?;
        }
        if envs.is_empty() {
            envs.push(e);
        }
    }
    if envs.is_empty() {
        return Err(ToolError::Failed(ctx.t(
            "No conversation with that id. Use the thread_id or id returned by buscar_correo.",
            "No hay conversación con ese id. Usa el thread_id o el id que devuelve buscar_correo.",
        )));
    }
    let skip = envs.len().saturating_sub(MAX_THREAD_MESSAGES);
    let mut text = String::new();
    if skip > 0 {
        text.push_str(
            &ctx.t(
                format!("[{skip} older messages omitted]\n\n"),
                format!("[{skip} mensajes anteriores omitidos]\n\n"),
            ),
        );
    }
    let names = folder_names(store);
    for e in envs.iter().skip(skip) {
        let body = store.get_raw(e.id).ok().and_then(|raw| body_text(&raw)).unwrap_or_else(|| e.preview.clone());
        let (body, _) = cut(body.trim(), max);
        text.push_str(&format!(
            "--- id {} · {}\nDe/From: {}\nPara/To: {}\n",
            e.id.get(),
            names.get(&e.folder).map(String::as_str).unwrap_or(""),
            mailboxes(&e.from),
            mailboxes(&e.to)
        ));
        if !e.cc.is_empty() {
            text.push_str(&format!("CC: {}\n", mailboxes(&e.cc)));
        }
        text.push_str(&format!("Fecha/Date: {}\nAsunto/Subject: {}\n", e.date.to_rfc3339(), e.subject));
        if !e.attachments.is_empty() {
            let list: Vec<&str> = e.attachments.iter().take(20).map(|x| x.filename.as_str()).collect();
            text.push_str(&format!("Adjuntos/Attachments: {}\n", list.join(", ")));
        }
        text.push('\n');
        text.push_str(&body);
        text.push_str("\n\n");
        if text.chars().count() > max {
            break;
        }
    }
    let (mut text, truncated) = cut(&text, max);
    if truncated {
        text.push_str(&ctx.t(
            "\n[truncated: call again with a larger max_chars]",
            "\n[recortado: vuelve a llamar con un max_chars mayor]",
        ));
    }
    Ok(ToolOutput::Text(text))
}

fn drafts_folder(store: &Store, account: AccountId) -> Option<FolderId> {
    store.folders(account).ok()?.into_iter().find(|f| f.role == FolderRole::Drafts).map(|f| f.id)
}

fn crear_borrador(store: &Store, a: &Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
    let fail = |e: &dyn std::fmt::Display| {
        ToolError::Failed(
            ctx.t(format!("The draft could not be created: {e}"), format!("No se pudo crear el borrador: {e}")),
        )
    };
    let md = a.get("body_markdown").and_then(Value::as_str).unwrap_or("");
    if md.chars().count() > MAX_WRITE_CHARS {
        return Err(ToolError::Failed(ctx.t("The body is too long.", "El cuerpo es demasiado largo.")));
    }
    let labels = store.account_labels().map_err(|e| store_err(ctx, e))?;
    let original = match id_arg(a, "reply_to_message_id") {
        Some(id) => {
            let env = store.get_envelope(MessageId(id)).map_err(|e| store_err(ctx, e))?.ok_or_else(|| {
                ToolError::Failed(ctx.t(format!("No message with id {id}."), format!("No hay mensaje con id {id}.")))
            })?;
            let raw = store.get_raw(env.id).map_err(|e| store_err(ctx, e))?;
            Some((env, raw))
        }
        None => None,
    };
    let wanted = str_arg(a, "account").map(str::to_ascii_lowercase);
    let label = match (&wanted, &original) {
        (Some(w), _) => labels.iter().find(|l| l.email.to_ascii_lowercase() == *w),
        (None, Some((env, _))) => labels.iter().find(|l| l.id == env.account),
        (None, None) => labels.first(),
    }
    .or_else(|| labels.first())
    .cloned()
    .ok_or_else(|| {
        ToolError::Failed(ctx.t(
            "There is no email account set up in IureDesk.",
            "No hay ninguna cuenta de correo configurada en IureDesk.",
        ))
    })?;
    let identity = Identity { name: label.name.clone(), email: label.email.clone(), signature: None, reply_to: None };
    let new_body = RichBody::from_markdown(md);
    let mut draft = match &original {
        Some((env, raw)) => {
            let opts = ReplyOptions {
                lang: if ctx.spanish { iuredesk_compose::Lang::Es } else { iuredesk_compose::Lang::En },
                original_id: Some(env.id),
                ..ReplyOptions::default()
            };
            let mut d = reply(raw, std::slice::from_ref(&identity), ReplyMode::Sender, &opts).map_err(|e| fail(&e))?;
            let quoted = std::mem::take(&mut d.body.blocks);
            d.body.blocks = new_body.blocks;
            d.body.blocks.extend(quoted);
            d
        }
        None => {
            let mut d = Draft::new(identity);
            d.body = new_body;
            d
        }
    };
    if let Some(to) = str_arg(a, "to") {
        draft.set_to(&cut(to, 8 * MAX_LINE_CHARS).0).map_err(|e| fail(&e))?;
    }
    if let Some(cc) = str_arg(a, "cc") {
        draft.set_cc(&cut(cc, 8 * MAX_LINE_CHARS).0).map_err(|e| fail(&e))?;
    }
    if let Some(s) = str_arg(a, "subject") {
        draft.subject = cut(s, MAX_LINE_CHARS).0;
    }
    let account = original.as_ref().map(|(e, _)| e.account).filter(|_| wanted.is_none()).unwrap_or(label.id);
    let folder = drafts_folder(store, account).ok_or_else(|| {
        ToolError::Failed(
            ctx.t("That account has no Drafts folder yet.", "Esa cuenta aún no tiene carpeta de Borradores."),
        )
    })?;
    let built = build_message(&draft, &FsResolver, &BuildOptions::at(ctx.now)).map_err(|e| fail(&e))?;
    let plain = iuredesk_compose::rich::plain::render(&draft.body);
    let env = MessageEnvelope {
        account,
        folder,
        message_id: Some(built.message_id.clone()),
        in_reply_to: draft.in_reply_to.clone(),
        references: draft.references.clone(),
        from: vec![Mailbox::new(label.name.clone(), label.email.clone())],
        to: draft.to.clone(),
        cc: draft.cc.clone(),
        subject: draft.subject.clone(),
        date: ctx.now,
        flags: Flags { seen: true, draft: true, ..Flags::default() },
        size: built.raw.len() as u64,
        preview: make_preview(&plain),
        ..MessageEnvelope::default()
    };
    let id = store.put_message(env, &built.raw).map_err(|e| store_err(ctx, e))?;
    let payload = serde_json::to_vec(&json!({"id": id.get()})).unwrap_or_default();
    // Si la cola falla, el borrador ya está en local: la sincronización lo verá en el repaso.
    let _ = store.enqueue_op(account, OP_APPEND_DRAFT, &payload);
    Ok(ToolOutput::Data(json!({
        "draft_id": id.get(),
        "from": label.email,
        "to": mailboxes(&draft.to),
        "subject": draft.subject,
        "sent": false,
        "note": ctx.t("Saved in Drafts. It was not sent: the user reviews and sends it from IureDesk.",
            "Guardado en Borradores. No se envió: el usuario lo revisa y lo envía desde IureDesk."),
    })))
}

// ------------------------------------------------------------------ agenda

fn agenda(store: &Store, a: &Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
    let tz = zone_arg(a, ctx)?;
    let from = match str_arg(a, "desde") {
        Some(s) => parse_time(s, tz, ctx)?.0,
        None => local(ctx.now.with_timezone(&tz).date_naive().and_time(NaiveTime::MIN), tz, ctx)?,
    };
    let to = match str_arg(a, "hasta") {
        Some(s) => parse_time(s, tz, ctx)?.0,
        None => from + Duration::days(7),
    };
    if to <= from {
        return Err(ToolError::Failed(
            ctx.t("«hasta» must be after «desde».", "«hasta» debe ser posterior a «desde»."),
        ));
    }
    if to - from > Duration::days(MAX_AGENDA_DAYS) {
        return Err(ToolError::Failed(ctx.t(
            format!("The range is limited to {MAX_AGENDA_DAYS} days."),
            format!("El rango está limitado a {MAX_AGENDA_DAYS} días."),
        )));
    }
    let names: HashMap<CalendarId, String> =
        store.calendars().map_err(|e| store_err(ctx, e))?.into_iter().map(|c| (c.id, c.name)).collect();
    let occ = iuredesk_pim_sync::occurrences_between(store, &[], from, to, tz).map_err(|e| store_err(ctx, e))?;
    let total = occ.len();
    let events: Vec<Value> = occ
        .iter()
        .filter(|o| o.status != EventStatus::Cancelled)
        .take(MAX_AGENDA_ITEMS)
        .map(|o| {
            json!({
                "event_id": o.event.get(),
                "calendar": names.get(&o.calendar).cloned().unwrap_or_default(),
                "title": o.summary,
                "start": fmt_time(&o.start, tz),
                "end": fmt_time(&o.end, tz),
                "all_day": o.is_all_day(),
                "location": o.location,
                "tentative": o.status == EventStatus::Tentative,
                "recurring": o.recurring,
            })
        })
        .collect();
    Ok(ToolOutput::Data(json!({
        "from": from.with_timezone(&tz).to_rfc3339(),
        "to": to.with_timezone(&tz).to_rfc3339(),
        "zone": tz.name(),
        "count": events.len(),
        "truncated": total > MAX_AGENDA_ITEMS,
        "events": events,
    })))
}

fn crear_evento(store: &Store, a: &Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
    let tz = zone_arg(a, ctx)?;
    let title = str_arg(a, "titulo").ok_or_else(|| ToolError::Failed(ctx.t("Missing «titulo».", "Falta «titulo».")))?;
    let start_s =
        str_arg(a, "inicio").ok_or_else(|| ToolError::Failed(ctx.t("Missing «inicio».", "Falta «inicio».")))?;
    let (start, start_date) = parse_time(start_s, tz, ctx)?;
    let (end, end_date) = match str_arg(a, "fin") {
        Some(s) => parse_time(s, tz, ctx)?,
        None => match start_date {
            Some(d) => (start + Duration::days(1), d.succ_opt()),
            None => (start + Duration::hours(1), None),
        },
    };
    let (start_t, end_t) = match (start_date, end_date) {
        (Some(s), Some(e)) => {
            // Fin de día completo exclusivo; «fin» = mismo día se entiende como ese día entero.
            let e = if e <= s { s.succ_opt().unwrap_or(s) } else { e };
            if e.signed_duration_since(s).num_days() > 366 {
                return Err(ToolError::Failed(ctx.t("The event is too long.", "El evento es demasiado largo.")));
            }
            (EventTime::Date(s), EventTime::Date(e))
        }
        _ => {
            if end <= start {
                return Err(ToolError::Failed(
                    ctx.t("«fin» must be after «inicio».", "«fin» debe ser posterior a «inicio»."),
                ));
            }
            if end - start > Duration::days(366) {
                return Err(ToolError::Failed(ctx.t("The event is too long.", "El evento es demasiado largo.")));
            }
            let tzid = (tz != Tz::UTC).then(|| tz.name().to_string());
            (EventTime::DateTime { utc: start, tzid: tzid.clone() }, EventTime::DateTime { utc: end, tzid })
        }
    };
    let calendars = store.calendars().map_err(|e| store_err(ctx, e))?;
    let cal = match id_arg(a, "calendario_id") {
        Some(id) => calendars.iter().find(|c| c.id.get() == id),
        None => calendars.iter().find(|c| !c.read_only),
    }
    .ok_or_else(|| ToolError::Failed(ctx.t("There is no writable calendar.", "No hay un calendario con escritura.")))?;
    if cal.read_only {
        return Err(ToolError::Failed(ctx.t("That calendar is read-only.", "Ese calendario es de solo lectura.")));
    }
    let ev = Event {
        calendar: cal.id,
        summary: cut(title, MAX_LINE_CHARS).0,
        location: str_arg(a, "lugar").map(|s| cut(s, MAX_LINE_CHARS).0).unwrap_or_default(),
        description: str_arg(a, "descripcion").map(|s| cut(s, MAX_WRITE_CHARS).0).unwrap_or_default(),
        start: start_t.clone(),
        end: end_t.clone(),
        ..Event::default()
    };
    let id = iuredesk_pim_sync::ops::save_event(store, &ev).map_err(|e| {
        ToolError::Failed(
            ctx.t(format!("The event could not be created: {e}"), format!("No se pudo crear el evento: {e}")),
        )
    })?;
    Ok(ToolOutput::Data(json!({
        "event_id": id.get(),
        "calendar": cal.name,
        "title": ev.summary,
        "start": fmt_time(&start_t, tz),
        "end": fmt_time(&end_t, tz),
        "all_day": start_t.is_all_day(),
    })))
}

// ------------------------------------------------------------------ contactos, notas, tareas

fn buscar_contacto(store: &Store, a: &Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
    let q = str_arg(a, "q").ok_or_else(|| ToolError::Failed(ctx.t("Missing «q».", "Falta «q».")))?;
    let limit = limit_arg(a, "limit", 10, 50);
    let found = store.find_contacts(&cut(q, MAX_LINE_CHARS).0, limit).map_err(|e| store_err(ctx, e))?;
    let list: Vec<Value> = found
        .iter()
        .map(|c| {
            json!({
                "id": c.id.get(),
                "name": c.full_name,
                "emails": c.emails.iter().take(10).map(|e| json!({"label": e.label, "value": e.value})).collect::<Vec<_>>(),
                "phones": c.phones.iter().take(10).map(|e| json!({"label": e.label, "value": e.value})).collect::<Vec<_>>(),
                "org": c.org,
                "title": c.title,
                "birthday": c.birthday.map(|d| d.format("%Y-%m-%d").to_string()),
            })
        })
        .collect();
    Ok(ToolOutput::Data(json!({"count": list.len(), "contacts": list})))
}

fn crear_nota(store: &Store, a: &Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
    let title = str_arg(a, "title").ok_or_else(|| ToolError::Failed(ctx.t("Missing «title».", "Falta «title».")))?;
    let body = a.get("markdown").and_then(Value::as_str).unwrap_or("");
    if body.chars().count() > MAX_WRITE_CHARS {
        return Err(ToolError::Failed(ctx.t("The note is too long.", "La nota es demasiado larga.")));
    }
    let note = Note {
        title: cut(title, MAX_LINE_CHARS).0,
        body: body.to_string(),
        created: ctx.now,
        updated: ctx.now,
        ..Note::default()
    };
    let id = store.put_note(&note).map_err(|e| store_err(ctx, e))?;
    Ok(ToolOutput::Data(json!({"note_id": id.get(), "title": note.title})))
}

fn mis_tareas(store: &Store, a: &Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
    let all = a.get("incluir_terminadas").and_then(Value::as_bool).unwrap_or(false);
    let limit = limit_arg(a, "limit", 50, 200);
    let mut out = Vec::new();
    for list in store.task_lists().map_err(|e| store_err(ctx, e))? {
        for t in store.tasks(list.id).map_err(|e| store_err(ctx, e))? {
            if all || !matches!(t.status, TaskStatus::Completed | TaskStatus::Cancelled) {
                out.push((list.name.clone(), t));
            }
        }
    }
    // Por vencimiento (sin fecha al final), luego prioridad (1 = alta; 0 = sin prioridad).
    out.sort_by_key(|(_, t)| {
        (
            t.due.as_ref().map(EventTime::to_utc).unwrap_or(DateTime::<Utc>::MAX_UTC),
            if t.priority == 0 { 10 } else { t.priority },
        )
    });
    let total = out.len();
    let tasks: Vec<Value> = out
        .iter()
        .take(limit)
        .map(|(list, t)| {
            json!({
                "id": t.id.get(),
                "list": list,
                "title": t.summary,
                "due": t.due.as_ref().map(|d| fmt_time(d, ctx.zone)),
                "overdue": t.due.as_ref().is_some_and(|d| d.to_utc() < ctx.now) && !t.is_done(),
                "priority": t.priority,
                "status": format!("{:?}", t.status),
                "percent": t.percent_complete,
                "notes": cut(&t.description, 300).0,
            })
        })
        .collect();
    Ok(ToolOutput::Data(json!({"count": tasks.len(), "total": total, "tasks": tasks})))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ToolContext {
        ToolContext { now: Utc::now(), spanish: true, zone: Tz::UTC }
    }

    #[test]
    fn fechas() {
        let c = ctx();
        let tz: Tz = "America/Mexico_City".parse().unwrap();
        let (t, d) = parse_time("2026-03-10T09:30", tz, &c).unwrap();
        assert_eq!(t.to_rfc3339(), "2026-03-10T15:30:00+00:00");
        assert!(d.is_none());
        let (_, d) = parse_time("2026-03-10", tz, &c).unwrap();
        assert_eq!(d, NaiveDate::from_ymd_opt(2026, 3, 10));
        assert!(parse_time("2026-03-10T10:00:00Z", tz, &c).is_ok());
        assert!(parse_time("mañana", tz, &c).is_err());
        // Hora inexistente (cambio de horario en Madrid): se corre, no falla.
        let mad: Tz = "Europe/Madrid".parse().unwrap();
        assert!(parse_time("2026-03-29T02:30", mad, &c).is_ok());
    }

    #[test]
    fn ids_y_lista() {
        assert_eq!(id_arg(&json!({"x": 5}), "x"), Some(5));
        assert_eq!(id_arg(&json!({"x": "thr:7"}), "x"), Some(7));
        assert_eq!(id_arg(&json!({"x": [1]}), "x"), None);
        let tools = tool_list();
        let names: Vec<&str> = tools.as_array().unwrap().iter().filter_map(|t| t["name"].as_str()).collect();
        assert_eq!(names, TOOL_NAMES);
        assert!(tools.as_array().unwrap().iter().all(|t| t["description"].as_str().unwrap().contains(" · ")));
    }

    #[test]
    fn html_a_texto() {
        assert_eq!(strip_html("<p>Hola&nbsp;<b>Ana</b></p>").trim(), "Hola  Ana");
    }
}
