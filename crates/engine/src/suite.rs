//! Apps hermanas en la aplicación (F4.2): IureEditor, IureOCR, IureDav, IureVoz, IureTranscribe e
//! IureProject vía `iuredesk-integrations`, y el registro de IureDesk como servidor MCP en
//! Claude/Copilot. Todo bloquea (procesos, sockets, disco), así que corre en el runtime de fondo
//! y vuelve como [`SuiteEvent`]. Una app que falta no es un error: [`SuiteEvent::NotInstalled`]
//! lleva la invitación con el enlace de descarga.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use iuredesk_integrations::{
    EditorTarget, InstallHint, IntegrationError, JobState, OcrOptions, Outcome as SuiteOutcome, SisterApp, Suite,
};
use iuredesk_mcp::AgentsStatus;
use iuredesk_model::MessageId;
use iuredesk_store::Store;

use crate::background::Waker;
use crate::lang::Text;
use crate::session::Session;

/// Tope de una transcripción leída directamente del disco.
const MAX_TRANSCRIPT_FILE: u64 = 4 * 1024 * 1024;
/// Vueltas máximas de sondeo de una transcripción (× 20 s ≈ 3 h).
const MAX_POLLS: usize = 540;

/// Lo que se pide a la suite.
#[derive(Clone, Debug, PartialEq)]
pub enum SuiteAction {
    /// Abrir un adjunto en IureEditor.
    OpenAttachment { msg: MessageId, name: String },
    /// OCR de un adjunto con IureOCR.
    OcrAttachment { msg: MessageId, name: String },
    /// Guardar un adjunto en la unidad de Iurefficient (IureDav).
    SaveAttachment { msg: MessageId, name: String },
    /// Leer en voz alta con IureVoz.
    Speak(String),
    /// Dictar al redactor `window` con IureVoz.
    Dictate { window: u64 },
    /// Transcribir una grabación con IureTranscribe (o leer un `.txt`/`.md`/`.srt`/`.vtt`).
    Transcribe { file: PathBuf, title: String },
    /// Documento nuevo en IureEditor.
    NewDocument { name: String, markdown: String },
    /// UX-6: abrir un archivo existente en IureEditor (`iureditor://open?path=`; si ya está
    /// abierto, IureEditor enfoca su ventana).
    OpenFile(PathBuf),
    /// Abrir una app.
    Launch(SisterApp),
    /// S1.6: estado de los planes de IureProject que se siguen (para «Hoy»).
    ProjectOverviews(Vec<PathBuf>),
    /// S1.6: abrir un plan en IureProject.
    OpenProject(PathBuf),
    /// S1.35: abrir el plan en una tarea (su id en Iurefficient).
    OpenProjectTask { file: PathBuf, task: String },
    /// S1.37: abrir en IureProject un expediente de Iurefficient (sin plan local).
    OpenProjectCase { id: String, title: String },
    /// «Correo → tarea del plan»: agregar una tarea al plan y guardarlo (sin abrir IureProject).
    AddProjectTask { file: PathBuf, task: iuredesk_integrations::NewProjectTask },
    /// S1.4: grabar una reunión con IureTranscribe (sin abrir su ventana).
    StartRecording { title: String },
    /// S1.4: pausar (`true`) o reanudar la grabación.
    PauseRecording(bool),
    /// S1.4: detener, guardar y transcribir.
    StopRecording,
    /// «Usar IureDesk desde Claude / Copilot».
    RegisterAgents,
    /// «Desconectar».
    UnregisterAgents,
    /// Volver a leer el estado de Claude/VS Code.
    RefreshAgents,
}

/// Resultado de una acción.
#[derive(Clone, Debug, PartialEq)]
pub enum SuiteEvent {
    /// Hecho (texto para un aviso).
    Done(Text),
    /// La app no está instalada: invitar a instalarla.
    NotInstalled(InstallHint),
    /// Instalada pero no disponible ahora.
    NotAvailable(Text),
    /// Falló de verdad.
    Failed(Text),
    /// Texto dictado para el redactor.
    Dictated { window: u64, text: String },
    /// Transcripción lista (para la minuta).
    Transcript { title: String, text: String },
    /// Texto reconocido por OCR.
    OcrText { name: String, text: String },
    /// S1.6: estado de los planes seguidos.
    Projects(Vec<iuredesk_integrations::ProjectStatus>),
    /// S1.4: transcripción de una reunión grabada desde IureDesk (para guardarla como minuta).
    MeetingTranscript { title: String, text: String, audio: PathBuf },
}

/// S1.4: grabación en curso (lo que muestra la interfaz).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RecordingState {
    pub title: String,
    pub elapsed_secs: f64,
    pub paused: bool,
    /// Nivel del micrófono y del sistema (0–1).
    pub mic_level: f32,
    pub sys_level: f32,
    /// Deteniendo y guardando.
    pub stopping: bool,
    pub error: Option<String>,
}

/// Transcripción en curso.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TranscribeProgress {
    pub percent: u8,
    pub stage: String,
}

enum Msg {
    Detected(Arc<Suite>),
    Event(SuiteEvent),
    Agents(AgentsStatus),
}

/// La suite en la aplicación.
pub struct SuiteHub {
    suite: Option<Arc<Suite>>,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    rt: tokio::runtime::Handle,
    waker: Waker,
    store: Arc<Store>,
    busy: Arc<AtomicUsize>,
    events: Vec<SuiteEvent>,
    agents: Option<AgentsStatus>,
    transcribe: Arc<Mutex<Option<TranscribeProgress>>>,
    /// S1.4: la grabación (su proceso MCP vive mientras exista) y lo que se muestra de ella.
    recording: Arc<Mutex<Option<iuredesk_integrations::Recording>>>,
    rec_state: Arc<Mutex<Option<RecordingState>>>,
}

impl std::fmt::Debug for SuiteHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SuiteHub").field("suite", &self.suite).finish_non_exhaustive()
    }
}

fn err_text(e: &IntegrationError) -> Text {
    Text::new(e.human(false), e.human(true))
}

fn itext(t: &iuredesk_integrations::Text) -> Text {
    Text::new(t.en.clone(), t.es.clone())
}

fn outcome<T>(r: iuredesk_integrations::Result<SuiteOutcome<T>>, ok: impl FnOnce(T) -> SuiteEvent) -> SuiteEvent {
    match r {
        Ok(SuiteOutcome::Ready(t)) => ok(t),
        Ok(SuiteOutcome::NotInstalled(h)) => SuiteEvent::NotInstalled(h),
        Ok(SuiteOutcome::NotAvailable(t)) => SuiteEvent::NotAvailable(itext(&t)),
        Err(e) => SuiteEvent::Failed(err_text(&e)),
    }
}

/// Bytes de un adjunto (por nombre) de un mensaje del almacén.
pub fn attachment_bytes(store: &Store, msg: MessageId, name: &str) -> Option<Vec<u8>> {
    let raw = store.get_raw(msg).ok()?;
    use mail_parser::MimeHeaders;
    let parsed = mail_parser::MessageParser::default().parse(&raw)?;
    parsed.attachments().take(200).find(|a| a.attachment_name() == Some(name)).map(|a| a.contents().to_vec())
}

fn is_text_transcript(p: &Path) -> bool {
    let e = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    matches!(e.as_str(), "txt" | "md" | "srt" | "vtt")
}

impl SuiteHub {
    /// Detecta las apps en segundo plano.
    pub fn new(session: &Session) -> Self {
        let (tx, rx) = channel();
        let mut h = SuiteHub {
            suite: None,
            tx,
            rx,
            rt: session.runtime(),
            waker: session.waker(),
            store: Arc::clone(session.store()),
            busy: Arc::new(AtomicUsize::new(0)),
            events: Vec::new(),
            agents: None,
            transcribe: Arc::new(Mutex::new(None)),
            recording: Arc::new(Mutex::new(None)),
            rec_state: Arc::new(Mutex::new(None)),
        };
        h.background(|| vec![Msg::Detected(Arc::new(Suite::detect()))]);
        h
    }

    /// Con una suite dada (pruebas, capturas).
    pub fn with_suite(mut self, suite: Suite) -> Self {
        self.suite = Some(Arc::new(suite));
        self
    }

    fn background(&mut self, f: impl FnOnce() -> Vec<Msg> + Send + 'static) {
        let tx = self.tx.clone();
        let waker = self.waker.clone();
        let busy = Arc::clone(&self.busy);
        busy.fetch_add(1, Ordering::AcqRel);
        self.rt.spawn_blocking(move || {
            let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|_| {
                vec![Msg::Event(SuiteEvent::Failed(Text::new(
                    "Something went wrong with the other app. Try again.",
                    "Algo falló con la otra app. Inténtalo de nuevo.",
                )))]
            });
            for m in out {
                let _ = tx.send(m);
            }
            busy.fetch_sub(1, Ordering::AcqRel);
            waker();
        });
    }

    /// La suite detectada (si ya terminó).
    pub fn suite(&self) -> Option<&Suite> {
        self.suite.as_deref()
    }

    /// ¿Está instalada?
    pub fn installed(&self, app: SisterApp) -> bool {
        self.suite.as_ref().is_some_and(|s| s.installed(app))
    }

    /// Estado de Claude Desktop / VS Code (tras [`SuiteAction::RefreshAgents`]).
    pub fn agents(&self) -> Option<&AgentsStatus> {
        self.agents.as_ref()
    }

    /// Transcripción en curso.
    pub fn transcribing(&self) -> Option<TranscribeProgress> {
        self.transcribe.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// S1.4: grabación en curso.
    pub fn recording(&self) -> Option<RecordingState> {
        self.rec_state.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// S1.4: como si hubiera una grabación con este estado (pruebas y capturas; sin proceso).
    pub fn set_recording_for_tests(&mut self, state: Option<RecordingState>) {
        *self.rec_state.lock().unwrap_or_else(PoisonError::into_inner) = state;
    }

    /// Hay trabajos en curso.
    pub fn busy(&self) -> bool {
        self.busy.load(Ordering::Acquire) > 0
    }

    /// Lanza una acción.
    pub fn run(&mut self, action: SuiteAction) {
        let Some(suite) = self.suite.clone() else {
            self.events.push(SuiteEvent::NotAvailable(Text::new(
                "Still looking for the Iurefficient apps on this computer. Try again in a moment.",
                "Aún se buscan las apps de Iurefficient en este equipo. Inténtalo en un momento.",
            )));
            return;
        };
        let store = Arc::clone(&self.store);
        let recording = Arc::clone(&self.recording);
        let rec_state = Arc::clone(&self.rec_state);
        let waker = self.waker.clone();
        let progress = Arc::clone(&self.transcribe);
        let missing =
            || SuiteEvent::Failed(Text::new("The attachment could not be read.", "No se pudo leer el adjunto."));
        // Sin instalar: la invitación de inmediato (sin hilo).
        let needs = match &action {
            SuiteAction::OpenAttachment { .. } | SuiteAction::NewDocument { .. } | SuiteAction::OpenFile(_) => {
                Some(SisterApp::Editor)
            }
            SuiteAction::OcrAttachment { .. } => Some(SisterApp::Ocr),
            SuiteAction::SaveAttachment { .. } => Some(SisterApp::Dav),
            SuiteAction::Speak(_) | SuiteAction::Dictate { .. } => Some(SisterApp::Voz),
            SuiteAction::Transcribe { file, .. } if !is_text_transcript(file) => Some(SisterApp::Transcribe),
            SuiteAction::Launch(a) => Some(*a),
            SuiteAction::ProjectOverviews(_)
            | SuiteAction::OpenProject(_)
            | SuiteAction::OpenProjectTask { .. }
            | SuiteAction::OpenProjectCase { .. }
            | SuiteAction::AddProjectTask { .. } => Some(SisterApp::Project),
            SuiteAction::StartRecording { .. } => Some(SisterApp::Transcribe),
            _ => None,
        };
        if let Some(app) = needs
            && !suite.installed(app)
        {
            self.events.push(SuiteEvent::NotInstalled(app.install_hint()));
            return;
        }
        self.background(move || {
            let ev = match action {
                SuiteAction::OpenAttachment { msg, name } => match attachment_bytes(&store, msg, &name) {
                    None => missing(),
                    Some(bytes) => match suite.stage_file(&name, &bytes) {
                        Err(e) => SuiteEvent::Failed(err_text(&e)),
                        Ok(path) => outcome(suite.open_in_iureditor(&EditorTarget::File(path)), |_| {
                            SuiteEvent::Done(Text::new(
                                format!("Opened {name} in IureEditor"),
                                format!("{name} abierto en IureEditor"),
                            ))
                        }),
                    },
                },
                SuiteAction::OcrAttachment { msg, name } => match attachment_bytes(&store, msg, &name) {
                    None => missing(),
                    Some(bytes) => match suite.stage_file(&name, &bytes) {
                        Err(e) => SuiteEvent::Failed(err_text(&e)),
                        Ok(path) => outcome(suite.ocr_attachment(&path, &OcrOptions::default()), |r| {
                            SuiteEvent::OcrText { name: name.clone(), text: r.text }
                        }),
                    },
                },
                SuiteAction::SaveAttachment { msg, name } => match attachment_bytes(&store, msg, &name) {
                    None => missing(),
                    Some(bytes) => outcome(suite.save_attachment(&name, &bytes), |p| {
                        let shown = p.display().to_string();
                        SuiteEvent::Done(Text::new(
                            format!("Saved in Iurefficient: {shown}"),
                            format!("Guardado en Iurefficient: {shown}"),
                        ))
                    }),
                },
                SuiteAction::Speak(text) => {
                    let t: String = text.chars().take(6_000).collect();
                    outcome(suite.speak(&t), |_| SuiteEvent::Done(Text::new("Read aloud", "Leído en voz alta")))
                }
                SuiteAction::Dictate { window } => {
                    outcome(suite.dictate(Some(crate::lang::pick("Dictate your message", "Dicta tu mensaje"))), |t| {
                        SuiteEvent::Dictated { window, text: t }
                    })
                }
                SuiteAction::Transcribe { file, title } => transcribe(&suite, &file, title, &progress),
                SuiteAction::NewDocument { name, markdown } => {
                    outcome(suite.open_in_iureditor(&EditorTarget::NewDocument { file_name: name, markdown }), |_| {
                        SuiteEvent::Done(Text::new("Opened in IureEditor", "Abierto en IureEditor"))
                    })
                }
                SuiteAction::OpenFile(path) => outcome(suite.open_in_iureditor(&EditorTarget::File(path)), |_| {
                    SuiteEvent::Done(Text::new(
                        "Opened in IureEditor. Changes you save there come back here.",
                        "Abierta en IureEditor. Lo que guardes allá vuelve aquí.",
                    ))
                }),
                SuiteAction::StartRecording { title } => {
                    if lock(&recording).is_some() {
                        return vec![Msg::Event(SuiteEvent::NotAvailable(Text::new(
                            "You are already recording.",
                            "Ya estás grabando.",
                        )))];
                    }
                    match suite.start_recording() {
                        Ok(SuiteOutcome::Ready(r)) => {
                            *lock(&recording) = Some(r);
                            *lock(&rec_state) = Some(RecordingState { title, ..Default::default() });
                            spawn_recording_status(Arc::clone(&recording), Arc::clone(&rec_state), waker.clone());
                            SuiteEvent::Done(Text::new(
                                "Recording. Stop it from the bar at the top.",
                                "Grabando. Detenla desde la barra de arriba.",
                            ))
                        }
                        other => outcome(other.map(|o| o.map(|_| ())), |_| SuiteEvent::Failed(Text::default())),
                    }
                }
                SuiteAction::PauseRecording(paused) => {
                    let r = lock(&recording).as_mut().map(|r| r.set_paused(paused));
                    match r {
                        None => return vec![],
                        Some(Ok(())) => {
                            if let Some(st) = lock(&rec_state).as_mut() {
                                st.paused = paused;
                            }
                            return vec![];
                        }
                        Some(Err(e)) => SuiteEvent::Failed(err_text(&e)),
                    }
                }
                SuiteAction::StopRecording => {
                    let title = match lock(&rec_state).as_mut() {
                        Some(st) => {
                            st.stopping = true;
                            st.title.clone()
                        }
                        None => return vec![],
                    };
                    let Some(r) = lock(&recording).take() else {
                        // Nada que detener (el proceso ya no estaba): no dejar la barra en «Guardando».
                        *lock(&rec_state) = None;
                        return vec![];
                    };
                    let saved = r.stop();
                    *lock(&rec_state) = None;
                    match saved {
                        Err(e) => SuiteEvent::Failed(err_text(&e)),
                        Ok(rec) => match transcribe(&suite, &rec.path, title, &progress) {
                            SuiteEvent::Transcript { title, text } => {
                                SuiteEvent::MeetingTranscript { title, text, audio: rec.path }
                            }
                            other => other,
                        },
                    }
                }
                SuiteAction::ProjectOverviews(files) => outcome(suite.project_overviews(&files), SuiteEvent::Projects),
                SuiteAction::OpenProject(file) => outcome(suite.open_in_iureproject(&file), |_| {
                    SuiteEvent::Done(Text::new("Opening the plan in IureProject", "Abriendo el plan en IureProject"))
                }),
                SuiteAction::OpenProjectTask { file, task } => {
                    outcome(suite.open_in_iureproject_at(&file, &task), |_| {
                        SuiteEvent::Done(Text::new(
                            "Opening the task in IureProject",
                            "Abriendo la tarea en IureProject",
                        ))
                    })
                }
                SuiteAction::OpenProjectCase { id, title } => {
                    outcome(suite.open_case_in_iureproject(&id, &title), |_| {
                        SuiteEvent::Done(Text::new(
                            "Opening the case in IureProject",
                            "Abriendo el expediente en IureProject",
                        ))
                    })
                }
                SuiteAction::AddProjectTask { file, task } => {
                    let plan = file.file_stem().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                    outcome(suite.add_project_task(&file, &task), |t| {
                        SuiteEvent::Done(Text::new(
                            format!("Task «{}» added to {plan}", t.name),
                            format!("Tarea «{}» agregada a {plan}", t.name),
                        ))
                    })
                }
                SuiteAction::Launch(app) => outcome(suite.launch(app), |_| {
                    SuiteEvent::Done(Text::new(format!("Opening {}", app.name()), format!("Abriendo {}", app.name())))
                }),
                SuiteAction::RegisterAgents => {
                    let r = iuredesk_mcp::register_mcp_agents(true);
                    let ev = match (&r.claude_error, r.claude_files, r.vscode_opened, &r.vscode_link) {
                        (Some(e), _, _, _) => SuiteEvent::Failed(Text::new(
                            format!("Claude Desktop could not be configured ({e})."),
                            format!("No se pudo configurar Claude Desktop ({e})."),
                        )),
                        (None, 0, false, None) => SuiteEvent::NotAvailable(Text::new(
                            "Neither Claude Desktop nor VS Code is installed on this computer.",
                            "Ni Claude Desktop ni VS Code están instalados en este equipo.",
                        )),
                        _ => SuiteEvent::Done(Text::new(
                            "IureDesk is connected. Restart Claude Desktop to use it; VS Code asks to confirm.",
                            "IureDesk quedó conectado. Reinicia Claude Desktop para usarlo; VS Code pide confirmar.",
                        )),
                    };
                    return vec![Msg::Event(ev), Msg::Agents(iuredesk_mcp::agents_status())];
                }
                SuiteAction::UnregisterAgents => {
                    let ev = match iuredesk_mcp::unregister_mcp_agents() {
                        Ok(()) => SuiteEvent::Done(Text::new(
                            "Disconnected from Claude Desktop",
                            "Desconectado de Claude Desktop",
                        )),
                        Err(e) => SuiteEvent::Failed(Text::new(e.clone(), e)),
                    };
                    return vec![Msg::Event(ev), Msg::Agents(iuredesk_mcp::agents_status())];
                }
                SuiteAction::RefreshAgents => return vec![Msg::Agents(iuredesk_mcp::agents_status())],
            };
            vec![Msg::Event(ev)]
        });
    }

    /// Atiende los resultados (cada cuadro).
    pub fn poll(&mut self) {
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Detected(s) => {
                    if self.suite.is_none() {
                        self.suite = Some(s);
                    }
                }
                Msg::Event(e) => self.events.push(e),
                Msg::Agents(a) => self.agents = Some(a),
            }
        }
    }

    /// Resultados para la UI (se consumen).
    pub fn take_events(&mut self) -> Vec<SuiteEvent> {
        std::mem::take(&mut self.events)
    }
}

/// Bloquea aunque otro hilo haya fallado con el candado tomado.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Segundos que una grabación se sigue consultando como mucho (8 h).
const MAX_RECORDING_SECS: usize = 8 * 3600;

/// S1.4: consulta el estado de la grabación cada segundo mientras exista (hilo propio: no cuenta
/// como trabajo en curso, la reunión puede durar horas).
fn spawn_recording_status(
    recording: Arc<Mutex<Option<iuredesk_integrations::Recording>>>,
    state: Arc<Mutex<Option<RecordingState>>>,
    waker: Waker,
) {
    let _ = std::thread::Builder::new().name("recording-status".into()).spawn(move || {
        for _ in 0..MAX_RECORDING_SECS {
            std::thread::sleep(Duration::from_secs(1));
            let st = match lock(&recording).as_mut() {
                None => break,
                Some(r) => r.status(),
            };
            match lock(&state).as_mut() {
                None => break,
                Some(s) if s.stopping => break,
                Some(s) => match st {
                    Ok(st) => {
                        s.elapsed_secs = st.elapsed_secs;
                        s.paused = st.paused;
                        s.mic_level = st.mic_level;
                        s.sys_level = st.sys_level;
                        s.error = st.error;
                    }
                    Err(e) => s.error = Some(e.human(crate::lang::spanish())),
                },
            }
            waker();
        }
    });
}

fn transcribe(
    suite: &Suite,
    file: &Path,
    title: String,
    progress: &Arc<Mutex<Option<TranscribeProgress>>>,
) -> SuiteEvent {
    if is_text_transcript(file) {
        let read = std::fs::metadata(file)
            .map_err(|e| e.to_string())
            .and_then(|m| if m.len() > MAX_TRANSCRIPT_FILE { Err("demasiado grande".into()) } else { Ok(()) })
            .and_then(|()| std::fs::read(file).map_err(|e| e.to_string()));
        return match read {
            Ok(b) => SuiteEvent::Transcript { title, text: String::from_utf8_lossy(&b).into_owned() },
            Err(e) => SuiteEvent::Failed(Text::new(
                format!("The transcript could not be read ({e})."),
                format!("No se pudo leer la transcripción ({e})."),
            )),
        };
    }
    let set = |p: Option<TranscribeProgress>| *progress.lock().unwrap_or_else(PoisonError::into_inner) = p;
    set(Some(TranscribeProgress { percent: 0, stage: String::new() }));
    let mut job = match suite.transcribe_recording(file) {
        Ok(SuiteOutcome::Ready(j)) => j,
        other => {
            set(None);
            return outcome(other.map(|o| o.map(|_| ())), |_| SuiteEvent::Failed(Text::default()));
        }
    };
    let mut result =
        SuiteEvent::Failed(Text::new("The transcription took too long.", "La transcripción tardó demasiado."));
    for _ in 0..MAX_POLLS {
        match job.poll(Duration::from_secs(20)) {
            Ok(JobState::Running { percent, stage }) => set(Some(TranscribeProgress { percent, stage })),
            Ok(JobState::Done(t)) => {
                let mut text = t.text;
                let mut next = t.next_offset;
                let mut guard = 0;
                while let Some(off) = next
                    && guard < 64
                {
                    guard += 1;
                    match job.read_more(off) {
                        Ok(more) => {
                            text.push_str(&more.text);
                            next = more.next_offset;
                        }
                        Err(_) => break,
                    }
                }
                result = SuiteEvent::Transcript { title, text };
                break;
            }
            Err(e) => {
                result = SuiteEvent::Failed(err_text(&e));
                break;
            }
        }
    }
    set(None);
    result
}
