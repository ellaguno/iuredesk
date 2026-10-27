//! Asistente ✦ en la aplicación (F4.2): une `iuredesk-assistant` con la sesión.
//!
//! «La IA propone, el usuario confirma»: [`Assist::request`] prepara la tarea sin red y, si el
//! proveedor es externo y el usuario no pidió «no volver a preguntar», la deja esperando el
//! consentimiento ([`Assist::consent`]) con el [`DataScope`] exacto. Al confirmar, corre en el
//! runtime de fondo con flujo de texto (lo recibido se lee con [`Assist::streamed`]) y se puede
//! cancelar. El resultado ([`AiValue`]) es una propuesta: crear un evento, una tarea, un lead o un
//! ticket siempre pasa por los diálogos de confirmación que ya existen.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use iuredesk_assistant::prompts::PromptId;
use iuredesk_assistant::provider::BoxFuture;
use iuredesk_assistant::schema::{
    DocumentOutline, DraftReplies, Extraction, Importance, LeadTicketClassification, MeetingMinutes, Rewritten,
    TaskOutput, ThreadSummary, Triage,
};
use iuredesk_assistant::tasks::{self, Prepared};
use iuredesk_assistant::{
    AiError, AiProvider, AssistantConfig, CancellationToken, ChatMessage, ChatRequest, Completion, DataField,
    DataScope, Lang, Length, Limits, ProviderConfig, ProviderKind, Role, StreamEvent, Target, TaskOptions, Tone,
};
use iuredesk_model::MessageId;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedSender;

use crate::background::Waker;
use crate::lang::Text;
use crate::session::Session;

/// Hilos de más de estos caracteres ofrecen «✦ Resumir».
pub const LONG_THREAD_CHARS: usize = 1_500;
/// Mensajes del chat que se conservan.
const MAX_CHAT: usize = 40;
/// Caracteres máximos de un mensaje del chat.
const MAX_CHAT_CHARS: usize = 8_000;

/// Qué se le pide al asistente.
#[derive(Clone, Debug, PartialEq)]
pub enum AiTask {
    /// Resumen del hilo del mensaje.
    Summarize(MessageId),
    /// Tres respuestas rápidas al mensaje.
    Replies(MessageId),
    /// Fechas, compromisos, montos… y ¿lead o ticket?
    Analyze(MessageId),
    /// Reescribir texto del redactor `window`.
    Rewrite { window: u64, text: String, tone: Tone, length: Length, lang: Lang },
    /// Redactar la respuesta en el redactor `window` al mensaje `msg`.
    DraftReply { window: u64, msg: MessageId, tone: Tone, instructions: String },
    /// «¿Qué necesito atender hoy?»
    Triage,
    /// Minuta desde una transcripción.
    Minutes { transcript: String, title: Option<String> },
    /// Documento (Markdown para IureEditor) desde correos.
    Document { msgs: Vec<MessageId>, purpose: String },
    /// Chat libre con el proveedor.
    Chat(String),
    /// Prueba de conexión (no lleva datos del usuario).
    Test,
}

/// Clave de un trabajo (uno por clave; repetir la petición sustituye al anterior).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AiKey {
    Summary(MessageId),
    Replies(MessageId),
    Analysis(MessageId),
    Compose(u64),
    Triage,
    Minutes,
    Document,
    Chat,
    Test,
}

impl AiTask {
    /// La clave del trabajo.
    pub fn key(&self) -> AiKey {
        match self {
            AiTask::Summarize(m) => AiKey::Summary(*m),
            AiTask::Replies(m) => AiKey::Replies(*m),
            AiTask::Analyze(m) => AiKey::Analysis(*m),
            AiTask::Rewrite { window, .. } | AiTask::DraftReply { window, .. } => AiKey::Compose(*window),
            AiTask::Triage => AiKey::Triage,
            AiTask::Minutes { .. } => AiKey::Minutes,
            AiTask::Document { .. } => AiKey::Document,
            AiTask::Chat(_) => AiKey::Chat,
            AiTask::Test => AiKey::Test,
        }
    }

    /// Nombre de la tarea para el diálogo de consentimiento.
    pub fn label(&self) -> Text {
        match self {
            AiTask::Summarize(_) => Text::new("Summarize the conversation", "Resumir la conversación"),
            AiTask::Replies(_) => Text::new("Suggest replies", "Sugerir respuestas"),
            AiTask::Analyze(_) => Text::new("Analyze the message", "Analizar el mensaje"),
            AiTask::Rewrite { .. } => Text::new("Rewrite your text", "Reescribir tu texto"),
            AiTask::DraftReply { .. } => Text::new("Draft the reply", "Redactar la respuesta"),
            AiTask::Triage => Text::new("What needs my attention today?", "¿Qué necesito atender hoy?"),
            AiTask::Minutes { .. } => Text::new("Meeting minutes", "Minuta de la reunión"),
            AiTask::Document { .. } => Text::new("Create a document", "Crear documento"),
            AiTask::Chat(_) => Text::new("Chat", "Chat"),
            AiTask::Test => Text::new("Connection test", "Prueba de conexión"),
        }
    }

    fn kind(&self) -> Option<PromptId> {
        Some(match self {
            AiTask::Summarize(_) => PromptId::SummarizeThread,
            AiTask::Replies(_) | AiTask::DraftReply { .. } => PromptId::DraftReply,
            AiTask::Analyze(_) => PromptId::Extract,
            AiTask::Rewrite { .. } => PromptId::Rewrite,
            AiTask::Triage => PromptId::TriageToday,
            AiTask::Minutes { .. } => PromptId::MeetingMinutes,
            AiTask::Document { .. } => PromptId::DocumentOutline,
            AiTask::Chat(_) | AiTask::Test => return None,
        })
    }
}

/// Un correo del triage, ya ordenado.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TriageRow {
    pub id: MessageId,
    pub importance: Importance,
    pub reason: String,
    pub from: String,
    pub subject: String,
}

/// Análisis de un correo: lo extraído y la clasificación (si se pudo).
#[derive(Clone, Debug, PartialEq)]
pub struct Analysis {
    pub extraction: Extraction,
    pub classification: Option<LeadTicketClassification>,
}

/// Lo que propone el asistente.
#[derive(Clone, Debug, PartialEq)]
pub enum AiValue {
    Summary(ThreadSummary),
    Replies(DraftReplies),
    Analysis(Box<Analysis>),
    /// Texto reescrito o respuesta redactada (para el redactor).
    Text(String),
    Triage(Vec<TriageRow>),
    Minutes(Box<MeetingMinutes>),
    Outline(DocumentOutline),
    /// Respuesta del chat (o de la prueba).
    Chat(String),
}

/// Estado de un trabajo.
#[derive(Clone, Debug, PartialEq)]
pub enum AiState {
    Running,
    Done(AiValue),
    Failed(Text),
    Cancelled,
}

/// Un trabajo del asistente.
#[derive(Debug)]
pub struct AiJob {
    pub task: AiTask,
    pub provider: String,
    pub scope: DataScope,
    pub state: AiState,
    generation: u64,
    stream: Arc<Mutex<String>>,
    cancel: CancellationToken,
}

/// Mensaje del chat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatLine {
    pub mine: bool,
    pub text: String,
}

/// Petición esperando que el usuario vea qué se enviará.
pub struct Consent {
    pub task: AiTask,
    pub provider_id: String,
    pub provider: String,
    pub scope: DataScope,
    runner: Runner,
    target: Arc<dyn AiProvider>,
    limits: Limits,
}

impl std::fmt::Debug for Consent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Consent").field("task", &self.task).field("provider", &self.provider).finish_non_exhaustive()
    }
}

/// Sondeo de Copilot.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum CopilotProbe {
    #[default]
    Unknown,
    Probing,
    Available,
    Unavailable(String),
}

/// Fabrica el proveedor de cada tarea (`None` = chat). Pruebas, capturas y buzón de ejemplo.
pub type ProviderFactory = Arc<dyn Fn(Option<PromptId>) -> Arc<dyn AiProvider> + Send + Sync>;

type Runner = Box<
    dyn FnOnce(
            Arc<dyn AiProvider>,
            Limits,
            UnboundedSender<StreamEvent>,
            CancellationToken,
        ) -> BoxFuture<'static, Result<AiValue, AiError>>
        + Send,
>;

/// Preferencias del asistente que no son de los proveedores.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssistPrefs {
    /// Proveedores para los que no se vuelve a preguntar qué se envía.
    #[serde(default)]
    pub no_ask: BTreeSet<String>,
    /// Generar las respuestas rápidas al abrir un mensaje (por omisión, solo bajo demanda).
    #[serde(default)]
    pub auto_replies: bool,
}

enum Msg {
    Done { key: AiKey, generation: u64, result: Result<AiValue, AiError> },
    Probe(CopilotProbe),
}

/// El asistente de la aplicación.
pub struct Assist {
    config: AssistantConfig,
    config_path: Option<PathBuf>,
    prefs: AssistPrefs,
    prefs_path: Option<PathBuf>,
    factory: Option<ProviderFactory>,
    jobs: HashMap<AiKey, AiJob>,
    consent: Option<Consent>,
    chat: Vec<ChatLine>,
    generation: u64,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    rt: tokio::runtime::Handle,
    waker: Waker,
    copilot: CopilotProbe,
    triage_meta: HashMap<String, (MessageId, String, String)>,
}

impl std::fmt::Debug for Assist {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Assist").field("jobs", &self.jobs.len()).finish_non_exhaustive()
    }
}

fn lang() -> Lang {
    if crate::lang::spanish() { Lang::Es } else { Lang::En }
}

fn ai_text(e: &AiError) -> Text {
    Text::new(e.human(false), e.human(true))
}

fn erase<T: TaskOutput>(p: Prepared<T>, map: impl FnOnce(T) -> AiValue + Send + 'static) -> (DataScope, Runner) {
    let scope = p.scope.clone();
    let runner: Runner = Box::new(move |provider, limits, tx, cancel| {
        Box::pin(async move {
            let prop = tasks::run(provider.as_ref(), p, &limits, Some(&tx), &cancel).await?;
            Ok(map(prop.value))
        })
    });
    (scope, runner)
}

impl Assist {
    /// Asistente con la configuración de `<config>/iuredesk/assistant.json` (o la de memoria si
    /// `config_path` es `None`).
    pub fn new(session: &Session, config_path: Option<PathBuf>) -> Self {
        let config = config_path.as_deref().and_then(|p| AssistantConfig::load(p).ok()).unwrap_or_default();
        let prefs_path = config_path.as_ref().map(|p| p.with_file_name("assistant-ui.json"));
        let prefs = prefs_path
            .as_deref()
            .and_then(|p| std::fs::read(p).ok())
            .filter(|b| b.len() < 256 * 1024)
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let (tx, rx) = channel();
        let factory = if session.is_sample() { Some(sample_factory()) } else { None };
        Assist {
            config,
            config_path,
            prefs,
            prefs_path,
            factory,
            jobs: HashMap::new(),
            consent: None,
            chat: Vec::new(),
            generation: 0,
            tx,
            rx,
            rt: session.runtime(),
            waker: session.waker(),
            copilot: CopilotProbe::Unknown,
            triage_meta: HashMap::new(),
        }
    }

    /// Usa siempre este proveedor (pruebas y capturas).
    pub fn with_factory(mut self, f: ProviderFactory) -> Self {
        self.factory = Some(f);
        self
    }

    // ------------------------------------------------------------------ configuración

    pub fn config(&self) -> &AssistantConfig {
        &self.config
    }

    pub fn prefs(&self) -> &AssistPrefs {
        &self.prefs
    }

    /// Cambia la configuración (valida y guarda). Devuelve el error en lenguaje humano.
    pub fn set_config(&mut self, cfg: AssistantConfig) -> Result<(), Text> {
        if let Some(p) = &self.config_path {
            cfg.save(p).map_err(|e| ai_text(&e))?;
        }
        self.config = cfg;
        Ok(())
    }

    /// Cambia las preferencias (y las guarda).
    pub fn set_prefs(&mut self, prefs: AssistPrefs) {
        self.prefs = prefs;
        if let Some(p) = &self.prefs_path
            && let Ok(b) = serde_json::to_vec_pretty(&self.prefs)
        {
            if let Some(d) = p.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            let _ = std::fs::write(p, b);
        }
    }

    /// El proveedor elegido (configuración).
    pub fn default_provider(&self) -> Option<&ProviderConfig> {
        self.config.default_provider()
    }

    /// Nombre del proveedor que se usará.
    pub fn provider_label(&self) -> String {
        if let Some(f) = &self.factory {
            return f(None).label();
        }
        self.default_provider().map(|p| p.kind.label()).unwrap_or_else(|| "—".into())
    }

    /// Guarda la clave de API de un proveedor en el llavero.
    pub fn set_api_key(&self, session: &Session, id: &str, key: &str) -> Result<(), Text> {
        let Some(p) = self.config.provider(id) else {
            return Err(Text::new("Unknown provider.", "Proveedor desconocido."));
        };
        let r = if key.trim().is_empty() {
            iuredesk_assistant::config::delete_api_key(session.secrets().as_ref(), p)
        } else {
            iuredesk_assistant::config::set_api_key(session.secrets().as_ref(), p, key)
        };
        r.map_err(|e| ai_text(&e))
    }

    /// ¿Hay clave guardada?
    pub fn has_api_key(&self, session: &Session, id: &str) -> bool {
        self.config.provider(id).is_some_and(|p| {
            iuredesk_assistant::config::api_key(session.secrets().as_ref(), p).ok().flatten().is_some()
        })
    }

    /// Estado del sondeo de Copilot.
    pub fn copilot(&self) -> &CopilotProbe {
        &self.copilot
    }

    /// Comprueba si el inquilino de Microsoft 365 tiene Copilot (solo entonces se ofrece).
    pub fn probe_copilot(&mut self, session: &Session) {
        let ctx = session.ai_provider_context();
        let Some((acc, tokens)) = ctx.microsoft else {
            self.copilot = CopilotProbe::Unavailable(
                crate::lang::pick("No Microsoft 365 account.", "No hay cuenta de Microsoft 365.").into(),
            );
            return;
        };
        let p = match iuredesk_assistant::providers::copilot::CopilotProvider::new(
            acc,
            tokens,
            iuredesk_assistant::providers::copilot::DEFAULT_BASE,
            &ctx.time_zone,
        ) {
            Ok(p) => p,
            Err(e) => {
                self.copilot = CopilotProbe::Unavailable(e.human(crate::lang::spanish()));
                return;
            }
        };
        self.copilot = CopilotProbe::Probing;
        let tx = self.tx.clone();
        let waker = self.waker.clone();
        self.rt.spawn(async move {
            let r = match tokio::time::timeout(Duration::from_secs(30), p.probe()).await {
                Ok(iuredesk_assistant::providers::copilot::CopilotCapability::Available) => CopilotProbe::Available,
                Ok(other) => CopilotProbe::Unavailable(format!("{other:?}")),
                Err(_) => CopilotProbe::Unavailable("timeout".into()),
            };
            let _ = tx.send(Msg::Probe(r));
            waker();
        });
    }

    fn resolve(
        &self,
        session: &Session,
        kind: Option<PromptId>,
    ) -> Result<(String, Arc<dyn AiProvider>, Limits), Text> {
        if let Some(f) = &self.factory {
            let p = f(kind);
            return Ok((p.label(), p, Limits { timeout: Duration::from_secs(60), ..Limits::default() }));
        }
        if !self.config.enabled {
            return Err(Text::new(
                "The assistant is turned off in Settings → Assistant.",
                "El asistente está desactivado en Ajustes → Asistente.",
            ));
        }
        let cfg = self.default_provider().ok_or_else(|| {
            Text::new("Choose a provider in Settings → Assistant.", "Elige un proveedor en Ajustes → Asistente.")
        })?;
        if cfg.kind == ProviderKind::Iurefficient && !session.iure_status().is_ready() {
            return Err(Text::new(
                "Connect Iurefficient in Settings (or choose another provider in Settings → Assistant).",
                "Conecta Iurefficient en Ajustes (o elige otro proveedor en Ajustes → Asistente).",
            ));
        }
        let ctx = session.ai_provider_context();
        let p = iuredesk_assistant::build_provider(cfg, session.secrets().as_ref(), &ctx).map_err(|e| ai_text(&e))?;
        let limits = Limits { timeout: Duration::from_secs(cfg.timeout_secs.clamp(5, 600)), ..Limits::default() };
        Ok((cfg.id.clone(), Arc::from(p), limits))
    }

    fn options(&self, session: &Session) -> TaskOptions {
        let mut o = TaskOptions {
            lang: lang(),
            me: session.ai_me(),
            today: session.today(),
            time_zone: crate::time::user_tz(session.zone()).name().to_string(),
            ..TaskOptions::default()
        };
        if let Some(cfg) = self.default_provider()
            && self.factory.is_none()
        {
            o.scope.redact = cfg.redact;
            o.max_output_tokens = cfg.max_output_tokens.clamp(256, 16_384);
        }
        o
    }

    // ------------------------------------------------------------------ peticiones

    /// Prepara una tarea. Con un proveedor externo queda esperando [`Assist::confirm`] (salvo
    /// «no volver a preguntar»); si no, arranca ya.
    pub fn request(&mut self, session: &mut Session, task: AiTask) -> Result<(), Text> {
        let (provider_id, provider, limits) = self.resolve(session, task.kind())?;
        let target = Target::of(provider.as_ref());
        let opts = self.options(session);
        let (scope, runner) = self.prepare(session, &task, &target, &opts)?;
        let consent =
            Consent { task, provider_id, provider: target.label.clone(), scope, runner, target: provider, limits };
        let ask = target.external
            && !self.prefs.no_ask.contains(&consent.provider_id)
            && !matches!(consent.task, AiTask::Test);
        if ask {
            self.consent = Some(consent);
        } else {
            self.start(consent);
        }
        Ok(())
    }

    fn prepare(
        &mut self,
        session: &mut Session,
        task: &AiTask,
        target: &Target,
        opts: &TaskOptions,
    ) -> Result<(DataScope, Runner), Text> {
        let none = || Text::new("That message is no longer available.", "Ese mensaje ya no está disponible.");
        Ok(match task {
            AiTask::Summarize(m) => {
                let thread = session.ai_thread(*m);
                if thread.is_empty() {
                    return Err(none());
                }
                erase(tasks::prepare_summarize_thread(target, &thread, opts), AiValue::Summary)
            }
            AiTask::Replies(m) => {
                let thread = session.ai_thread(*m);
                if thread.is_empty() {
                    return Err(none());
                }
                erase(tasks::prepare_draft_reply(target, &thread, Tone::Neutral, opts.lang, "", opts), AiValue::Replies)
            }
            AiTask::DraftReply { msg, tone, instructions, .. } => {
                let thread = session.ai_thread(*msg);
                if thread.is_empty() {
                    return Err(none());
                }
                let p = tasks::prepare_draft_reply(target, &thread, *tone, opts.lang, instructions, opts);
                erase(p, |d: DraftReplies| {
                    let pick = d
                        .suggestions
                        .iter()
                        .find(|s| s.kind == iuredesk_assistant::schema::ReplyKind::Neutral)
                        .or_else(|| d.suggestions.first())
                        .map(|s| s.body.clone())
                        .unwrap_or_default();
                    AiValue::Text(pick)
                })
            }
            AiTask::Rewrite { text, tone, length, lang, .. } => {
                if text.trim().is_empty() {
                    return Err(Text::new("Write something first.", "Primero escribe algo."));
                }
                erase(tasks::prepare_rewrite(target, text, *tone, *length, *lang, opts), |r: Rewritten| {
                    AiValue::Text(r.text)
                })
            }
            AiTask::Analyze(m) => {
                let email = session.ai_email(*m).ok_or_else(none)?;
                let a = tasks::prepare_extract(target, &email, opts);
                let b = tasks::prepare_classify_lead_or_ticket(target, &email, opts);
                // Un solo consentimiento cubre las dos llamadas (mismos datos).
                let mut scope = a.scope.clone();
                scope.total_chars = scope.total_chars.saturating_add(b.scope.total_chars);
                let runner: Runner = Box::new(move |provider, limits, tx, cancel| {
                    Box::pin(async move {
                        let ex = tasks::run(provider.as_ref(), a, &limits, Some(&tx), &cancel).await?.value;
                        let cl = tasks::run(provider.as_ref(), b, &limits, None, &cancel).await.ok().map(|p| p.value);
                        Ok(AiValue::Analysis(Box::new(Analysis { extraction: ex, classification: cl })))
                    })
                });
                (scope, runner)
            }
            AiTask::Triage => {
                let unread = session.ai_unread();
                if unread.is_empty() {
                    return Err(Text::new(
                        "No unread mail in your inbox. Nothing to triage.",
                        "No hay correo sin leer en tu bandeja. Nada que ordenar.",
                    ));
                }
                self.triage_meta =
                    unread.iter().map(|(id, e)| (e.id.clone(), (*id, e.from.display(), e.subject.clone()))).collect();
                let inputs: Vec<_> = unread.into_iter().map(|(_, e)| e).collect();
                let meta = self.triage_meta.clone();
                erase(tasks::prepare_triage_today(target, &inputs, opts), move |t: Triage| {
                    AiValue::Triage(
                        t.items
                            .into_iter()
                            .filter_map(|i| {
                                let (id, from, subject) = meta.get(&i.id)?.clone();
                                Some(TriageRow { id, importance: i.importance, reason: i.reason, from, subject })
                            })
                            .collect(),
                    )
                })
            }
            AiTask::Minutes { transcript, title } => {
                if transcript.trim().is_empty() {
                    return Err(Text::new(
                        "Paste or load a transcript first.",
                        "Primero pega o carga una transcripción.",
                    ));
                }
                erase(tasks::prepare_meeting_minutes(target, transcript, title.as_deref(), opts), |m| {
                    AiValue::Minutes(Box::new(m))
                })
            }
            AiTask::Document { msgs, purpose } => {
                let emails: Vec<_> = msgs.iter().take(20).filter_map(|m| session.ai_email(*m)).collect();
                if emails.is_empty() {
                    return Err(Text::new(
                        "Select one or more messages first.",
                        "Primero selecciona uno o más correos.",
                    ));
                }
                erase(tasks::prepare_document_outline(target, &emails, purpose, opts), AiValue::Outline)
            }
            AiTask::Chat(text) => {
                let text: String = text.trim().chars().take(MAX_CHAT_CHARS).collect();
                if text.is_empty() {
                    return Err(Text::new("Write a message.", "Escribe un mensaje."));
                }
                self.chat.push(ChatLine { mine: true, text: text.clone() });
                if self.chat.len() > MAX_CHAT {
                    self.chat.remove(0);
                }
                let messages: Vec<ChatMessage> = self
                    .chat
                    .iter()
                    .map(|l| ChatMessage {
                        role: if l.mine { Role::User } else { Role::Assistant },
                        content: l.text.clone(),
                    })
                    .collect();
                let system = crate::lang::pick(
                    "You are the IureDesk assistant (mail, calendar, contacts, notes, tasks). Answer briefly and in plain language. Never claim to have sent or created anything.",
                    "Eres el asistente de IureDesk (correo, calendario, contactos, notas y tareas). Responde breve y en lenguaje claro. Nunca digas que enviaste o creaste algo.",
                )
                .to_string();
                let req = ChatRequest {
                    system: system.clone(),
                    messages,
                    max_output_tokens: opts.max_output_tokens,
                    temperature: 0.4,
                    json: false,
                    case_id: None,
                };
                let mut scope = DataScope {
                    provider: target.label.clone(),
                    external: target.external,
                    items: 0,
                    total_chars: req.input_chars(),
                    payload: format!("{system}\n\n{}", req.flatten_messages()),
                    ..Default::default()
                };
                scope.fields.insert(DataField::UserText, req.input_chars().saturating_sub(system.chars().count()));
                scope.fields.insert(DataField::Instructions, system.chars().count());
                chat_runner(scope, req)
            }
            AiTask::Test => {
                let req = ChatRequest {
                    system: "Reply with the single word OK.".into(),
                    messages: vec![ChatMessage::user("ping")],
                    max_output_tokens: 16,
                    temperature: 0.0,
                    json: false,
                    case_id: None,
                };
                chat_runner(
                    DataScope { provider: target.label.clone(), external: target.external, ..Default::default() },
                    req,
                )
            }
        })
    }

    /// La petición esperando consentimiento.
    pub fn consent(&self) -> Option<&Consent> {
        self.consent.as_ref()
    }

    /// El usuario confirmó el envío (con «no volver a preguntar para este proveedor»).
    pub fn confirm(&mut self, remember: bool) {
        let Some(c) = self.consent.take() else { return };
        if remember {
            let mut p = self.prefs.clone();
            p.no_ask.insert(c.provider_id.clone());
            self.set_prefs(p);
        }
        self.start(c);
    }

    /// El usuario no quiso enviar.
    pub fn decline(&mut self) {
        if let Some(c) = self.consent.take()
            && matches!(c.task, AiTask::Chat(_))
        {
            self.chat.pop();
        }
    }

    fn start(&mut self, c: Consent) {
        let key = c.task.key();
        if let Some(old) = self.jobs.remove(&key) {
            old.cancel.cancel();
        }
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        let stream = Arc::new(Mutex::new(String::new()));
        let cancel = CancellationToken::new();
        let (dtx, mut drx) = tokio::sync::mpsc::unbounded_channel::<StreamEvent>();
        let sbuf = Arc::clone(&stream);
        let waker = self.waker.clone();
        self.rt.spawn(async move {
            while let Some(ev) = drx.recv().await {
                if let StreamEvent::Delta(d) = ev {
                    let mut s = sbuf.lock().unwrap_or_else(PoisonError::into_inner);
                    if s.len() < 256 * 1024 {
                        s.push_str(&d);
                    }
                }
                waker();
            }
        });
        let fut = (c.runner)(c.target, c.limits, dtx, cancel.clone());
        let tx = self.tx.clone();
        let waker = self.waker.clone();
        self.rt.spawn(async move {
            let result = fut.await;
            let _ = tx.send(Msg::Done { key, generation, result });
            waker();
        });
        self.jobs.insert(
            key,
            AiJob {
                task: c.task,
                provider: c.provider,
                scope: c.scope,
                state: AiState::Running,
                generation,
                stream,
                cancel,
            },
        );
    }

    /// Cancela un trabajo.
    pub fn cancel(&mut self, key: AiKey) {
        if let Some(j) = self.jobs.get_mut(&key)
            && j.state == AiState::Running
        {
            j.cancel.cancel();
            j.state = AiState::Cancelled;
        }
    }

    /// Olvida un trabajo (la UI cerró su tarjeta).
    pub fn dismiss(&mut self, key: AiKey) {
        if let Some(j) = self.jobs.remove(&key) {
            j.cancel.cancel();
        }
    }

    /// Un trabajo.
    pub fn job(&self, key: AiKey) -> Option<&AiJob> {
        self.jobs.get(&key)
    }

    /// Texto recibido hasta ahora (flujo).
    pub fn streamed(&self, key: AiKey) -> String {
        self.jobs.get(&key).map(|j| j.stream.lock().unwrap_or_else(PoisonError::into_inner).clone()).unwrap_or_default()
    }

    /// La conversación del chat.
    pub fn chat(&self) -> &[ChatLine] {
        &self.chat
    }

    /// Empieza otra conversación.
    pub fn clear_chat(&mut self) {
        self.dismiss(AiKey::Chat);
        self.chat.clear();
    }

    /// No queda nada en curso.
    pub fn is_idle(&self) -> bool {
        !self.jobs.values().any(|j| j.state == AiState::Running) && self.copilot != CopilotProbe::Probing
    }

    /// Atiende los resultados (cada cuadro).
    pub fn poll(&mut self) {
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Probe(p) => self.copilot = p,
                Msg::Done { key, generation, result } => {
                    let Some(j) = self.jobs.get_mut(&key) else { continue };
                    if j.generation != generation || j.state == AiState::Cancelled {
                        continue;
                    }
                    j.state = match result {
                        Ok(v) => {
                            if key == AiKey::Chat
                                && let AiValue::Chat(t) = &v
                            {
                                self.chat.push(ChatLine { mine: false, text: t.clone() });
                            }
                            AiState::Done(v)
                        }
                        Err(AiError::Cancelled) => AiState::Cancelled,
                        Err(e) => AiState::Failed(ai_text(&e)),
                    };
                }
            }
        }
    }
}

fn chat_runner(scope: DataScope, req: ChatRequest) -> (DataScope, Runner) {
    let runner: Runner = Box::new(move |provider, limits, tx, cancel| {
        Box::pin(async move {
            let c = iuredesk_assistant::run_chat(provider.as_ref(), &req, &limits, Some(&tx), &cancel).await?;
            Ok(AiValue::Chat(c.text.trim().to_string()))
        })
    });
    (scope, runner)
}

// ---------------------------------------------------------------------------------------------
// Proveedor de respuestas fijas (buzón de ejemplo, capturas y pruebas)

/// Responde con un texto calculado a partir de la petición, en trozos (como un flujo real).
pub struct CannedProvider {
    label: String,
    external: bool,
    respond: Arc<dyn Fn(&ChatRequest) -> String + Send + Sync>,
    delay: Duration,
}

impl CannedProvider {
    pub fn new(label: &str, external: bool, respond: impl Fn(&ChatRequest) -> String + Send + Sync + 'static) -> Self {
        CannedProvider { label: label.into(), external, respond: Arc::new(respond), delay: Duration::from_millis(8) }
    }

    /// Espera entre trozos.
    pub fn delay(mut self, d: Duration) -> Self {
        self.delay = d;
        self
    }
}

impl AiProvider for CannedProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Iurefficient
    }

    fn label(&self) -> String {
        self.label.clone()
    }

    fn is_external(&self) -> bool {
        self.external
    }

    fn chat<'a>(
        &'a self,
        req: &'a ChatRequest,
        _limits: &'a Limits,
        tx: Option<&'a UnboundedSender<StreamEvent>>,
    ) -> BoxFuture<'a, iuredesk_assistant::error::Result<Completion>> {
        Box::pin(async move {
            let text = (self.respond)(req);
            let chars: Vec<char> = text.chars().collect();
            for chunk in chars.chunks(24) {
                if let Some(tx) = tx {
                    let _ = tx.send(StreamEvent::Delta(chunk.iter().collect()));
                }
                if !self.delay.is_zero() {
                    tokio::time::sleep(self.delay).await;
                }
            }
            Ok(Completion { text, stop_reason: Some("end_turn".into()), usage: None })
        })
    }
}

/// Ids de correo en una petición de triage («(id: X)»).
pub fn ids_in(req: &ChatRequest) -> Vec<String> {
    let text = req.flatten_messages();
    text.split("(id: ").skip(1).filter_map(|s| s.split(')').next()).map(str::to_string).take(200).collect()
}

/// Respuesta de ejemplo de cada tarea (válida para su esquema).
pub fn sample_answer(kind: Option<PromptId>, req: &ChatRequest) -> String {
    let es = crate::lang::spanish();
    let l = |en: &str, sp: &str| if es { sp.to_string() } else { en.to_string() };
    // «Analizar» hace dos llamadas con el mismo proveedor: la segunda es la clasificación.
    let kind = if req.system.contains("sales_opportunity") { Some(PromptId::Classify) } else { kind };
    match kind {
        Some(PromptId::DraftReply) => serde_json::json!({"suggestions": [
            {"kind": "short", "body": l("Thanks, received. I'll get back to you tomorrow.", "Gracias, recibido. Te respondo mañana.")},
            {"kind": "neutral", "body": l("Hello,\n\nThank you for your message. I'll review it and send you an answer before Friday.\n\nBest regards,", "Hola:\n\nGracias por tu mensaje. Lo reviso y te envío una respuesta antes del viernes.\n\nSaludos,")},
            {"kind": "detailed", "body": l("Hello,\n\nThank you for writing. I've read your message carefully; I'll check the documents with the team and send you a full answer, with dates and next steps, before Friday.\n\nIf anything is urgent, call me.\n\nBest regards,", "Hola:\n\nGracias por escribir. Leí tu mensaje con atención; voy a revisar los documentos con el equipo y te envío una respuesta completa, con fechas y siguientes pasos, antes del viernes.\n\nSi algo es urgente, llámame.\n\nSaludos,")}
        ]})
        .to_string(),
        Some(PromptId::Rewrite) => {
            let user = req.messages.first().map(|m| m.content.as_str()).unwrap_or("");
            let body = user.split_once("===\n").map_or(user, |(_, b)| b).trim();
            serde_json::json!({"text": format!("{body}\n\n{}", l("Thank you for your time.", "Gracias por tu tiempo."))}).to_string()
        }
        Some(PromptId::SummarizeThread) => serde_json::json!({
            "summary": l("The client asks for an updated fee proposal and wants to meet next week to review the case.", "El cliente pide una propuesta de honorarios actualizada y quiere reunirse la próxima semana para revisar el caso."),
            "key_points": [l("Updated proposal requested", "Piden la propuesta actualizada"), l("Meeting next week", "Reunión la próxima semana")],
            "decisions": [],
            "open_questions": [l("Which day works for the meeting?", "¿Qué día conviene para la reunión?")],
            "next_steps": [l("Send the proposal before Friday", "Enviar la propuesta antes del viernes")]
        })
        .to_string(),
        Some(PromptId::Extract) => {
            let d = chrono::Local::now().date_naive() + chrono::Duration::days(3);
            serde_json::json!({
                "intent": "scheduling",
                "summary": l("Asks to meet and to receive the proposal.", "Pide reunirse y recibir la propuesta."),
                "events": [{"title": l("Case review meeting", "Reunión de revisión del caso"), "start": format!("{}T10:00", d.format("%Y-%m-%d")), "end": format!("{}T11:00", d.format("%Y-%m-%d"))}],
                "tasks": [{"title": l("Send the updated proposal", "Enviar la propuesta actualizada"), "due_date": d.format("%Y-%m-%d").to_string()}],
                "amounts": [], "people": [], "organizations": [],
                "sentiment": "neutral", "urgency": "high"
            })
            .to_string()
        }
        Some(PromptId::Classify) => serde_json::json!({
            "category": "sales_opportunity", "confidence": 0.82,
            "reason": l("Asks for a fee proposal for a new matter.", "Pide una propuesta de honorarios para un asunto nuevo."),
            "lead": {"name": "Carmen Brown", "company_name": "Brown & Co.", "email": "carmen.brown@example.net", "notes": l("Interested in representation", "Interesada en representación")}
        })
        .to_string(),
        Some(PromptId::TriageToday) => {
            // Bloques «=== Correo N (id: X) ===»: el remitente y el asunto deciden.
            let text = req.flatten_messages();
            let items: Vec<_> = text
                .split("(id: ")
                .skip(1)
                .take(200)
                .filter_map(|b| {
                    let (id, rest) = b.split_once(')')?;
                    let low = rest.to_lowercase();
                    let (imp, why) = if low.contains("news") || low.contains("boletín") || low.contains("noreply") {
                        ("low", l("Newsletter: read when you have time", "Boletín: léelo cuando tengas tiempo"))
                    } else if low.contains("invita") || low.contains("urgent") || low.contains("plazo") {
                        ("high", l("Needs your answer today", "Requiere tu respuesta hoy"))
                    } else if low.contains("re:") || low.contains('?') {
                        ("medium", l("Someone is waiting for your reply", "Alguien espera tu respuesta"))
                    } else {
                        ("medium", l("Review this week", "Revísalo esta semana"))
                    };
                    Some(serde_json::json!({"id": id, "importance": imp, "reason": why}))
                })
                .collect();
            serde_json::json!({ "items": items }).to_string()
        }
        Some(PromptId::MeetingMinutes) => serde_json::json!({
            "title": l("Case review", "Revisión del caso"),
            "attendees": ["Laura Méndez", "Carmen Brown"],
            "summary": l("The status of the case was reviewed and next steps were agreed.", "Se revisó el estado del caso y se acordaron los siguientes pasos."),
            "topics": [{"title": l("Status", "Estado"), "notes": l("Evidence filed.", "Pruebas presentadas.")}],
            "decisions": [l("File the appeal", "Presentar la apelación")],
            "action_items": [{"title": l("Draft the appeal", "Redactar la apelación"), "assignee_hint": "Laura"}]
        })
        .to_string(),
        Some(PromptId::DocumentOutline) => serde_json::json!({
            "title": l("Case summary", "Resumen del caso"),
            "sections": [l("Background", "Antecedentes"), l("Next steps", "Siguientes pasos")],
            "markdown": l("# Case summary\n\n## Background\n\n- …\n\n## Next steps\n\n- …\n", "# Resumen del caso\n\n## Antecedentes\n\n- …\n\n## Siguientes pasos\n\n- …\n")
        })
        .to_string(),
        None => {
            if req.system.contains("single word OK") {
                "OK".into()
            } else {
                l(
                    "In the sample mailbox I answer with examples. Connect Iurefficient or choose a provider in Settings → Assistant.",
                    "En el buzón de ejemplo respondo con ejemplos. Conecta Iurefficient o elige un proveedor en Ajustes → Asistente.",
                )
            }
        }
    }
}

/// Proveedor del buzón de ejemplo (local, respuestas fijas).
pub fn sample_factory() -> ProviderFactory {
    Arc::new(|kind| {
        Arc::new(CannedProvider::new("Iurefficient (ejemplo)", false, move |req| sample_answer(kind, req)))
            as Arc<dyn AiProvider>
    })
}

/// Proveedor de ejemplo externo (capturas y pruebas del diálogo de consentimiento).
pub fn external_sample_factory(label: &str) -> ProviderFactory {
    let label = label.to_string();
    Arc::new(move |kind| {
        Arc::new(CannedProvider::new(&label, true, move |req| sample_answer(kind, req))) as Arc<dyn AiProvider>
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_found_in_triage_requests() {
        let req = ChatRequest {
            messages: vec![ChatMessage::user("=== Correo 1 (id: 7) ===\nx\n=== Correo 2 (id: 9) ===")],
            ..Default::default()
        };
        assert_eq!(ids_in(&req), vec!["7".to_string(), "9".to_string()]);
    }

    #[test]
    fn sample_answers_parse_for_every_task() {
        use iuredesk_assistant::schema::ValidateCtx;
        let req =
            ChatRequest { messages: vec![ChatMessage::user("=== Texto ===\nhola (id: 3)")], ..Default::default() };
        let ctx = ValidateCtx { known_ids: vec!["3".into()] };
        let s = |k| sample_answer(Some(k), &req);
        assert!(tasks::parse_output::<DraftReplies>(&s(PromptId::DraftReply), None, &ctx).is_ok());
        assert!(tasks::parse_output::<Rewritten>(&s(PromptId::Rewrite), None, &ctx).is_ok());
        assert!(tasks::parse_output::<ThreadSummary>(&s(PromptId::SummarizeThread), None, &ctx).is_ok());
        assert!(tasks::parse_output::<Extraction>(&s(PromptId::Extract), None, &ctx).is_ok());
        assert!(tasks::parse_output::<LeadTicketClassification>(&s(PromptId::Classify), None, &ctx).is_ok());
        assert!(tasks::parse_output::<Triage>(&s(PromptId::TriageToday), None, &ctx).is_ok());
        assert!(tasks::parse_output::<MeetingMinutes>(&s(PromptId::MeetingMinutes), None, &ctx).is_ok());
        assert!(tasks::parse_output::<DocumentOutline>(&s(PromptId::DocumentOutline), None, &ctx).is_ok());
    }
}
