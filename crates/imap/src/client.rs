//! Cliente IMAP4rev1/IMAP4rev2 (RFC 3501 / 9051) con las extensiones que usa la sincronización.

use std::collections::BTreeSet;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use chrono::{DateTime, FixedOffset};
use iuredesk_model::account::{Security, ServerConfig};
use iuredesk_net::{ClientConfig, NetError};

use crate::codec::{Code, FetchData, ListEntry, MailboxStatus, Response, Status, Untagged, parse_response};
use crate::error::{ImapError, Result};
use crate::seqset::SeqSet;
use crate::transport::{Limits, Transport};

/// Tiempo por omisión de cada operación de red.
pub const DEFAULT_IO_TIMEOUT: Duration = Duration::from_secs(60);
/// Cada cuánto se reemite IDLE (RFC 2177 pide menos de 30 min).
pub const IDLE_REISSUE: Duration = Duration::from_secs(29 * 60);
/// Respuestas sin etiqueta máximas que se guardan por orden.
const MAX_UNTAGGED: usize = 500_000;
/// Avisos espontáneos que se guardan entre lecturas.
const MAX_UNSOLICITED: usize = 10_000;
/// UIDs máximos que se expanden de un resultado de búsqueda.
pub const MAX_SEARCH_RESULTS: usize = 5_000_000;
/// Rondas máximas de un intercambio SASL.
const MAX_SASL_ROUNDS: usize = 8;

/// Dónde y cómo conectar.
#[derive(Clone)]
pub struct ImapConfig {
    /// Servidor.
    pub host: String,
    /// Puerto.
    pub port: u16,
    /// TLS directo, STARTTLS o en claro.
    pub security: Security,
    /// Tiempo máximo de cada operación de red.
    pub io_timeout: Duration,
    /// Topes de lo que se acepta.
    pub limits: Limits,
    /// Configuración TLS propia (pruebas); `None` usa las raíces de Mozilla.
    pub tls: Option<Arc<ClientConfig>>,
}

impl std::fmt::Debug for ImapConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImapConfig")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("security", &self.security)
            .finish_non_exhaustive()
    }
}

impl ImapConfig {
    /// Configuración con valores por omisión.
    pub fn new(host: impl Into<String>, port: u16, security: Security) -> Self {
        Self { host: host.into(), port, security, io_timeout: DEFAULT_IO_TIMEOUT, limits: Limits::default(), tls: None }
    }

    /// Desde el servidor entrante de una cuenta.
    pub fn from_server(s: &ServerConfig) -> Self {
        Self::new(s.host.clone(), s.port, s.security)
    }
}

/// Capacidades anunciadas (en mayúsculas).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Capabilities(BTreeSet<String>);

impl Capabilities {
    /// Desde una lista.
    pub fn from_list(list: impl IntoIterator<Item = String>) -> Self {
        Self(list.into_iter().map(|c| c.to_ascii_uppercase()).collect())
    }

    /// ¿Se anuncia `cap`?
    pub fn has(&self, cap: &str) -> bool {
        self.0.contains(&cap.to_ascii_uppercase())
    }

    /// ¿Se anuncia `AUTH=<mech>`?
    pub fn auth(&self, mech: &str) -> bool {
        self.has(&format!("AUTH={mech}"))
    }

    /// Todas.
    pub fn iter(&self) -> impl Iterator<Item = &String> {
        self.0.iter()
    }
}

/// Estado del buzón seleccionado.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selected {
    /// Nombre (tal como se envió).
    pub name: String,
    /// Mensajes.
    pub exists: u32,
    /// `UIDVALIDITY`.
    pub uid_validity: Option<u32>,
    /// `UIDNEXT`.
    pub uid_next: Option<u32>,
    /// `HIGHESTMODSEQ`.
    pub highest_modseq: Option<u64>,
    /// `NOMODSEQ`.
    pub no_modseq: bool,
    /// Solo lectura.
    pub read_only: bool,
    /// `FLAGS`.
    pub flags: Vec<String>,
    /// `PERMANENTFLAGS`.
    pub permanent_flags: Vec<String>,
}

/// Parámetros QRESYNC de `SELECT` (RFC 7162).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Qresync {
    /// UIDVALIDITY conocido.
    pub uid_validity: u32,
    /// Último modseq conocido.
    pub modseq: u64,
    /// UIDs que tenemos (opcional; reduce el `VANISHED`).
    pub known_uids: Option<SeqSet>,
}

/// Opciones de `SELECT`/`EXAMINE`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SelectOptions {
    /// `EXAMINE` en vez de `SELECT`.
    pub read_only: bool,
    /// Pedir `CONDSTORE` (si el servidor lo tiene).
    pub condstore: bool,
    /// Resincronización rápida (si QRESYNC está habilitado).
    pub qresync: Option<Qresync>,
}

/// Resultado de `SELECT`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SelectResult {
    /// Estado del buzón.
    pub mailbox: Selected,
    /// UIDs borrados desde el modseq dado (`VANISHED (EARLIER)`).
    pub vanished: Vec<u32>,
    /// Mensajes cambiados desde el modseq dado.
    pub changed: Vec<FetchData>,
}

/// Resultado de `UID FETCH`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FetchResult {
    /// Datos por mensaje.
    pub messages: Vec<FetchData>,
    /// UIDs borrados (`VANISHED (EARLIER)`, con `CHANGEDSINCE … VANISHED`).
    pub vanished: Vec<u32>,
}

/// Cómo aplica `STORE` las banderas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreMode {
    /// `+FLAGS`.
    Add,
    /// `-FLAGS`.
    Remove,
    /// `FLAGS`.
    Replace,
}

/// Correspondencia de UIDs tras copiar o mover (`COPYUID`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CopyUid {
    /// UIDVALIDITY del destino.
    pub uid_validity: u32,
    /// `(uid de origen, uid en el destino)`.
    pub mapping: Vec<(u32, u32)>,
}

/// Aviso recibido durante IDLE.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IdleEvent {
    /// Hay `n` mensajes (llegó correo nuevo).
    Exists(u32),
    /// Se borró el mensaje con ese número de secuencia.
    Expunge(u32),
    /// Se borraron estos UIDs (QRESYNC).
    Vanished(SeqSet),
    /// Cambiaron las banderas de un mensaje.
    Fetch(Box<FetchData>),
    /// Cambió otro buzón (NOTIFY, RFC 5465): nombre tal como lo usa el servidor.
    MailboxStatus(String),
}

/// Cómo terminó un IDLE.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IdleOutcome {
    /// El servidor avisó de cambios.
    Changed(Vec<IdleEvent>),
    /// Se cumplió el plazo (reemitir).
    Timeout,
    /// Se pidió parar (`DONE` a petición).
    Stopped,
}

/// Un buzón de `LIST`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MailboxInfo {
    /// Nombre tal como lo usa el servidor (UTF-7 modificado); úsalo en `SELECT`.
    pub path: String,
    /// Nombre completo decodificado.
    pub name: String,
    /// Último segmento decodificado (para mostrar).
    pub leaf: String,
    /// Separador de jerarquía.
    pub delimiter: Option<char>,
    /// Atributos.
    pub attributes: Vec<String>,
}

impl MailboxInfo {
    fn has_attr(&self, a: &str) -> bool {
        self.attributes.iter().any(|x| x.eq_ignore_ascii_case(a))
    }

    /// ¿Se puede seleccionar?
    pub fn selectable(&self) -> bool {
        !self.has_attr("\\Noselect") && !self.has_attr("\\NonExistent")
    }

    /// Es la carpeta con todo el correo (`\\All`, «Todos» de Gmail).
    pub fn is_all_mail(&self) -> bool {
        self.has_attr("\\All")
    }

    /// Carpeta virtual que duplica otras (Gmail «Todos» y «Destacados»).
    pub fn is_virtual(&self) -> bool {
        self.has_attr("\\All") || self.has_attr("\\Flagged")
    }

    /// Función especial: atributos SPECIAL-USE (RFC 6154) y, si no hay, nombres habituales.
    pub fn role(&self) -> iuredesk_model::FolderRole {
        use iuredesk_model::FolderRole as R;
        if self.path.eq_ignore_ascii_case("INBOX") {
            return R::Inbox;
        }
        for (attr, role) in [
            ("\\Sent", R::Sent),
            ("\\Drafts", R::Drafts),
            ("\\Trash", R::Trash),
            ("\\Junk", R::Junk),
            ("\\Archive", R::Archive),
        ] {
            if self.has_attr(attr) {
                return role;
            }
        }
        if self.is_virtual() {
            return R::Custom;
        }
        let leaf = self.leaf.to_lowercase();
        match leaf.as_str() {
            "sent" | "sent items" | "sent messages" | "sent mail" | "enviados" | "elementos enviados"
            | "correo enviado" | "mensajes enviados" => R::Sent,
            "drafts" | "draft" | "borradores" | "borrador" => R::Drafts,
            "trash"
            | "deleted"
            | "deleted items"
            | "deleted messages"
            | "papelera"
            | "elementos eliminados"
            | "eliminados"
            | "bin" => R::Trash,
            "junk" | "spam" | "junk e-mail" | "junk email" | "bulk mail" | "correo no deseado" | "no deseado" => {
                R::Junk
            }
            "archive" | "archives" | "archivo" | "archivados" => R::Archive,
            _ => R::Custom,
        }
    }
}

/// Fragmento de una orden: texto o literal.
enum Part {
    Text(String),
    Literal(Vec<u8>),
}

/// Fin de una orden con éxito.
struct Done {
    code: Option<Code>,
    untagged: Vec<Untagged>,
}

/// Rechaza datos que romperían la orden.
fn check_text(what: &str, s: &str) -> Result<()> {
    if s.bytes().any(|b| matches!(b, b'\r' | b'\n' | 0)) {
        return Err(ImapError::InvalidInput(format!("{what} con saltos de línea o NUL")));
    }
    Ok(())
}

/// Nombre de buzón aceptable para `CREATE`/`RENAME`/`DELETE`: no vacío, acotado, sin saltos de
/// línea ni comodines de `LIST`.
fn check_mailbox_name(s: &str) -> Result<()> {
    check_text("buzón", s)?;
    if s.trim().is_empty() || s.len() > 1_000 || s.contains(['*', '%']) {
        return Err(ImapError::InvalidInput(format!("nombre de buzón no válido: {s}")));
    }
    Ok(())
}

/// `astring` de IMAP: entrecomillado si es ASCII imprimible; literal si no.
fn astring(s: &str) -> Part {
    if s.bytes().all(|b| (0x20..0x7f).contains(&b)) {
        let mut q = String::with_capacity(s.len().saturating_add(2));
        q.push('"');
        for c in s.chars() {
            if c == '"' || c == '\\' {
                q.push('\\');
            }
            q.push(c);
        }
        q.push('"');
        Part::Text(q)
    } else {
        Part::Literal(s.as_bytes().to_vec())
    }
}

/// Bandera válida como átomo (`\Seen`, `$Forwarded`, `Etiqueta`).
fn check_flag(f: &str) -> Result<()> {
    let ok = !f.is_empty()
        && f.len() <= 200
        && f.bytes().enumerate().all(|(i, b)| {
            b > 0x20
                && b < 0x7f
                && !matches!(b, b'(' | b')' | b'{' | b'"' | b'%' | b'*' | b']')
                && (b != b'\\' || i == 0)
        });
    if ok { Ok(()) } else { Err(ImapError::InvalidInput(format!("bandera no válida: {f}"))) }
}

/// Fecha de `APPEND`/`INTERNALDATE`.
pub fn format_internal_date(d: &DateTime<FixedOffset>) -> String {
    d.format("%d-%b-%Y %H:%M:%S %z").to_string()
}

/// Cliente IMAP sobre una conexión.
pub struct ImapClient {
    t: Transport,
    tag: u32,
    caps: Capabilities,
    cfg: ImapConfig,
    selected: Option<Selected>,
    enabled: BTreeSet<String>,
    authenticated: bool,
    greeting: String,
    unsolicited: Vec<Untagged>,
    bye: Option<String>,
}

impl std::fmt::Debug for ImapClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImapClient")
            .field("host", &self.cfg.host)
            .field("authenticated", &self.authenticated)
            .field("selected", &self.selected.as_ref().map(|s| &s.name))
            .finish_non_exhaustive()
    }
}

impl ImapClient {
    /// Conecta, lee el saludo, negocia STARTTLS si toca y lee las capacidades. Aún sin
    /// autenticar (salvo saludo `PREAUTH`).
    pub async fn connect(cfg: ImapConfig) -> Result<Self> {
        let tls = cfg.security == Security::Tls;
        let stream = iuredesk_net::connect_with(&cfg.host, cfg.port, tls, cfg.io_timeout, cfg.tls.clone()).await?;
        let t = Transport::new(stream, &cfg.host, cfg.limits);
        let mut c = Self {
            t,
            tag: 0,
            caps: Capabilities::default(),
            cfg,
            selected: None,
            enabled: BTreeSet::new(),
            authenticated: false,
            greeting: String::new(),
            unsolicited: Vec::new(),
            bye: None,
        };
        c.read_greeting().await?;
        if c.cfg.security == Security::StartTls {
            // Las capacidades del saludo bastan (RFC 3501 §7.2.1); si no vinieron, se piden.
            if c.caps.0.is_empty() {
                c.capability().await?;
            }
            if !c.caps.has("STARTTLS") {
                return Err(ImapError::StartTlsUnavailable);
            }
            c.exec("STARTTLS", vec![Part::Text("STARTTLS".into())]).await?;
            let Self { t, .. } = c;
            let (stream, buffered) = t.into_stream();
            let host = c.cfg.host.clone();
            let stream = iuredesk_net::starttls(stream, buffered, &host, c.cfg.io_timeout, c.cfg.tls.clone()).await?;
            c = Self { t: Transport::new(stream, &host, c.cfg.limits), caps: Capabilities::default(), ..c };
            c.capability().await?;
        } else if c.caps.0.is_empty() {
            c.capability().await?;
        }
        Ok(c)
    }

    async fn read_greeting(&mut self) -> Result<()> {
        let frame = self.t.read_frame(self.cfg.io_timeout).await?;
        match parse_response(&frame)? {
            Response::Untagged(Untagged::State { status, code, text }) => {
                self.greeting = text.clone();
                if let Some(Code::Capability(c)) = code {
                    self.caps = Capabilities::from_list(c);
                }
                match status {
                    Status::Ok => Ok(()),
                    Status::PreAuth => {
                        self.authenticated = true;
                        Ok(())
                    }
                    Status::Bye => Err(ImapError::Bye(text)),
                    _ => Err(ImapError::Protocol(format!("saludo inesperado: {text}"))),
                }
            }
            other => Err(ImapError::Protocol(format!("saludo inesperado: {other:?}"))),
        }
    }

    /// Texto del saludo.
    pub fn greeting(&self) -> &str {
        &self.greeting
    }

    /// Capacidades actuales.
    pub fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    /// Buzón seleccionado.
    pub fn selected(&self) -> Option<&Selected> {
        self.selected.as_ref()
    }

    /// ¿Se autenticó?
    pub fn is_authenticated(&self) -> bool {
        self.authenticated
    }

    /// ¿Está activo COMPRESS=DEFLATE?
    pub fn is_compressed(&self) -> bool {
        self.t.is_compressed()
    }

    /// ¿Se habilitó la extensión (`ENABLE`)?
    pub fn is_enabled(&self, ext: &str) -> bool {
        self.enabled.contains(&ext.to_ascii_uppercase())
    }

    /// Avisos espontáneos (EXISTS/EXPUNGE/VANISHED/FETCH) acumulados; los vacía.
    pub fn take_unsolicited(&mut self) -> Vec<Untagged> {
        std::mem::take(&mut self.unsolicited)
    }

    fn next_tag(&mut self) -> String {
        self.tag = self.tag.wrapping_add(1);
        format!("a{}", self.tag)
    }

    fn net_error(&self, e: ImapError) -> ImapError {
        match (&e, &self.bye) {
            (ImapError::Net(NetError::Closed), Some(b)) => ImapError::Bye(b.clone()),
            _ => e,
        }
    }

    async fn write(&mut self, data: &[u8]) -> Result<()> {
        let r = self.t.write_all(data, self.cfg.io_timeout).await;
        r.map_err(|e| self.net_error(e))
    }

    async fn read(&mut self) -> Result<Response> {
        let frame = match self.t.read_frame(self.cfg.io_timeout).await {
            Ok(f) => f,
            Err(e) => return Err(self.net_error(e)),
        };
        Ok(parse_response(&frame)?)
    }

    /// Actualiza el estado con una respuesta sin etiqueta.
    fn observe(&mut self, u: &Untagged) {
        match u {
            Untagged::Capability(c) => self.caps = Capabilities::from_list(c.iter().cloned()),
            Untagged::State { status: Status::Bye, text, .. } => self.bye = Some(text.clone()),
            Untagged::State { code: Some(Code::Capability(c)), .. } => {
                self.caps = Capabilities::from_list(c.iter().cloned());
            }
            Untagged::Enabled(list) => {
                for e in list {
                    self.enabled.insert(e.to_ascii_uppercase());
                }
            }
            Untagged::Exists(n) => {
                if let Some(s) = self.selected.as_mut() {
                    s.exists = *n;
                }
            }
            Untagged::Expunge(_) => {
                if let Some(s) = self.selected.as_mut() {
                    s.exists = s.exists.saturating_sub(1);
                }
            }
            Untagged::Vanished { earlier: false, uids } => {
                if let Some(s) = self.selected.as_mut() {
                    let n = u32::try_from(uids.len()).unwrap_or(u32::MAX);
                    s.exists = s.exists.saturating_sub(n);
                }
            }
            _ => {}
        }
    }

    /// Envía una orden (con literales si los hay) y espera su respuesta con etiqueta.
    async fn exec(&mut self, name: &str, parts: Vec<Part>) -> Result<Done> {
        let tag = self.next_tag();
        let mut line: Vec<u8> = format!("{tag} ").into_bytes();
        let nonsync_ok = self.caps.has("LITERAL+");
        let nonsync_small = self.caps.has("LITERAL-");
        let mut early: Vec<Untagged> = Vec::new();
        for part in parts {
            match part {
                Part::Text(t) => line.extend_from_slice(t.as_bytes()),
                Part::Literal(data) => {
                    if data.len() > self.cfg.limits.max_literal {
                        return Err(ImapError::TooLarge("literal"));
                    }
                    let nonsync = nonsync_ok || (nonsync_small && data.len() <= 4096);
                    if nonsync {
                        line.extend_from_slice(format!("{{{}+}}\r\n", data.len()).as_bytes());
                        line.extend_from_slice(&data);
                    } else {
                        line.extend_from_slice(format!("{{{}}}\r\n", data.len()).as_bytes());
                        self.write(&line).await?;
                        line.clear();
                        // Espera la continuación; si llega la respuesta con etiqueta, es un rechazo.
                        loop {
                            match self.read().await? {
                                Response::Continue(_) => break,
                                Response::Untagged(u) => {
                                    self.observe(&u);
                                    early.push(u);
                                }
                                Response::Tagged { tag: t, status, code, text } if t == tag => {
                                    return Err(self.status_error(name, status, code, text));
                                }
                                Response::Tagged { .. } => {}
                            }
                        }
                        line.extend_from_slice(&data);
                    }
                }
            }
        }
        line.extend_from_slice(b"\r\n");
        self.write(&line).await?;
        let mut done = self.collect(&tag, name).await?;
        if !early.is_empty() {
            early.append(&mut done.untagged);
            done.untagged = early;
        }
        Ok(done)
    }

    fn status_error(&self, name: &str, status: Status, code: Option<Code>, text: String) -> ImapError {
        match status {
            Status::No => ImapError::No { command: name.to_string(), code, text },
            Status::Bye => ImapError::Bye(text),
            _ => ImapError::Bad { command: name.to_string(), text },
        }
    }

    async fn collect(&mut self, tag: &str, name: &str) -> Result<Done> {
        let mut untagged = Vec::new();
        loop {
            match self.read().await? {
                Response::Tagged { tag: t, status, code, text } if t == tag => {
                    if let Some(Code::Capability(c)) = &code {
                        self.caps = Capabilities::from_list(c.iter().cloned());
                    }
                    return match status {
                        Status::Ok => Ok(Done { code, untagged }),
                        _ => Err(self.status_error(name, status, code, text)),
                    };
                }
                Response::Tagged { .. } => {}
                Response::Untagged(u) => {
                    self.observe(&u);
                    if untagged.len() < MAX_UNTAGGED {
                        untagged.push(u);
                    }
                }
                Response::Continue(t) => {
                    return Err(ImapError::Protocol(format!("continuación inesperada en {name}: {t}")));
                }
            }
        }
    }

    /// Guarda los avisos que no son respuesta a la orden (EXISTS, EXPUNGE, VANISHED).
    fn keep_unsolicited(&mut self, list: &[Untagged]) {
        for u in list {
            if matches!(u, Untagged::Exists(_) | Untagged::Expunge(_) | Untagged::Vanished { earlier: false, .. })
                && self.unsolicited.len() < MAX_UNSOLICITED
            {
                self.unsolicited.push(u.clone());
            }
        }
    }

    // ------------------------------------------------------------------ sesión

    /// `CAPABILITY`.
    pub async fn capability(&mut self) -> Result<&Capabilities> {
        let done = self.exec("CAPABILITY", vec![Part::Text("CAPABILITY".into())]).await?;
        for u in &done.untagged {
            if let Untagged::Capability(c) = u {
                self.caps = Capabilities::from_list(c.iter().cloned());
            }
        }
        Ok(&self.caps)
    }

    /// `LOGIN usuario contraseña`.
    pub async fn login(&mut self, user: &str, password: &str) -> Result<()> {
        check_text("usuario", user)?;
        check_text("contraseña", password)?;
        if self.caps.has("LOGINDISABLED") {
            return Err(ImapError::Unsupported("LOGIN (deshabilitado sin TLS)".into()));
        }
        let parts = vec![Part::Text("LOGIN ".into()), astring(user), Part::Text(" ".into()), astring(password)];
        match self.exec("LOGIN", parts).await {
            Ok(done) => self.after_auth(done.code).await,
            Err(ImapError::No { code, text, .. }) => Err(ImapError::AuthFailed { text, code, sasl: None }),
            Err(ImapError::Bad { text, .. }) => Err(ImapError::AuthFailed { text, code: None, sasl: None }),
            Err(e) => Err(e),
        }
    }

    /// `AUTHENTICATE PLAIN`.
    pub async fn authenticate_plain(&mut self, user: &str, password: &str) -> Result<()> {
        check_text("usuario", user)?;
        check_text("contraseña", password)?;
        let ir = base64::engine::general_purpose::STANDARD.encode(format!("\0{user}\0{password}"));
        self.authenticate("PLAIN", &ir).await
    }

    /// `AUTHENTICATE <mecanismo>` con la respuesta inicial ya en base64 (p. ej. de
    /// `iuredesk_oauth::sasl::xoauth2`). Si el servidor manda un reto de error (XOAUTH2 /
    /// OAUTHBEARER) se contesta como pide cada mecanismo y el reto queda en el error.
    pub async fn authenticate(&mut self, mechanism: &str, initial_b64: &str) -> Result<()> {
        check_text("mecanismo", mechanism)?;
        check_text("respuesta SASL", initial_b64)?;
        if mechanism.bytes().any(|b| !(b.is_ascii_alphanumeric() || b == b'-' || b == b'_')) {
            return Err(ImapError::InvalidInput("mecanismo SASL no válido".into()));
        }
        let mech = mechanism.to_ascii_uppercase();
        let tag = self.next_tag();
        let sasl_ir = self.caps.has("SASL-IR");
        let first = if sasl_ir {
            let ir = if initial_b64.is_empty() { "=" } else { initial_b64 };
            format!("{tag} AUTHENTICATE {mech} {ir}\r\n")
        } else {
            format!("{tag} AUTHENTICATE {mech}\r\n")
        };
        self.write(first.as_bytes()).await?;
        let mut sent_initial = sasl_ir;
        let mut challenge: Option<String> = None;
        for _ in 0..MAX_SASL_ROUNDS {
            match self.read().await? {
                Response::Continue(text) => {
                    let reply = if !sent_initial {
                        sent_initial = true;
                        initial_b64.to_string()
                    } else {
                        challenge = Some(text);
                        match mech.as_str() {
                            "XOAUTH2" => String::new(),
                            "OAUTHBEARER" => "AQ==".to_string(),
                            _ => "*".to_string(),
                        }
                    };
                    self.write(format!("{reply}\r\n").as_bytes()).await?;
                }
                Response::Untagged(u) => self.observe(&u),
                Response::Tagged { tag: t, status, code, text } if t == tag => {
                    if status == Status::Ok {
                        return self.after_auth(code).await;
                    }
                    if status == Status::Bye {
                        return Err(ImapError::Bye(text));
                    }
                    return Err(ImapError::AuthFailed { text, code, sasl: challenge });
                }
                Response::Tagged { .. } => {}
            }
        }
        Err(ImapError::Protocol("intercambio SASL demasiado largo".into()))
    }

    async fn after_auth(&mut self, code: Option<Code>) -> Result<()> {
        self.authenticated = true;
        match code {
            Some(Code::Capability(c)) => self.caps = Capabilities::from_list(c),
            _ => {
                self.capability().await?;
            }
        }
        Ok(())
    }

    /// `ID` (RFC 2971): algunos servidores lo exigen. La respuesta se ignora.
    pub async fn id(&mut self, fields: &[(&str, &str)]) -> Result<()> {
        if !self.caps.has("ID") {
            return Ok(());
        }
        let mut parts = vec![Part::Text("ID (".into())];
        for (i, (k, v)) in fields.iter().enumerate().take(30) {
            check_text("ID", k)?;
            check_text("ID", v)?;
            if i > 0 {
                parts.push(Part::Text(" ".into()));
            }
            parts.push(astring(k));
            parts.push(Part::Text(" ".into()));
            parts.push(astring(v));
        }
        parts.push(Part::Text(")".into()));
        self.exec("ID", parts).await.map(|_| ())
    }

    /// `ENABLE` (RFC 5161). Devuelve las extensiones que quedaron habilitadas.
    pub async fn enable(&mut self, exts: &[&str]) -> Result<Vec<String>> {
        if !self.caps.has("ENABLE") {
            return Ok(Vec::new());
        }
        let wanted: Vec<&str> = exts.iter().copied().filter(|e| self.caps.has(e)).collect();
        if wanted.is_empty() {
            return Ok(Vec::new());
        }
        for e in &wanted {
            check_flag(e)?;
        }
        let done = self.exec("ENABLE", vec![Part::Text(format!("ENABLE {}", wanted.join(" ")))]).await?;
        let mut out = Vec::new();
        for u in done.untagged {
            if let Untagged::Enabled(l) = u {
                out.extend(l);
            }
        }
        Ok(out)
    }

    /// `COMPRESS DEFLATE` (RFC 4978). `false` si el servidor no lo ofrece.
    pub async fn compress(&mut self) -> Result<bool> {
        if self.t.is_compressed() || !self.caps.has("COMPRESS=DEFLATE") {
            return Ok(false);
        }
        self.exec("COMPRESS", vec![Part::Text("COMPRESS DEFLATE".into())]).await?;
        self.t.enable_deflate()?;
        Ok(true)
    }

    /// `NOOP`: devuelve los avisos recibidos (también quedan en `take_unsolicited`).
    pub async fn noop(&mut self) -> Result<Vec<Untagged>> {
        let done = self.exec("NOOP", vec![Part::Text("NOOP".into())]).await?;
        self.keep_unsolicited(&done.untagged);
        Ok(done.untagged)
    }

    /// `LOGOUT` (los fallos se ignoran: la conexión se cierra igual).
    pub async fn logout(mut self) {
        let _ = self.exec("LOGOUT", vec![Part::Text("LOGOUT".into())]).await;
    }

    // ------------------------------------------------------------------ buzones

    /// `LIST "" "*"` (con `RETURN (SPECIAL-USE)` si se puede).
    pub async fn list(&mut self) -> Result<Vec<MailboxInfo>> {
        let cmd = if self.caps.has("LIST-EXTENDED") && self.caps.has("SPECIAL-USE") {
            r#"LIST "" "*" RETURN (SPECIAL-USE)"#
        } else {
            r#"LIST "" "*""#
        };
        let done = self.exec("LIST", vec![Part::Text(cmd.into())]).await?;
        let mut out = Vec::new();
        for u in done.untagged {
            if let Untagged::List(l) = u {
                out.push(mailbox_info(l));
            }
        }
        Ok(out)
    }

    /// `STATUS`.
    pub async fn status(&mut self, mailbox: &str, items: &[&str]) -> Result<MailboxStatus> {
        check_text("buzón", mailbox)?;
        for i in items {
            check_flag(i)?;
        }
        let parts = vec![Part::Text("STATUS ".into()), astring(mailbox), Part::Text(format!(" ({})", items.join(" ")))];
        let done = self.exec("STATUS", parts).await?;
        for u in done.untagged {
            if let Untagged::Status(s) = u {
                return Ok(s);
            }
        }
        Err(ImapError::Protocol("STATUS sin respuesta".into()))
    }

    /// `SELECT` / `EXAMINE` (con CONDSTORE o QRESYNC si se piden y el servidor los tiene).
    pub async fn select(&mut self, mailbox: &str, opts: &SelectOptions) -> Result<SelectResult> {
        check_text("buzón", mailbox)?;
        let verb = if opts.read_only { "EXAMINE" } else { "SELECT" };
        let mut tail = String::new();
        match &opts.qresync {
            Some(q) if self.is_enabled("QRESYNC") => {
                tail.push_str(&format!(" (QRESYNC ({} {}", q.uid_validity, q.modseq.max(1)));
                if let Some(k) = q.known_uids.as_ref().filter(|k| !k.is_empty()) {
                    tail.push_str(&format!(" {k}"));
                }
                tail.push_str("))");
            }
            _ if (opts.condstore || opts.qresync.is_some()) && self.caps.has("CONDSTORE") => {
                tail.push_str(" (CONDSTORE)");
            }
            _ => {}
        }
        self.selected = None;
        let parts = vec![Part::Text(format!("{verb} ")), astring(mailbox), Part::Text(tail)];
        let done = self.exec(verb, parts).await?;
        let mut res = SelectResult {
            mailbox: Selected { name: mailbox.to_string(), read_only: opts.read_only, ..Default::default() },
            ..Default::default()
        };
        let m = &mut res.mailbox;
        for u in done.untagged {
            match u {
                Untagged::Exists(n) => m.exists = n,
                Untagged::Flags(f) => m.flags = f,
                Untagged::State { code: Some(code), .. } => match code {
                    Code::UidValidity(v) => m.uid_validity = Some(v),
                    Code::UidNext(v) => m.uid_next = Some(v),
                    Code::HighestModSeq(v) => m.highest_modseq = Some(v),
                    Code::NoModSeq => m.no_modseq = true,
                    Code::PermanentFlags(f) => m.permanent_flags = f,
                    _ => {}
                },
                Untagged::Vanished { uids, .. } => {
                    let room = MAX_SEARCH_RESULTS.saturating_sub(res.vanished.len());
                    res.vanished.extend(uids.to_vec(0, room));
                }
                Untagged::Fetch(f) if res.changed.len() < MAX_UNTAGGED => res.changed.push(*f),
                _ => {}
            }
        }
        match done.code {
            Some(Code::ReadOnly) => m.read_only = true,
            Some(Code::ReadWrite) => m.read_only = false,
            _ => {}
        }
        self.selected = Some(m.clone());
        Ok(res)
    }

    /// `UNSELECT` (o `CLOSE` sin expunge si no existe: se usa EXAMINE antes). Ignora fallos.
    pub async fn unselect(&mut self) -> Result<()> {
        if self.selected.is_none() {
            return Ok(());
        }
        if self.caps.has("UNSELECT") {
            self.exec("UNSELECT", vec![Part::Text("UNSELECT".into())]).await?;
        } else {
            self.exec("CLOSE", vec![Part::Text("CLOSE".into())]).await?;
        }
        self.selected = None;
        Ok(())
    }

    // ------------------------------------------------------------------ buzones (F7.1)

    /// `CREATE` (nombre ya en UTF-7 modificado: [`crate::mutf7::encode`]). Un buzón que ya
    /// existe (`[ALREADYEXISTS]`) no es un error: lo que se quería ya está.
    pub async fn create(&mut self, mailbox: &str) -> Result<()> {
        check_mailbox_name(mailbox)?;
        match self.exec("CREATE", vec![Part::Text("CREATE ".into()), astring(mailbox)]).await {
            Ok(_) => Ok(()),
            Err(ImapError::No { code: Some(Code::Other(c, _)), .. }) if c.eq_ignore_ascii_case("ALREADYEXISTS") => {
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// `SUBSCRIBE` (para que otros clientes que solo muestran lo suscrito vean el buzón).
    pub async fn subscribe(&mut self, mailbox: &str) -> Result<()> {
        check_mailbox_name(mailbox)?;
        self.exec("SUBSCRIBE", vec![Part::Text("SUBSCRIBE ".into()), astring(mailbox)]).await?;
        Ok(())
    }

    /// `RENAME viejo nuevo`.
    pub async fn rename(&mut self, from: &str, to: &str) -> Result<()> {
        check_mailbox_name(from)?;
        check_mailbox_name(to)?;
        if self.selected.as_ref().is_some_and(|s| s.name == from) {
            self.unselect().await?;
        }
        self.exec("RENAME", vec![Part::Text("RENAME ".into()), astring(from), Part::Text(" ".into()), astring(to)])
            .await?;
        Ok(())
    }

    /// `DELETE`. Un buzón que ya no existe (`[NONEXISTENT]`) no es un error.
    pub async fn delete_mailbox(&mut self, mailbox: &str) -> Result<()> {
        check_mailbox_name(mailbox)?;
        if mailbox.eq_ignore_ascii_case("INBOX") {
            return Err(ImapError::InvalidInput("no se puede borrar INBOX".into()));
        }
        if self.selected.as_ref().is_some_and(|s| s.name == mailbox) {
            self.unselect().await?;
        }
        match self.exec("DELETE", vec![Part::Text("DELETE ".into()), astring(mailbox)]).await {
            Ok(_) => Ok(()),
            Err(ImapError::No { code: Some(Code::Other(c, _)), .. }) if c.eq_ignore_ascii_case("NONEXISTENT") => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn require_selected(&self) -> Result<()> {
        if self.selected.is_some() { Ok(()) } else { Err(ImapError::NoMailbox) }
    }

    // ------------------------------------------------------------------ mensajes

    /// `UID FETCH set items [(CHANGEDSINCE m [VANISHED])]`. `items` es la lista entre paréntesis
    /// (ver [`crate::fetch_items`]).
    pub async fn uid_fetch(
        &mut self,
        set: &SeqSet,
        items: &str,
        changed_since: Option<u64>,
        vanished: bool,
    ) -> Result<FetchResult> {
        self.require_selected()?;
        check_text("FETCH", items)?;
        if set.is_empty() {
            return Ok(FetchResult::default());
        }
        let mut cmd = format!("UID FETCH {set} {items}");
        if let Some(m) = changed_since {
            if vanished && self.is_enabled("QRESYNC") {
                cmd.push_str(&format!(" (CHANGEDSINCE {m} VANISHED)"));
            } else {
                cmd.push_str(&format!(" (CHANGEDSINCE {m})"));
            }
        }
        let done = self.exec("UID FETCH", vec![Part::Text(cmd)]).await?;
        let mut res = FetchResult::default();
        let mut rest = Vec::new();
        for u in done.untagged {
            match u {
                Untagged::Fetch(f) if f.uid.is_some() => res.messages.push(*f),
                Untagged::Vanished { earlier: true, uids } => {
                    let room = MAX_SEARCH_RESULTS.saturating_sub(res.vanished.len());
                    res.vanished.extend(uids.to_vec(0, room));
                }
                other => rest.push(other),
            }
        }
        self.keep_unsolicited(&rest);
        Ok(res)
    }

    /// `UID STORE set [(UNCHANGEDSINCE m)] ±FLAGS[.SILENT] (flags)`. Devuelve los `FETCH`
    /// recibidos (vacío si `silent`) y los UIDs no cambiados por conflicto (`MODIFIED`).
    pub async fn uid_store(
        &mut self,
        set: &SeqSet,
        mode: StoreMode,
        flags: &[String],
        silent: bool,
        unchanged_since: Option<u64>,
    ) -> Result<(Vec<FetchData>, Option<SeqSet>)> {
        self.require_selected()?;
        for f in flags {
            check_flag(f)?;
        }
        if set.is_empty() {
            return Ok((Vec::new(), None));
        }
        let op = match mode {
            StoreMode::Add => "+FLAGS",
            StoreMode::Remove => "-FLAGS",
            StoreMode::Replace => "FLAGS",
        };
        let unch = match unchanged_since {
            Some(m) if self.caps.has("CONDSTORE") => format!(" (UNCHANGEDSINCE {m})"),
            _ => String::new(),
        };
        let cmd = format!("UID STORE {set}{unch} {op}{} ({})", if silent { ".SILENT" } else { "" }, flags.join(" "));
        let done = self.exec("UID STORE", vec![Part::Text(cmd)]).await?;
        let modified = match done.code {
            Some(Code::Modified(s)) => Some(s),
            _ => None,
        };
        let mut out = Vec::new();
        let mut rest = Vec::new();
        for u in done.untagged {
            match u {
                Untagged::Fetch(f) => out.push(*f),
                other => rest.push(other),
            }
        }
        self.keep_unsolicited(&rest);
        Ok((out, modified))
    }

    fn copyuid(code: Option<&Code>, untagged: &[Untagged]) -> Option<CopyUid> {
        let from_untagged = untagged.iter().find_map(|u| match u {
            Untagged::State { code: Some(c @ Code::CopyUid { .. }), .. } => Some(c),
            _ => None,
        });
        match code.filter(|c| matches!(c, Code::CopyUid { .. })).or(from_untagged)? {
            Code::CopyUid { uidvalidity, source, dest } => {
                let src = source.to_vec(0, MAX_SEARCH_RESULTS);
                let dst = dest.to_vec(0, MAX_SEARCH_RESULTS);
                Some(CopyUid { uid_validity: *uidvalidity, mapping: src.into_iter().zip(dst).collect() })
            }
            _ => None,
        }
    }

    /// `UID COPY`.
    pub async fn uid_copy(&mut self, set: &SeqSet, dest: &str) -> Result<Option<CopyUid>> {
        self.require_selected()?;
        check_text("buzón", dest)?;
        let parts = vec![Part::Text(format!("UID COPY {set} ")), astring(dest)];
        let done = self.exec("UID COPY", parts).await?;
        self.keep_unsolicited(&done.untagged);
        Ok(Self::copyuid(done.code.as_ref(), &done.untagged))
    }

    /// `UID MOVE` (RFC 6851); sin MOVE: `UID COPY` + `\Deleted` + `UID EXPUNGE` (o `EXPUNGE` si
    /// no hay UIDPLUS).
    pub async fn uid_move(&mut self, set: &SeqSet, dest: &str) -> Result<Option<CopyUid>> {
        self.require_selected()?;
        check_text("buzón", dest)?;
        if self.caps.has("MOVE") {
            let parts = vec![Part::Text(format!("UID MOVE {set} ")), astring(dest)];
            let done = self.exec("UID MOVE", parts).await?;
            self.keep_unsolicited(&done.untagged);
            return Ok(Self::copyuid(done.code.as_ref(), &done.untagged));
        }
        let map = self.uid_copy(set, dest).await?;
        self.uid_store(set, StoreMode::Add, &["\\Deleted".to_string()], true, None).await?;
        self.uid_expunge(set).await?;
        Ok(map)
    }

    /// `UID EXPUNGE` (UIDPLUS); sin UIDPLUS, `EXPUNGE` (borra todo lo marcado `\Deleted`).
    pub async fn uid_expunge(&mut self, set: &SeqSet) -> Result<()> {
        self.require_selected()?;
        let cmd = if self.caps.has("UIDPLUS") { format!("UID EXPUNGE {set}") } else { "EXPUNGE".to_string() };
        let done = self.exec("EXPUNGE", vec![Part::Text(cmd)]).await?;
        self.keep_unsolicited(&done.untagged);
        Ok(())
    }

    /// `APPEND buzón [(banderas)] [fecha] {literal}`. Devuelve `(UIDVALIDITY, UID)` si el servidor
    /// tiene UIDPLUS.
    pub async fn append(
        &mut self,
        mailbox: &str,
        flags: &[String],
        date: Option<DateTime<FixedOffset>>,
        message: &[u8],
    ) -> Result<Option<(u32, u32)>> {
        check_text("buzón", mailbox)?;
        for f in flags {
            check_flag(f)?;
        }
        if message.contains(&0) {
            return Err(ImapError::InvalidInput("mensaje con NUL".into()));
        }
        let mut parts = vec![Part::Text("APPEND ".into()), astring(mailbox)];
        if !flags.is_empty() {
            parts.push(Part::Text(format!(" ({})", flags.join(" "))));
        }
        if let Some(d) = date {
            parts.push(Part::Text(format!(" \"{}\"", format_internal_date(&d))));
        }
        parts.push(Part::Text(" ".into()));
        parts.push(Part::Literal(message.to_vec()));
        let done = self.exec("APPEND", parts).await?;
        self.keep_unsolicited(&done.untagged);
        Ok(match done.code {
            Some(Code::AppendUid { uidvalidity, uids }) => uids.to_vec(0, 1).first().map(|u| (uidvalidity, *u)),
            _ => None,
        })
    }

    /// `UID STORE set ±X-GM-LABELS.SILENT (etiquetas)` (Gmail, `X-GM-EXT-1`). Las etiquetas del
    /// sistema (`\\Inbox`, `\\Starred`…) van como átomo; las demás, entrecomilladas o como literal.
    pub async fn uid_store_gm_labels(&mut self, set: &SeqSet, mode: StoreMode, labels: &[String]) -> Result<()> {
        self.require_selected()?;
        if !self.caps.has("X-GM-EXT-1") {
            return Err(ImapError::Unsupported("X-GM-EXT-1".into()));
        }
        if set.is_empty() || labels.is_empty() {
            return Ok(());
        }
        let op = match mode {
            StoreMode::Add => "+X-GM-LABELS.SILENT",
            StoreMode::Remove => "-X-GM-LABELS.SILENT",
            StoreMode::Replace => "X-GM-LABELS.SILENT",
        };
        let mut parts = vec![Part::Text(format!("UID STORE {set} {op} ("))];
        for (i, l) in labels.iter().take(500).enumerate() {
            check_text("etiqueta", l)?;
            if l.is_empty() || l.len() > 1_000 {
                return Err(ImapError::InvalidInput("etiqueta vacía o demasiado larga".into()));
            }
            if i > 0 {
                parts.push(Part::Text(" ".into()));
            }
            if l.starts_with('\\') {
                check_flag(l)?;
                parts.push(Part::Text(l.clone()));
            } else {
                parts.push(astring(l));
            }
        }
        parts.push(Part::Text(")".into()));
        let done = self.exec("UID STORE", parts).await?;
        self.keep_unsolicited(&done.untagged);
        Ok(())
    }

    /// `NOTIFY SET` (RFC 5465): avisos de mensajes nuevos, borrados y banderas del buzón
    /// seleccionado y de los demás buzones personales. Después, [`ImapClient::idle`] devuelve
    /// [`IdleEvent::MailboxStatus`] cuando cambia otro buzón.
    pub async fn notify_personal(&mut self) -> Result<()> {
        if !self.caps.has("NOTIFY") {
            return Err(ImapError::Unsupported("NOTIFY".into()));
        }
        let cmd = "NOTIFY SET (selected (MessageNew MessageExpunge FlagChange)) (personal (MessageNew MessageExpunge))";
        let done = self.exec("NOTIFY", vec![Part::Text(cmd.into())]).await?;
        self.keep_unsolicited(&done.untagged);
        Ok(())
    }

    /// `UID SEARCH` (con `RETURN (ALL)` si hay ESEARCH). `criteria` es texto IMAP (`SINCE
    /// 1-Jan-2026`, `UID 1:500`, `ALL`).
    pub async fn uid_search(&mut self, criteria: &str) -> Result<Vec<u32>> {
        self.require_selected()?;
        check_text("SEARCH", criteria)?;
        let esearch = self.caps.has("ESEARCH");
        let cmd =
            if esearch { format!("UID SEARCH RETURN (ALL) {criteria}") } else { format!("UID SEARCH {criteria}") };
        let done = self.exec("UID SEARCH", vec![Part::Text(cmd)]).await?;
        let mut out = Vec::new();
        let mut rest = Vec::new();
        for u in done.untagged {
            match u {
                Untagged::ESearch(e) => {
                    if let Some(all) = e.all {
                        let room = MAX_SEARCH_RESULTS.saturating_sub(out.len());
                        out.extend(all.to_vec(0, room));
                    }
                }
                Untagged::Search(nums, _) => {
                    let room = MAX_SEARCH_RESULTS.saturating_sub(out.len());
                    out.extend(nums.into_iter().take(room));
                }
                other => rest.push(other),
            }
        }
        self.keep_unsolicited(&rest);
        out.sort_unstable();
        out.dedup();
        Ok(out)
    }

    /// `IDLE` (RFC 2177) hasta que llegue un aviso, pase `max` o termine `stop`; después `DONE`.
    /// Llamar en bucle con [`IDLE_REISSUE`] reemite IDLE cada 29 minutos.
    pub async fn idle(&mut self, max: Duration, stop: impl Future<Output = ()>) -> Result<IdleOutcome> {
        self.require_selected()?;
        if !self.caps.has("IDLE") {
            return Err(ImapError::Unsupported("IDLE".into()));
        }
        let tag = self.next_tag();
        self.write(format!("{tag} IDLE\r\n").as_bytes()).await?;
        let mut events = Vec::new();
        loop {
            match self.read().await? {
                Response::Continue(_) => break,
                Response::Untagged(u) => {
                    self.observe(&u);
                    push_idle_event(&mut events, u);
                }
                Response::Tagged { tag: t, status, code, text } if t == tag => {
                    return Err(self.status_error("IDLE", status, code, text));
                }
                Response::Tagged { .. } => {}
            }
        }
        let deadline = tokio::time::Instant::now() + max;
        tokio::pin!(stop);
        let mut ended_by_server = false;
        let mut quiet = IdleOutcome::Timeout;
        'wait: loop {
            while let Some(frame) = self.t.try_frame().map_err(|e| self.net_error(e))? {
                match parse_response(&frame)? {
                    Response::Untagged(u) => {
                        self.observe(&u);
                        push_idle_event(&mut events, u);
                    }
                    Response::Tagged { tag: t, .. } if t == tag => {
                        ended_by_server = true;
                        break 'wait;
                    }
                    _ => {}
                }
            }
            if !events.is_empty() {
                break;
            }
            tokio::select! {
                r = self.t.fill() => {
                    if let Err(e) = r {
                        return Err(self.net_error(e));
                    }
                }
                _ = tokio::time::sleep_until(deadline) => break,
                _ = &mut stop => {
                    quiet = IdleOutcome::Stopped;
                    break;
                }
            }
        }
        if !ended_by_server {
            self.write(b"DONE\r\n").await?;
            loop {
                match self.read().await? {
                    Response::Untagged(u) => {
                        self.observe(&u);
                        push_idle_event(&mut events, u);
                    }
                    Response::Tagged { tag: t, status, code, text } if t == tag => {
                        if status != Status::Ok {
                            return Err(self.status_error("IDLE", status, code, text));
                        }
                        break;
                    }
                    _ => {}
                }
            }
        }
        Ok(if events.is_empty() { quiet } else { IdleOutcome::Changed(events) })
    }
}

fn push_idle_event(events: &mut Vec<IdleEvent>, u: Untagged) {
    if events.len() >= MAX_UNSOLICITED {
        return;
    }
    match u {
        Untagged::Exists(n) => events.push(IdleEvent::Exists(n)),
        Untagged::Expunge(n) => events.push(IdleEvent::Expunge(n)),
        Untagged::Vanished { uids, .. } => events.push(IdleEvent::Vanished(uids)),
        Untagged::Fetch(f) => events.push(IdleEvent::Fetch(f)),
        Untagged::Status(st) => {
            events.push(IdleEvent::MailboxStatus(String::from_utf8_lossy(&st.raw_name).into_owned()))
        }
        _ => {}
    }
}

fn mailbox_info(l: ListEntry) -> MailboxInfo {
    let path = String::from_utf8_lossy(&l.raw_name).into_owned();
    let name = crate::mutf7::decode(&l.raw_name);
    let leaf = match l.delimiter {
        Some(d) => name.rsplit(d).next().unwrap_or(&name).to_string(),
        None => name.clone(),
    };
    MailboxInfo { path, name, leaf, delimiter: l.delimiter, attributes: l.attributes }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iuredesk_model::FolderRole;

    #[test]
    fn astring_quotes_or_literal() {
        assert!(matches!(astring(r#"a"b\c"#), Part::Text(t) if t == r#""a\"b\\c""#));
        assert!(matches!(astring("contraseña"), Part::Literal(_)));
        assert!(check_text("x", "a\r\nb").is_err());
        assert!(check_flag("\\Seen").is_ok());
        assert!(check_flag("$Forwarded").is_ok());
        assert!(check_flag("a b").is_err());
        assert!(check_flag("x)").is_err());
        assert!(check_flag("a\\b").is_err());
    }

    #[test]
    fn roles() {
        let mb = |path: &str, attrs: &[&str]| {
            mailbox_info(ListEntry {
                attributes: attrs.iter().map(|s| s.to_string()).collect(),
                delimiter: Some('/'),
                raw_name: path.as_bytes().to_vec(),
            })
        };
        assert_eq!(mb("INBOX", &[]).role(), FolderRole::Inbox);
        assert_eq!(mb("[Gmail]/Enviados", &["\\Sent"]).role(), FolderRole::Sent);
        assert_eq!(mb("Elementos enviados", &[]).role(), FolderRole::Sent);
        assert_eq!(mb("INBOX/Papelera", &[]).role(), FolderRole::Trash);
        assert_eq!(mb("[Gmail]/Todos", &["\\All"]).role(), FolderRole::Custom);
        assert!(mb("[Gmail]/Todos", &["\\All"]).is_virtual());
        assert!(!mb("[Gmail]", &["\\Noselect"]).selectable());
        assert_eq!(mb("Entw&APw-rfe", &["\\Drafts"]).leaf, "Entwürfe");
    }
}
