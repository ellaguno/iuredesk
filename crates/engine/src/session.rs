//! `Session`: el estado de la aplicación sin egui (módulo actual, carpeta, selección, búsqueda,
//! preferencias, deshacer) sobre el almacén local. La UI lee de aquí y despacha comandos por id.
//!
//! Las mutaciones (archivar, eliminar, mover, banderas) se aplican **primero** al almacén y se
//! encolan en la cola de operaciones (offline-first, PLAN.md §4); cada una deja una entrada de
//! deshacer con su inversa. Deshacer cancela la operación si aún no salió de la máquina o encola
//! la inversa si ya se replicó.

mod cal;
mod iure;
mod live;
mod mailops;
mod outbox;
mod pim;

pub use cal::{CalState, PimBackend, occurrence_start};
pub use iure::{IURE_CALENDAR_PATH, IURE_TASKS_PATH, IurePanel, IureSetup, LeadSuggestions, LoginState};
pub use live::{AccountSync, SyncStatus};
pub use mailops::AccountRow;
pub use outbox::OfflineTransport;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use chrono::{DateTime, Duration, NaiveDate, Utc};
use iuredesk_accounts::{AccountStore, SecretPurpose, SecretStore};
use iuredesk_model::account::{Account, AuthMethod, OAuthProvider};
use iuredesk_model::{
    AccountId, Calendar, Contact, Event as CalEvent, Flags, Folder, FolderId, FolderRole, MessageEnvelope, MessageId,
    Note, Task, TaskId, TaskList, TaskStatus,
};
use iuredesk_store::{AccountLabel, FolderCount, Store};

use crate::background::{Background, Done, JobId, JobOutput, Waker};
use crate::body::MessageBody;
use crate::calendar::Occurrence;
use crate::command;
use crate::compose::ComposeDraft;
use crate::context::ContextCard;
use crate::lang::Text;
use crate::onboarding::{Flow, Wizard, WizardStep};
use crate::prefs::{DensityPref, Prefs, ShortcutScheme, ThemePref};
use crate::time::Zone;
use crate::{EngineError, Result};
use iuredesk_compose::{OutboxId, SignatureBook, Transport};

/// Sobres en caché antes de vaciarla.
const ENVELOPE_CACHE: usize = 8_192;
/// Cuerpos en caché.
const BODY_CACHE: usize = 64;
/// Entradas de deshacer que se conservan.
const UNDO_DEPTH: usize = 50;
/// Mensajes que una sola acción puede tocar.
const MAX_BATCH: usize = 10_000;

/// Módulo del riel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Module {
    #[default]
    Mail,
    Calendar,
    Contacts,
    Notes,
    Tasks,
    Assistant,
}

impl Module {
    pub const ALL: [Module; 6] =
        [Module::Mail, Module::Calendar, Module::Contacts, Module::Notes, Module::Tasks, Module::Assistant];

    pub fn key(self) -> &'static str {
        match self {
            Module::Mail => "mail",
            Module::Calendar => "calendar",
            Module::Contacts => "contacts",
            Module::Notes => "notes",
            Module::Tasks => "tasks",
            Module::Assistant => "assistant",
        }
    }

    pub fn from_key(s: &str) -> Option<Module> {
        Module::ALL.into_iter().find(|m| m.key() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            Module::Mail => crate::lang::pick("Mail", "Correo"),
            Module::Calendar => crate::lang::pick("Calendar", "Calendario"),
            Module::Contacts => crate::lang::pick("Contacts", "Contactos"),
            Module::Notes => crate::lang::pick("Notes", "Notas"),
            Module::Tasks => crate::lang::pick("Tasks", "Tareas"),
            Module::Assistant => crate::lang::pick("Assistant", "Asistente"),
        }
    }
}

/// Qué muestra la lista de mensajes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FolderSel {
    /// Favoritos: la carpeta de ese papel en todas las cuentas (bandeja unificada…).
    Unified(FolderRole),
    /// Marcados para seguimiento, en todas las cuentas.
    Flagged,
    /// Una carpeta concreta.
    Folder(FolderId),
    /// Bandeja de salida (lo que espera para enviarse).
    Outbox,
}

impl Default for FolderSel {
    fn default() -> Self {
        FolderSel::Unified(FolderRole::Inbox)
    }
}

/// Una fila de la lista: el mensaje que la representa y cuántos tiene su conversación.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ListRow {
    pub id: MessageId,
    pub count: u32,
}

/// Tipo de aviso.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum NoticeKind {
    #[default]
    Info,
    Success,
    Error,
}

/// Aviso para el usuario (la UI lo muestra como notificación pasajera).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Notice {
    pub kind: NoticeKind,
    pub text: Text,
    /// Ofrece «Deshacer».
    pub undo: bool,
    /// Segundos a la vista (`None` = lo de siempre).
    pub secs: Option<u32>,
}

impl Notice {
    pub fn info(en: impl Into<String>, es: impl Into<String>) -> Notice {
        Notice { kind: NoticeKind::Info, text: Text::new(en, es), undo: false, secs: None }
    }
    pub fn success(en: impl Into<String>, es: impl Into<String>) -> Notice {
        Notice { kind: NoticeKind::Success, text: Text::new(en, es), undo: false, secs: None }
    }
    pub fn error(text: Text) -> Notice {
        Notice { kind: NoticeKind::Error, text, undo: false, secs: None }
    }
}

/// Lo que la UI debe hacer tras un comando.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Nada más (la vista se actualiza sola).
    Done,
    /// Mostrar un aviso.
    Notice(Notice),
    /// Abrir el redactor.
    Compose(Box<ComposeDraft>),
    /// Elegir la carpeta destino de «Mover» (la UI abre la paleta con `mail.move:<id>`).
    PickFolder,
    /// Abrir la paleta de comandos.
    OpenPalette,
    /// Abrir el asistente de alta.
    AddAccount,
    /// Llevar el foco al campo de búsqueda.
    FocusSearch,
    /// Cambió el tema o la densidad: volver a aplicar el estilo.
    Restyle,
    /// Abrir el editor de eventos con este formulario.
    EditEvent(Box<crate::calendar::draft::EventDraft>),
}

/// Resultados de trabajos de fondo que interpreta la sesión.
pub enum EngineJob {
    Rows {
        generation: u64,
        rows: Vec<ListRow>,
    },
    Search {
        generation: u64,
        result: std::result::Result<Vec<MessageEnvelope>, Text>,
    },
    Body {
        id: MessageId,
        body: MessageBody,
    },
    Discovery(std::result::Result<iuredesk_autoconfig::Discovery, Text>),
    /// Prueba de conexión (y secreto ya guardado) de la cuenta en curso.
    Probe(std::result::Result<(), Text>),
    OAuth(std::result::Result<(), Text>),
    Failed(Text),
}

/// Eventos para la UI tras [`Session::poll`].
pub enum Event {
    Notice(Notice),
    /// Correo nuevo (aviso y notificación del escritorio).
    NewMail {
        summary: Text,
        /// Hasta tres «Remitente — Asunto».
        detail: String,
        /// Cuenta que lo recibió.
        account: String,
    },
    /// Abrir el redactor con este borrador (p. ej. «Responder vía la dirección de la
    /// oportunidad», que primero consulta la instancia).
    Compose(Box<ComposeDraft>),
    /// Resultado de un trabajo encolado por la UI.
    Custom {
        id: JobId,
        value: Box<dyn std::any::Any + Send>,
    },
}

/// Recursos remotos descargados por [`Session::fetch_remote`].
#[derive(Debug, Clone, Default)]
pub struct Fetched {
    pub tag: u64,
    pub items: Vec<(String, Vec<u8>)>,
}

/// Paso inverso de una acción.
#[derive(Clone, Debug)]
enum Inverse {
    Flags {
        id: MessageId,
        account: AccountId,
        flags: Flags,
    },
    /// Volver a `folder`, donde el mensaje tenía el id remoto `remote`.
    Move {
        id: MessageId,
        account: AccountId,
        folder: FolderId,
        remote: Option<String>,
    },
    Task(Box<Task>),
    /// Sacar el mensaje de la bandeja de salida.
    Send(OutboxId),
    /// Edición de calendario.
    Calendar(Box<cal::CalUndo>),
}

#[derive(Clone, Debug)]
struct UndoEntry {
    text: Text,
    steps: Vec<(Inverse, Option<u64>)>,
}

/// Configuración de una sesión.
pub struct SessionConfig {
    pub store: Arc<Store>,
    /// Lista de cuentas (`accounts.json`); `None` en el buzón de ejemplo y en pruebas.
    pub accounts: Option<AccountStore>,
    pub secrets: Arc<dyn SecretStore>,
    /// Dónde se guardan las preferencias; `None` = no se guardan.
    pub prefs_path: Option<PathBuf>,
    /// «Ahora» fijo (capturas y pruebas deterministas).
    pub now: Option<DateTime<Utc>>,
    pub zone: Zone,
    /// Es el buzón de ejemplo (`--sample`).
    pub sample: bool,
    /// Sincronizar las cuentas con sus servidores (`None` = sin red: buzón de ejemplo y pruebas).
    pub sync: Option<iuredesk_sync::SyncOptions>,
    /// Transporte para todas las cuentas en lugar de SMTP (buzón de ejemplo y pruebas).
    pub transport: Option<Arc<dyn Transport>>,
}

impl SessionConfig {
    /// Sesión sin red sobre un almacén (pruebas y capturas): sin cuentas, sin preferencias en
    /// disco, zona UTC.
    pub fn offline(store: Arc<Store>) -> SessionConfig {
        SessionConfig {
            store,
            accounts: None,
            secrets: Arc::new(iuredesk_accounts::MemoryStore::default()),
            prefs_path: None,
            now: None,
            zone: Zone::utc(),
            sample: false,
            sync: None,
            transport: None,
        }
    }
}

/// La sesión.
pub struct Session {
    store: Arc<Store>,
    account_store: Option<AccountStore>,
    secrets: Arc<dyn SecretStore>,
    accounts: Vec<Account>,
    labels: Vec<AccountLabel>,
    folders: Vec<Folder>,
    counts: HashMap<FolderId, FolderCount>,
    pub prefs: Prefs,
    prefs_path: Option<PathBuf>,
    pub module: Module,
    folder: FolderSel,
    rows: Arc<Vec<ListRow>>,
    generation: u64,
    rows_job: Option<JobId>,
    loading: bool,
    envelopes: HashMap<MessageId, MessageEnvelope>,
    bodies: HashMap<MessageId, MessageBody>,
    body_jobs: HashSet<MessageId>,
    selection: Vec<MessageId>,
    focused: Option<MessageId>,
    focus_request: Option<MessageId>,
    search: String,
    undo: Vec<UndoEntry>,
    now: Option<DateTime<Utc>>,
    zone: Zone,
    sample: bool,
    bg: Background,
    pub wizard: Option<Wizard>,
    contacts_cache: Option<Vec<Contact>>,
    calendars: Vec<Calendar>,
    events_cache: Option<((DateTime<Utc>, DateTime<Utc>), Vec<Occurrence>)>,
    data_gen: u64,
    /// `Account::id` → id local.
    account_ids: HashMap<String, AccountId>,
    live: live::Live,
    sync_options: Option<iuredesk_sync::SyncOptions>,
    sender: Option<outbox::Sender>,
    transport_override: Option<Arc<dyn Transport>>,
    signatures: SignatureBook,
    /// Mensaje que se marcará como leído cuando pase el retraso.
    pending_read: Option<(MessageId, Instant)>,
    /// Avisos para la UI que no salen de un comando.
    notices: Vec<Notice>,
    pim: pim::Pim,
    /// Último error de calendario/contactos avisado (no se repite en cada pasada).
    pim_last_error: Option<Text>,
    /// Iurefficient (F3.1).
    iure: iure::Iure,
    /// Calendario: día de referencia, invitaciones, disponibilidad, recordatorios.
    pub cal: cal::CalState,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session").field("module", &self.module).field("folder", &self.folder).finish_non_exhaustive()
    }
}

fn store_err(e: iuredesk_store::StoreError) -> EngineError {
    EngineError::Store(e.to_string())
}

impl Session {
    /// Abre la sesión. Si hay un archivo de preferencias, se cargan.
    pub fn new(cfg: SessionConfig) -> Result<Session> {
        let prefs = cfg.prefs_path.as_deref().map(Prefs::load).unwrap_or_default();
        let bg = Background::new()?;
        let mut s = Session {
            store: cfg.store,
            account_store: cfg.accounts,
            secrets: cfg.secrets,
            accounts: Vec::new(),
            labels: Vec::new(),
            folders: Vec::new(),
            counts: HashMap::new(),
            prefs,
            prefs_path: cfg.prefs_path,
            module: Module::Mail,
            folder: FolderSel::default(),
            rows: Arc::new(Vec::new()),
            generation: 0,
            rows_job: None,
            loading: false,
            envelopes: HashMap::new(),
            bodies: HashMap::new(),
            body_jobs: HashSet::new(),
            selection: Vec::new(),
            focused: None,
            focus_request: None,
            search: String::new(),
            undo: Vec::new(),
            now: cfg.now,
            zone: cfg.zone,
            sample: cfg.sample,
            bg,
            wizard: None,
            contacts_cache: None,
            calendars: Vec::new(),
            events_cache: None,
            data_gen: 0,
            account_ids: HashMap::new(),
            live: live::Live::default(),
            sync_options: cfg.sync,
            sender: None,
            transport_override: cfg.transport,
            signatures: SignatureBook::default(),
            pending_read: None,
            notices: Vec::new(),
            pim: pim::Pim::default(),
            pim_last_error: None,
            iure: iure::Iure::default(),
            cal: cal::CalState::default(),
        };
        s.signatures = SignatureBook::load(&s.store).unwrap_or_default();
        s.reload_accounts()?;
        s.rebuild_rows();
        s.start_outbox();
        s.restart_sync();
        if s.sample {
            s.iure_start(IureSetup::Sample);
        }
        Ok(s)
    }

    /// Fija la función que pide un cuadro a la UI cuando termina un trabajo de fondo.
    pub fn set_waker(&self, waker: Waker) {
        self.bg.set_waker(waker);
    }

    /// El almacén.
    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }

    /// Ahora (fijo en capturas y pruebas).
    pub fn now(&self) -> DateTime<Utc> {
        self.now.unwrap_or_else(Utc::now)
    }

    /// Zona de la vista.
    pub fn zone(&self) -> Zone {
        self.zone
    }

    /// Hoy, en la zona de la vista.
    pub fn today(&self) -> NaiveDate {
        self.zone.local(self.now()).date()
    }

    /// Es el buzón de ejemplo.
    pub fn is_sample(&self) -> bool {
        self.sample
    }

    /// No hay ninguna cuenta: la UI muestra la bienvenida.
    pub fn needs_onboarding(&self) -> bool {
        self.labels.is_empty() && self.accounts.is_empty()
    }

    /// Vuelve a leer cuentas y carpetas.
    pub fn reload_accounts(&mut self) -> Result<()> {
        self.accounts = match &self.account_store {
            Some(a) => a.load().unwrap_or_default(),
            None => Vec::new(),
        };
        self.account_ids.clear();
        for i in 0..self.accounts.len() {
            if let Some(a) = self.accounts.get(i).cloned()
                && let Some(aid) = self.local_account_id(&a)
            {
                self.account_ids.insert(a.id.clone(), aid);
            }
        }
        self.labels = self.store.account_labels().map_err(store_err)?;
        self.folders = self.store.all_folders().map_err(store_err)?;
        self.calendars = self.store.calendars().map_err(store_err)?;
        self.counts.clear();
        self.data_gen = self.data_gen.wrapping_add(1);
        Ok(())
    }

    /// Id local de una cuenta: el ya asociado; el que reservó el asistente de alta en versiones
    /// anteriores (`acct<N>` con la misma dirección); o uno nuevo.
    fn local_account_id(&self, a: &Account) -> Option<AccountId> {
        if let Some(aid) = iuredesk_sync::state::lookup_account(&self.store, &a.id) {
            return Some(aid);
        }
        let reserved =
            a.id.strip_prefix("acct")
                .and_then(|r| r.split('-').next())
                .and_then(|n| n.parse::<u64>().ok())
                .map(AccountId);
        let labels = self.store.account_labels().unwrap_or_default();
        if let Some(aid) = reserved
            && labels.iter().any(|l| l.id == aid && l.email.eq_ignore_ascii_case(&a.email))
        {
            let _ = iuredesk_sync::state::bind_account(&self.store, &a.id, aid);
            return Some(aid);
        }
        iuredesk_sync::state::account_id(&self.store, a).ok()
    }

    /// Cuentas para la barra lateral.
    pub fn account_labels(&self) -> &[AccountLabel] {
        &self.labels
    }

    /// Carpetas de una cuenta (bandeja primero).
    pub fn folders_of(&self, account: AccountId) -> Vec<&Folder> {
        let mut v: Vec<&Folder> = self.folders.iter().filter(|f| f.account == account).collect();
        v.sort_by(|a, b| role_rank(a.role).cmp(&role_rank(b.role)).then_with(|| a.name.cmp(&b.name)));
        v
    }

    /// Una carpeta.
    pub fn folder(&self, id: FolderId) -> Option<&Folder> {
        self.folders.iter().find(|f| f.id == id)
    }

    fn role_folder(&self, account: AccountId, role: FolderRole) -> Option<FolderId> {
        self.folders.iter().find(|f| f.account == account && f.role == role).map(|f| f.id)
    }

    /// Recuento (total, no leídos) de una carpeta, con caché.
    pub fn count(&mut self, f: FolderId) -> FolderCount {
        if let Some(c) = self.counts.get(&f) {
            return *c;
        }
        let c = self.store.count(f).unwrap_or_default();
        self.counts.insert(f, c);
        c
    }

    /// No leídos de un favorito (todas las cuentas).
    pub fn unified_unread(&mut self, role: FolderRole) -> u64 {
        let ids: Vec<FolderId> = self.folders.iter().filter(|f| f.role == role).map(|f| f.id).collect();
        ids.into_iter().map(|f| self.count(f).unread).sum()
    }

    /// Mis direcciones (para no incluirme al responder).
    pub fn my_addresses(&self) -> Vec<String> {
        let mut v: Vec<String> = self.labels.iter().map(|l| l.email.clone()).collect();
        v.extend(self.accounts.iter().map(|a| a.email.clone()));
        v
    }

    // ------------------------------------------------------------------ lista

    /// Lo que muestra la lista.
    pub fn folder_sel(&self) -> FolderSel {
        self.folder
    }

    /// Cambia de carpeta (limpia la búsqueda y la selección).
    pub fn select_folder(&mut self, f: FolderSel) {
        if self.folder != f || !self.search.is_empty() {
            self.folder = f;
            self.search.clear();
            self.selection.clear();
            self.focused = None;
            self.rebuild_rows();
        }
        self.module = Module::Mail;
    }

    /// Nombre de lo que muestra la lista («Entrada», «Destacados», «Clientes»).
    pub fn folder_title(&self) -> String {
        if !self.search.trim().is_empty() {
            return crate::lang::pick("Search results", "Resultados de la búsqueda").to_string();
        }
        match self.folder {
            FolderSel::Unified(r) => role_label(r).to_string(),
            FolderSel::Flagged => crate::lang::pick("Flagged", "Marcados").to_string(),
            FolderSel::Folder(id) => self.folder(id).map(|f| f.name.clone()).unwrap_or_default(),
            FolderSel::Outbox => crate::lang::pick("Outbox", "Bandeja de salida").to_string(),
        }
    }

    /// Filas de la lista.
    pub fn rows(&self) -> &[ListRow] {
        &self.rows
    }

    /// Filas compartidas (la UI las guarda sin copiar mientras pinta la lista).
    pub fn rows_shared(&self) -> Arc<Vec<ListRow>> {
        Arc::clone(&self.rows)
    }

    /// Cambia cada vez que se recalculan las filas (clave de caché para la UI).
    pub fn list_generation(&self) -> u64 {
        self.generation
    }

    /// Se está calculando la lista (búsqueda o conversaciones).
    pub fn is_loading(&self) -> bool {
        self.loading
    }

    /// Carpetas que forman la vista actual.
    fn view_folders(&self) -> Vec<FolderId> {
        match self.folder {
            FolderSel::Unified(role) => self.folders.iter().filter(|f| f.role == role).map(|f| f.id).collect(),
            FolderSel::Folder(id) => vec![id],
            FolderSel::Flagged => self.folders.iter().map(|f| f.id).collect(),
            FolderSel::Outbox => Vec::new(),
        }
    }

    /// Recalcula las filas: al instante en plano; conversaciones y búsquedas en segundo plano.
    pub fn rebuild_rows(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        self.counts.clear();
        let query = self.search.trim().to_string();
        if !query.is_empty() {
            let store = Arc::clone(&self.store);
            let scope = self.search_scope();
            self.loading = true;
            self.rows_job = Some(self.bg.spawn_blocking(move || {
                let q = format!("{scope}{query}");
                let result = store
                    .search(&q, 1_000)
                    .map_err(|e| Text::new(format!("Search failed: {e}"), format!("La búsqueda falló: {e}")));
                JobOutput::Engine(EngineJob::Search { generation, result })
            }));
            return;
        }
        if self.folder == FolderSel::Flagged {
            let store = Arc::clone(&self.store);
            self.loading = true;
            self.rows_job = Some(self.bg.spawn_blocking(move || {
                let rows = store
                    .search("is:flagged", 1_000)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|e| ListRow { id: e.id, count: 1 })
                    .collect();
                JobOutput::Engine(EngineJob::Rows { generation, rows })
            }));
            return;
        }
        let order = self.merged_order();
        self.rows = Arc::new(order.iter().map(|&id| ListRow { id, count: 1 }).collect());
        if self.prefs.conversations && !order.is_empty() {
            let store = Arc::clone(&self.store);
            self.loading = true;
            self.rows_job = Some(self.bg.spawn_blocking(move || {
                JobOutput::Engine(EngineJob::Rows { generation, rows: group_conversations(&store, &order) })
            }));
        } else {
            self.loading = false;
            self.rows_job = None;
        }
    }

    /// `in:` para limitar la búsqueda a la vista actual (vacío = todo).
    fn search_scope(&self) -> String {
        match self.folder {
            FolderSel::Folder(id) => match self.folder(id) {
                Some(f) if f.role != FolderRole::Custom => format!("in:{} ", f.role.slug()),
                Some(f) if !f.name.contains(' ') => format!("in:{} ", f.name),
                _ => String::new(),
            },
            FolderSel::Unified(r) => format!("in:{} ", r.slug()),
            FolderSel::Flagged => "is:flagged ".into(),
            FolderSel::Outbox => String::new(),
        }
    }

    /// Ids de la vista ordenados por fecha (mezcla las carpetas sin leer sobres).
    fn merged_order(&self) -> Vec<MessageId> {
        let lists: Vec<Arc<Vec<(u64, u64)>>> =
            self.view_folders().into_iter().filter_map(|f| self.store.list_order(f).ok()).collect();
        if let [one] = lists.as_slice() {
            return one.iter().map(|&(_, id)| MessageId(id)).collect();
        }
        let total: usize = lists.iter().map(|l| l.len()).sum();
        let mut out = Vec::with_capacity(total);
        let mut pos = vec![0usize; lists.len()];
        loop {
            let mut best: Option<(usize, (u64, u64))> = None;
            for (i, l) in lists.iter().enumerate() {
                if let Some(&k) = pos.get(i).and_then(|&p| l.get(p))
                    && best.is_none_or(|(_, b)| k < b)
                {
                    best = Some((i, k));
                }
            }
            let Some((i, (_, id))) = best else { break };
            out.push(MessageId(id));
            if let Some(p) = pos.get_mut(i) {
                *p += 1;
            }
        }
        out
    }

    /// Sobre de un mensaje (caché; lee del almacén si falta).
    pub fn envelope(&mut self, id: MessageId) -> Option<&MessageEnvelope> {
        if !self.envelopes.contains_key(&id) {
            if self.envelopes.len() >= ENVELOPE_CACHE {
                self.envelopes.clear();
            }
            let e = self.store.get_envelope(id).ok().flatten()?;
            self.envelopes.insert(id, e);
        }
        self.envelopes.get(&id)
    }

    /// Texto de búsqueda actual.
    pub fn search_query(&self) -> &str {
        &self.search
    }

    /// Cambia la búsqueda (la ejecuta en segundo plano con el lenguaje `de:`, `tiene:adjunto`…).
    pub fn set_search(&mut self, q: &str) {
        if q != self.search {
            self.search = q.chars().take(1_000).collect();
            self.selection.clear();
            self.focused = None;
            self.module = Module::Mail;
            self.rebuild_rows();
        }
    }

    /// Activa o desactiva el agrupado por conversación.
    pub fn set_conversations(&mut self, on: bool) {
        if self.prefs.conversations != on {
            self.prefs.conversations = on;
            self.save_prefs();
            self.rebuild_rows();
        }
    }

    // ------------------------------------------------------------------ selección

    /// La UI informa de la selección de la lista (claves = ids) y de la fila del cursor.
    pub fn set_selection(&mut self, ids: Vec<MessageId>, cursor: Option<MessageId>) {
        self.selection = ids;
        let focus = if self.selection.len() == 1 { self.selection.first().copied() } else { cursor };
        let focus = focus.filter(|f| self.selection.contains(f)).or(if self.selection.len() == 1 {
            self.selection.first().copied()
        } else {
            None
        });
        if focus != self.focused {
            self.focused = focus;
            if let Some(id) = focus {
                self.on_open(id);
            }
        }
    }

    /// Selección actual.
    pub fn selection(&self) -> &[MessageId] {
        &self.selection
    }

    /// El mensaje del panel de lectura.
    pub fn focused(&self) -> Option<MessageId> {
        self.focused
    }

    /// Pide a la UI que seleccione y muestre este mensaje (atajos, capturas, enlaces).
    pub fn request_focus(&mut self, id: MessageId) {
        self.focus_request = Some(id);
    }

    /// Fila que la UI debe seleccionar (tras archivar, deshacer…). Se consume al leerla.
    pub fn take_focus_request(&mut self) -> Option<(usize, MessageId)> {
        let id = self.focus_request?;
        match self.rows.iter().position(|r| r.id == id) {
            Some(idx) => {
                self.focus_request = None;
                Some((idx, id))
            }
            None => {
                // Mientras se recalculan las filas, la petición espera.
                if !self.loading {
                    self.focus_request = None;
                }
                None
            }
        }
    }

    fn on_open(&mut self, id: MessageId) {
        self.request_body(id);
        self.pending_read = None;
        if !self.prefs.mark_read_on_open {
            return;
        }
        if self.prefs.mark_read_delay_ms == 0 {
            self.mark_read_now(id);
        } else {
            self.pending_read = Some((id, Instant::now()));
        }
    }

    /// Marca como leído (local al instante; el servidor se entera por la cola).
    fn mark_read_now(&mut self, id: MessageId) {
        let Some(env) = self.envelope(id).cloned() else { return };
        if !env.flags.seen {
            let flags = Flags { seen: true, ..env.flags };
            if self.apply_flags(id, env.account, flags).is_ok() {
                self.refresh_envelope(id);
                self.counts.clear();
            }
        }
    }

    /// Marca como leído el mensaje abierto cuando pasa el retraso (desde `poll`).
    fn poll_pending_read(&mut self) {
        let Some((id, since)) = self.pending_read else { return };
        let delay = std::time::Duration::from_millis(self.prefs.mark_read_delay_ms.into());
        if since.elapsed() < delay {
            return;
        }
        self.pending_read = None;
        if self.focused == Some(id) {
            self.mark_read_now(id);
        }
    }

    /// Cuándo debe la UI volver a llamar a `poll` aunque no pase nada (retraso de «leído»,
    /// refresco pendiente de la sincronización).
    pub fn next_deadline(&self) -> Option<std::time::Duration> {
        let read = self.pending_read.map(|(_, since)| {
            std::time::Duration::from_millis(self.prefs.mark_read_delay_ms.into()).saturating_sub(since.elapsed())
        });
        [read, self.sync_deadline(), self.iure_deadline()].into_iter().flatten().min()
    }

    /// Cambia banderas por la API local-primero de `sync` y la despierta.
    fn apply_flags(&mut self, id: MessageId, account: AccountId, flags: Flags) -> Result<Option<u64>> {
        let seq =
            iuredesk_sync::ops::set_flags(&self.store, id, flags).map_err(|e| EngineError::Store(e.to_string()))?;
        if seq.is_some() {
            self.kick(account);
        }
        Ok(seq)
    }

    /// Mueve por la API local-primero de `sync` y la despierta.
    fn apply_move(&mut self, id: MessageId, account: AccountId, to: FolderId) -> Result<Option<u64>> {
        let seq =
            iuredesk_sync::ops::move_message(&self.store, id, to).map_err(|e| EngineError::Store(e.to_string()))?;
        if seq.is_some() {
            self.kick(account);
        }
        Ok(seq)
    }

    fn refresh_envelope(&mut self, id: MessageId) {
        self.envelopes.remove(&id);
        let _ = self.envelope(id);
    }

    /// Mensajes de la conversación del mensaje enfocado (o solo él), en orden cronológico.
    pub fn conversation(&mut self, id: MessageId) -> Vec<MessageEnvelope> {
        let Some(env) = self.envelope(id).cloned() else { return Vec::new() };
        if !self.prefs.conversations {
            return vec![env];
        }
        match env.thread {
            Some(t) => {
                let mut v = self.store.thread_envelopes(t).unwrap_or_default();
                v.truncate(200);
                if v.is_empty() { vec![env] } else { v }
            }
            None => vec![env],
        }
    }

    /// Cuerpo de un mensaje si ya se leyó; si no, lo pide en segundo plano y devuelve `None`.
    pub fn body(&mut self, id: MessageId) -> Option<&MessageBody> {
        if !self.bodies.contains_key(&id) {
            self.request_body(id);
            return None;
        }
        self.bodies.get(&id)
    }

    /// Pide el cuerpo en segundo plano (no hace nada si ya está o ya se pidió).
    pub fn request_body(&mut self, id: MessageId) {
        if self.bodies.contains_key(&id) || self.body_jobs.contains(&id) {
            return;
        }
        self.body_jobs.insert(id);
        let store = Arc::clone(&self.store);
        let downloaded = store.get_envelope(id).ok().flatten().is_some_and(|e| e.blob.is_some());
        match (&self.live.handle, downloaded) {
            // El cuerpo aún está en el servidor: se baja y luego se lee.
            (Some(h), false) => {
                let h = h.clone();
                self.bg.spawn(async move {
                    let fetched = h.fetch_body(id).await;
                    let body = match (fetched, store.get_raw(id)) {
                        (_, Ok(raw)) => crate::body::parse(&raw),
                        (r, Err(_)) => {
                            let preview = store.get_envelope(id).ok().flatten().map(|e| e.preview).unwrap_or_default();
                            let why = match r {
                                Err(e) => Text::new(e.message(false), e.message(true)),
                                Ok(()) => Text::new("The message is empty.", "El mensaje está vacío."),
                            };
                            MessageBody {
                                text: preview,
                                note: Some(Text::new(
                                    format!("Only the preview is available: {}", why.en),
                                    format!("Solo está el extracto: {}", why.es),
                                )),
                                ..Default::default()
                            }
                        }
                    };
                    JobOutput::Engine(EngineJob::Body { id, body })
                });
            }
            _ => {
                self.bg.spawn_blocking(move || {
                    let body = match store.get_raw(id) {
                        Ok(raw) => crate::body::parse(&raw),
                        Err(_) => {
                            let preview = store.get_envelope(id).ok().flatten().map(|e| e.preview).unwrap_or_default();
                            MessageBody { text: preview, ..Default::default() }
                        }
                    };
                    JobOutput::Engine(EngineJob::Body { id, body })
                });
            }
        }
    }

    /// Vuelve a pedir el cuerpo (tras un fallo de descarga).
    pub fn retry_body(&mut self, id: MessageId) {
        self.bodies.remove(&id);
        self.body_jobs.remove(&id);
        self.request_body(id);
    }

    /// Panel contextual del mensaje.
    pub fn context_card(&mut self, id: MessageId) -> ContextCard {
        let Some(env) = self.envelope(id).cloned() else { return ContextCard::default() };
        let mine = self.my_addresses();
        let other = env
            .from
            .iter()
            .chain(env.to.iter())
            .find(|m| !mine.iter().any(|a| a.eq_ignore_ascii_case(&m.addr)))
            .cloned();
        let contact = other.as_ref().and_then(|m| {
            self.contacts().iter().find(|c| c.emails.iter().any(|e| e.value.eq_ignore_ascii_case(&m.addr))).cloned()
        });
        let links = crate::context::detect_links(&env, contact.as_ref());
        ContextCard { contact, links, history: 0 }
    }

    // ------------------------------------------------------------------ comandos

    /// Mensajes a los que se aplica una acción: la selección, o el enfocado. En modo
    /// conversación, incluye los mensajes de cada conversación que están en la vista actual.
    fn targets(&mut self) -> Vec<MessageId> {
        let base: Vec<MessageId> =
            if self.selection.is_empty() { self.focused.into_iter().collect() } else { self.selection.clone() };
        if !self.prefs.conversations || !self.search.is_empty() || self.folder == FolderSel::Flagged {
            return base;
        }
        let view: HashSet<FolderId> = self.view_folders().into_iter().collect();
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for id in base.into_iter().take(MAX_BATCH) {
            let thread = self.envelope(id).and_then(|e| e.thread);
            let members = match thread {
                Some(t) => self.store.thread_envelopes(t).unwrap_or_default(),
                None => Vec::new(),
            };
            let mut any = false;
            for m in members.into_iter().filter(|m| view.contains(&m.folder)) {
                any = true;
                if seen.insert(m.id) {
                    out.push(m.id);
                }
            }
            if !any && seen.insert(id) {
                out.push(id);
            }
        }
        out
    }

    fn has_target(&self) -> bool {
        self.focused.is_some() || !self.selection.is_empty()
    }

    /// ¿Se puede ejecutar el comando ahora?
    pub fn is_enabled(&self, id: &str) -> bool {
        let base = id.split_once(':').map_or(id, |(b, _)| b);
        match base {
            "mail.reply" | "mail.reply_all" | "mail.forward" => self.focused.is_some(),
            "mail.archive" | "mail.delete" | "mail.toggle_read" | "mail.toggle_flag" | "mail.move" => self.has_target(),
            "undo" => !self.undo.is_empty(),
            _ => command::find(base).is_some(),
        }
    }

    /// Texto de lo que desharía «Deshacer».
    pub fn undo_label(&self) -> Option<&Text> {
        self.undo.last().map(|u| &u.text)
    }

    /// Ejecuta un comando por id (`mail.archive`, `mail.move:12`, `view.density.dense`…).
    pub fn run(&mut self, id: &str) -> Result<Outcome> {
        let (base, arg) = match id.split_once(':') {
            Some((b, a)) => (b, Some(a)),
            None => (id, None),
        };
        if command::find(base).is_none() {
            return Err(EngineError::UnknownCommand(id.to_string()));
        }
        if !self.is_enabled(id) {
            return Ok(Outcome::Notice(match base {
                "undo" => Notice::info("Nothing to undo.", "No hay nada que deshacer."),
                _ => Notice::info("Select a message first.", "Primero selecciona un mensaje."),
            }));
        }
        match base {
            // «Nuevo» en el calendario es un evento (como en Outlook).
            "mail.new" | "cal.new_event" if self.module == Module::Calendar || base == "cal.new_event" => {
                self.run_calendar("cal.new_event")
            }
            b if b.starts_with("cal.") => self.run_calendar(b),
            "mail.new" => {
                let account = match self.folder {
                    FolderSel::Folder(f) => self.folder(f).map(|f| f.account),
                    _ => None,
                };
                Ok(Outcome::Compose(Box::new(self.compose_new(account))))
            }
            "mail.reply" | "mail.reply_all" | "mail.forward" => self.reply_like(base),
            "mail.archive" => self.archive(),
            "mail.delete" => self.delete(),
            "mail.toggle_read" => self.toggle_read(),
            "mail.toggle_flag" => self.toggle_flag(),
            "mail.move" => match arg.and_then(|a| a.parse::<u64>().ok()) {
                Some(f) => self.move_to(FolderId(f)),
                None => Ok(Outcome::PickFolder),
            },
            "mail.search" => {
                self.module = Module::Mail;
                Ok(Outcome::FocusSearch)
            }
            "mail.toggle_conversations" => {
                let on = !self.prefs.conversations;
                self.set_conversations(on);
                Ok(Outcome::Done)
            }
            "nav.mail" | "nav.calendar" | "nav.contacts" | "nav.notes" | "nav.tasks" | "nav.assistant" => {
                if let Some(m) = Module::from_key(base.trim_start_matches("nav.")) {
                    self.module = m;
                }
                Ok(Outcome::Done)
            }
            "view.density.comfortable" | "view.density.compact" | "view.density.dense" => {
                if let Some(d) = DensityPref::from_key(base.trim_start_matches("view.density.")) {
                    self.prefs.density = d;
                    self.save_prefs();
                }
                Ok(Outcome::Restyle)
            }
            "view.theme.system" | "view.theme.light" | "view.theme.dark" | "view.theme.high_contrast" => {
                if let Some(t) = ThemePref::from_key(base.trim_start_matches("view.theme.")) {
                    self.prefs.theme = t;
                    self.save_prefs();
                }
                Ok(Outcome::Restyle)
            }
            "view.scheme.outlook" | "view.scheme.gmail" | "view.scheme.apple_mail" => {
                if let Some(s) = ShortcutScheme::from_key(base.trim_start_matches("view.scheme.")) {
                    self.prefs.scheme = s;
                    self.save_prefs();
                    return Ok(Outcome::Notice(Notice::success(
                        format!("Keyboard shortcuts: {}", s.label()),
                        format!("Atajos de teclado: {}", s.label()),
                    )));
                }
                Ok(Outcome::Done)
            }
            "app.palette" => Ok(Outcome::OpenPalette),
            "app.add_account" => {
                self.wizard_begin();
                Ok(Outcome::AddAccount)
            }
            "undo" => self.undo_last(),
            _ => Err(EngineError::UnknownCommand(id.to_string())),
        }
    }

    fn push_undo(&mut self, text: Text, steps: Vec<(Inverse, Option<u64>)>) {
        if steps.is_empty() {
            return;
        }
        self.undo.push(UndoEntry { text, steps });
        if self.undo.len() > UNDO_DEPTH {
            self.undo.remove(0);
        }
    }

    /// La fila que queda a la vista tras quitar `gone` (la siguiente, o la anterior al final).
    fn next_focus_after(&self, gone: &HashSet<MessageId>) -> Option<MessageId> {
        let last = self.rows.iter().rposition(|r| gone.contains(&r.id))?;
        self.rows
            .iter()
            .skip(last + 1)
            .find(|r| !gone.contains(&r.id))
            .or_else(|| self.rows.iter().take(last).rev().find(|r| !gone.contains(&r.id)))
            .map(|r| r.id)
    }

    /// Mueve `ids` a la carpeta que da `dest(cuenta)`; devuelve cuántos se movieron.
    fn move_with(
        &mut self,
        ids: &[MessageId],
        dest: impl Fn(&Session, AccountId) -> Option<FolderId>,
    ) -> Result<(usize, Vec<(Inverse, Option<u64>)>)> {
        let mut steps = Vec::new();
        let mut moved = 0usize;
        for &id in ids.iter().take(MAX_BATCH) {
            let Some(env) = self.envelope(id).cloned() else { continue };
            let Some(to) = dest(self, env.account) else { continue };
            if to == env.folder {
                continue;
            }
            let seq = self.apply_move(id, env.account, to)?;
            steps.push((
                Inverse::Move { id, account: env.account, folder: env.folder, remote: env.remote_id.clone() },
                seq,
            ));
            self.envelopes.remove(&id);
            moved += 1;
        }
        Ok((moved, steps))
    }

    fn after_removal(&mut self, ids: &[MessageId]) {
        let gone: HashSet<MessageId> = ids.iter().copied().collect();
        // Las filas pueden representar una conversación: lo que se va es la fila entera.
        let gone_rows: HashSet<MessageId> =
            self.rows.iter().filter(|r| gone.contains(&r.id) || self.selection.contains(&r.id)).map(|r| r.id).collect();
        self.focus_request = self.next_focus_after(&gone_rows);
        self.selection.clear();
        self.focused = None;
        self.rebuild_rows();
    }

    fn plural(n: usize, en1: &str, enn: &str, es1: &str, esn: &str) -> Text {
        if n == 1 { Text::new(en1, es1) } else { Text::new(format!("{n} {enn}"), format!("{n} {esn}")) }
    }

    fn archive(&mut self) -> Result<Outcome> {
        let ids = self.targets();
        let (n, steps) = self.move_with(&ids, |s, acc| s.role_folder(acc, FolderRole::Archive))?;
        if n == 0 {
            return Ok(Outcome::Notice(Notice::error(Text::new(
                "This account has no archive folder.",
                "Esta cuenta no tiene carpeta de archivo.",
            ))));
        }
        let text = Self::plural(n, "Message archived", "messages archived", "Mensaje archivado", "mensajes archivados");
        self.push_undo(text.clone(), steps);
        self.after_removal(&ids);
        Ok(Outcome::Notice(Notice { kind: NoticeKind::Success, text, undo: true, secs: None }))
    }

    fn delete(&mut self) -> Result<Outcome> {
        let ids = self.targets();
        // Lo que ya está en la papelera se elimina de verdad (sin deshacer); lo demás va a ella.
        let mut in_trash = Vec::new();
        let mut rest = Vec::new();
        for id in ids.iter().copied() {
            let folder = self.envelope(id).map(|e| e.folder);
            let trash = folder.and_then(|f| self.folder(f)).is_some_and(|f| f.role == FolderRole::Trash);
            if trash { in_trash.push(id) } else { rest.push(id) }
        }
        let (n, steps) = self.move_with(&rest, |s, acc| s.role_folder(acc, FolderRole::Trash))?;
        for id in &in_trash {
            if self.envelope(*id).is_some() {
                self.discard_message(*id);
            }
        }
        self.after_removal(&ids);
        if !in_trash.is_empty() && n == 0 {
            return Ok(Outcome::Notice(Notice::info(
                if in_trash.len() == 1 {
                    "Deleted permanently".to_string()
                } else {
                    format!("{} deleted permanently", in_trash.len())
                },
                if in_trash.len() == 1 {
                    "Eliminado definitivamente".to_string()
                } else {
                    format!("{} eliminados definitivamente", in_trash.len())
                },
            )));
        }
        let text = Self::plural(
            n,
            "Moved to Trash",
            "messages moved to Trash",
            "Enviado a la papelera",
            "mensajes enviados a la papelera",
        );
        self.push_undo(text.clone(), steps);
        Ok(Outcome::Notice(Notice { kind: NoticeKind::Success, text, undo: true, secs: None }))
    }

    fn move_to(&mut self, folder: FolderId) -> Result<Outcome> {
        let Some(dest) = self.folder(folder).cloned() else {
            return Ok(Outcome::Notice(Notice::error(Text::new(
                "That folder no longer exists.",
                "Esa carpeta ya no existe.",
            ))));
        };
        let ids = self.targets();
        let (n, steps) = self.move_with(&ids, |_, acc| (acc == dest.account).then_some(dest.id))?;
        if n == 0 {
            return Ok(Outcome::Notice(Notice::error(Text::new(
                "Messages can only be moved within the same account.",
                "Los mensajes solo se pueden mover dentro de la misma cuenta.",
            ))));
        }
        let text = Text::new(
            if n == 1 { format!("Moved to {}", dest.name) } else { format!("{n} messages moved to {}", dest.name) },
            if n == 1 { format!("Movido a {}", dest.name) } else { format!("{n} mensajes movidos a {}", dest.name) },
        );
        self.push_undo(text.clone(), steps);
        self.after_removal(&ids);
        Ok(Outcome::Notice(Notice { kind: NoticeKind::Success, text, undo: true, secs: None }))
    }

    /// Cambia las banderas de `ids` con `f` y deja la entrada de deshacer.
    fn set_flags_with(&mut self, ids: &[MessageId], text: Text, f: impl Fn(Flags) -> Flags) -> Result<Outcome> {
        let mut steps = Vec::new();
        for &id in ids.iter().take(MAX_BATCH) {
            let Some(env) = self.envelope(id).cloned() else { continue };
            let new = f(env.flags);
            if new == env.flags {
                continue;
            }
            let seq = self.apply_flags(id, env.account, new)?;
            steps.push((Inverse::Flags { id, account: env.account, flags: env.flags }, seq));
            self.refresh_envelope(id);
        }
        self.counts.clear();
        if steps.is_empty() {
            return Ok(Outcome::Done);
        }
        self.push_undo(text.clone(), steps);
        if self.folder == FolderSel::Flagged {
            self.rebuild_rows();
        }
        Ok(Outcome::Notice(Notice { kind: NoticeKind::Success, text, undo: true, secs: None }))
    }

    fn toggle_read(&mut self) -> Result<Outcome> {
        let ids = self.targets();
        let any_unread = ids.iter().any(|&id| self.envelope(id).is_some_and(|e| !e.flags.seen));
        let text = if any_unread {
            Text::new("Marked as read", "Marcado como leído")
        } else {
            Text::new("Marked as unread", "Marcado como no leído")
        };
        // Al marcar como no leído, el mensaje no debe volver a leerse solo al seguir enfocado.
        self.set_flags_with(&ids, text, |f| Flags { seen: any_unread, ..f })
    }

    fn toggle_flag(&mut self) -> Result<Outcome> {
        let ids = self.targets();
        let any_unflagged = ids.iter().any(|&id| self.envelope(id).is_some_and(|e| !e.flags.flagged));
        let text = if any_unflagged {
            Text::new("Flagged for follow-up", "Marcado para seguimiento")
        } else {
            Text::new("Flag removed", "Seguimiento quitado")
        };
        self.set_flags_with(&ids, text, |f| Flags { flagged: any_unflagged, ..f })
    }

    /// Deshace la última acción.
    pub fn undo_last(&mut self) -> Result<Outcome> {
        let Some(entry) = self.undo.pop() else {
            return Ok(Outcome::Notice(Notice::info("Nothing to undo.", "No hay nada que deshacer.")));
        };
        let pending: HashSet<u64> =
            self.store.peek_ops(10_000).unwrap_or_default().into_iter().map(|o| o.seq).collect();
        let mut restored = None;
        for (inv, seq) in entry.steps.into_iter().rev() {
            // Si la operación no salió todavía de la máquina, se cancela; si ya se replicó, se
            // encola la inversa.
            let cancel = seq.filter(|s| pending.contains(s));
            match inv {
                Inverse::Flags { id, account, flags } => {
                    match cancel {
                        // Aún no salió: se cancela y se restauran las banderas solo en local.
                        Some(s) => {
                            self.store.ack_op(s).map_err(store_err)?;
                            self.store.set_flags(id, flags).map_err(store_err)?;
                        }
                        None => {
                            self.apply_flags(id, account, flags)?;
                        }
                    }
                    self.refresh_envelope(id);
                    restored = Some(id);
                }
                Inverse::Move { id, account, folder, remote } => {
                    match cancel {
                        Some(s) => iuredesk_sync::ops::cancel_move(&self.store, s, id, folder, remote)
                            .map_err(|e| EngineError::Store(e.to_string()))?,
                        None if seq.is_none() => {
                            // Nunca se encoló (mensaje solo local): se devuelve tal cual.
                            self.store.move_message(id, folder).map_err(store_err)?;
                            self.store.set_remote_id(id, remote).map_err(store_err)?;
                        }
                        None => {
                            self.apply_move(id, account, folder)?;
                        }
                    }
                    self.envelopes.remove(&id);
                    restored = Some(id);
                }
                Inverse::Send(outbox_id) => {
                    return Ok(self.undo_send(outbox_id));
                }
                Inverse::Calendar(u) => self.undo_calendar(*u)?,
                Inverse::Task(t) => {
                    self.store.put_task(&t).map_err(store_err)?;
                    self.data_gen = self.data_gen.wrapping_add(1);
                }
            }
        }
        self.counts.clear();
        self.rebuild_rows();
        if let Some(id) = restored {
            // En modo conversación la fila es la del mensaje más reciente; se busca por hilo
            // cuando lleguen las filas.
            self.focus_request = Some(id);
        }
        Ok(Outcome::Notice(Notice::success(
            format!("Undone: {}", entry.text.en),
            format!("Deshecho: {}", entry.text.es),
        )))
    }

    // ------------------------------------------------------------------ preferencias

    /// Guarda las preferencias (sin ruta, no hace nada). Un fallo no interrumpe la sesión.
    pub fn save_prefs(&self) {
        if let Some(p) = &self.prefs_path {
            let _ = self.prefs.save(p);
        }
    }

    /// Esquema de atajos activo.
    pub fn scheme(&self) -> ShortcutScheme {
        self.prefs.scheme
    }

    // ------------------------------------------------------------------ fondo

    /// Encola trabajo de la UI (p. ej. el render de HTML) en el runtime de fondo.
    pub fn spawn_blocking(&mut self, f: impl FnOnce() -> JobOutput + Send + 'static) -> JobId {
        self.bg.spawn_blocking(f)
    }

    /// Descarga en segundo plano los recursos remotos que el usuario decidió ver (imágenes de un
    /// correo). Responde con [`Event::Custom`] y un [`Fetched`] con la misma `tag`. Acotado: 40
    /// recursos, 5 MB cada uno, 10 s por petición, solo `http`/`https`.
    pub fn fetch_remote(&mut self, tag: u64, urls: Vec<String>) -> JobId {
        self.bg.spawn(async move {
            let mut items = Vec::new();
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .user_agent(concat!("IureDesk/", env!("CARGO_PKG_VERSION")))
                .build();
            if let Ok(client) = client {
                for url in urls.into_iter().filter(|u| u.starts_with("https://") || u.starts_with("http://")).take(40) {
                    let Ok(resp) = client.get(&url).send().await else { continue };
                    if !resp.status().is_success() || resp.content_length().is_some_and(|l| l > 5 * 1024 * 1024) {
                        continue;
                    }
                    if let Ok(bytes) = resp.bytes().await
                        && bytes.len() <= 5 * 1024 * 1024
                    {
                        items.push((url, bytes.to_vec()));
                    }
                }
            }
            JobOutput::Custom(Box::new(Fetched { tag, items }))
        })
    }

    /// No hay trabajos en curso (capturas deterministas).
    pub fn is_idle(&self) -> bool {
        self.bg.pending() == 0
    }

    /// Procesa los resultados de fondo; devuelve lo que la UI debe atender.
    pub fn poll(&mut self) -> Vec<Event> {
        let done: Vec<Done> = self.bg.poll();
        let mut events = Vec::new();
        self.poll_sync(&mut events);
        self.poll_pim();
        self.poll_imip_replies();
        self.poll_outbox();
        self.poll_pending_read();
        self.poll_iure(&mut events);
        events.extend(self.notices.drain(..).map(Event::Notice));
        for d in done {
            match d.output {
                JobOutput::Custom(value) => events.push(Event::Custom { id: d.id, value }),
                JobOutput::Engine(job) => {
                    if let Some(n) = self.handle_job(d.id, job) {
                        events.push(Event::Notice(n));
                    }
                }
            }
        }
        events
    }

    fn handle_job(&mut self, id: JobId, job: EngineJob) -> Option<Notice> {
        match job {
            EngineJob::Rows { generation, rows } => {
                if generation == self.generation {
                    self.rows = Arc::new(rows);
                    self.loading = false;
                    self.fix_focus_request();
                }
                None
            }
            EngineJob::Search { generation, result } => {
                if generation != self.generation {
                    return None;
                }
                self.loading = false;
                match result {
                    Ok(envs) => {
                        self.rows = Arc::new(envs.iter().map(|e| ListRow { id: e.id, count: 1 }).collect());
                        for e in envs {
                            self.envelopes.insert(e.id, e);
                        }
                        None
                    }
                    Err(t) => {
                        self.rows = Arc::new(Vec::new());
                        Some(Notice::error(t))
                    }
                }
            }
            EngineJob::Body { id, body } => {
                self.body_jobs.remove(&id);
                if self.bodies.len() >= BODY_CACHE {
                    self.bodies.clear();
                }
                self.bodies.insert(id, body);
                None
            }
            EngineJob::Discovery(r) => self.wizard_on_discovery(id, r),
            EngineJob::Probe(r) => self.wizard_on_probe(id, r, false),
            EngineJob::OAuth(r) => self.wizard_on_probe(id, r, true),
            EngineJob::Failed(t) => {
                if self.rows_job == Some(id) {
                    self.loading = false;
                }
                if let Some(w) = self.wizard.as_mut()
                    && w.job == Some(id)
                {
                    w.step = WizardStep::Provider;
                    w.job = None;
                }
                Some(Notice::error(t))
            }
        }
    }

    /// Si la fila pedida ya no existe (p. ej. la conversación ahora la encabeza otro mensaje),
    /// busca la fila de su hilo.
    fn fix_focus_request(&mut self) {
        let Some(want) = self.focus_request else { return };
        if self.rows.iter().any(|r| r.id == want) {
            return;
        }
        let thread = self.envelope(want).and_then(|e| e.thread);
        let Some(t) = thread else { return };
        let ids: Vec<MessageId> = self.rows.iter().map(|r| r.id).take(5_000).collect();
        for rid in ids {
            if self.envelope(rid).and_then(|e| e.thread) == Some(t) {
                self.focus_request = Some(rid);
                return;
            }
        }
    }

    // ------------------------------------------------------------------ calendario, contactos…

    /// Calendarios.
    pub fn calendars(&self) -> &[Calendar] {
        &self.calendars
    }

    /// Apariciones de eventos en `[from, to)` (con caché por ventana).
    pub fn occurrences(&mut self, from: DateTime<Utc>, to: DateTime<Utc>) -> &[Occurrence] {
        let fresh = self.events_cache.as_ref().is_some_and(|(k, _)| *k == (from, to));
        if !fresh {
            let occ = self.occurrences_uncached(from, to);
            self.events_cache = Some(((from, to), occ));
        }
        self.events_cache.as_ref().map(|(_, v)| v.as_slice()).unwrap_or(&[])
    }

    /// Eventos de hoy (panel «Hoy»).
    pub fn today_events(&mut self) -> Vec<Occurrence> {
        let today = self.today();
        let from = self.zone.day_start(today);
        let to = self.zone.day_start(today + Duration::days(1));
        self.occurrences_uncached(from, to)
    }

    /// Expansión completa de RFC 5545 de `iuredesk-pim-sync` (excepciones, `RECURRENCE-ID`,
    /// zonas); si fallara, la expansión sencilla de [`crate::calendar::occurrences`].
    fn occurrences_uncached(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<Occurrence> {
        let color = |cal: iuredesk_model::CalendarId| {
            self.calendars.iter().find(|c| c.id == cal).and_then(|c| c.color).unwrap_or(0x3B82F6)
        };
        let tz = crate::time::user_tz(self.zone);
        match iuredesk_pim_sync::occurrences_between(&self.store, &[], from, to, tz) {
            Ok(list) => {
                let mut v: Vec<Occurrence> =
                    list.iter().map(|o| crate::calendar::from_pim(o, color(o.calendar), tz)).collect();
                v.sort_by_key(|o| o.start);
                v
            }
            Err(_) => {
                let events: Vec<CalEvent> = self.store.events_between(&[], from, to).unwrap_or_default();
                crate::calendar::occurrences(&events, from, to, |e| color(e.calendar))
            }
        }
    }

    /// Contactos de todas las libretas, por nombre.
    pub fn contacts(&mut self) -> &[Contact] {
        if self.contacts_cache.is_none() {
            let mut all = Vec::new();
            for b in self.store.address_books().unwrap_or_default() {
                all.extend(self.store.contacts(b.id).unwrap_or_default());
            }
            all.sort_by_key(|c| crate::sort_key(&c.full_name));
            self.contacts_cache = Some(all);
        }
        self.contacts_cache.as_deref().unwrap_or(&[])
    }

    /// Notas: fijadas primero, luego las más recientes.
    pub fn notes(&self) -> Vec<Note> {
        let mut n = self.store.notes().unwrap_or_default();
        n.sort_by(|a, b| b.pinned.cmp(&a.pinned).then(b.updated.cmp(&a.updated)));
        n
    }

    /// Listas de tareas.
    pub fn task_lists(&self) -> Vec<TaskList> {
        self.store.task_lists().unwrap_or_default()
    }

    /// Tareas de una lista: pendientes por vencimiento, luego las terminadas.
    pub fn tasks(&self, list: iuredesk_model::TaskListId) -> Vec<Task> {
        let mut t = self.store.tasks(list).unwrap_or_default();
        t.sort_by_key(|t| (t.is_done(), t.due.as_ref().map(|d| d.to_utc()), t.priority == 0, t.priority));
        t
    }

    /// Tareas pendientes que vencen hoy o antes (panel «Hoy»).
    pub fn due_tasks(&self) -> Vec<Task> {
        let end = self.zone.day_start(self.today() + Duration::days(1));
        let mut out: Vec<Task> = self
            .task_lists()
            .into_iter()
            .flat_map(|l| self.store.tasks(l.id).unwrap_or_default())
            .filter(|t| !t.is_done() && t.due.as_ref().is_some_and(|d| d.to_utc() < end))
            .collect();
        out.sort_by_key(|t| t.due.as_ref().map(|d| d.to_utc()));
        out
    }

    /// Marca o desmarca una tarea como terminada (con deshacer).
    pub fn toggle_task(&mut self, id: TaskId, lists: &[TaskList]) -> Result<Outcome> {
        let task = lists
            .iter()
            .flat_map(|l| self.store.tasks(l.id).unwrap_or_default())
            .find(|t| t.id == id)
            .ok_or_else(|| EngineError::Invalid("tarea inexistente".into()))?;
        let mut new = task.clone();
        if task.is_done() {
            new.status = TaskStatus::NeedsAction;
            new.completed = None;
            new.percent_complete = 0;
        } else {
            new.status = TaskStatus::Completed;
            new.completed = Some(self.now());
            new.percent_complete = 100;
        }
        self.store.put_task(&new).map_err(store_err)?;
        self.data_gen = self.data_gen.wrapping_add(1);
        self.iure_task_toggled(&new);
        let text = if new.is_done() {
            Text::new("Task completed", "Tarea terminada")
        } else {
            Text::new("Task reopened", "Tarea reabierta")
        };
        self.push_undo(text.clone(), vec![(Inverse::Task(Box::new(task)), None)]);
        Ok(Outcome::Notice(Notice { kind: NoticeKind::Success, text, undo: true, secs: None }))
    }

    /// Generación de los datos de PIM (cambia al editar tareas o cuentas): sirve de clave de caché.
    pub fn data_generation(&self) -> u64 {
        self.data_gen
    }

    // ------------------------------------------------------------------ asistente de alta

    /// Abre el asistente de alta.
    pub fn wizard_begin(&mut self) {
        if self.wizard.as_ref().is_none_or(|w| w.step == WizardStep::Done) {
            self.wizard = Some(Wizard::default());
        }
    }

    /// Cierra el asistente (cancela el inicio de sesión en curso).
    pub fn wizard_close(&mut self) {
        if let Some(w) = self.wizard.take()
            && let Some(c) = w.cancel
        {
            c.cancel();
        }
    }

    /// Vuelve al paso del correo.
    pub fn wizard_back(&mut self) {
        if let Some(w) = self.wizard.as_mut() {
            if let Some(c) = w.cancel.take() {
                c.cancel();
            }
            w.step = WizardStep::Email;
            w.flow = None;
            w.discovery = None;
            w.message = None;
            w.job = None;
            w.password.clear();
        }
    }

    /// Busca el proveedor del correo escrito (en segundo plano).
    pub fn wizard_detect(&mut self) {
        let Some(email) = self.wizard.as_ref().map(|w| w.email.trim().to_string()) else { return };
        if let Err(e) = iuredesk_autoconfig::EmailAddress::parse(&email) {
            if let Some(w) = self.wizard.as_mut() {
                w.message = Some(Notice::error(Text::new(
                    format!("That doesn't look like an email address ({e})."),
                    "Eso no parece una dirección de correo. Revisa que tenga la forma nombre@dominio.com.",
                )));
            }
            return;
        }
        let job = self.bg.spawn(async move {
            let r = iuredesk_autoconfig::discover(&email).await.map_err(|e| {
                Text::new(
                    format!("We couldn't look up your provider ({e}). Check your internet connection."),
                    format!("No pudimos consultar tu proveedor ({e}). Revisa tu conexión a internet."),
                )
            });
            JobOutput::Engine(EngineJob::Discovery(r))
        });
        if let Some(w) = self.wizard.as_mut() {
            w.step = WizardStep::Detecting;
            w.message = None;
            w.job = Some(job);
        }
    }

    fn wizard_on_discovery(
        &mut self,
        id: JobId,
        r: std::result::Result<iuredesk_autoconfig::Discovery, Text>,
    ) -> Option<Notice> {
        let w = self.wizard.as_mut().filter(|w| w.job == Some(id))?;
        w.job = None;
        match r {
            Ok(d) if d.best().is_some() => {
                self.wizard_apply_discovery(d);
            }
            Ok(_) => {
                w.step = WizardStep::Email;
                w.message = Some(Notice::error(Text::new(
                    "We couldn't find the mail servers for that address. Check the spelling, or ask your provider for the IMAP and SMTP settings.",
                    "No encontramos los servidores de correo de esa dirección. Revisa cómo está escrita o pide a tu proveedor los datos de IMAP y SMTP.",
                )));
            }
            Err(t) => {
                w.step = WizardStep::Email;
                w.message = Some(Notice::error(t));
            }
        }
        None
    }

    /// Usa un descubrimiento ya hecho (p. ej. [`crate::onboarding::known_provider_discovery`]) y
    /// pasa a la tarjeta del proveedor.
    pub fn wizard_apply_discovery(&mut self, d: iuredesk_autoconfig::Discovery) {
        let w = self.wizard.get_or_insert_with(Wizard::default);
        let flow = crate::onboarding::flow_for(d.kind);
        w.oauth_ready = match &flow {
            Flow::OAuth(p) => crate::onboarding::oauth_available(*p),
            _ => false,
        };
        if w.email.trim().is_empty() {
            w.email = d.email.clone();
        }
        w.flow = Some(flow);
        w.discovery = Some(d);
        w.step = WizardStep::Provider;
        w.message = None;
    }

    /// Cambia a contraseña de app (cuando el OAuth no está disponible).
    pub fn wizard_use_app_password(&mut self) {
        if let Some(w) = self.wizard.as_mut() {
            w.flow = Some(Flow::AppPassword {
                help_url: match w.discovery.as_ref().map(|d| d.kind) {
                    Some(iuredesk_model::account::AccountKind::Gmail) => "https://myaccount.google.com/apppasswords",
                    _ => "https://account.microsoft.com/security",
                },
                steps: Text::new(
                    "1. Turn on two-step verification in your account.\n2. Create an app password named «IureDesk».\n3. Paste it below.",
                    "1. Activa la verificación en dos pasos en tu cuenta.\n2. Crea una contraseña de app llamada «IureDesk».\n3. Pégala aquí abajo.",
                ),
            });
            w.message = None;
        }
    }

    /// Prueba la conexión con la contraseña escrita y, si funciona, guarda la cuenta.
    pub fn wizard_test_and_save(&mut self) {
        let Some(w) = self.wizard.as_ref() else { return };
        let reauth = w.reauth.clone();
        let cand = w.discovery.as_ref().and_then(|d| d.best()).cloned();
        if cand.is_none() && reauth.is_none() {
            return;
        }
        let auth = match w.flow {
            Some(Flow::AppPassword { .. }) => AuthMethod::AppPassword,
            _ => AuthMethod::Password,
        };
        let aid = match w.reserved {
            Some(a) => a,
            None => match self.store.new_account_id() {
                Ok(a) => a,
                Err(e) => {
                    if let Some(w) = self.wizard.as_mut() {
                        w.message = Some(Notice::error(Text::new(
                            format!("Could not prepare the account: {e}"),
                            format!("No se pudo preparar la cuenta: {e}"),
                        )));
                    }
                    return;
                }
            },
        };
        let account = match (reauth, cand) {
            (Some(a), _) => a,
            (None, Some(cand)) => {
                crate::onboarding::build_account(new_account_key(aid), &w.name, w.email.trim(), &cand, auth)
            }
            (None, None) => return,
        };
        let password = w.password.clone();
        let secrets = Arc::clone(&self.secrets);
        let acc = account.clone();
        let job = self.bg.spawn(async move {
            let r = async {
                iuredesk_accounts::test_incoming(&acc.incoming, &password)
                    .await
                    .map_err(|e| crate::onboarding::probe_text(&e))?;
                iuredesk_accounts::test_outgoing(&acc.outgoing, &password)
                    .await
                    .map_err(|e| crate::onboarding::probe_text(&e))?;
                let (id, pw) = (acc.id.clone(), password.clone());
                tokio::task::spawn_blocking(move || secrets.set(&id, SecretPurpose::Incoming, &pw))
                    .await
                    .map_err(|e| Text::new(e.to_string(), e.to_string()))?
                    .map_err(|e| {
                        Text::new(
                            format!("The connection works, but the password could not be saved in the system keyring: {e}"),
                            format!("La conexión funciona, pero no se pudo guardar la contraseña en el llavero del sistema: {e}"),
                        )
                    })?;
                Ok(())
            }
            .await;
            JobOutput::Engine(EngineJob::Probe(r))
        });
        if let Some(w) = self.wizard.as_mut() {
            w.reserved = Some(aid);
            w.saved = Some(account);
            w.step = WizardStep::Working;
            w.message = None;
            w.job = Some(job);
        }
    }

    /// «Continuar con Google/Microsoft»: abre el navegador y espera el permiso.
    pub fn wizard_oauth(&mut self) {
        let Some(w) = self.wizard.as_ref() else { return };
        let Some(Flow::OAuth(provider)) = w.flow.clone() else { return };
        let reauth = w.reauth.clone();
        let cand = w.discovery.as_ref().and_then(|d| d.best()).cloned();
        let aid = match w.reserved.map(Ok).unwrap_or_else(|| self.store.new_account_id()) {
            Ok(a) => a,
            Err(_) => return,
        };
        let account = match (reauth, cand) {
            (Some(a), _) => a,
            (None, Some(cand)) => crate::onboarding::build_account(
                new_account_key(aid),
                &w.name,
                w.email.trim(),
                &cand,
                AuthMethod::OAuth2 { provider },
            ),
            (None, None) => return,
        };
        let hint = w.email.trim().to_string();
        let secrets = Arc::clone(&self.secrets);
        let cancel = iuredesk_oauth::CancellationToken::new();
        let token = cancel.clone();
        let id = account.id.clone();
        let job = self.bg.spawn(async move {
            let r = async {
                let settings = iuredesk_oauth::OAuthSettings::default();
                let cfg = match provider {
                    OAuthProvider::Google => settings.google(),
                    OAuthProvider::Microsoft => settings.microsoft(),
                }
                .map_err(|e| crate::onboarding::oauth_text(&e))?;
                let http = iuredesk_oauth::http_client().map_err(|e| crate::onboarding::oauth_text(&e))?;
                let pending = iuredesk_oauth::start_login(&cfg, Some(&hint))
                    .await
                    .map_err(|e| crate::onboarding::oauth_text(&e))?;
                pending.open_browser().map_err(|e| crate::onboarding::oauth_text(&e))?;
                let tokens = pending
                    .finish(&http, iuredesk_oauth::DEFAULT_LOGIN_TIMEOUT, &token)
                    .await
                    .map_err(|e| crate::onboarding::oauth_text(&e))?;
                iuredesk_oauth::TokenManager::new(http, secrets)
                    .store_login(&id, &tokens)
                    .await
                    .map_err(|e| crate::onboarding::oauth_text(&e))?;
                Ok(())
            }
            .await;
            JobOutput::Engine(EngineJob::OAuth(r))
        });
        if let Some(w) = self.wizard.as_mut() {
            w.reserved = Some(aid);
            w.saved = Some(account);
            w.cancel = Some(cancel);
            w.step = WizardStep::Working;
            w.message = None;
            w.job = Some(job);
        }
    }

    fn wizard_on_probe(&mut self, id: JobId, r: std::result::Result<(), Text>, oauth: bool) -> Option<Notice> {
        let w = self.wizard.as_mut().filter(|w| w.job == Some(id))?;
        w.job = None;
        w.cancel = None;
        match r {
            Ok(()) => {
                let account = w.saved.clone()?;
                let aid = w.reserved?;
                w.password.clear();
                match self.finish_account(&account, aid) {
                    Ok(()) => {
                        if let Some(w) = self.wizard.as_mut() {
                            w.step = WizardStep::Done;
                        }
                        Some(Notice::success(
                            format!("{} is ready", account.email),
                            format!("{} está lista", account.email),
                        ))
                    }
                    Err(e) => {
                        if let Some(w) = self.wizard.as_mut() {
                            w.step = WizardStep::Provider;
                            w.message = Some(Notice::error(e.text()));
                        }
                        None
                    }
                }
            }
            Err(t) => {
                w.step = WizardStep::Provider;
                w.message = Some(Notice::error(t));
                let _ = oauth;
                None
            }
        }
    }

    /// Guarda la cuenta (lista de cuentas + etiqueta y carpetas estándar en el almacén).
    fn finish_account(&mut self, account: &Account, aid: AccountId) -> Result<()> {
        if let Some(st) = &self.account_store {
            st.upsert(account.clone()).map_err(|e| EngineError::Io(e.to_string()))?;
        }
        // La sincronización usará este mismo id local (etiqueta y carpetas de abajo).
        iuredesk_sync::state::bind_account(&self.store, &account.id, aid)
            .map_err(|e| EngineError::Store(e.to_string()))?;
        self.store
            .put_account_label(&AccountLabel {
                id: aid,
                name: account.display_name.clone(),
                email: account.email.clone(),
            })
            .map_err(store_err)?;
        let es = crate::lang::spanish();
        for (role, en, esn, path) in [
            (FolderRole::Inbox, "Inbox", "Bandeja de entrada", "INBOX"),
            (FolderRole::Sent, "Sent", "Enviados", "Sent"),
            (FolderRole::Drafts, "Drafts", "Borradores", "Drafts"),
            (FolderRole::Archive, "Archive", "Archivo", "Archive"),
            (FolderRole::Junk, "Junk", "No deseado", "Junk"),
            (FolderRole::Trash, "Trash", "Papelera", "Trash"),
        ] {
            if self.role_folder(aid, role).is_none() {
                self.store
                    .put_folder(&Folder {
                        account: aid,
                        name: if es { esn.into() } else { en.into() },
                        remote_path: path.into(),
                        role,
                        ..Default::default()
                    })
                    .map_err(store_err)?;
            }
        }
        self.reload_accounts()?;
        self.rebuild_rows();
        // Empieza a sincronizar ya (y a poder enviar desde ella).
        if let Some(st) = self.live.status.get_mut(&aid) {
            *st = AccountSync::Starting;
        }
        self.restart_sync();
        self.refresh_transports();
        Ok(())
    }

    /// La cuenta que acaba de dar de alta el asistente (para mostrar su avance).
    pub fn wizard_account(&self) -> Option<AccountId> {
        self.wizard.as_ref().and_then(|w| w.reserved)
    }
}

/// Identificador único de una cuenta nueva (`acct<N>-<marca>`): el número es su id local y la
/// marca evita que dos carpetas de datos distintas (`IUREDESK_DATA_DIR`) compartan entradas del
/// llavero.
fn new_account_key(aid: AccountId) -> String {
    let nanos =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or_default();
    let mix = (nanos as u64) ^ u64::from(std::process::id()).rotate_left(32);
    format!("acct{}-{:08x}", aid.get(), mix & 0xffff_ffff)
}

/// Agrupa por conversación: la primera aparición (la más reciente) de cada hilo representa a la
/// conversación; `count` es el total de mensajes del hilo.
pub fn group_conversations(store: &Store, order: &[MessageId]) -> Vec<ListRow> {
    let mut seen: HashMap<u64, ()> = HashMap::new();
    let mut counts: HashMap<u64, u32> = HashMap::new();
    let mut out = Vec::with_capacity(order.len() / 2 + 1);
    for &id in order {
        let Ok(Some(env)) = store.get_envelope(id) else { continue };
        match env.thread {
            Some(t) => {
                if seen.insert(t.get(), ()).is_some() {
                    continue;
                }
                let n = *counts
                    .entry(t.get())
                    .or_insert_with(|| store.get_thread(t).ok().flatten().map_or(1, |th| th.message_count.max(1)));
                out.push(ListRow { id, count: n });
            }
            None => out.push(ListRow { id, count: 1 }),
        }
    }
    out
}

/// Orden de las carpetas en la barra lateral.
pub fn role_rank(r: FolderRole) -> u8 {
    match r {
        FolderRole::Inbox => 0,
        FolderRole::Drafts => 1,
        FolderRole::Sent => 2,
        FolderRole::Archive => 3,
        FolderRole::Junk => 4,
        FolderRole::Trash => 5,
        FolderRole::Custom => 6,
    }
}

/// Nombre de un favorito.
pub fn role_label(r: FolderRole) -> &'static str {
    match r {
        FolderRole::Inbox => crate::lang::pick("Inbox", "Entrada"),
        FolderRole::Sent => crate::lang::pick("Sent", "Enviados"),
        FolderRole::Drafts => crate::lang::pick("Drafts", "Borradores"),
        FolderRole::Trash => crate::lang::pick("Trash", "Papelera"),
        FolderRole::Junk => crate::lang::pick("Junk", "No deseado"),
        FolderRole::Archive => crate::lang::pick("Archive", "Archivo"),
        FolderRole::Custom => crate::lang::pick("Folder", "Carpeta"),
    }
}
