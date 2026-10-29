//! «Mi día» (F6.1): el resumen del día con el que abre IureDesk.
//!
//! Dos pasos, los dos fuera del hilo de la interfaz:
//! - [`gather`] lee del almacén lo necesario (tareas de todas las fuentes, citas de hoy y de
//!   mañana, sobres recientes de entrada y enviados, marcados, cumpleaños);
//! - [`build`] es puro (sin E/S, con un «ahora» dado) y arma el [`DaySummary`]: tareas de todas
//!   las fuentes mezcladas con su insignia, vencidas/hoy/próximas, correo que espera respuesta,
//!   enlace para unirse a la reunión, cumpleaños.
//!
//! La cuenta atrás de la próxima cita ([`DaySummary::next_event`]) se calcula al pintar con la
//! hora de ese momento, así la línea de «ahora» avanza aunque el resumen tenga un minuto.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};
use iuredesk_model::{
    AccountId, Calendar, ContactId, EventId, EventTime, Folder, FolderId, FolderRole, MessageEnvelope, MessageId, Task,
    TaskList, TaskListId,
};
use iuredesk_store::{AccountLabel, Store};

use crate::calendar::Occurrence;
use crate::iure::FeedItem;
use crate::lang::{Text, pick};
use crate::session::{IURE_CALENDAR_PATH, IURE_TASKS_PATH};
use crate::time::Zone;

/// Un correo «espera respuesta» si lleva al menos esto sin contestar…
const NEEDS_REPLY_AFTER_HOURS: i64 = 24;
/// …y no es más viejo que esto (lo antiguo ya no es «de hoy»).
const NEEDS_REPLY_WINDOW_DAYS: i64 = 10;
/// Sobres que se leen por carpeta (entrada y enviados).
const MAIL_SCAN: usize = 400;
/// Elementos como máximo por grupo.
const MAX_ITEMS: usize = 50;
/// «Próximas»: tareas que vencen en estos días.
const UPCOMING_DAYS: i64 = 7;
/// Cumpleaños que se anuncian con estos días de antelación.
const BIRTHDAY_DAYS: i64 = 14;
/// Tareas que se leen como máximo (todas las listas).
const MAX_TASKS: usize = 5_000;
/// Texto de un evento que se revisa para encontrar el enlace de la reunión.
const MAX_EVENT_TEXT: usize = 20_000;

// ---------------------------------------------------------------------------------------------
// Tipos

/// De dónde viene una tarea.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskSource {
    /// Lista solo local (sin cuenta).
    Local,
    /// Lista de una cuenta (CalDAV `VTODO` o Microsoft To Do, vía `pim-sync`).
    Account { email: String },
    /// Lista de solo lectura copiada de Iurefficient (F3.1).
    Iurefficient,
}

/// Una tarea del día con su origen.
#[derive(Clone, Debug, PartialEq)]
pub struct DayTask {
    pub task: Task,
    pub list: TaskListId,
    pub list_name: String,
    pub source: TaskSource,
    /// Texto de la insignia: «Iurefficient» o el nombre de la lista.
    pub badge: String,
    pub due: Option<NaiveDate>,
    /// Días de retraso (0 si no está vencida).
    pub overdue_days: i64,
}

/// Tareas abiertas agrupadas.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DayTasks {
    pub overdue: Vec<DayTask>,
    pub today: Vec<DayTask>,
    pub upcoming: Vec<DayTask>,
}

impl DayTasks {
    pub fn is_empty(&self) -> bool {
        self.overdue.is_empty() && self.today.is_empty() && self.upcoming.is_empty()
    }
}

/// Enlace para unirse a una reunión en línea.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeetingLink {
    pub url: String,
    /// «Teams», «Google Meet», «Zoom»… (solo texto descriptivo).
    pub provider: &'static str,
}

/// Una cita de la agenda.
#[derive(Clone, Debug, PartialEq)]
pub struct AgendaItem {
    pub occ: Occurrence,
    /// Nombre del calendario.
    pub calendar: String,
    pub join: Option<MeetingLink>,
    /// Viene del calendario «Iurefficient».
    pub iurefficient: bool,
}

/// Estado de la próxima cita respecto a «ahora».
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NextState {
    /// En curso; termina en…
    Now { ends_in: Duration },
    /// Empieza en…
    Soon { starts_in: Duration },
}

impl NextState {
    /// «en 25 min», «ahora · termina en 20 min».
    pub fn label(&self) -> Text {
        match self {
            NextState::Soon { starts_in } => {
                let (en, es) = span(*starts_in);
                if starts_in.num_seconds() <= 0 {
                    Text::new("now", "ahora")
                } else {
                    Text::new(format!("in {en}"), format!("en {es}"))
                }
            }
            NextState::Now { ends_in } => {
                let (en, es) = span(*ends_in);
                Text::new(format!("now · ends in {en}"), format!("ahora · termina en {es}"))
            }
        }
    }
}

/// Duración legible: «25 min», «1 h 5 min», «2 h». Se redondea hacia arriba al minuto.
pub fn span(d: Duration) -> (String, String) {
    let secs = d.num_seconds().max(0);
    let mins = secs.saturating_add(59) / 60;
    if mins < 60 {
        let s = format!("{mins} min");
        return (s.clone(), s);
    }
    let (h, m) = (mins / 60, mins % 60);
    let s = if m == 0 { format!("{h} h") } else { format!("{h} h {m} min") };
    (s.clone(), s)
}

/// Un correo destacado en el resumen.
#[derive(Clone, Debug, PartialEq)]
pub struct MailItem {
    pub id: MessageId,
    pub account: AccountId,
    pub folder: FolderId,
    pub from: String,
    pub from_addr: String,
    pub subject: String,
    pub date: DateTime<Utc>,
}

/// Recuento de correo de una cuenta.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccountMail {
    pub account: AccountId,
    pub name: String,
    pub email: String,
    /// No leídos en la bandeja de entrada.
    pub unread: u64,
    /// No leídos importantes (marcados, con la palabra «importante» o de un contacto conocido).
    pub important: u64,
    pub inbox: Option<FolderId>,
}

/// Un cumpleaños próximo.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Birthday {
    pub contact: ContactId,
    pub name: String,
    /// Fecha de este año (o del siguiente).
    pub date: NaiveDate,
    pub in_days: i64,
    /// Años que cumple, si se conoce el año de nacimiento.
    pub turns: Option<i32>,
}

/// Lo de Iurefficient (solo con sesión lista).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IureDigest {
    pub host: String,
    pub unread: u64,
    /// Sin leer primero; luego los recientes (hasta 6).
    pub items: Vec<FeedItem>,
}

/// El resumen del día.
#[derive(Clone, Debug, PartialEq)]
pub struct DaySummary {
    /// Cuándo se armó.
    pub built: DateTime<Utc>,
    pub date: NaiveDate,
    /// Nombre de pila para el saludo (puede estar vacío).
    pub user: String,
    /// Citas de hoy: días completos primero, luego por hora.
    pub events: Vec<AgendaItem>,
    /// Primera cita de mañana.
    pub tomorrow: Option<AgendaItem>,
    pub tasks: DayTasks,
    pub accounts: Vec<AccountMail>,
    pub needs_reply: Vec<MailItem>,
    pub flagged: Vec<MailItem>,
    pub birthdays: Vec<Birthday>,
    pub iure: Option<IureDigest>,
}

impl DaySummary {
    /// La cita en curso o la siguiente (con hora) y cuánto falta. `None` si ya no queda nada hoy.
    pub fn next_event(&self, now: DateTime<Utc>) -> Option<(usize, NextState)> {
        self.events.iter().enumerate().find(|(_, e)| !e.occ.all_day && e.occ.end > now).map(|(i, e)| {
            let st = if e.occ.start <= now {
                NextState::Now { ends_in: e.occ.end - now }
            } else {
                NextState::Soon { starts_in: e.occ.start - now }
            };
            (i, st)
        })
    }

    /// No leídos importantes de todas las cuentas.
    pub fn important_unread(&self) -> u64 {
        self.accounts.iter().map(|a| a.important).fold(0u64, u64::saturating_add)
    }

    /// No leídos de todas las cuentas.
    pub fn unread(&self) -> u64 {
        self.accounts.iter().map(|a| a.unread).fold(0u64, u64::saturating_add)
    }
}

/// Saludo según la hora local: «Buenos días» (5–12), «Buenas tardes» (12–20), «Buenas noches».
pub fn greeting(hour: u32) -> Text {
    match hour {
        5..=11 => Text::new("Good morning", "Buenos días"),
        12..=19 => Text::new("Good afternoon", "Buenas tardes"),
        _ => Text::new("Good evening", "Buenas noches"),
    }
}

/// «martes 27 de octubre» / «Tuesday, October 27».
pub fn long_day(d: NaiveDate) -> String {
    let i = d.weekday().num_days_from_monday() as usize;
    let en = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
    let es = ["lunes", "martes", "miércoles", "jueves", "viernes", "sábado", "domingo"];
    let month = crate::time::month_name(d.month());
    if crate::lang::spanish() {
        format!("{} {} de {month}", es.get(i).copied().unwrap_or(""), d.day())
    } else {
        format!("{}, {month} {}", en.get(i).copied().unwrap_or(""), d.day())
    }
}

/// Fecha de vencimiento local de una tarea.
pub fn due_date(due: &EventTime, zone: Zone) -> NaiveDate {
    match due {
        EventTime::Date(d) => *d,
        EventTime::DateTime { utc, .. } => zone.local(*utc).date(),
    }
}

/// Busca el enlace de una reunión en línea (Teams, Meet, Zoom, Webex, Whereby, Jitsi) en los
/// textos de un evento. Solo `https://`; acotado.
pub fn meeting_link(texts: &[&str]) -> Option<MeetingLink> {
    for t in texts {
        let t: String = t.chars().take(MAX_EVENT_TEXT).collect();
        for raw in t.split(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\'' | '(' | ')' | '[' | ']')) {
            let Some(rest) = raw.get(..8).filter(|p| p.eq_ignore_ascii_case("https://")).and(raw.get(8..)) else {
                continue;
            };
            let url = raw.trim_end_matches(['.', ',', ';', ':', '!', '?']);
            if url.len() > 2_000 || rest.is_empty() {
                continue;
            }
            let host = rest.split(['/', '?', '#']).next().unwrap_or("").to_ascii_lowercase();
            let host = host.rsplit('@').next().unwrap_or("");
            let host = host.split(':').next().unwrap_or("");
            let is = |d: &str| host == d || host.ends_with(&format!(".{d}"));
            let provider = if is("teams.microsoft.com") || is("teams.live.com") {
                "Teams"
            } else if is("meet.google.com") {
                "Google Meet"
            } else if is("zoom.us") || is("zoom.com") {
                "Zoom"
            } else if is("webex.com") {
                "Webex"
            } else if is("whereby.com") {
                "Whereby"
            } else if is("meet.jit.si") {
                "Jitsi"
            } else {
                continue;
            };
            return Some(MeetingLink { url: url.to_string(), provider });
        }
    }
    None
}

/// ¿Remitente automático (boletines, avisos)? No «espera respuesta».
fn is_bulk(addr: &str) -> bool {
    let a = addr.to_ascii_lowercase();
    let local = a.split('@').next().unwrap_or("");
    [
        "noreply",
        "no-reply",
        "donotreply",
        "do-not-reply",
        "newsletter",
        "news",
        "promo",
        "notifications",
        "mailer-daemon",
    ]
    .iter()
    .any(|k| local == *k || local.starts_with(&format!("{k}-")) || local.starts_with(&format!("{k}.")))
        || local.contains("noreply")
        || local.contains("no-reply")
}

fn important_keyword(k: &str) -> bool {
    let k = k.trim().trim_start_matches('$').to_lowercase();
    k == "important" || k == "importante"
}

// ---------------------------------------------------------------------------------------------
// Entrada

/// Lo que lee [`gather`] y usa [`build`] (en pruebas se arma a mano).
#[derive(Clone, Debug, Default)]
pub struct DayInput {
    pub now: DateTime<Utc>,
    pub zone: Zone,
    /// Mis direcciones.
    pub me: Vec<String>,
    pub user: String,
    /// Cuentas con sus no leídos (los importantes los calcula `build`).
    pub accounts: Vec<AccountMail>,
    pub lists: Vec<TaskList>,
    pub tasks: Vec<Task>,
    /// Apariciones de hoy y de mañana.
    pub events: Vec<Occurrence>,
    pub calendars: Vec<Calendar>,
    /// Lugar y descripción de los eventos (para el enlace de la reunión).
    pub event_text: HashMap<EventId, String>,
    /// Sobres recientes de la bandeja de entrada.
    pub inbox: Vec<MessageEnvelope>,
    /// Sobres recientes de enviados.
    pub sent: Vec<MessageEnvelope>,
    /// Marcados para seguimiento (fuera de papelera y no deseado).
    pub flagged: Vec<MessageEnvelope>,
    /// Direcciones de mis contactos (minúsculas).
    pub known: HashSet<String>,
    /// (contacto, nombre, fecha de nacimiento).
    pub birthdays: Vec<(ContactId, String, NaiveDate)>,
    pub iure: Option<IureDigest>,
}

fn mail_item(e: &MessageEnvelope) -> MailItem {
    let from = e.from.first();
    MailItem {
        id: e.id,
        account: e.account,
        folder: e.folder,
        from: from.map(|m| m.display().to_string()).unwrap_or_default(),
        from_addr: from.map(|m| m.addr.clone()).unwrap_or_default(),
        subject: e.subject.chars().take(300).collect(),
        date: e.date,
    }
}

/// Arma el resumen (puro).
pub fn build(i: &DayInput) -> DaySummary {
    let today = i.zone.local(i.now).date();
    let tomorrow = today.succ_opt().unwrap_or(today);
    let me: HashSet<String> = i.me.iter().map(|a| a.trim().to_lowercase()).filter(|a| !a.is_empty()).collect();
    let from_me = |e: &MessageEnvelope| e.from.iter().any(|m| me.contains(&m.addr.to_lowercase()));

    // --- Tareas de todas las fuentes.
    let mut tasks = DayTasks::default();
    let labels: HashMap<AccountId, &AccountMail> = i.accounts.iter().map(|a| (a.account, a)).collect();
    for t in
        i.tasks.iter().take(MAX_TASKS).filter(|t| !t.is_done() && t.status != iuredesk_model::TaskStatus::Cancelled)
    {
        let Some(due) = t.due.as_ref().map(|d| due_date(d, i.zone)) else { continue };
        if due > today + Duration::days(UPCOMING_DAYS) {
            continue;
        }
        let list = i.lists.iter().find(|l| l.id == t.list);
        let list_name = list.map(|l| l.name.clone()).unwrap_or_default();
        let source = match list {
            Some(l) if l.remote_path == IURE_TASKS_PATH => TaskSource::Iurefficient,
            Some(l) => match l.account {
                Some(a) => TaskSource::Account { email: labels.get(&a).map(|x| x.email.clone()).unwrap_or_default() },
                None => TaskSource::Local,
            },
            None => TaskSource::Local,
        };
        let google = list.is_some_and(|l| l.remote_path.starts_with(iuredesk_pim_sync::GOOGLE_TASKS_LIST_PREFIX));
        let badge = match &source {
            TaskSource::Iurefficient => "Iurefficient".to_string(),
            // F7.1: Google Tasks (la lista suele llamarse «Mis tareas», como las demás).
            _ if google && !list_name.is_empty() => format!("Google · {list_name}"),
            _ if google => "Google".to_string(),
            _ if !list_name.is_empty() => list_name.clone(),
            _ => pick("Tasks", "Tareas").to_string(),
        };
        let dt = DayTask {
            task: t.clone(),
            list: t.list,
            list_name,
            source,
            badge,
            due: Some(due),
            overdue_days: (today - due).num_days().max(0),
        };
        if due < today {
            tasks.overdue.push(dt);
        } else if due == today {
            tasks.today.push(dt);
        } else {
            tasks.upcoming.push(dt);
        }
    }
    // Prioridad 1 (alta) primero; 0 = sin prioridad, al final.
    let prio = |t: &DayTask| if t.task.priority == 0 { 10 } else { t.task.priority };
    tasks.overdue.sort_by(|a, b| a.due.cmp(&b.due).then(prio(a).cmp(&prio(b))));
    tasks.today.sort_by(|a, b| prio(a).cmp(&prio(b)).then_with(|| a.task.summary.cmp(&b.task.summary)));
    tasks.upcoming.sort_by(|a, b| a.due.cmp(&b.due).then(prio(a).cmp(&prio(b))));
    for g in [&mut tasks.overdue, &mut tasks.today, &mut tasks.upcoming] {
        g.truncate(MAX_ITEMS);
    }

    // --- Agenda.
    let cal = |id| i.calendars.iter().find(|c| c.id == id);
    let item = |o: &Occurrence| {
        let text = i.event_text.get(&o.id).map(String::as_str).unwrap_or("");
        AgendaItem {
            occ: o.clone(),
            calendar: cal(o.calendar).map(|c| c.name.clone()).unwrap_or_default(),
            join: meeting_link(&[&o.location, text]),
            iurefficient: cal(o.calendar).is_some_and(|c| c.remote_path == IURE_CALENDAR_PATH),
        }
    };
    let (day_start, day_end, next_end) = (
        i.zone.day_start(today),
        i.zone.day_start(tomorrow),
        i.zone.day_start(tomorrow.succ_opt().unwrap_or(tomorrow)),
    );
    let on = |o: &Occurrence, from: DateTime<Utc>, to: DateTime<Utc>, d: NaiveDate| match o.days {
        Some((a, b)) if o.all_day => a <= d && d < b,
        _ => o.start < to && (o.end > from || (o.end == o.start && o.start >= from)),
    };
    let mut events: Vec<AgendaItem> =
        i.events.iter().filter(|o| !o.cancelled && on(o, day_start, day_end, today)).take(200).map(item).collect();
    events.sort_by(|a, b| b.occ.all_day.cmp(&a.occ.all_day).then(a.occ.start.cmp(&b.occ.start)));
    let mut next_day: Vec<&Occurrence> = i
        .events
        .iter()
        .filter(|o| !o.cancelled && on(o, day_end, next_end, tomorrow) && (o.all_day || o.start >= day_end))
        .collect();
    next_day.sort_by(|a, b| a.all_day.cmp(&b.all_day).then(a.start.cmp(&b.start)));
    let tomorrow_first = next_day.first().map(|o| item(o));

    // --- Correo: importantes, espera respuesta, marcados.
    let mut accounts = i.accounts.clone();
    for a in &mut accounts {
        a.important = i
            .inbox
            .iter()
            .filter(|e| e.account == a.account && !e.flags.seen && !e.flags.deleted)
            .filter(|e| {
                e.flags.flagged
                    || e.keywords.iter().any(|k| important_keyword(k))
                    || e.from.iter().any(|m| i.known.contains(&m.addr.to_lowercase()))
            })
            .count() as u64;
        a.important = a.important.min(a.unread);
    }
    let needs_reply = needs_reply(i, &me, &from_me);
    let mut flagged: Vec<MailItem> = i.flagged.iter().filter(|e| !e.flags.deleted).map(mail_item).collect();
    flagged.sort_by_key(|m| std::cmp::Reverse(m.date));
    flagged.dedup_by_key(|m| m.id);
    flagged.truncate(MAX_ITEMS);

    // --- Cumpleaños.
    let mut birthdays: Vec<Birthday> = i
        .birthdays
        .iter()
        .take(10_000)
        .filter_map(|(id, name, born)| {
            let next = next_birthday(*born, today)?;
            let in_days = (next - today).num_days();
            (in_days <= BIRTHDAY_DAYS).then(|| Birthday {
                contact: *id,
                name: name.clone(),
                date: next,
                in_days,
                // 1604 es el «sin año» de vCard 3; antes de 1900, dato dudoso.
                turns: (born.year() > 1900 && born.year() <= next.year()).then(|| next.year() - born.year()),
            })
        })
        .collect();
    birthdays.sort_by(|a, b| a.in_days.cmp(&b.in_days).then_with(|| a.name.cmp(&b.name)));
    birthdays.truncate(MAX_ITEMS);

    DaySummary {
        built: i.now,
        date: today,
        user: i.user.clone(),
        events,
        tomorrow: tomorrow_first,
        tasks,
        accounts,
        needs_reply,
        flagged,
        birthdays,
        iure: i.iure.clone(),
    }
}

/// El próximo cumpleaños a partir de `today` (el 29 de febrero se celebra el 28 en años no
/// bisiestos).
fn next_birthday(born: NaiveDate, today: NaiveDate) -> Option<NaiveDate> {
    let on = |y: i32| {
        NaiveDate::from_ymd_opt(y, born.month(), born.day())
            .or_else(|| NaiveDate::from_ymd_opt(y, born.month(), born.day().saturating_sub(1)))
    };
    let this = on(today.year())?;
    if this >= today { Some(this) } else { on(today.year().checked_add(1)?) }
}

/// Hilos cuyo último mensaje es para mí, sin responder, de hace más de un día (y menos de diez
/// días). Lo más reciente primero.
fn needs_reply(i: &DayInput, me: &HashSet<String>, from_me: &dyn Fn(&MessageEnvelope) -> bool) -> Vec<MailItem> {
    if me.is_empty() {
        return Vec::new();
    }
    let key = |e: &MessageEnvelope| match (e.thread, &e.message_id) {
        (Some(t), _) => format!("t{}", t.get()),
        (None, Some(m)) => format!("m{m}"),
        (None, None) => format!("i{}", e.id.get()),
    };
    // Lo último de cada hilo (entrada y enviados) y si es mío.
    let mut last: HashMap<String, (DateTime<Utc>, bool)> = HashMap::new();
    // Respuestas mías por `In-Reply-To`/`References` (hilos sin id local).
    let mut replied_ids: HashMap<String, DateTime<Utc>> = HashMap::new();
    for e in i.inbox.iter().chain(i.sent.iter()) {
        let mine = from_me(e);
        let k = key(e);
        let cur = last.entry(k).or_insert((e.date, mine));
        if e.date > cur.0 {
            *cur = (e.date, mine);
        }
        if mine {
            for r in e.in_reply_to.iter().chain(e.references.iter()).take(100) {
                let d = replied_ids.entry(r.clone()).or_insert(e.date);
                if e.date > *d {
                    *d = e.date;
                }
            }
        }
    }
    let (oldest, newest) =
        (i.now - Duration::days(NEEDS_REPLY_WINDOW_DAYS), i.now - Duration::hours(NEEDS_REPLY_AFTER_HOURS));
    let mut seen = HashSet::new();
    let mut out: Vec<MailItem> = Vec::new();
    let mut inbox: Vec<&MessageEnvelope> = i.inbox.iter().collect();
    inbox.sort_by_key(|e| std::cmp::Reverse(e.date));
    for e in inbox {
        if from_me(e) || e.flags.answered || e.flags.draft || e.flags.deleted {
            continue;
        }
        if e.date < oldest || e.date > newest {
            continue;
        }
        if !e.to.iter().any(|m| me.contains(&m.addr.to_lowercase())) {
            continue;
        }
        if e.from.iter().any(|m| is_bulk(&m.addr)) {
            continue;
        }
        let k = key(e);
        if !seen.insert(k.clone()) {
            continue;
        }
        // Debe ser lo último del hilo.
        if last.get(&k).is_some_and(|(d, _)| *d > e.date) {
            continue;
        }
        if e.message_id.as_ref().and_then(|m| replied_ids.get(m)).is_some_and(|d| *d > e.date) {
            continue;
        }
        out.push(mail_item(e));
        if out.len() >= MAX_ITEMS {
            break;
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Lectura del almacén

/// Lo que la sesión sabe y el almacén no (se copia al lanzar el trabajo de fondo).
#[derive(Clone, Debug, Default)]
pub struct GatherArgs {
    pub now: DateTime<Utc>,
    pub zone: Zone,
    pub me: Vec<String>,
    pub user: String,
    pub labels: Vec<AccountLabel>,
    pub folders: Vec<Folder>,
    pub calendars: Vec<Calendar>,
    /// Calendarios ocultos por el usuario.
    pub hidden: Vec<u64>,
    pub iure: Option<IureDigest>,
}

/// Lee del almacén lo que necesita [`build`]. Lento (segundo plano); nunca falla: lo que no se
/// pueda leer queda vacío.
pub fn gather(store: &Store, a: GatherArgs) -> DayInput {
    let today = a.zone.local(a.now).date();
    let from = a.zone.day_start(today);
    let to = a.zone.day_start(today + Duration::days(2));
    let color = |cal: iuredesk_model::CalendarId| {
        a.calendars.iter().find(|c| c.id == cal).and_then(|c| c.color).unwrap_or(0x3B82F6)
    };
    let tz = crate::time::user_tz(a.zone);
    let mut events: Vec<Occurrence> = match iuredesk_pim_sync::occurrences_between(store, &[], from, to, tz) {
        Ok(list) => list.iter().map(|o| crate::calendar::from_pim(o, color(o.calendar), tz)).collect(),
        Err(_) => {
            let evs = store.events_between(&[], from, to).unwrap_or_default();
            crate::calendar::occurrences(&evs, from, to, |e| color(e.calendar))
        }
    };
    events.retain(|o| !a.hidden.contains(&o.calendar.get()));
    events.truncate(500);
    let mut event_text = HashMap::new();
    for o in events.iter().take(200) {
        if event_text.contains_key(&o.id) {
            continue;
        }
        if let Ok(Some(e)) = store.get_event(o.id) {
            let mut t = e.description.chars().take(MAX_EVENT_TEXT).collect::<String>();
            t.push('\n');
            t.push_str(&e.location);
            event_text.insert(o.id, t);
        }
    }

    let lists = store.task_lists().unwrap_or_default();
    let mut tasks = Vec::new();
    for l in lists.iter().take(500) {
        tasks.extend(store.tasks(l.id).unwrap_or_default());
        if tasks.len() >= MAX_TASKS {
            break;
        }
    }

    let role = |r: FolderRole| a.folders.iter().filter(move |f| f.role == r);
    let mut inbox = Vec::new();
    let mut accounts: Vec<AccountMail> = a
        .labels
        .iter()
        .map(|l| AccountMail {
            account: l.id,
            name: l.name.clone(),
            email: l.email.clone(),
            unread: 0,
            important: 0,
            inbox: role(FolderRole::Inbox).find(|f| f.account == l.id).map(|f| f.id),
        })
        .collect();
    for f in role(FolderRole::Inbox).take(50) {
        let unread = store.count(f.id).map(|c| c.unread).unwrap_or(0);
        if let Some(acc) = accounts.iter_mut().find(|x| x.account == f.account) {
            acc.unread = acc.unread.saturating_add(unread);
        }
        inbox.extend(store.list_envelopes(f.id, 0, MAIL_SCAN).unwrap_or_default());
    }
    let mut sent = Vec::new();
    for f in role(FolderRole::Sent).take(50) {
        sent.extend(store.list_envelopes(f.id, 0, MAIL_SCAN).unwrap_or_default());
    }
    let skip: HashSet<FolderId> = a
        .folders
        .iter()
        .filter(|f| matches!(f.role, FolderRole::Trash | FolderRole::Junk | FolderRole::Drafts))
        .map(|f| f.id)
        .collect();
    let flagged: Vec<MessageEnvelope> =
        store.search("is:flagged", 200).unwrap_or_default().into_iter().filter(|e| !skip.contains(&e.folder)).collect();

    let mut known = HashSet::new();
    let mut birthdays = Vec::new();
    for b in store.address_books().unwrap_or_default().into_iter().take(100) {
        for c in store.contacts(b.id).unwrap_or_default().into_iter().take(50_000) {
            for e in c.emails.iter().take(20) {
                known.insert(e.value.trim().to_lowercase());
            }
            if let Some(d) = c.birthday {
                let name = if c.full_name.trim().is_empty() {
                    c.emails.first().map(|e| e.value.clone()).unwrap_or_default()
                } else {
                    c.full_name.clone()
                };
                birthdays.push((c.id, name, d));
            }
        }
    }
    DayInput {
        now: a.now,
        zone: a.zone,
        me: a.me,
        user: a.user,
        accounts,
        lists,
        tasks,
        events,
        calendars: a.calendars,
        event_text,
        inbox,
        sent,
        flagged,
        known,
        birthdays,
        iure: a.iure,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use iuredesk_model::{CalendarId, Flags, Mailbox, TaskId, TaskStatus, ThreadId};

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 27, 9, 35, 0).unwrap()
    }

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn list(id: u64, name: &str, account: Option<u64>, path: &str) -> TaskList {
        TaskList {
            id: TaskListId(id),
            account: account.map(AccountId),
            name: name.into(),
            remote_path: path.into(),
            sync_token: None,
        }
    }

    fn task(id: u64, list: u64, summary: &str, due: Option<NaiveDate>, priority: u8) -> Task {
        Task {
            id: TaskId(id),
            list: TaskListId(list),
            summary: summary.into(),
            due: due.map(EventTime::Date),
            priority,
            ..Default::default()
        }
    }

    fn occ(id: u64, cal: u64, title: &str, h: u32, m: u32, mins: i64) -> Occurrence {
        let start = Utc.with_ymd_and_hms(2026, 10, 27, h, m, 0).unwrap();
        Occurrence {
            id: EventId(id),
            summary: title.into(),
            location: String::new(),
            start,
            end: start + Duration::minutes(mins),
            all_day: false,
            days: None,
            color: 0,
            tentative: false,
            recurring: false,
            calendar: CalendarId(cal),
            recurrence_id: None,
            busy: true,
            cancelled: false,
        }
    }

    fn msg(id: u64, thread: u64, from: &str, to: &str, hours_ago: i64) -> MessageEnvelope {
        MessageEnvelope {
            id: MessageId(id),
            account: AccountId(1),
            thread: Some(ThreadId(thread)),
            message_id: Some(format!("{id}@x")),
            from: vec![Mailbox::new("", from)],
            to: vec![Mailbox::new("", to)],
            subject: format!("asunto {id}"),
            date: now() - Duration::hours(hours_ago),
            flags: Flags { seen: true, ..Default::default() },
            ..Default::default()
        }
    }

    fn input() -> DayInput {
        DayInput {
            now: now(),
            zone: Zone::utc(),
            me: vec!["yo@despacho.mx".into()],
            user: "Eduardo".into(),
            accounts: vec![AccountMail {
                account: AccountId(1),
                name: "Eduardo".into(),
                email: "yo@despacho.mx".into(),
                unread: 3,
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn task_sources_are_merged_with_badges_and_grouped() {
        let mut i = input();
        i.lists = vec![
            list(1, "Despacho", Some(1), "/caldav/despacho/"),
            list(2, "Iurefficient", Some(99), IURE_TASKS_PATH),
            list(3, "Personal", None, ""),
            list(4, "Mis tareas", Some(1), "gtasks:MDk"),
        ];
        let mut done = task(9, 1, "Ya hecha", Some(d(2026, 10, 27)), 1);
        done.status = TaskStatus::Completed;
        i.tasks = vec![
            task(1, 1, "Escrito 241/2026", Some(d(2026, 10, 27)), 5),
            task(2, 2, "Revisar contestación", Some(d(2026, 10, 25)), 1),
            task(3, 3, "Comprar regalo", Some(d(2026, 10, 30)), 0),
            task(4, 1, "Lejana", Some(d(2026, 12, 1)), 1),
            task(5, 3, "Sin fecha", None, 1),
            task(6, 2, "Urgente hoy", Some(d(2026, 10, 27)), 1),
            task(7, 4, "Pagar renta", Some(d(2026, 10, 29)), 0),
            done,
        ];
        let s = build(&i);
        assert_eq!(s.tasks.overdue.len(), 1);
        let o = &s.tasks.overdue[0];
        assert_eq!((o.task.id, o.overdue_days, o.badge.as_str()), (TaskId(2), 2, "Iurefficient"));
        assert_eq!(o.source, TaskSource::Iurefficient);
        // Hoy: la de prioridad alta primero, sin importar la fuente.
        let today: Vec<_> = s.tasks.today.iter().map(|t| (t.task.id, t.badge.as_str())).collect();
        assert_eq!(today, vec![(TaskId(6), "Iurefficient"), (TaskId(1), "Despacho")]);
        assert_eq!(s.tasks.today[1].source, TaskSource::Account { email: "yo@despacho.mx".into() });
        assert_eq!(s.tasks.upcoming.len(), 2, "las de más de una semana y sin fecha no entran");
        assert_eq!(s.tasks.upcoming[0].source, TaskSource::Account { email: "yo@despacho.mx".into() });
        assert_eq!(s.tasks.upcoming[0].badge, "Google · Mis tareas", "F7.1: insignia de Google Tasks");
        assert_eq!(s.tasks.upcoming[1].source, TaskSource::Local);
        assert!(!s.tasks.is_empty());
    }

    #[test]
    fn overdue_uses_the_local_day() {
        // 23:30 del 26 en UTC-6 es el 26 local, aunque en UTC ya sea el 27.
        let mut i = input();
        i.now = Utc.with_ymd_and_hms(2026, 10, 27, 5, 30, 0).unwrap();
        i.zone = Zone::Fixed(chrono::FixedOffset::west_opt(6 * 3600).unwrap());
        i.lists = vec![list(1, "L", None, "")];
        i.tasks = vec![
            task(1, 1, "Vence el 26", Some(d(2026, 10, 26)), 0),
            task(2, 1, "Vence el 25", Some(d(2026, 10, 25)), 0),
        ];
        let s = build(&i);
        assert_eq!(s.date, d(2026, 10, 26));
        assert_eq!(s.tasks.today.len(), 1);
        assert_eq!(s.tasks.overdue.len(), 1);
        assert_eq!(s.tasks.overdue[0].overdue_days, 1);
    }

    #[test]
    fn next_event_countdown() {
        let mut i = input();
        i.events = vec![
            occ(1, 1, "Junta", 8, 30, 30),
            occ(2, 1, "Llamada con Northwind", 10, 0, 60),
            occ(3, 1, "Comida", 14, 0, 90),
        ];
        let s = build(&i);
        let (idx, st) = s.next_event(now()).unwrap();
        assert_eq!(s.events[idx].occ.summary, "Llamada con Northwind");
        assert_eq!(st, NextState::Soon { starts_in: Duration::minutes(25) });
        crate::lang::set_lang(crate::lang::Lang::Es);
        assert_eq!(st.label().es, "en 25 min");
        assert_eq!(st.label().en, "in 25 min");
        // Durante la llamada: en curso.
        let during = Utc.with_ymd_and_hms(2026, 10, 27, 10, 40, 0).unwrap();
        let (idx, st) = s.next_event(during).unwrap();
        assert_eq!(idx, 1);
        assert_eq!(st, NextState::Now { ends_in: Duration::minutes(20) });
        assert_eq!(st.label().es, "ahora · termina en 20 min");
        // Después de la última: nada.
        assert!(s.next_event(Utc.with_ymd_and_hms(2026, 10, 27, 16, 0, 0).unwrap()).is_none());
        assert_eq!(span(Duration::minutes(65)).1, "1 h 5 min");
        assert_eq!(span(Duration::minutes(120)).1, "2 h");
        assert_eq!(span(Duration::seconds(61)).1, "2 min", "se redondea hacia arriba");
        assert_eq!(span(Duration::seconds(-5)).1, "0 min");
    }

    #[test]
    fn agenda_keeps_today_and_previews_tomorrow() {
        let mut i = input();
        let mut allday = occ(10, 1, "Cumpleaños de mamá", 0, 0, 24 * 60);
        allday.all_day = true;
        allday.days = Some((d(2026, 10, 27), d(2026, 10, 28)));
        let mut cancelled = occ(11, 1, "Cancelada", 12, 0, 30);
        cancelled.cancelled = true;
        let mut tomorrow = occ(12, 1, "Audiencia", 9, 0, 60);
        tomorrow.start += Duration::days(1);
        tomorrow.end += Duration::days(1);
        let mut team = occ(13, 2, "Revisión", 16, 0, 30);
        team.location = "https://teams.microsoft.com/l/meetup-join/19%3a123".into();
        i.events = vec![occ(1, 1, "Junta", 8, 30, 30), allday, cancelled, tomorrow, team];
        i.calendars = vec![
            Calendar { id: CalendarId(1), name: "Trabajo".into(), ..Default::default() },
            Calendar {
                id: CalendarId(2),
                name: "Iurefficient".into(),
                remote_path: IURE_CALENDAR_PATH.into(),
                ..Default::default()
            },
        ];
        let s = build(&i);
        let names: Vec<_> = s.events.iter().map(|e| e.occ.summary.as_str()).collect();
        assert_eq!(names, vec!["Cumpleaños de mamá", "Junta", "Revisión"]);
        assert_eq!(s.tomorrow.as_ref().map(|t| t.occ.summary.as_str()), Some("Audiencia"));
        let rev = &s.events[2];
        assert!(rev.iurefficient);
        assert_eq!(rev.join.as_ref().map(|j| j.provider), Some("Teams"));
        assert_eq!(s.events[1].calendar, "Trabajo");
    }

    #[test]
    fn meeting_links_are_detected_safely() {
        let l = meeting_link(&["Sala 2", "Únete: <https://us02web.zoom.us/j/8123?pwd=x>."]).unwrap();
        assert_eq!((l.provider, l.url.as_str()), ("Zoom", "https://us02web.zoom.us/j/8123?pwd=x"));
        assert_eq!(
            meeting_link(&["https://meet.google.com/abc-defg-hij,"]).unwrap().url,
            "https://meet.google.com/abc-defg-hij"
        );
        assert!(meeting_link(&["http://meet.google.com/abc"]).is_none(), "solo https");
        assert!(meeting_link(&["https://zoom.us.evil.example/j/1"]).is_none());
        assert!(meeting_link(&["https://evil.example/?u=https://zoom.us/j/1"]).is_none());
        assert!(meeting_link(&["https://"]).is_none());
        assert!(meeting_link(&[&"x ".repeat(50_000)]).is_none());
    }

    #[test]
    fn needs_reply_detection() {
        let mut i = input();
        let me = "yo@despacho.mx";
        i.inbox = vec![
            // 1: me escribieron hace dos días y no contesté → espera respuesta.
            msg(1, 1, "ana@cliente.mx", me, 48),
            // 2: hace 3 horas → aún no.
            msg(2, 2, "luis@cliente.mx", me, 3),
            // 3: contestada (bandera).
            {
                let mut m = msg(3, 3, "eva@cliente.mx", me, 50);
                m.flags.answered = true;
                m
            },
            // 4: contesté después (en enviados).
            msg(4, 4, "raul@cliente.mx", me, 72),
            // 5: un boletín.
            msg(5, 5, "newsletter@updates.example.org", me, 30),
            // 6: me pusieron en copia, no «para mí».
            {
                let mut m = msg(6, 6, "sara@cliente.mx", "otro@x.mx", 30);
                m.cc = vec![Mailbox::new("", me)];
                m
            },
            // 7: hace un mes.
            msg(7, 7, "viejo@cliente.mx", me, 24 * 30),
            // 8 y 9: mismo hilo; el último (9) llegó hace 2 h → el hilo aún no espera.
            msg(8, 8, "pepe@cliente.mx", me, 60),
            msg(9, 8, "pepe@cliente.mx", me, 2),
            // 10: hilo sin id local; mi respuesta lo cita por In-Reply-To.
            {
                let mut m = msg(10, 0, "kim@cliente.mx", me, 40);
                m.thread = None;
                m
            },
        ];
        let mut reply = msg(100, 4, me, "raul@cliente.mx", 10);
        reply.thread = Some(ThreadId(4));
        let mut reply2 = msg(101, 0, me, "kim@cliente.mx", 20);
        reply2.thread = None;
        reply2.message_id = Some("r2@x".into());
        reply2.in_reply_to = Some("10@x".into());
        i.sent = vec![reply, reply2];
        let s = build(&i);
        let ids: Vec<u64> = s.needs_reply.iter().map(|m| m.id.get()).collect();
        assert_eq!(ids, vec![1]);
        assert_eq!(s.needs_reply[0].from_addr, "ana@cliente.mx");
        // Sin direcciones propias no se adivina nada.
        i.me.clear();
        assert!(build(&i).needs_reply.is_empty());
    }

    #[test]
    fn important_unread_and_birthdays() {
        let mut i = input();
        let me = "yo@despacho.mx";
        let unread = |id, from: &str| {
            let mut m = msg(id, id, from, me, 1);
            m.flags.seen = false;
            m
        };
        let mut kw = unread(3, "x@y.mx");
        kw.keywords = vec!["$Important".into()];
        i.inbox = vec![unread(1, "ana@cliente.mx"), unread(2, "news@updates.example.org"), kw];
        i.known.insert("ana@cliente.mx".into());
        i.birthdays = vec![
            (ContactId(1), "Ana".into(), d(1985, 10, 27)),
            (ContactId(2), "Beto".into(), d(1604, 11, 2)),
            (ContactId(3), "Ciro".into(), d(1990, 10, 20)),
            (ContactId(4), "Dana".into(), d(1992, 2, 29)),
        ];
        let s = build(&i);
        assert_eq!(s.accounts[0].important, 2);
        assert_eq!(s.important_unread(), 2);
        let b: Vec<_> = s.birthdays.iter().map(|b| (b.name.as_str(), b.in_days, b.turns)).collect();
        assert_eq!(b, vec![("Ana", 0, Some(41)), ("Beto", 6, None)]);
        assert_eq!(next_birthday(d(1992, 2, 29), d(2027, 1, 1)), Some(d(2027, 2, 28)));
    }

    #[test]
    fn greeting_and_long_day() {
        assert_eq!(greeting(9).es, "Buenos días");
        assert_eq!(greeting(15).es, "Buenas tardes");
        assert_eq!(greeting(22).en, "Good evening");
        assert_eq!(greeting(2).es, "Buenas noches");
        crate::lang::set_lang(crate::lang::Lang::Es);
        assert_eq!(long_day(d(2026, 10, 27)), "martes 27 de octubre");
    }

    #[test]
    fn gather_and_build_on_the_sample_never_fail() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        iuredesk_store::sample::populate(&store, iuredesk_store::sample::DEFAULT_SEED, now()).unwrap();
        let args = GatherArgs {
            now: now(),
            zone: Zone::utc(),
            me: store.account_labels().unwrap().into_iter().map(|l| l.email).collect(),
            labels: store.account_labels().unwrap(),
            folders: store.all_folders().unwrap(),
            calendars: store.calendars().unwrap(),
            ..Default::default()
        };
        let s = build(&gather(&store, args));
        assert!(!s.events.is_empty());
        assert!(!s.tasks.is_empty());
        assert!(s.unread() > 0);
        assert!(!s.needs_reply.is_empty(), "el buzón de ejemplo tiene hilos sin responder");
        // Vacío: nada que mostrar, sin fallar.
        let dir2 = tempfile::tempdir().unwrap();
        let empty = Store::open(dir2.path()).unwrap();
        let s = build(&gather(&empty, GatherArgs { now: now(), ..Default::default() }));
        assert!(s.events.is_empty() && s.tasks.is_empty() && s.needs_reply.is_empty());
    }
}
