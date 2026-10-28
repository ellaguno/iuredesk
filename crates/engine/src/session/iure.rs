//! Iurefficient en la sesión (F3.1): conexión con la sesión compartida de la suite (o inicio de
//! sesión con TOTP), panel contextual del mensaje (consulta en segundo plano, con espera y
//! caché), acciones del panel (vincular, crear lead/ticket/tarea, responder por la dirección de la
//! oportunidad), avisos y menciones con consulta periódica y espera creciente, y la agenda y las
//! tareas propias de la instancia copiadas en el almacén como calendario y lista «Iurefficient».
//!
//! Nada bloquea la interfaz: la red corre en el runtime de fondo y avisa por un canal propio que
//! [`Session::poll`] atiende. Sin instancia o sin red, el panel muestra lo que se recuerda
//! localmente (vínculos en el almacén y la última consulta).

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use chrono::{DateTime, NaiveDate, Utc};
use iuredesk_accounts::iurefficient::{
    IurefficientStatus, KeyringVault, SharedVault, iurefficient_session_with, shared_entry_user,
};
use iuredesk_model::account::Identity;
use iuredesk_model::{
    AccountId, Calendar, Event as CalEvent, EventId, EventTime, Mailbox, MessageEnvelope, MessageId, Task, TaskId,
    TaskList, TaskListId, TaskStatus,
};
use iuredesk_store::EmailLink;
use iurefficient_connect::Account;
use iurefficient_connect::pim::CrmReplyAddress;
use iurefficient_connect::rest::{Login, Session as Remote};

use super::{Event, Notice, Outcome, Session};
use crate::iure::{
    self, Agenda, ContextQuery, EmailDigest, Feed, IureContext, IureError, IureStatus, LeadForm, LinkOptions,
    LinkTarget, TaskForm, TicketForm,
};
use crate::lang::Text;

/// Espera antes de consultar el contexto del mensaje enfocado (recorrer la lista no consulta).
const DEBOUNCE: Duration = Duration::from_millis(350);
/// Vigencia del contexto en caché.
const CONTEXT_TTL: Duration = Duration::from_secs(300);
/// Tras un fallo, cuándo se vuelve a consultar el mismo mensaje.
const CONTEXT_RETRY: Duration = Duration::from_secs(60);
/// Entradas de caché de contexto.
const CONTEXT_CACHE: usize = 128;
/// Cada cuánto se copia la agenda de la instancia.
const AGENDA_EVERY: Duration = Duration::from_secs(15 * 60);
/// Espera máxima entre consultas de avisos tras fallos seguidos.
const MAX_BACKOFF: Duration = Duration::from_secs(30 * 60);
/// Ventana de la agenda que se copia (días antes y después de hoy).
const AGENDA_PAST_DAYS: i64 = 30;
const AGENDA_FUTURE_DAYS: i64 = 180;
/// Ruta remota del calendario y de la lista de tareas de la instancia (fuente propia, distinta de
/// las de `pim-sync`).
pub const IURE_CALENDAR_PATH: &str = "iurefficient:calendar-events";
pub const IURE_TASKS_PATH: &str = "iurefficient:tasks";
/// Clave del almacén con el id de cuenta reservado para la fuente «Iurefficient».
const ACCOUNT_KEY: &str = "iurefficient:account";
/// Prefijo del id remoto de lo copiado de la instancia.
const REMOTE_PREFIX: &str = "iure:";
/// Color del calendario «Iurefficient».
const IURE_COLOR: u32 = 0x0F766E;
const APP: &str = "IureDesk";

/// Cómo conectar con Iurefficient.
#[derive(Clone)]
pub enum IureSetup {
    /// Sesión compartida de la suite (cuenta activa + llavero del sistema).
    System,
    /// Una cuenta y un llavero dados (pruebas).
    With { account: Account, vault: Arc<dyn SharedVault> },
    /// Respuestas de ejemplo sin red (`--sample`, capturas).
    Sample,
}

impl std::fmt::Debug for IureSetup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IureSetup::System => f.write_str("System"),
            IureSetup::With { account, .. } => write!(f, "With({})", account.host()),
            IureSetup::Sample => f.write_str("Sample"),
        }
    }
}

#[derive(Clone)]
enum Backend {
    None,
    Sample,
    Live(Arc<Remote>),
}

/// Estado del inicio de sesión (diálogo de Ajustes → Iurefficient).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LoginState {
    pub busy: bool,
    /// Falta el código de verificación en dos pasos.
    pub needs_totp: bool,
    pub error: Option<Text>,
}

/// Propuestas de lead en curso para un mensaje.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LeadSuggestions {
    Loading,
    Ready(Vec<LeadForm>),
    Failed(Text),
}

/// Lo que el panel contextual muestra de un mensaje.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct IurePanel {
    pub status: IureStatus,
    /// Lo que sabe la instancia (o lo último que supo).
    pub context: Option<IureContext>,
    /// Consulta en curso.
    pub loading: bool,
    /// Por qué no hay contexto fresco.
    pub error: Option<Text>,
    /// Vínculos recordados localmente.
    pub links: Vec<EmailLink>,
    /// El mensaje tiene `Message-ID` (sin él no se puede vincular).
    pub linkable: bool,
}

impl IurePanel {
    /// `false` = sin sesión de Iurefficient: el panel no se muestra.
    pub fn visible(&self) -> bool {
        matches!(
            self.status,
            IureStatus::Ready { .. }
                | IureStatus::Offline { .. }
                | IureStatus::TermsRequired { .. }
                | IureStatus::Expired { .. }
        )
    }
}

enum CtxEntry {
    Ready(IureContext, Instant),
    Failed(Option<IureContext>, Text, Instant),
}

enum LoginMsg {
    Done { session: Arc<Remote>, name: Option<String>, email: String },
    NeedsTotp { session: Arc<Remote>, token: String },
    Failed(Text),
}

enum Msg {
    Connected { status: IurefficientStatus, session: Option<Arc<Remote>> },
    Context { key: String, result: Result<IureContext, IureError> },
    Targets { query: String, result: Result<Vec<LinkTarget>, IureError> },
    Feed(Result<Feed, IureError>),
    Agenda { result: Result<Agenda, IureError>, user: Option<String> },
    Done { message_id: String, result: Result<Vec<EmailLink>, IureError>, done: Text },
    Leads { message: MessageId, result: Result<Vec<LeadForm>, IureError> },
    Login(LoginMsg),
    Reply { message: MessageId, result: Result<Option<CrmReplyAddress>, IureError> },
    Plain(Result<Text, IureError>),
}

/// Parte Iurefficient de la sesión.
pub(crate) struct Iure {
    setup: Option<IureSetup>,
    backend: Backend,
    status: IureStatus,
    who: Option<(String, String, Option<String>)>,
    generation: u64,
    tx: Sender<(u64, Msg)>,
    rx: Receiver<(u64, Msg)>,
    contexts: HashMap<String, CtxEntry>,
    want: Option<(String, ContextQuery, Instant)>,
    inflight: HashSet<String>,
    links_gen: u64,
    links_cache: Option<(String, u64, Vec<EmailLink>)>,
    targets: (String, Vec<LinkTarget>, bool),
    leads: Option<(MessageId, LeadSuggestions)>,
    feed: Feed,
    feed_next: Option<Instant>,
    feed_busy: bool,
    feed_failures: u32,
    agenda_next: Option<Instant>,
    agenda_busy: bool,
    user_id: Option<String>,
    login: LoginState,
    pending_totp: Option<(Arc<Remote>, String)>,
    busy: usize,
    pending_compose: Vec<crate::compose::ComposeDraft>,
}

impl Default for Iure {
    fn default() -> Self {
        let (tx, rx) = channel();
        Iure {
            setup: None,
            backend: Backend::None,
            status: IureStatus::Disabled,
            who: None,
            generation: 0,
            tx,
            rx,
            contexts: HashMap::new(),
            want: None,
            inflight: HashSet::new(),
            links_gen: 0,
            links_cache: None,
            targets: (String::new(), Vec::new(), false),
            leads: None,
            feed: Feed::default(),
            feed_next: None,
            feed_busy: false,
            feed_failures: 0,
            agenda_next: None,
            agenda_busy: false,
            user_id: None,
            login: LoginState::default(),
            pending_totp: None,
            busy: 0,
            pending_compose: Vec::new(),
        }
    }
}

fn user_agent() -> String {
    iurefficient_connect::user_agent(APP, env!("CARGO_PKG_VERSION"))
}

fn mailbox_text(m: &[Mailbox]) -> String {
    m.iter()
        .take(20)
        .map(|b| match b.name.as_deref().filter(|n| !n.is_empty()) {
            None => b.addr.clone(),
            Some(n) => format!("{n} <{}>", b.addr),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

impl Session {
    // ------------------------------------------------------------------ conexión

    /// Empieza a usar Iurefficient (la app llama con [`IureSetup::System`]; el buzón de ejemplo lo
    /// hace solo con [`IureSetup::Sample`]).
    pub fn iure_start(&mut self, setup: IureSetup) {
        self.iure.setup = Some(setup);
        self.iure_connect();
    }

    /// Estado de la conexión.
    pub fn iure_status(&self) -> &IureStatus {
        &self.iure.status
    }

    /// Estado del inicio de sesión.
    pub fn iure_login_state(&self) -> &LoginState {
        &self.iure.login
    }

    /// Instancia y correo para prellenar el inicio de sesión.
    pub fn iure_login_hint(&self) -> (String, String) {
        if let Some((h, e, _)) = &self.iure.who {
            return (h.clone(), e.clone());
        }
        if matches!(self.iure.setup, Some(IureSetup::System))
            && let Some(a) = iurefficient_connect::account::active()
        {
            return (a.domain.clone(), a.email.clone());
        }
        (String::new(), self.my_addresses().into_iter().next().unwrap_or_default())
    }

    /// Página web de la instancia (aceptar términos, abrir un aviso).
    pub fn iure_web_url(&self) -> Option<String> {
        match &self.iure.backend {
            Backend::Live(s) => Some(s.account().web_url()),
            Backend::Sample => Some("https://demo.iurefficient.com/".into()),
            Backend::None => match &self.iure.setup {
                Some(IureSetup::With { account, .. }) => Some(account.web_url()),
                _ => self.iure.who.as_ref().map(|(h, _, _)| format!("https://{h}/")),
            },
        }
    }

    /// Reintenta: vuelve a restaurar la sesión, o consulta de nuevo si ya había una.
    pub fn iure_retry(&mut self) {
        match self.iure.backend {
            Backend::Live(_) => {
                self.iure.contexts.retain(|_, e| matches!(e, CtxEntry::Ready(..)));
                self.iure.feed_next = Some(Instant::now());
                self.iure.agenda_next = Some(Instant::now());
                self.iure.feed_failures = 0;
            }
            Backend::Sample => {}
            Backend::None => self.iure_connect(),
        }
    }

    fn iure_connect(&mut self) {
        let iu = &mut self.iure;
        iu.generation = iu.generation.wrapping_add(1);
        iu.backend = Backend::None;
        iu.contexts.clear();
        iu.inflight.clear();
        iu.want = None;
        iu.user_id = None;
        iu.feed_busy = false;
        iu.agenda_busy = false;
        match iu.setup.clone() {
            None => iu.status = IureStatus::Disabled,
            Some(IureSetup::Sample) => {
                iu.backend = Backend::Sample;
                iu.status = IureStatus::Ready {
                    host: "demo.iurefficient.com".into(),
                    email: "laura@despacho.example.org".into(),
                    name: Some("Laura Méndez".into()),
                };
                iu.feed = iure::sample::feed();
                let agenda = iure::sample::agenda(self.now());
                self.iure_apply_agenda(&agenda);
            }
            Some(IureSetup::System) => {
                iu.status = IureStatus::Connecting;
                self.iure_spawn(async move {
                    let acc = iurefficient_connect::account::active().and_then(|a| a.account().ok());
                    let s = iurefficient_session_with(acc, &KeyringVault).await;
                    Msg::Connected { status: s.status, session: s.session.map(Arc::new) }
                });
            }
            Some(IureSetup::With { account, vault }) => {
                iu.status = IureStatus::Connecting;
                self.iure_spawn(async move {
                    let s = iurefficient_session_with(Some(account), &*vault).await;
                    Msg::Connected { status: s.status, session: s.session.map(Arc::new) }
                });
            }
        }
    }

    fn iure_spawn(&self, fut: impl Future<Output = Msg> + Send + 'static) {
        let (tx, generation, wake) = (self.iure.tx.clone(), self.iure.generation, self.bg.waker());
        self.bg.handle().spawn(async move {
            let m = fut.await;
            if tx.send((generation, m)).is_ok() {
                wake();
            }
        });
    }

    pub(crate) fn iure_remote(&self) -> Option<Arc<Remote>> {
        match &self.iure.backend {
            Backend::Live(s) => Some(Arc::clone(s)),
            _ => None,
        }
    }

    /// Inicia sesión en una instancia (después puede pedir el código TOTP).
    pub fn iure_login(&mut self, domain: &str, email: &str, password: &str) {
        let acc = match Account::new(domain.trim(), email.trim()) {
            Ok(a) => a,
            Err(e) => {
                let m = e.to_string();
                self.iure.login.error = Some(Text::new(m.clone(), m));
                return;
            }
        };
        if password.is_empty() {
            self.iure.login.error = Some(Text::new("Type your password.", "Escribe tu contraseña."));
            return;
        }
        self.iure.login = LoginState { busy: true, ..Default::default() };
        self.iure.pending_totp = None;
        let password = password.to_string();
        self.iure_spawn(async move {
            let s = match Remote::new(acc, &user_agent()) {
                Ok(s) => Arc::new(s),
                Err(e) => return Msg::Login(LoginMsg::Failed(login_error(&e))),
            };
            match tokio::time::timeout(iure::CALL_TIMEOUT, s.login(&password)).await {
                Err(_) => Msg::Login(LoginMsg::Failed(IureError::Offline(String::new()).text())),
                Ok(Err(e)) => Msg::Login(LoginMsg::Failed(login_error(&e))),
                Ok(Ok(Login::TotpRequired { totp_token })) => {
                    Msg::Login(LoginMsg::NeedsTotp { session: s, token: totp_token })
                }
                Ok(Ok(Login::Ok(user))) => {
                    let email = if user.email.trim().is_empty() { s.account().email.clone() } else { user.email };
                    Msg::Login(LoginMsg::Done { session: s, name: user.name, email })
                }
            }
        });
    }

    /// Segundo paso del inicio de sesión: el código de la app de autenticación.
    pub fn iure_verify_totp(&mut self, code: &str) {
        let Some((s, token)) = self.iure.pending_totp.clone() else {
            self.iure.login = LoginState {
                error: Some(Text::new("Sign in again.", "Vuelve a iniciar sesión.")),
                ..Default::default()
            };
            return;
        };
        self.iure.login.busy = true;
        self.iure.login.error = None;
        let code = code.trim().to_string();
        self.iure_spawn(async move {
            match tokio::time::timeout(iure::CALL_TIMEOUT, s.verify_totp(&token, &code)).await {
                Err(_) => Msg::Login(LoginMsg::Failed(IureError::Offline(String::new()).text())),
                Ok(Err(e)) => Msg::Login(LoginMsg::Failed(login_error(&e))),
                Ok(Ok(user)) => {
                    let email = if user.email.trim().is_empty() { s.account().email.clone() } else { user.email };
                    Msg::Login(LoginMsg::Done { session: s, name: user.name, email })
                }
            }
        });
    }

    /// Cancela el inicio de sesión a medias.
    pub fn iure_login_reset(&mut self) {
        self.iure.login = LoginState::default();
        self.iure.pending_totp = None;
    }

    fn iure_vault(&self) -> Option<Arc<dyn SharedVault>> {
        match &self.iure.setup {
            Some(IureSetup::System) => Some(Arc::new(KeyringVault)),
            Some(IureSetup::With { vault, .. }) => Some(Arc::clone(vault)),
            _ => None,
        }
    }

    fn iure_set_ready(&mut self, s: Arc<Remote>, email: String, name: Option<String>) {
        let host = s.account().host();
        self.iure.who = Some((host.clone(), email.clone(), name.clone()));
        self.iure.status = IureStatus::Ready { host, email, name };
        self.iure.backend = Backend::Live(s);
        self.iure.feed_next = Some(Instant::now());
        self.iure.agenda_next = Some(Instant::now());
        self.iure.feed_failures = 0;
        self.iure.contexts.retain(|_, e| matches!(e, CtxEntry::Ready(..)));
    }

    // ------------------------------------------------------------------ panel contextual

    fn iure_query(&mut self, env: &MessageEnvelope) -> ContextQuery {
        let mine = self.my_addresses();
        let other = env
            .from
            .iter()
            .chain(env.to.iter())
            .chain(env.cc.iter())
            .find(|m| !mine.iter().any(|a| a.eq_ignore_ascii_case(&m.addr)))
            .or_else(|| env.from.first())
            .cloned()
            .unwrap_or_default();
        let org = self
            .contacts()
            .iter()
            .find(|c| c.emails.iter().any(|e| e.value.eq_ignore_ascii_case(&other.addr)))
            .map(|c| c.org.clone())
            .filter(|o| !o.trim().is_empty());
        ContextQuery {
            email: iure::clip(&other.addr, 254),
            name: iure::clip(other.name.as_deref().filter(|n| !n.is_empty()).unwrap_or(&other.addr), 200),
            org,
            subject: iure::clip(&env.subject, 300),
            case_refs: crate::context::case_refs(&env.subject).into_iter().take(3).collect(),
        }
    }

    fn iure_links(&mut self, message_id: &str) -> Vec<EmailLink> {
        let key = iuredesk_store::link_key(message_id);
        if let Some((k, g, v)) = &self.iure.links_cache
            && *k == key
            && *g == self.iure.links_gen
        {
            return v.clone();
        }
        let v = self.store.email_links(&key).unwrap_or_default();
        self.iure.links_cache = Some((key, self.iure.links_gen, v.clone()));
        v
    }

    /// F5.3: contexto ya conocido de un correo (sin red), para la insignia de la lista.
    pub(crate) fn iure_known_context(&mut self, env: &MessageEnvelope) -> Option<IureContext> {
        if !matches!(self.iure.backend, Backend::Sample) && self.iure.contexts.is_empty() {
            return None;
        }
        let q = self.iure_query(env);
        match &self.iure.backend {
            Backend::Sample => Some(iure::sample::context(&q)).filter(|c| !c.is_empty()),
            _ => match self.iure.contexts.get(&q.key()) {
                Some(CtxEntry::Ready(c, _) | CtxEntry::Failed(Some(c), _, _)) if !c.is_empty() => Some(c.clone()),
                _ => None,
            },
        }
    }

    /// F5.3: cambia cuando hay vínculos o contextos nuevos (para rehacer las insignias).
    pub(crate) fn iure_badge_gen(&self) -> (u64, usize) {
        (self.iure.links_gen, self.iure.contexts.len())
    }

    /// F5.3: hay sesión de Iurefficient (o el ejemplo).
    pub(crate) fn iure_visible(&self) -> bool {
        IurePanel { status: self.iure.status.clone(), ..Default::default() }.visible()
    }

    /// Lo que el panel contextual muestra del mensaje (barato: se puede llamar en cada cuadro).
    /// Si hace falta, pide la consulta en segundo plano tras una breve espera.
    pub fn iure_panel(&mut self, id: MessageId) -> IurePanel {
        let mut p = IurePanel { status: self.iure.status.clone(), ..Default::default() };
        if !p.visible() {
            return p;
        }
        let Some(env) = self.envelope(id).cloned() else { return p };
        if let Some(mid) = env.message_id.as_deref().filter(|m| !m.trim().is_empty()) {
            p.linkable = true;
            p.links = self.iure_links(mid);
        }
        let q = self.iure_query(&env);
        match self.iure.backend.clone() {
            Backend::Sample => p.context = Some(iure::sample::context(&q)),
            Backend::None => {
                if let Some(CtxEntry::Ready(c, _) | CtxEntry::Failed(Some(c), _, _)) = self.iure.contexts.get(&q.key())
                {
                    p.context = Some(c.clone());
                }
            }
            Backend::Live(_) => {
                let key = q.key();
                let mut stale = true;
                match self.iure.contexts.get(&key) {
                    Some(CtxEntry::Ready(c, at)) => {
                        p.context = Some(c.clone());
                        stale = at.elapsed() > CONTEXT_TTL;
                    }
                    Some(CtxEntry::Failed(c, why, at)) => {
                        p.context = c.clone();
                        p.error = Some(why.clone());
                        stale = at.elapsed() > CONTEXT_RETRY;
                    }
                    None => {}
                }
                let ready = self.iure.status.is_ready();
                if stale && ready {
                    p.loading = true;
                    if !self.iure.inflight.contains(&key) && self.iure.want.as_ref().is_none_or(|(k, _, _)| *k != key) {
                        self.iure.want = Some((key, q, Instant::now()));
                    }
                } else if !ready {
                    p.error.get_or_insert_with(|| self.iure.status.label());
                }
            }
        }
        p
    }

    /// Cuándo hay que volver a llamar a `poll` por Iurefficient (espera del panel, avisos).
    pub(crate) fn iure_deadline(&self) -> Option<Duration> {
        let now = Instant::now();
        let want = self.iure.want.as_ref().map(|(_, _, at)| DEBOUNCE.saturating_sub(at.elapsed()));
        let feed = self.iure.feed_next.filter(|_| !self.iure.feed_busy).map(|t| t.saturating_duration_since(now));
        let agenda = self.iure.agenda_next.filter(|_| !self.iure.agenda_busy).map(|t| t.saturating_duration_since(now));
        [want, feed, agenda].into_iter().flatten().min()
    }

    // ------------------------------------------------------------------ avisos

    /// Avisos y menciones de Iurefficient.
    pub fn iure_feed(&self) -> &Feed {
        &self.iure.feed
    }

    /// Marca un aviso (o mención) como leído.
    pub fn iure_mark_read(&mut self, mention: bool, id: &str) {
        let mut changed = false;
        for i in self.iure.feed.items.iter_mut().filter(|i| i.id == id && i.mention == mention && !i.read) {
            i.read = true;
            changed = true;
        }
        if changed {
            self.iure.feed.unread = self.iure.feed.unread.saturating_sub(1);
        }
        let Some(s) = self.iure_remote() else { return };
        let id = id.to_string();
        self.iure_spawn(async move { Msg::Plain(iure::mark_read(&s, mention, &id).await.map(|_| Text::default())) });
    }

    /// Marca todos los avisos como leídos.
    pub fn iure_mark_all_read(&mut self) {
        let unread: Vec<(bool, String)> =
            self.iure.feed.items.iter().filter(|i| !i.read).map(|i| (i.mention, i.id.clone())).take(100).collect();
        for i in &mut self.iure.feed.items {
            i.read = true;
        }
        self.iure.feed.unread = 0;
        let Some(s) = self.iure_remote() else { return };
        self.iure_spawn(async move {
            let r = iure::call(iurefficient_connect::pim::mark_all_notifications_read(&s)).await;
            for (_, id) in unread.iter().filter(|(m, _)| *m) {
                let _ = iure::mark_read(&s, true, id).await;
            }
            Msg::Plain(r.map(|_| Text::default()))
        });
    }

    // ------------------------------------------------------------------ vincular y crear

    /// Busca destinos para «Vincular a…» (resultado en [`Session::iure_targets`]).
    pub fn iure_search_targets(&mut self, q: &str) {
        let q = q.trim().to_string();
        if q == self.iure.targets.0 && !self.iure.targets.1.is_empty() {
            return;
        }
        match self.iure.backend.clone() {
            Backend::Sample => self.iure.targets = (q.clone(), iure::sample::targets(&q), false),
            Backend::Live(s) => {
                self.iure.targets = (q.clone(), Vec::new(), true);
                self.iure_spawn(async move {
                    let result = iure::search_targets(&s, &q).await;
                    Msg::Targets { query: q, result }
                });
            }
            Backend::None => self.iure.targets = (q, Vec::new(), false),
        }
    }

    /// Último resultado de búsqueda de destinos: (consulta, destinos, buscando).
    pub fn iure_targets(&self) -> (&str, &[LinkTarget], bool) {
        (&self.iure.targets.0, &self.iure.targets.1, self.iure.targets.2)
    }

    /// Hay acciones de Iurefficient en curso.
    pub fn iure_busy(&self) -> bool {
        self.iure.busy > 0
    }

    /// El correo en texto para mandarlo a la instancia.
    pub fn iure_digest(&mut self, id: MessageId) -> Option<EmailDigest> {
        let env = self.envelope(id).cloned()?;
        let message_id = env.message_id.clone().filter(|m| !m.trim().is_empty())?;
        let text = match self.bodies.get(&id) {
            Some(b) => b.text.clone(),
            None => match self.store.get_raw(id) {
                Ok(raw) => crate::body::parse(&raw).text,
                Err(_) => env.preview.clone(),
            },
        };
        Some(EmailDigest {
            message_id: iuredesk_store::link_key(&message_id),
            from: mailbox_text(&env.from),
            to: mailbox_text(&env.to),
            date: env.date,
            subject: iure::clip(&env.subject, 300),
            text: iure::clip(&text, iure::MAX_EMAIL_TEXT),
        })
    }

    fn iure_action(
        &mut self,
        id: MessageId,
        done: Text,
        run: impl FnOnce(
            Arc<Remote>,
            EmailDigest,
            DateTime<Utc>,
        ) -> std::pin::Pin<Box<dyn Future<Output = Result<Vec<EmailLink>, IureError>> + Send>>,
        sample: impl FnOnce(&EmailDigest, DateTime<Utc>) -> Vec<EmailLink>,
    ) -> Result<(), Text> {
        let Some(digest) = self.iure_digest(id) else {
            return Err(Text::new(
                "This message has no Message-ID; it cannot be linked.",
                "Este mensaje no tiene Message-ID; no se puede vincular.",
            ));
        };
        let now = self.now();
        match self.iure.backend.clone() {
            Backend::Sample => {
                let links = sample(&digest, now);
                self.iure_remember(&digest.message_id, &links);
                self.notices.push(Notice { kind: super::NoticeKind::Success, text: done, undo: false, secs: None });
                Ok(())
            }
            Backend::Live(s) => {
                self.iure.busy = self.iure.busy.saturating_add(1);
                let mid = digest.message_id.clone();
                let fut = run(s, digest, now);
                self.iure_spawn(async move { Msg::Done { message_id: mid, result: fut.await, done } });
                Ok(())
            }
            Backend::None => Err(self.iure.status.label()),
        }
    }

    fn iure_remember(&mut self, message_id: &str, links: &[EmailLink]) {
        for l in links.iter().take(10) {
            if let Err(e) = self.store.add_email_link(message_id, l) {
                self.notices.push(Notice::error(Text::new(
                    format!("The link could not be saved on this computer ({e})."),
                    format!("No se pudo guardar el vínculo en este equipo ({e})."),
                )));
            }
        }
        self.iure.links_gen = self.iure.links_gen.wrapping_add(1);
    }

    /// «Vincular a…»: hace en la instancia lo que admite hoy y lo recuerda localmente.
    pub fn iure_link(&mut self, id: MessageId, target: LinkTarget, opts: LinkOptions) -> Result<(), Text> {
        let done = Text::new(
            format!("Linked to {} {}", target.kind.label().en.to_lowercase(), target.label),
            format!("Vinculado a {} {}", target.kind.label().es.to_lowercase(), target.label),
        );
        let t2 = target.clone();
        self.iure_action(
            id,
            done,
            move |s, d, now| Box::pin(async move { iure::link(&s, &target, &d, opts, now).await }),
            move |_, now| {
                vec![EmailLink {
                    kind: t2.kind.key().into(),
                    id: t2.id.clone(),
                    label: t2.label.clone(),
                    how: match t2.kind {
                        iure::TargetKind::Case | iure::TargetKind::Client => "document",
                        iure::TargetKind::Ticket => "comment",
                        _ => "activity",
                    }
                    .into(),
                    remote_ref: None,
                    created: now,
                }]
            },
        )
    }

    /// Olvida un vínculo local (no borra nada en la instancia).
    pub fn iure_unlink(&mut self, id: MessageId, kind: &str, target: &str) {
        let Some(mid) = self.envelope(id).and_then(|e| e.message_id.clone()) else { return };
        let _ = self.store.remove_email_link(&mid, kind, target);
        self.iure.links_gen = self.iure.links_gen.wrapping_add(1);
    }

    /// El remitente como lead (prellenado del formulario).
    pub fn iure_sender_lead(&mut self, id: MessageId) -> LeadForm {
        let Some(env) = self.envelope(id).cloned() else { return LeadForm::default() };
        let q = self.iure_query(&env);
        LeadForm {
            name: q.name.clone(),
            company: q.org.clone().unwrap_or_default(),
            email: q.email.clone(),
            notes: iure::clip(&format!("Desde el correo «{}»", env.subject), 300),
            ..Default::default()
        }
    }

    /// Pide a la IA de la instancia propuestas de lead para el correo (`persist:false`).
    pub fn iure_suggest_leads(&mut self, id: MessageId) {
        let sender = self.iure_sender_lead(id);
        let Some(d) = self.iure_digest(id) else {
            self.iure.leads = Some((id, LeadSuggestions::Ready(vec![sender])));
            return;
        };
        match self.iure.backend.clone() {
            Backend::Live(s) => {
                self.iure.leads = Some((id, LeadSuggestions::Loading));
                self.iure_spawn(async move {
                    let result = iure::suggest_leads(&s, &d, &sender).await;
                    Msg::Leads { message: id, result }
                });
            }
            _ => self.iure.leads = Some((id, LeadSuggestions::Ready(vec![sender]))),
        }
    }

    /// Propuestas de lead del mensaje.
    pub fn iure_lead_suggestions(&self, id: MessageId) -> Option<&LeadSuggestions> {
        self.iure.leads.as_ref().filter(|(m, _)| *m == id).map(|(_, s)| s)
    }

    /// Crea el lead (tras confirmarlo el usuario) y registra el correo como actividad.
    pub fn iure_create_lead(&mut self, id: MessageId, form: LeadForm) -> Result<(), Text> {
        if form.name.trim().is_empty() {
            return Err(Text::new("The lead needs a name.", "El lead necesita nombre."));
        }
        let done = Text::new(format!("Lead created: {}", form.name), format!("Lead creado: {}", form.name));
        let f2 = form.clone();
        self.iure_action(
            id,
            done,
            move |s, d, now| Box::pin(async move { iure::create_lead(&s, &form, &d, now).await }),
            move |_, now| {
                vec![EmailLink {
                    kind: "lead".into(),
                    id: "l-new".into(),
                    label: f2.name.clone(),
                    how: "created".into(),
                    remote_ref: None,
                    created: now,
                }]
            },
        )
    }

    /// Ticket prellenado desde el correo.
    pub fn iure_ticket_form(&mut self, id: MessageId) -> TicketForm {
        let Some(env) = self.envelope(id).cloned() else { return TicketForm::default() };
        let q = self.iure_query(&env);
        let text = self.iure_digest(id).map(|d| d.text).unwrap_or_else(|| env.preview.clone());
        TicketForm {
            title: iure::clip(iuredesk_model::normalize_subject(&env.subject).0.as_str(), 200),
            description: iure::clip(&text, 4_000),
            priority: "medium".into(),
            requester_name: q.name,
            requester_email: q.email,
        }
    }

    /// Crea un ticket desde el correo.
    pub fn iure_create_ticket(&mut self, id: MessageId, form: TicketForm) -> Result<(), Text> {
        if form.title.trim().is_empty() {
            return Err(Text::new("The ticket needs a title.", "El ticket necesita título."));
        }
        let done = Text::new(format!("Ticket created: {}", form.title), format!("Ticket creado: {}", form.title));
        let f2 = form.clone();
        self.iure_action(
            id,
            done,
            move |s, _, now| Box::pin(async move { iure::create_ticket(&s, &form, now).await }),
            move |_, now| {
                vec![EmailLink {
                    kind: "ticket".into(),
                    id: "t-new".into(),
                    label: format!("TK-1099 · {}", f2.title),
                    how: "created".into(),
                    remote_ref: None,
                    created: now,
                }]
            },
        )
    }

    /// Tarea prellenada desde el correo (vence en dos días; del primer caso si hay uno).
    pub fn iure_task_form(&mut self, id: MessageId) -> TaskForm {
        let Some(env) = self.envelope(id).cloned() else { return TaskForm::default() };
        let (base, _) = iuredesk_model::normalize_subject(&env.subject);
        let who = env.from.first().map(|m| m.name.clone().filter(|n| !n.is_empty()).unwrap_or_else(|| m.addr.clone()));
        let case_id = self
            .iure_panel(id)
            .context
            .and_then(|c| c.cases.first().map(|c| c.id.clone()))
            .filter(|c| !c.starts_with("c-") || self.is_sample());
        TaskForm {
            title: iure::clip(&format!("Responder: {base}"), 200),
            notes: iure::clip(&format!("Correo de {} — «{}»", who.unwrap_or_default(), env.subject), 1_000),
            due: Some(self.today() + chrono::Duration::days(2)),
            case_id,
        }
    }

    /// Crea una tarea en Iurefficient desde el correo.
    pub fn iure_create_task(&mut self, id: MessageId, form: TaskForm) -> Result<(), Text> {
        if form.title.trim().is_empty() {
            return Err(Text::new("The task needs a title.", "La tarea necesita título."));
        }
        let done = Text::new(format!("Task created: {}", form.title), format!("Tarea creada: {}", form.title));
        let f2 = form.clone();
        self.iure_action(
            id,
            done,
            move |s, _, now| Box::pin(async move { iure::create_task(&s, &form, now).await }),
            move |_, now| {
                vec![EmailLink {
                    kind: "task".into(),
                    id: "it-new".into(),
                    label: f2.title.clone(),
                    how: "created".into(),
                    remote_ref: None,
                    created: now,
                }]
            },
        )
    }

    /// «Responder vía la dirección de la oportunidad»: abre una respuesta con `Reply-To` y `Bcc`
    /// del buzón de ventas para que la instancia archive el envío y la contestación.
    pub fn iure_reply_via(&mut self, id: MessageId, opportunity: &str) {
        match self.iure.backend.clone() {
            Backend::Sample => {
                let addr = CrmReplyAddress {
                    reply_to: format!("ventas+opp-{opportunity}@demo.iurefficient.com"),
                    bcc: Some("ventas@demo.iurefficient.com".into()),
                };
                self.iure_open_reply(id, Ok(Some(addr)));
            }
            Backend::Live(s) => {
                self.iure.busy = self.iure.busy.saturating_add(1);
                let opp = opportunity.to_string();
                self.iure_spawn(async move {
                    let result = iure::reply_address(&s, &opp).await;
                    Msg::Reply { message: id, result }
                });
            }
            Backend::None => self.notices.push(Notice::error(self.iure.status.label())),
        }
    }

    fn iure_open_reply(&mut self, id: MessageId, result: Result<Option<CrmReplyAddress>, IureError>) {
        let addr = match result {
            Ok(Some(a)) => a,
            Ok(None) => {
                self.notices.push(Notice::info(
                    "This opportunity has no sales mailbox set up on the instance. Reply normally.",
                    "Esta oportunidad no tiene buzón de ventas configurado en la instancia. Responde normalmente.",
                ));
                return;
            }
            Err(e) => {
                self.iure_on_error(&e);
                self.notices.push(Notice::error(e.text()));
                return;
            }
        };
        if self.focused != Some(id) {
            self.request_focus(id);
            self.focused = Some(id);
        }
        match self.reply_like("mail.reply") {
            Ok(Outcome::Compose(mut cd)) => {
                let ident: &mut Identity = &mut cd.draft.identity;
                ident.reply_to = Some(iure::clip(&addr.reply_to, 254));
                if let Some(b) = addr.bcc.filter(|b| b.contains('@')) {
                    cd.draft.bcc.push(Mailbox::new("", iure::clip(&b, 254)));
                }
                self.iure.pending_compose.push(*cd);
            }
            Ok(_) => {}
            Err(e) => self.notices.push(Notice::error(e.text())),
        }
    }

    // ------------------------------------------------------------------ fondo

    fn iure_on_error(&mut self, e: &IureError) {
        if !e.affects_status() {
            return;
        }
        let (host, email, _) = self.iure.who.clone().unwrap_or_default();
        match e {
            IureError::Offline(why) => {
                if matches!(self.iure.status, IureStatus::Ready { .. }) {
                    self.iure.status = IureStatus::Offline { host, email, reason: why.clone() };
                }
            }
            IureError::Expired => {
                self.iure.status = IureStatus::Expired { host, email };
                self.iure.backend = Backend::None;
            }
            IureError::Terms => self.iure.status = IureStatus::TermsRequired { host, email },
            IureError::Server(_) => {}
        }
    }

    fn iure_on_success(&mut self) {
        if matches!(self.iure.status, IureStatus::Offline { .. } | IureStatus::TermsRequired { .. })
            && let Some((host, email, name)) = self.iure.who.clone()
        {
            self.iure.status = IureStatus::Ready { host, email, name };
        }
    }

    /// Atiende Iurefficient (desde `poll`).
    pub(crate) fn poll_iure(&mut self, events: &mut Vec<Event>) {
        let current = self.iure.generation;
        let mut n = 0usize;
        while let Ok((generation, msg)) = self.iure.rx.try_recv() {
            n += 1;
            if n > 1_000 {
                break;
            }
            // Las respuestas del inicio de sesión valen aunque haya cambiado la generación.
            if generation != current && !matches!(msg, Msg::Login(_)) {
                continue;
            }
            self.iure_handle(msg);
        }
        events.extend(self.iure.pending_compose.drain(..).map(|cd| Event::Compose(Box::new(cd))));
        if !matches!(self.iure.backend, Backend::Live(_)) {
            return;
        }
        let now = Instant::now();
        // Consulta del panel tras la espera.
        if let Some((key, q, at)) = self.iure.want.take() {
            if at.elapsed() < DEBOUNCE {
                self.iure.want = Some((key, q, at));
            } else if self.iure.inflight.insert(key.clone())
                && let Some(s) = self.iure_remote()
            {
                self.iure_spawn(async move {
                    let result = iure::lookup(&s, &q).await;
                    Msg::Context { key, result }
                });
            }
        }
        let live = matches!(self.iure.status, IureStatus::Ready { .. } | IureStatus::Offline { .. });
        if live
            && !self.iure.feed_busy
            && self.iure.feed_next.is_some_and(|t| now >= t)
            && let Some(s) = self.iure_remote()
        {
            self.iure.feed_busy = true;
            self.iure_spawn(async move { Msg::Feed(iure::fetch_feed(&s).await) });
        }
        if self.iure.status.is_ready()
            && !self.iure.agenda_busy
            && self.iure.agenda_next.is_some_and(|t| now >= t)
            && let Some(s) = self.iure_remote()
        {
            self.iure.agenda_busy = true;
            let user = self.iure.user_id.clone();
            let today = self.today();
            let (from, to) = (
                self.zone.day_start(today - chrono::Duration::days(AGENDA_PAST_DAYS)),
                self.zone.day_start(today + chrono::Duration::days(AGENDA_FUTURE_DAYS)),
            );
            self.iure_spawn(async move {
                let uid = match user {
                    Some(u) => u,
                    None => match iure::user_id(&s).await {
                        Ok(u) => u,
                        Err(e) => return Msg::Agenda { result: Err(e), user: None },
                    },
                };
                let result = iure::fetch_agenda(&s, &uid, from, to).await;
                Msg::Agenda { result, user: Some(uid) }
            });
        }
    }

    fn iure_poll_every(&self) -> Duration {
        Duration::from_secs(u64::from(self.prefs.iure_poll_secs.clamp(60, 3_600)))
    }

    fn iure_handle(&mut self, msg: Msg) {
        match msg {
            Msg::Connected { status, session } => {
                let st = match status {
                    IurefficientStatus::Ready { host, email, name } => {
                        if let Some(s) = session {
                            self.iure_set_ready(s, email, name);
                            return;
                        }
                        IureStatus::Offline { host, email, reason: String::new() }
                    }
                    IurefficientStatus::NotConfigured => IureStatus::NotConfigured,
                    IurefficientStatus::Expired { host, email } => IureStatus::Expired { host, email },
                    IurefficientStatus::Offline { host, email, reason } => IureStatus::Offline { host, email, reason },
                    IurefficientStatus::TermsRequired { host, email } => IureStatus::TermsRequired { host, email },
                };
                if let Some(h) = st.host() {
                    self.iure.who = Some((h.to_string(), st.email().unwrap_or_default().to_string(), None));
                }
                self.iure.status = st;
            }
            Msg::Context { key, result } => {
                self.iure.inflight.remove(&key);
                if self.iure.contexts.len() >= CONTEXT_CACHE {
                    self.iure.contexts.retain(|_, e| match e {
                        CtxEntry::Ready(_, at) => at.elapsed() < CONTEXT_TTL,
                        CtxEntry::Failed(..) => false,
                    });
                    if self.iure.contexts.len() >= CONTEXT_CACHE {
                        self.iure.contexts.clear();
                    }
                }
                match result {
                    Ok(c) => {
                        self.iure_on_success();
                        self.iure.contexts.insert(key, CtxEntry::Ready(c, Instant::now()));
                    }
                    Err(e) => {
                        self.iure_on_error(&e);
                        let old = match self.iure.contexts.remove(&key) {
                            Some(CtxEntry::Ready(c, _) | CtxEntry::Failed(Some(c), _, _)) => Some(c),
                            _ => None,
                        };
                        self.iure.contexts.insert(key, CtxEntry::Failed(old, e.text(), Instant::now()));
                    }
                }
            }
            Msg::Targets { query, result } => {
                if query != self.iure.targets.0 {
                    return;
                }
                match result {
                    Ok(v) => self.iure.targets = (query, v, false),
                    Err(e) => {
                        self.iure_on_error(&e);
                        self.iure.targets = (query, Vec::new(), false);
                        self.notices.push(Notice::error(e.text()));
                    }
                }
            }
            Msg::Feed(r) => {
                self.iure.feed_busy = false;
                match r {
                    Ok(f) => {
                        self.iure_on_success();
                        self.iure.feed = f;
                        self.iure.feed_failures = 0;
                        self.iure.feed_next = Some(Instant::now() + self.iure_poll_every());
                    }
                    Err(e) => {
                        self.iure_on_error(&e);
                        self.iure.feed_failures = self.iure.feed_failures.saturating_add(1).min(6);
                        let wait = self
                            .iure_poll_every()
                            .saturating_mul(1u32 << self.iure.feed_failures.min(5))
                            .min(MAX_BACKOFF);
                        self.iure.feed_next = Some(Instant::now() + wait);
                    }
                }
            }
            Msg::Agenda { result, user } => {
                self.iure.agenda_busy = false;
                if user.is_some() {
                    self.iure.user_id = user;
                }
                match result {
                    Ok(a) => {
                        self.iure_on_success();
                        self.iure_apply_agenda(&a);
                        self.iure.agenda_next = Some(Instant::now() + AGENDA_EVERY);
                    }
                    Err(e) => {
                        self.iure_on_error(&e);
                        self.iure.agenda_next = Some(Instant::now() + AGENDA_EVERY);
                    }
                }
            }
            Msg::Done { message_id, result, done } => {
                self.iure.busy = self.iure.busy.saturating_sub(1);
                match result {
                    Ok(links) => {
                        self.iure_on_success();
                        self.iure_remember(&message_id, &links);
                        self.notices.push(Notice {
                            kind: super::NoticeKind::Success,
                            text: done,
                            undo: false,
                            secs: None,
                        });
                        // Lo nuevo aparece en la próxima consulta.
                        self.iure.contexts.clear();
                    }
                    Err(e) => {
                        self.iure_on_error(&e);
                        self.notices.push(Notice::error(e.text()));
                    }
                }
            }
            Msg::Leads { message, result } => {
                let st = match result {
                    Ok(v) => LeadSuggestions::Ready(v),
                    Err(e) => {
                        self.iure_on_error(&e);
                        LeadSuggestions::Failed(e.text())
                    }
                };
                if self.iure.leads.as_ref().is_some_and(|(m, _)| *m == message) {
                    self.iure.leads = Some((message, st));
                }
            }
            Msg::Login(LoginMsg::NeedsTotp { session, token }) => {
                self.iure.pending_totp = Some((session, token));
                self.iure.login = LoginState { busy: false, needs_totp: true, error: None };
            }
            Msg::Login(LoginMsg::Failed(t)) => {
                let needs_totp = self.iure.pending_totp.is_some();
                self.iure.login = LoginState { busy: false, needs_totp, error: Some(t) };
            }
            Msg::Login(LoginMsg::Done { session, name, email }) => {
                self.iure.pending_totp = None;
                self.iure.login = LoginState::default();
                // Se recuerda la sesión (nunca la contraseña) para las demás apps de la suite.
                let acc = session.account().clone();
                if let Some(v) = self.iure_vault()
                    && let Ok(json) = serde_json::to_string(&session.export())
                    && let Err(e) = v.write(&acc, &json)
                {
                    self.notices.push(Notice::error(Text::new(
                        format!("Signed in, but the session could not be remembered ({e})."),
                        format!("Sesión iniciada, pero no se pudo recordar ({e})."),
                    )));
                }
                if matches!(self.iure.setup, Some(IureSetup::System)) {
                    let _ = iurefficient_connect::account::set_active(&acc, APP);
                }
                let _ = shared_entry_user(&acc);
                self.iure.generation = self.iure.generation.wrapping_add(1);
                self.iure_set_ready(session, email, name);
                self.notices.push(Notice::success("Connected to Iurefficient", "Conectado a Iurefficient"));
            }
            Msg::Reply { message, result } => {
                self.iure.busy = self.iure.busy.saturating_sub(1);
                if result.is_ok() {
                    self.iure_on_success();
                }
                self.iure_open_reply(message, result);
            }
            Msg::Plain(r) => match r {
                Ok(t) => {
                    self.iure_on_success();
                    if !t.en.is_empty() {
                        self.notices.push(Notice::success(t.en, t.es));
                    }
                }
                Err(e) => {
                    self.iure_on_error(&e);
                    self.notices.push(Notice::error(e.text()));
                }
            },
        }
    }

    // ------------------------------------------------------------------ agenda en el almacén

    fn iure_account(&self) -> Option<AccountId> {
        if let Ok(Some(b)) = self.store.sync_state(ACCOUNT_KEY)
            && let Ok(arr) = <[u8; 8]>::try_from(b.as_slice())
        {
            let id = u64::from_le_bytes(arr);
            if id != 0 {
                return Some(AccountId(id));
            }
        }
        let id = self.store.new_account_id().ok()?;
        self.store.put_sync_state(ACCOUNT_KEY, &id.get().to_le_bytes()).ok()?;
        Some(id)
    }

    /// Calendario «Iurefficient» del almacén (lo crea si falta).
    fn iure_calendar(&self, account: AccountId) -> Option<Calendar> {
        let cals = self.store.calendars().ok()?;
        if let Some(c) = cals.into_iter().find(|c| c.remote_path == IURE_CALENDAR_PATH) {
            return Some(c);
        }
        let mut c = Calendar {
            id: iuredesk_model::CalendarId::UNASSIGNED,
            account,
            name: "Iurefficient".into(),
            color: Some(IURE_COLOR),
            remote_path: IURE_CALENDAR_PATH.into(),
            read_only: true,
            sync_token: None,
        };
        c.id = self.store.put_calendar(&c).ok()?;
        Some(c)
    }

    fn iure_task_list(&self, account: AccountId) -> Option<TaskList> {
        let lists = self.store.task_lists().ok()?;
        if let Some(l) = lists.into_iter().find(|l| l.remote_path == IURE_TASKS_PATH) {
            return Some(l);
        }
        let mut l = TaskList {
            id: TaskListId::UNASSIGNED,
            account: Some(account),
            name: "Iurefficient".into(),
            remote_path: IURE_TASKS_PATH.into(),
            sync_token: None,
        };
        l.id = self.store.put_task_list(&l).ok()?;
        Some(l)
    }

    /// Copia la agenda y las tareas abiertas de la instancia al almacén (fuente propia: no la
    /// toca `pim-sync`). Lo que ya no está en la instancia se quita.
    fn iure_apply_agenda(&mut self, a: &Agenda) {
        let Some(account) = self.iure_account() else { return };
        let mut changed = false;
        if let Some(cal) = self.iure_calendar(account) {
            let old = self
                .store
                .events_between(&[cal.id], DateTime::<Utc>::MIN_UTC, DateTime::<Utc>::MAX_UTC)
                .unwrap_or_default();
            let by_remote: HashMap<String, CalEvent> =
                old.into_iter().filter_map(|e| e.remote_id.clone().map(|r| (r, e))).collect();
            let mut seen = HashSet::new();
            for e in a.events.iter().take(2_000) {
                let Some(new) = event_from_remote(e, cal.id) else { continue };
                let rid = new.remote_id.clone().unwrap_or_default();
                seen.insert(rid.clone());
                let mut new = new;
                if let Some(cur) = by_remote.get(&rid) {
                    new.id = cur.id;
                    if *cur == new {
                        continue;
                    }
                }
                if self.store.put_event(&new).is_ok() {
                    changed = true;
                }
            }
            for (rid, e) in &by_remote {
                if !seen.contains(rid) && self.store.delete_event(e.id).is_ok() {
                    changed = true;
                }
            }
        }
        if let Some(list) = self.iure_task_list(account) {
            let old = self.store.tasks(list.id).unwrap_or_default();
            let by_remote: HashMap<String, Task> =
                old.into_iter().filter_map(|t| t.remote_id.clone().map(|r| (r, t))).collect();
            let mut seen = HashSet::new();
            for t in a.tasks.iter().take(500) {
                let Some(mut new) = task_from_remote(t, list.id) else { continue };
                let rid = new.remote_id.clone().unwrap_or_default();
                seen.insert(rid.clone());
                if let Some(cur) = by_remote.get(&rid) {
                    new.id = cur.id;
                    new.created = cur.created;
                    if *cur == new {
                        continue;
                    }
                }
                if self.store.put_task(&new).is_ok() {
                    changed = true;
                }
            }
            for (rid, t) in &by_remote {
                // Terminada aquí y aún sin confirmar en la instancia: se deja hasta la próxima.
                if !seen.contains(rid) && !t.is_done() && self.store.delete_task(t.id).is_ok() {
                    changed = true;
                }
            }
        }
        if changed {
            if let Ok(c) = self.store.calendars() {
                self.calendars = c;
            }
            self.events_cache = None;
            self.data_gen = self.data_gen.wrapping_add(1);
        }
    }

    /// Una tarea de la lista «Iurefficient» cambió aquí: se lleva a la instancia.
    pub(crate) fn iure_task_toggled(&mut self, t: &Task) {
        let Some(rid) = t.remote_id.as_deref().and_then(|r| r.strip_prefix(REMOTE_PREFIX)) else { return };
        let Some(list) = self.store.get_task_list(t.list).ok().flatten() else { return };
        if list.remote_path != IURE_TASKS_PATH {
            return;
        }
        let Some(s) = self.iure_remote() else {
            // Nunca en silencio: sin conexión, la instancia no se entera (el ejemplo no tiene red).
            if matches!(self.iure.backend, Backend::None) {
                self.notices.push(Notice::error(Text::new(
                    "Iurefficient is not connected: the task changed only on this computer and will reopen on the next sync. Complete it in Iurefficient.",
                    "Iurefficient no está conectado: la tarea cambió solo en este equipo y se reabrirá en la próxima sincronización. Termínala en Iurefficient.",
                )));
            }
            return;
        };
        let (rid, done) = (rid.to_string(), t.is_done());
        self.iure_spawn(async move {
            let r = iure::call(iurefficient_connect::pim::set_task_done(&s, &rid, done)).await;
            Msg::Plain(r.map(|_| Text::default()))
        });
    }

    /// ¿Se puede cambiar una tarea de la lista «Iurefficient» desde aquí? (sesión lista o el
    /// ejemplo sin red). Si no, la interfaz ofrece abrirla en la web.
    pub fn iure_can_write(&self) -> bool {
        self.iure.status.is_ready() && !matches!(self.iure.backend, Backend::None)
    }

    /// ¿La tarea es de la lista «Iurefficient»?
    pub fn is_iure_task(&self, list: TaskListId) -> bool {
        self.store.get_task_list(list).ok().flatten().is_some_and(|l| l.remote_path == IURE_TASKS_PATH)
    }

    /// Mensaje de prueba: ¿hay consulta del panel esperando? (pruebas).
    #[doc(hidden)]
    pub fn iure_pending(&self) -> bool {
        self.iure.want.is_some() || !self.iure.inflight.is_empty() || self.iure.busy > 0 || self.iure.feed_busy
    }
}

fn login_error(e: &anyhow::Error) -> Text {
    match iure::classify(e) {
        IureError::Server(t) => t,
        other => other.text(),
    }
}

fn parse_instant(s: &str) -> Option<DateTime<Utc>> {
    let s = s.trim();
    if let Ok(d) = DateTime::parse_from_rfc3339(s) {
        return Some(d.with_timezone(&Utc));
    }
    // Sin zona: la instancia lo toma como UTC.
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f").ok().map(|n| n.and_utc())
}

fn parse_day(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.trim().get(..10)?, "%Y-%m-%d").ok()
}

fn event_from_remote(
    e: &iurefficient_connect::pim::CalendarEvent,
    cal: iuredesk_model::CalendarId,
) -> Option<CalEvent> {
    if e.id.trim().is_empty() {
        return None;
    }
    let (start, end) = if e.all_day {
        let s = parse_day(&e.start)?;
        let mut en = parse_day(&e.end).unwrap_or(s);
        if en <= s {
            en = s.succ_opt().unwrap_or(s);
        }
        (EventTime::Date(s), EventTime::Date(en))
    } else {
        let s = parse_instant(&e.start)?;
        let mut en = parse_instant(&e.end).unwrap_or(s);
        if en < s {
            en = s;
        }
        (EventTime::DateTime { utc: s, tzid: None }, EventTime::DateTime { utc: en, tzid: None })
    };
    let mut description = iure::clip(e.description.as_deref().unwrap_or(""), 4_000);
    if let Some(c) = e.case_title.as_deref().filter(|c| !c.is_empty()) {
        description = format!("{}\n\nCaso: {}", description, iure::clip(c, 200)).trim().to_string();
    }
    if let Some(u) = e.meeting_url.as_deref().filter(|u| u.starts_with("https://")) {
        description = format!("{description}\n{}", iure::clip(u, 500)).trim().to_string();
    }
    Some(CalEvent {
        id: EventId::UNASSIGNED,
        calendar: cal,
        uid: format!("iurefficient-event-{}@iuredesk", iure::clip(&e.id, 100)),
        summary: iure::clip(&e.title, 300),
        location: iure::clip(e.location.as_deref().unwrap_or(""), 300),
        description,
        start,
        end,
        rrule: e.recurrence_rule.as_deref().map(|r| iure::clip(r, 500)).filter(|r| !r.is_empty()),
        reminders: e
            .reminder_minutes
            .filter(|m| (0..=40_320).contains(m))
            .map(|m| vec![iuredesk_model::Reminder { minutes_before: m }])
            .unwrap_or_default(),
        remote_id: Some(format!("{REMOTE_PREFIX}{}", iure::clip(&e.id, 100))),
        updated: e.updated_at.as_deref().and_then(parse_instant),
        ..Default::default()
    })
}

fn task_from_remote(t: &iurefficient_connect::pim::Task, list: TaskListId) -> Option<Task> {
    if t.id.trim().is_empty() {
        return None;
    }
    let status = match t.status.as_str() {
        "completed" => TaskStatus::Completed,
        "in_progress" => TaskStatus::InProcess,
        "cancelled" => TaskStatus::Cancelled,
        _ => TaskStatus::NeedsAction,
    };
    let mut description = iure::clip(t.description.as_deref().unwrap_or(""), 4_000);
    if let Some(c) = t.case_title.as_deref().filter(|c| !c.is_empty()) {
        description = format!("{description}\n\nCaso: {}", iure::clip(c, 200)).trim().to_string();
    }
    Some(Task {
        id: TaskId::UNASSIGNED,
        list,
        uid: format!("iurefficient-task-{}@iuredesk", iure::clip(&t.id, 100)),
        summary: iure::clip(&t.title, 300),
        description,
        due: t.due_date.as_deref().and_then(parse_day).map(EventTime::Date),
        priority: match t.priority.as_deref() {
            Some("urgent" | "high") => 1,
            Some("medium") => 5,
            Some("low") => 9,
            _ => 0,
        },
        percent_complete: if status == TaskStatus::Completed { 100 } else { 0 },
        status,
        remote_id: Some(format!("{REMOTE_PREFIX}{}", iure::clip(&t.id, 100))),
        updated: t.updated_at.as_deref().and_then(parse_instant),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_dates_are_parsed_without_panicking() {
        assert!(parse_instant("2026-10-27T16:00:00Z").is_some());
        assert!(parse_instant("2026-10-27T16:00:00").is_some());
        assert!(parse_instant("basura").is_none());
        assert_eq!(parse_day("2026-10-27"), NaiveDate::from_ymd_opt(2026, 10, 27));
        assert_eq!(parse_day("2026-10-27T00:00:00"), NaiveDate::from_ymd_opt(2026, 10, 27));
        assert!(parse_day("2").is_none());
        let e = iurefficient_connect::pim::CalendarEvent {
            id: "e1".into(),
            title: "Audiencia".into(),
            start: "2026-10-27T16:00:00Z".into(),
            end: "2026-10-27T15:00:00Z".into(),
            ..Default::default()
        };
        let ev = event_from_remote(&e, iuredesk_model::CalendarId(3)).unwrap();
        assert!(ev.end.to_utc() >= ev.start.to_utc());
        assert!(event_from_remote(&iurefficient_connect::pim::CalendarEvent::default(), ev.calendar).is_none());
        let all = iurefficient_connect::pim::CalendarEvent {
            id: "e2".into(),
            all_day: true,
            start: "2026-10-27".into(),
            end: "2026-10-27".into(),
            ..Default::default()
        };
        let ev = event_from_remote(&all, ev.calendar).unwrap();
        assert!(ev.start.is_all_day() && ev.end.to_utc() > ev.start.to_utc());
    }
}
