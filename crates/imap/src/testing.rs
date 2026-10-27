//! Servidor IMAP falso y programable (solo con la *feature* `test-server`).
//!
//! Lo bastante completo para probar la sincronización sin red: LOGIN, AUTHENTICATE PLAIN/XOAUTH2,
//! CAPABILITY, ENABLE, ID, COMPRESS, LIST (SPECIAL-USE), STATUS, SELECT/EXAMINE con CONDSTORE y
//! QRESYNC (`VANISHED (EARLIER)` + cambios), UID FETCH (ENVELOPE, BODYSTRUCTURE, secciones,
//! parciales, CHANGEDSINCE), UID STORE, UID COPY/MOVE, EXPUNGE/UID EXPUNGE, APPEND, UID SEARCH
//! (ALL/UID/SINCE/BEFORE, ESEARCH) e IDLE con avisos `EXISTS`/`VANISHED`.
//! Se puede apagar cada capacidad para probar los caminos alternativos.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use base64::Engine;
use chrono::{DateTime, FixedOffset, NaiveDate, Utc};
use iuredesk_model::account::Security;
use iuredesk_net::Stream;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

use crate::client::ImapConfig;
use crate::codec::{Item, parse_internal_date, parse_items};
use crate::error::{ImapError, Result};
use crate::seqset::{STAR, SeqSet};
use crate::transport::{Limits, Transport, trailing_literal};

/// Capacidades que anuncia el servidor falso.
#[derive(Clone, Debug)]
pub struct FakeCaps {
    /// IDLE.
    pub idle: bool,
    /// CONDSTORE.
    pub condstore: bool,
    /// QRESYNC (y ENABLE).
    pub qresync: bool,
    /// UIDPLUS.
    pub uidplus: bool,
    /// MOVE.
    pub move_: bool,
    /// ESEARCH.
    pub esearch: bool,
    /// COMPRESS=DEFLATE.
    pub compress: bool,
    /// LITERAL+.
    pub literal_plus: bool,
    /// SPECIAL-USE.
    pub special_use: bool,
    /// SASL-IR.
    pub sasl_ir: bool,
    /// `X-GM-EXT-1`: el servidor se comporta como Gmail (un mensaje con varias etiquetas aparece
    /// en varios buzones con el mismo `X-GM-MSGID`).
    pub gmail: bool,
    /// NOTIFY (RFC 5465): durante IDLE avisa con `STATUS` de los cambios en otros buzones.
    pub notify: bool,
}

impl Default for FakeCaps {
    fn default() -> Self {
        Self {
            idle: true,
            condstore: true,
            qresync: true,
            uidplus: true,
            move_: true,
            esearch: true,
            compress: true,
            literal_plus: true,
            special_use: true,
            sasl_ir: true,
            gmail: false,
            notify: false,
        }
    }
}

impl FakeCaps {
    /// Un servidor IMAP4rev1 «desnudo» (sin ninguna extensión).
    pub fn bare() -> Self {
        Self {
            idle: false,
            condstore: false,
            qresync: false,
            uidplus: false,
            move_: false,
            esearch: false,
            compress: false,
            literal_plus: false,
            special_use: false,
            sasl_ir: false,
            gmail: false,
            notify: false,
        }
    }

    /// Como Gmail: las extensiones de siempre más `X-GM-EXT-1`.
    pub fn gmail() -> Self {
        Self { gmail: true, ..Self::default() }
    }

    fn line(&self) -> String {
        let mut c = vec!["IMAP4rev1", "AUTH=PLAIN", "AUTH=XOAUTH2", "ID", "UNSELECT", "ENABLE"];
        let opt = [
            (self.idle, "IDLE"),
            (self.condstore, "CONDSTORE"),
            (self.qresync, "QRESYNC"),
            (self.uidplus, "UIDPLUS"),
            (self.move_, "MOVE"),
            (self.esearch, "ESEARCH"),
            (self.compress, "COMPRESS=DEFLATE"),
            (self.literal_plus, "LITERAL+"),
            (self.special_use, "SPECIAL-USE"),
            (self.sasl_ir, "SASL-IR"),
            (self.gmail, "X-GM-EXT-1"),
            (self.notify, "NOTIFY"),
        ];
        c.extend(opt.iter().filter(|(on, _)| *on).map(|(_, n)| *n));
        c.join(" ")
    }
}

/// Un mensaje del buzón falso.
#[derive(Clone, Debug)]
pub struct FakeMessage {
    /// UID.
    pub uid: u32,
    /// Banderas.
    pub flags: BTreeSet<String>,
    /// MODSEQ del último cambio.
    pub modseq: u64,
    /// Fecha interna.
    pub internal_date: DateTime<FixedOffset>,
    /// RFC 822.
    pub raw: Vec<u8>,
    /// `X-GM-MSGID` (0 fuera del modo Gmail).
    pub gm_msgid: u64,
    /// `X-GM-THRID`.
    pub gm_thrid: u64,
    /// `X-GM-LABELS`.
    pub labels: BTreeSet<String>,
}

/// Un buzón del servidor falso.
#[derive(Clone, Debug)]
pub struct FakeMailbox {
    /// Atributo SPECIAL-USE (`\Sent`…).
    pub special_use: Option<String>,
    /// UIDVALIDITY.
    pub uid_validity: u32,
    /// Siguiente UID.
    pub uid_next: u32,
    /// Mensajes por UID ascendente.
    pub messages: Vec<FakeMessage>,
    /// `(uid, modseq)` de los borrados.
    pub expunged: Vec<(u32, u64)>,
}

struct State {
    mailboxes: BTreeMap<String, FakeMailbox>,
    modseq: u64,
    next_msgid: u64,
    log: Vec<String>,
    active: usize,
    max_active: usize,
    fail_next: Option<String>,
}

struct Shared {
    state: Mutex<State>,
    changed: watch::Sender<u64>,
    user: String,
    password: String,
    token: String,
    caps: FakeCaps,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn bump(&self) {
        self.changed.send_modify(|v| *v = v.wrapping_add(1));
    }
}

/// Servidor IMAP falso en `127.0.0.1`.
#[derive(Clone)]
pub struct FakeImap {
    shared: Arc<Shared>,
    port: u16,
}

impl std::fmt::Debug for FakeImap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeImap").field("port", &self.port).finish_non_exhaustive()
    }
}

/// Usuario de pruebas.
pub const USER: &str = "ana@example.com";
/// Contraseña de pruebas (con comillas y acento, para probar el entrecomillado y los literales).
pub const PASSWORD: &str = "clave \"segura\" ñ";
/// Token OAuth de pruebas.
pub const TOKEN: &str = "ya29.token-de-prueba";

fn now() -> DateTime<FixedOffset> {
    Utc::now().fixed_offset()
}

impl FakeImap {
    /// Arranca el servidor con `INBOX`, `Sent`, `Drafts`, `Trash` y `Archive`.
    pub async fn start(caps: FakeCaps) -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let (changed, _) = watch::channel(0u64);
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                mailboxes: BTreeMap::new(),
                modseq: 1,
                next_msgid: 1_000_000,
                log: Vec::new(),
                active: 0,
                max_active: 0,
                fail_next: None,
            }),
            changed,
            user: USER.into(),
            password: PASSWORD.into(),
            token: TOKEN.into(),
            caps,
        });
        let s = Self { shared: Arc::clone(&shared), port };
        for (name, su) in [
            ("INBOX", None),
            ("Sent", Some("\\Sent")),
            ("Drafts", Some("\\Drafts")),
            ("Trash", Some("\\Trash")),
            ("Archive", Some("\\Archive")),
        ] {
            s.add_mailbox(name, su);
        }
        tokio::spawn(async move {
            while let Ok((sock, _)) = listener.accept().await {
                let sh = Arc::clone(&shared);
                tokio::spawn(async move {
                    {
                        let mut st = sh.lock();
                        st.active = st.active.saturating_add(1);
                        st.max_active = st.max_active.max(st.active);
                    }
                    let _ = serve(sock, &sh).await;
                    let mut st = sh.lock();
                    st.active = st.active.saturating_sub(1);
                });
            }
        });
        Ok(s)
    }

    /// Puerto.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Configuración de cliente (texto plano, tiempos cortos).
    pub fn config(&self) -> ImapConfig {
        let mut c = ImapConfig::new("127.0.0.1", self.port, Security::Plain);
        c.io_timeout = Duration::from_secs(10);
        c
    }

    /// Crea un buzón (nombre ya en UTF-7 modificado).
    pub fn add_mailbox(&self, name: &str, special_use: Option<&str>) {
        let mut st = self.shared.lock();
        let v = 1000u32.saturating_add(u32::try_from(st.mailboxes.len()).unwrap_or(0));
        st.mailboxes.entry(name.to_string()).or_insert(FakeMailbox {
            special_use: special_use.map(String::from),
            uid_validity: v,
            uid_next: 1,
            messages: Vec::new(),
            expunged: Vec::new(),
        });
    }

    /// Quita un buzón (p. ej. para imitar a Gmail, que no tiene «Archive»).
    pub fn remove_mailbox(&self, name: &str) {
        self.shared.lock().mailboxes.remove(name);
    }

    /// Añade un mensaje (como si llegara correo) y avisa a quien esté en IDLE. Devuelve el UID.
    pub fn deliver(&self, mailbox: &str, raw: &[u8], flags: &[&str], date: Option<DateTime<FixedOffset>>) -> u32 {
        let uid = {
            let mut st = self.shared.lock();
            st.modseq = st.modseq.saturating_add(1);
            let ms = st.modseq;
            let Some(mb) = st.mailboxes.get_mut(mailbox) else { return 0 };
            add_message(mb, raw.to_vec(), flags.iter().map(|f| f.to_string()).collect(), date.unwrap_or_else(now), ms)
        };
        self.shared.bump();
        uid
    }

    /// Añade muchos mensajes de golpe (sin avisar a IDLE).
    pub fn deliver_many(&self, mailbox: &str, msgs: Vec<(Vec<u8>, Vec<String>, DateTime<FixedOffset>)>) {
        let mut st = self.shared.lock();
        for (raw, flags, date) in msgs {
            st.modseq = st.modseq.saturating_add(1);
            let ms = st.modseq;
            if let Some(mb) = st.mailboxes.get_mut(mailbox) {
                add_message(mb, raw, flags.into_iter().collect(), date, ms);
            }
        }
    }

    /// Modo Gmail: entrega un mensaje con estas etiquetas (`\\Inbox`, `Trabajo`…). Queda en
    /// «Todos» y en el buzón de cada etiqueta con el mismo `X-GM-MSGID`. Devuelve el `X-GM-MSGID`.
    pub fn deliver_gmail(&self, raw: &[u8], labels: &[&str], flags: &[&str], thread: Option<u64>) -> u64 {
        let msgid = {
            let mut st = self.shared.lock();
            st.next_msgid = st.next_msgid.saturating_add(1);
            let msgid = st.next_msgid;
            st.modseq = st.modseq.saturating_add(1);
            let ms = st.modseq;
            let labels: BTreeSet<String> = labels.iter().map(|l| l.to_string()).collect();
            let flags: BTreeSet<String> = flags.iter().map(|f| f.to_string()).collect();
            let mut targets: Vec<String> = labels.iter().filter_map(|l| label_mailbox(&st, l)).collect();
            targets.extend(all_mail(&st));
            for name in targets {
                if let Some(mb) = st.mailboxes.get_mut(&name) {
                    let uid = add_message(mb, raw.to_vec(), flags.clone(), now(), ms);
                    if let Some(m) = mb.messages.iter_mut().find(|m| m.uid == uid) {
                        m.gm_msgid = msgid;
                        m.gm_thrid = thread.unwrap_or(msgid);
                        m.labels = labels.clone();
                    }
                }
            }
            msgid
        };
        self.shared.bump();
        msgid
    }

    /// Modo Gmail: cambia las etiquetas de un mensaje (como otro cliente o la web).
    pub fn set_gmail_labels(&self, msgid: u64, labels: &[&str]) {
        {
            let mut st = self.shared.lock();
            let flags = st
                .mailboxes
                .values()
                .flat_map(|mb| mb.messages.iter())
                .find(|m| m.gm_msgid == msgid)
                .map(|m| m.flags.clone())
                .unwrap_or_default();
            let labels: BTreeSet<String> = labels.iter().map(|l| l.to_string()).collect();
            gmail_apply(&mut st, msgid, &labels, &flags);
        }
        self.shared.bump();
    }

    /// Modo Gmail: etiquetas actuales de un mensaje.
    pub fn gmail_labels(&self, msgid: u64) -> Vec<String> {
        let st = self.shared.lock();
        st.mailboxes
            .values()
            .flat_map(|mb| mb.messages.iter())
            .find(|m| m.gm_msgid == msgid)
            .map(|m| m.labels.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Cambia las banderas de un mensaje (como otro cliente).
    pub fn set_flags(&self, mailbox: &str, uid: u32, flags: &[&str]) {
        {
            let mut st = self.shared.lock();
            st.modseq = st.modseq.saturating_add(1);
            let ms = st.modseq;
            if let Some(m) = st.mailboxes.get_mut(mailbox).and_then(|mb| mb.messages.iter_mut().find(|m| m.uid == uid))
            {
                m.flags = flags.iter().map(|f| f.to_string()).collect();
                m.modseq = ms;
            }
        }
        self.shared.bump();
    }

    /// Borra un mensaje (como otro cliente).
    pub fn expunge(&self, mailbox: &str, uid: u32) {
        {
            let mut st = self.shared.lock();
            st.modseq = st.modseq.saturating_add(1);
            let ms = st.modseq;
            if let Some(mb) = st.mailboxes.get_mut(mailbox) {
                mb.messages.retain(|m| m.uid != uid);
                mb.expunged.push((uid, ms));
            }
        }
        self.shared.bump();
    }

    /// Cambia el UIDVALIDITY y renumera los UID (como un servidor reconstruido).
    pub fn reset_uid_validity(&self, mailbox: &str) {
        let mut st = self.shared.lock();
        st.modseq = st.modseq.saturating_add(1);
        let ms = st.modseq;
        if let Some(mb) = st.mailboxes.get_mut(mailbox) {
            mb.uid_validity = mb.uid_validity.saturating_add(7);
            mb.expunged.clear();
            let mut uid = 0u32;
            for m in &mut mb.messages {
                uid = uid.saturating_add(1);
                m.uid = uid;
                m.modseq = ms;
            }
            mb.uid_next = uid.saturating_add(1);
        }
    }

    /// `(uid, banderas)` de un buzón.
    pub fn messages(&self, mailbox: &str) -> Vec<(u32, Vec<String>)> {
        let st = self.shared.lock();
        st.mailboxes
            .get(mailbox)
            .map(|mb| mb.messages.iter().map(|m| (m.uid, m.flags.iter().cloned().collect())).collect())
            .unwrap_or_default()
    }

    /// Mensaje crudo por UID.
    pub fn raw(&self, mailbox: &str, uid: u32) -> Option<Vec<u8>> {
        let st = self.shared.lock();
        st.mailboxes.get(mailbox)?.messages.iter().find(|m| m.uid == uid).map(|m| m.raw.clone())
    }

    /// Órdenes recibidas (sin argumentos de LOGIN/AUTHENTICATE).
    pub fn log(&self) -> Vec<String> {
        self.shared.lock().log.clone()
    }

    /// Máximo de conexiones simultáneas observado.
    pub fn max_connections(&self) -> usize {
        self.shared.lock().max_active
    }

    /// La próxima orden con este nombre (`UID MOVE`, `SELECT`…) recibirá `NO`.
    pub fn fail_next(&self, command: &str) {
        self.shared.lock().fail_next = Some(command.to_ascii_uppercase());
    }
}

fn add_message(
    mb: &mut FakeMailbox,
    raw: Vec<u8>,
    flags: BTreeSet<String>,
    date: DateTime<FixedOffset>,
    modseq: u64,
) -> u32 {
    let uid = mb.uid_next;
    mb.uid_next = mb.uid_next.saturating_add(1);
    mb.messages.push(FakeMessage {
        uid,
        flags,
        modseq,
        internal_date: date,
        raw,
        gm_msgid: 0,
        gm_thrid: 0,
        labels: BTreeSet::new(),
    });
    uid
}

/// Buzón que corresponde a una etiqueta de Gmail (`\Inbox` → `INBOX`, `\Sent` → el de
/// `\Sent`…); `None` para las que no tienen buzón (`\Important`).
fn label_mailbox(st: &State, label: &str) -> Option<String> {
    let su = match label {
        "\\Inbox" => return Some("INBOX".into()),
        "\\Sent" => "\\Sent",
        "\\Draft" => "\\Drafts",
        "\\Starred" => "\\Flagged",
        "\\Important" => "\\Important",
        l if l.starts_with('\\') => return None,
        l => return st.mailboxes.contains_key(l).then(|| l.to_string()),
    };
    st.mailboxes.iter().find(|(_, mb)| mb.special_use.as_deref() == Some(su)).map(|(n, _)| n.clone())
}

/// Etiqueta de Gmail de un buzón (`None` para «Todos», Papelera y Spam).
fn mailbox_label(st: &State, name: &str) -> Option<String> {
    if name == "INBOX" {
        return Some("\\Inbox".into());
    }
    let mb = st.mailboxes.get(name)?;
    match mb.special_use.as_deref() {
        Some("\\Sent") => Some("\\Sent".into()),
        Some("\\Drafts") => Some("\\Draft".into()),
        Some("\\Flagged") => Some("\\Starred".into()),
        Some("\\Important") => Some("\\Important".into()),
        Some(_) => None,
        None => Some(name.to_string()),
    }
}

/// Buzón «Todos» (`\All`) del modo Gmail.
fn all_mail(st: &State) -> Option<String> {
    st.mailboxes.iter().find(|(_, mb)| mb.special_use.as_deref() == Some("\\All")).map(|(n, _)| n.clone())
}

/// Aplica a todas las copias de `msgid` las etiquetas y banderas nuevas: crea la copia en los
/// buzones de las etiquetas añadidas y la borra de los de las quitadas (como Gmail).
fn gmail_apply(st: &mut State, msgid: u64, labels: &BTreeSet<String>, flags: &BTreeSet<String>) {
    st.modseq = st.modseq.saturating_add(1);
    let ms = st.modseq;
    let mut template: Option<FakeMessage> = None;
    for mb in st.mailboxes.values_mut() {
        for m in mb.messages.iter_mut().filter(|m| m.gm_msgid == msgid) {
            m.labels = labels.clone();
            m.flags = flags.clone();
            m.modseq = ms;
            template.get_or_insert_with(|| m.clone());
        }
    }
    let Some(t) = template else { return };
    let names: Vec<String> = st.mailboxes.keys().cloned().collect();
    for name in names {
        let Some(label) = mailbox_label(st, &name) else { continue };
        let want = labels.contains(&label);
        let Some(mb) = st.mailboxes.get_mut(&name) else { continue };
        let has = mb.messages.iter().any(|m| m.gm_msgid == msgid);
        if want && !has {
            let uid = add_message(mb, t.raw.clone(), flags.clone(), t.internal_date, ms);
            if let Some(m) = mb.messages.iter_mut().find(|m| m.uid == uid) {
                m.gm_msgid = msgid;
                m.gm_thrid = t.gm_thrid;
                m.labels = labels.clone();
            }
        } else if !want && has {
            let mut out = Vec::new();
            expunge_where(mb, &mut out, true, ms, |m| m.gm_msgid == msgid);
        }
    }
}

// ---------------------------------------------------------------------- sesión

struct Session {
    authed: bool,
    selected: Option<String>,
    read_only: bool,
    qresync: bool,
    notify: bool,
}

fn quote(s: &[u8]) -> Vec<u8> {
    if s.iter().all(|b| (0x20..0x7f).contains(b)) {
        let mut out = vec![b'"'];
        for &b in s {
            if b == b'"' || b == b'\\' {
                out.push(b'\\');
            }
            out.push(b);
        }
        out.push(b'"');
        out
    } else {
        let mut out = format!("{{{}}}\r\n", s.len()).into_bytes();
        out.extend_from_slice(s);
        out
    }
}

fn nstring(s: Option<&str>) -> Vec<u8> {
    match s {
        Some(v) => quote(v.as_bytes()),
        None => b"NIL".to_vec(),
    }
}

/// Cabeceras desplegadas `(nombre en minúsculas, valor, líneas originales)`.
fn headers(raw: &[u8]) -> Vec<(String, String, Vec<u8>)> {
    let end = raw.windows(4).position(|w| w == b"\r\n\r\n").map_or(raw.len(), |p| p.saturating_add(2));
    let head = raw.get(..end).unwrap_or_default();
    let mut out: Vec<(String, String, Vec<u8>)> = Vec::new();
    for line in head.split(|b| *b == b'\n') {
        let line_s = String::from_utf8_lossy(line).trim_end_matches('\r').to_string();
        if line_s.is_empty() {
            continue;
        }
        if line_s.starts_with([' ', '\t']) {
            if let Some(last) = out.last_mut() {
                last.1.push(' ');
                last.1.push_str(line_s.trim());
                last.2.extend_from_slice(line);
                last.2.push(b'\n');
            }
            continue;
        }
        if let Some((k, v)) = line_s.split_once(':') {
            let mut orig = line.to_vec();
            orig.push(b'\n');
            out.push((k.trim().to_ascii_lowercase(), v.trim().to_string(), orig));
        }
    }
    out
}

fn header<'a>(h: &'a [(String, String, Vec<u8>)], name: &str) -> Option<&'a str> {
    h.iter().find(|(k, _, _)| k == name).map(|(_, v, _)| v.as_str())
}

fn address_list(v: Option<&str>) -> Vec<u8> {
    let Some(v) = v.filter(|v| !v.trim().is_empty()) else { return b"NIL".to_vec() };
    let mut out = b"(".to_vec();
    for part in v.split(',') {
        let part = part.trim();
        let (name, addr) = match (part.find('<'), part.rfind('>')) {
            (Some(a), Some(b)) if a < b => {
                (Some(part.get(..a).unwrap_or("").trim().trim_matches('"')), part.get(a + 1..b).unwrap_or(""))
            }
            _ => (None, part),
        };
        let (mbox, host) = addr.split_once('@').unwrap_or((addr, ""));
        out.push(b'(');
        out.extend(nstring(name.filter(|n| !n.is_empty())));
        out.extend_from_slice(b" NIL ");
        out.extend(quote(mbox.as_bytes()));
        out.push(b' ');
        out.extend(quote(host.as_bytes()));
        out.push(b')');
    }
    out.push(b')');
    out
}

fn envelope(raw: &[u8]) -> Vec<u8> {
    let h = headers(raw);
    let mut out = b"(".to_vec();
    out.extend(nstring(header(&h, "date")));
    out.push(b' ');
    out.extend(nstring(header(&h, "subject")));
    out.push(b' ');
    let from = address_list(header(&h, "from"));
    out.extend(&from);
    out.push(b' ');
    out.extend(&from);
    out.push(b' ');
    let reply = header(&h, "reply-to").map(|r| address_list(Some(r))).unwrap_or(from);
    out.extend(reply);
    for k in ["to", "cc", "bcc"] {
        out.push(b' ');
        out.extend(address_list(header(&h, k)));
    }
    out.push(b' ');
    out.extend(nstring(header(&h, "in-reply-to")));
    out.push(b' ');
    out.extend(nstring(header(&h, "message-id")));
    out.push(b')');
    out
}

fn body_structure(raw: &[u8]) -> Vec<u8> {
    let h = headers(raw);
    let ct = header(&h, "content-type").unwrap_or("text/plain").to_ascii_lowercase();
    if ct.contains("multipart/mixed") {
        let text = String::from_utf8_lossy(raw);
        let name =
            text.split("filename=\"").nth(1).and_then(|r| r.split('"').next()).unwrap_or("adjunto.bin").to_string();
        format!(
            "((\"TEXT\" \"PLAIN\" (\"CHARSET\" \"utf-8\") NIL NIL \"7BIT\" 10 1 NIL NIL NIL NIL)(\"APPLICATION\" \"PDF\" (\"NAME\" \"{name}\") NIL NIL \"BASE64\" 400 NIL (\"ATTACHMENT\" (\"FILENAME\" \"{name}\")) NIL NIL) \"MIXED\" (\"BOUNDARY\" \"b\") NIL NIL NIL)"
        )
        .into_bytes()
    } else {
        let lines = raw.iter().filter(|b| **b == b'\n').count();
        format!("(\"TEXT\" \"PLAIN\" (\"CHARSET\" \"utf-8\") NIL NIL \"8BIT\" {} {lines} NIL NIL NIL NIL)", raw.len())
            .into_bytes()
    }
}

/// Sección pedida (`BODY.PEEK[HEADER.FIELDS (A B)]<0.10>`) → (nombre de respuesta, datos).
fn section(attr: &str, raw: &[u8]) -> (String, Vec<u8>) {
    let upper = attr.to_ascii_uppercase().replacen(".PEEK", "", 1);
    let (base, partial) = match upper.rfind(']') {
        Some(i) => (upper.get(..=i).unwrap_or(&upper).to_string(), upper.get(i + 1..).unwrap_or("").to_string()),
        None => (upper.clone(), String::new()),
    };
    let inner = base.split_once('[').map(|(_, r)| r.trim_end_matches(']')).unwrap_or("");
    let header_end = raw.windows(4).position(|w| w == b"\r\n\r\n").map_or(raw.len(), |p| p.saturating_add(4));
    let mut data = if inner.is_empty() {
        raw.to_vec()
    } else if inner == "HEADER" {
        raw.get(..header_end).unwrap_or_default().to_vec()
    } else if inner == "TEXT" {
        raw.get(header_end..).unwrap_or_default().to_vec()
    } else if let Some(fields) = inner.strip_prefix("HEADER.FIELDS") {
        let wanted: Vec<String> = fields
            .trim()
            .trim_matches(['(', ')'])
            .split_whitespace()
            .map(|f| f.trim_matches('"').to_ascii_lowercase())
            .collect();
        let mut out = Vec::new();
        for (k, _, orig) in headers(raw) {
            if wanted.contains(&k) {
                out.extend(orig);
            }
        }
        out.extend_from_slice(b"\r\n");
        out
    } else {
        raw.to_vec()
    };
    let mut name = base;
    if let Some(p) = partial.strip_prefix('<').and_then(|p| p.strip_suffix('>')) {
        let (o, n) = p.split_once('.').unwrap_or((p, "0"));
        let o: usize = o.parse().unwrap_or(0);
        let n: usize = n.parse().unwrap_or(0);
        let end = o.saturating_add(n).min(data.len());
        data = data.get(o.min(end)..end).unwrap_or_default().to_vec();
        name = format!("{name}<{o}>");
    }
    (name, data)
}

fn fetch_line(seq: usize, m: &FakeMessage, attrs: &[String], with_modseq: bool) -> Vec<u8> {
    let mut out = format!("* {seq} FETCH (UID {}", m.uid).into_bytes();
    let mut modseq_done = false;
    for a in attrs {
        let up = a.to_ascii_uppercase();
        match up.as_str() {
            "UID" => {}
            "FLAGS" => {
                out.extend(format!(" FLAGS ({})", m.flags.iter().cloned().collect::<Vec<_>>().join(" ")).into_bytes());
            }
            "INTERNALDATE" => {
                out.extend(format!(" INTERNALDATE \"{}\"", crate::format_internal_date(&m.internal_date)).into_bytes());
            }
            "RFC822.SIZE" => out.extend(format!(" RFC822.SIZE {}", m.raw.len()).into_bytes()),
            "ENVELOPE" => {
                out.extend_from_slice(b" ENVELOPE ");
                out.extend(envelope(&m.raw));
            }
            "BODYSTRUCTURE" => {
                out.extend_from_slice(b" BODYSTRUCTURE ");
                out.extend(body_structure(&m.raw));
            }
            "MODSEQ" => {
                out.extend(format!(" MODSEQ ({})", m.modseq).into_bytes());
                modseq_done = true;
            }
            "X-GM-MSGID" if m.gm_msgid != 0 => out.extend(format!(" X-GM-MSGID {}", m.gm_msgid).into_bytes()),
            "X-GM-THRID" if m.gm_msgid != 0 => out.extend(format!(" X-GM-THRID {}", m.gm_thrid).into_bytes()),
            "X-GM-LABELS" if m.gm_msgid != 0 => {
                out.extend_from_slice(b" X-GM-LABELS (");
                for (i, l) in m.labels.iter().enumerate() {
                    if i > 0 {
                        out.push(b' ');
                    }
                    if l.starts_with('\\') {
                        out.extend(l.as_bytes());
                    } else {
                        out.extend(quote(l.as_bytes()));
                    }
                }
                out.push(b')');
            }
            _ if up.starts_with("BODY[") || up.starts_with("BODY.PEEK[") => {
                let (name, data) = section(a, &m.raw);
                out.extend(format!(" {name} {{{}}}\r\n", data.len()).into_bytes());
                out.extend(data);
            }
            _ => {}
        }
    }
    if with_modseq && !modseq_done {
        out.extend(format!(" MODSEQ ({})", m.modseq).into_bytes());
    }
    out.extend_from_slice(b")\r\n");
    out
}

/// Interpreta un conjunto con `*` = mayor UID (o número de secuencia).
fn resolve_set(text: &str, max: u32) -> Vec<(u32, u32)> {
    let Some(s) = SeqSet::parse(text) else { return Vec::new() };
    s.ranges()
        .iter()
        .map(|&(a, b)| {
            let a = if a == STAR { max } else { a };
            let b = if b == STAR { max } else { b };
            (a.min(b), a.max(b))
        })
        .collect()
}

fn in_ranges(r: &[(u32, u32)], n: u32) -> bool {
    r.iter().any(|&(a, b)| a <= n && n <= b)
}

fn parse_date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.trim_matches('"'), "%d-%b-%Y").ok()
}

async fn send(t: &mut Transport, data: &[u8]) -> Result<()> {
    t.write_all(data, Duration::from_secs(10)).await
}

async fn serve(sock: TcpStream, sh: &Shared) -> Result<()> {
    let caps = sh.caps.line();
    let mut t = Transport::new(Stream::Plain(sock), "fake", Limits::default());
    send(&mut t, format!("* OK [CAPABILITY {caps}] Fake IMAP lista\r\n").as_bytes()).await?;
    let mut sess = Session { authed: false, selected: None, read_only: false, qresync: false, notify: false };
    let idle_wait = Duration::from_secs(600);
    loop {
        // Lee la orden con sus literales.
        let mut line = t.read_line(idle_wait).await?;
        let mut cmd = line.clone();
        for _ in 0..16 {
            let Some(n) = trailing_literal(&line) else { break };
            if !line.ends_with(b"+}") {
                send(&mut t, b"+ adelante\r\n").await?;
            }
            let data = t.read_exact(n, idle_wait).await?;
            cmd.extend_from_slice(b"\r\n");
            cmd.extend(data);
            line = t.read_line(idle_wait).await?;
            cmd.extend_from_slice(&line);
        }
        let items = match parse_items(&cmd) {
            Ok(i) => i,
            Err(_) => {
                send(&mut t, b"* BAD orden ilegible\r\n").await?;
                continue;
            }
        };
        let tag = items.first().and_then(Item::text).unwrap_or_else(|| "*".into());
        let mut name = items.get(1).and_then(Item::text).unwrap_or_default().to_ascii_uppercase();
        let mut args: Vec<Item> = items.get(2..).unwrap_or_default().to_vec();
        if name == "UID" {
            name = format!("UID {}", args.first().and_then(Item::text).unwrap_or_default().to_ascii_uppercase());
            args = args.get(1..).unwrap_or_default().to_vec();
        }
        let fail = {
            let mut st = sh.lock();
            let logged = if matches!(name.as_str(), "LOGIN" | "AUTHENTICATE") {
                name.clone()
            } else {
                String::from_utf8_lossy(&cmd).chars().take(200).collect()
            };
            st.log.push(logged);
            let fail = st.fail_next.as_deref() == Some(name.as_str());
            if fail {
                st.fail_next = None;
            }
            fail
        };
        if fail {
            send(&mut t, format!("{tag} NO [UNAVAILABLE] fallo simulado\r\n").as_bytes()).await?;
            continue;
        }
        let text = |i: usize| args.get(i).and_then(Item::text).unwrap_or_default();
        match name.as_str() {
            "CAPABILITY" => {
                send(&mut t, format!("* CAPABILITY {caps}\r\n{tag} OK listo\r\n").as_bytes()).await?;
            }
            "NOOP" | "CHECK" => send(&mut t, format!("{tag} OK nada\r\n").as_bytes()).await?,
            "LOGOUT" => {
                send(&mut t, format!("* BYE adiós\r\n{tag} OK fin\r\n").as_bytes()).await?;
                return Ok(());
            }
            "ID" => send(&mut t, format!("* ID NIL\r\n{tag} OK id\r\n").as_bytes()).await?,
            "LOGIN" => {
                if text(0) == sh.user && text(1) == sh.password {
                    sess.authed = true;
                    send(&mut t, format!("{tag} OK [CAPABILITY {caps}] dentro\r\n").as_bytes()).await?;
                } else {
                    send(&mut t, format!("{tag} NO [AUTHENTICATIONFAILED] Invalid credentials\r\n").as_bytes()).await?;
                }
            }
            "AUTHENTICATE" => {
                let mech = text(0).to_ascii_uppercase();
                let ir = match args.get(1) {
                    Some(i) => i.text().unwrap_or_default(),
                    None => {
                        send(&mut t, b"+ \r\n").await?;
                        String::from_utf8_lossy(&t.read_line(idle_wait).await?).into_owned()
                    }
                };
                let decoded = base64::engine::general_purpose::STANDARD.decode(ir.trim()).unwrap_or_default();
                let d = String::from_utf8_lossy(&decoded).into_owned();
                let ok = match mech.as_str() {
                    "PLAIN" => {
                        let mut p = d.split('\0').skip(1);
                        p.next() == Some(sh.user.as_str()) && p.next() == Some(sh.password.as_str())
                    }
                    "XOAUTH2" => d == format!("user={}\x01auth=Bearer {}\x01\x01", sh.user, sh.token),
                    _ => false,
                };
                if ok {
                    sess.authed = true;
                    send(&mut t, format!("{tag} OK [CAPABILITY {caps}] dentro\r\n").as_bytes()).await?;
                } else if mech == "XOAUTH2" {
                    let err = base64::engine::general_purpose::STANDARD
                        .encode(r#"{"status":"401","schemes":"bearer","scope":"https://mail.google.com/"}"#);
                    send(&mut t, format!("+ {err}\r\n").as_bytes()).await?;
                    let _ = t.read_line(idle_wait).await?;
                    send(&mut t, format!("{tag} NO [AUTHENTICATIONFAILED] Invalid credentials\r\n").as_bytes()).await?;
                } else {
                    send(&mut t, format!("{tag} NO [AUTHENTICATIONFAILED] Invalid credentials\r\n").as_bytes()).await?;
                }
            }
            _ if !sess.authed => send(&mut t, format!("{tag} BAD primero autentícate\r\n").as_bytes()).await?,
            "ENABLE" => {
                let mut on = Vec::new();
                for a in &args {
                    let e = a.text().unwrap_or_default().to_ascii_uppercase();
                    if e == "QRESYNC" && sh.caps.qresync {
                        sess.qresync = true;
                        on.push(e);
                    } else if e == "CONDSTORE" && sh.caps.condstore {
                        on.push(e);
                    }
                }
                send(&mut t, format!("* ENABLED {}\r\n{tag} OK habilitado\r\n", on.join(" ")).as_bytes()).await?;
            }
            "NOTIFY" if sh.caps.notify => {
                sess.notify = !text(0).eq_ignore_ascii_case("NONE");
                send(&mut t, format!("{tag} OK NOTIFY hecho\r\n").as_bytes()).await?;
            }
            "COMPRESS" if sh.caps.compress => {
                send(&mut t, format!("{tag} OK DEFLATE activo\r\n").as_bytes()).await?;
                t.enable_deflate()?;
            }
            "LIST" => {
                let mut out = Vec::new();
                {
                    let st = sh.lock();
                    for (n, mb) in &st.mailboxes {
                        let su = match (&mb.special_use, sh.caps.special_use) {
                            (Some(s), true) => format!(" {s}"),
                            _ => String::new(),
                        };
                        out.extend(format!("* LIST (\\HasNoChildren{su}) \"/\" ").into_bytes());
                        out.extend(quote(n.as_bytes()));
                        out.extend_from_slice(b"\r\n");
                    }
                }
                out.extend(format!("{tag} OK LIST hecho\r\n").into_bytes());
                send(&mut t, &out).await?;
            }
            "STATUS" => {
                let mbox = text(0);
                let line = {
                    let st = sh.lock();
                    st.mailboxes.get(&mbox).map(|mb| {
                        let unseen = mb.messages.iter().filter(|m| !m.flags.contains("\\Seen")).count();
                        format!(
                            "* STATUS {} (MESSAGES {} UIDNEXT {} UIDVALIDITY {} UNSEEN {unseen} HIGHESTMODSEQ {})\r\n",
                            String::from_utf8_lossy(&quote(mbox.as_bytes())),
                            mb.messages.len(),
                            mb.uid_next,
                            mb.uid_validity,
                            st.modseq
                        )
                    })
                };
                match line {
                    Some(l) => send(&mut t, format!("{l}{tag} OK STATUS hecho\r\n").as_bytes()).await?,
                    None => send(&mut t, format!("{tag} NO [NONEXISTENT] no existe\r\n").as_bytes()).await?,
                }
            }
            "SELECT" | "EXAMINE" => {
                let mbox = text(0);
                let mut out = Vec::new();
                let found = {
                    let st = sh.lock();
                    match st.mailboxes.get(&mbox) {
                        None => false,
                        Some(mb) => {
                            out.extend(
                                format!(
                                    "* FLAGS (\\Answered \\Flagged \\Deleted \\Seen \\Draft $Forwarded)\r\n* OK [PERMANENTFLAGS (\\Answered \\Flagged \\Deleted \\Seen \\Draft $Forwarded \\*)] ok\r\n* {} EXISTS\r\n* OK [UIDVALIDITY {}] ok\r\n* OK [UIDNEXT {}] ok\r\n",
                                    mb.messages.len(),
                                    mb.uid_validity,
                                    mb.uid_next
                                )
                                .into_bytes(),
                            );
                            if sh.caps.condstore {
                                out.extend(format!("* OK [HIGHESTMODSEQ {}] ok\r\n", st.modseq).into_bytes());
                            }
                            // QRESYNC: (QRESYNC (v m [known]))
                            let q = args.get(1).map(Item::list).and_then(|l| {
                                let inner = l.get(1)?.list();
                                Some((
                                    inner.first()?.number()?,
                                    inner.get(1)?.number()?,
                                    inner.get(2).and_then(Item::text),
                                ))
                            });
                            if let (true, Some((v, m, known))) = (sess.qresync, q)
                                && u64::from(mb.uid_validity) == v
                            {
                                let known_r = known.as_deref().map(|k| resolve_set(k, mb.uid_next));
                                let gone: Vec<u32> = mb
                                    .expunged
                                    .iter()
                                    .filter(|(u, ms)| *ms > m && known_r.as_ref().is_none_or(|r| in_ranges(r, *u)))
                                    .map(|(u, _)| *u)
                                    .collect();
                                if !gone.is_empty() {
                                    out.extend(
                                        format!("* VANISHED (EARLIER) {}\r\n", SeqSet::from_numbers(gone)).into_bytes(),
                                    );
                                }
                                for (i, msg) in mb.messages.iter().enumerate() {
                                    if msg.modseq > m {
                                        out.extend(fetch_line(i.saturating_add(1), msg, &["FLAGS".to_string()], true));
                                    }
                                }
                            }
                            true
                        }
                    }
                };
                if found {
                    sess.selected = Some(mbox);
                    sess.read_only = name == "EXAMINE";
                    let rw = if sess.read_only { "READ-ONLY" } else { "READ-WRITE" };
                    out.extend(format!("{tag} OK [{rw}] seleccionado\r\n").into_bytes());
                } else {
                    sess.selected = None;
                    out.extend(format!("{tag} NO [NONEXISTENT] no existe\r\n").into_bytes());
                }
                send(&mut t, &out).await?;
            }
            "UNSELECT" | "CLOSE" => {
                sess.selected = None;
                send(&mut t, format!("{tag} OK cerrado\r\n").as_bytes()).await?;
            }
            "APPEND" => {
                let mbox = text(0);
                let mut flags = BTreeSet::new();
                let mut date = now();
                let mut data = Vec::new();
                for a in args.get(1..).unwrap_or_default() {
                    match a {
                        Item::List(l) => flags = l.iter().filter_map(Item::text).collect(),
                        Item::Str(s) => {
                            let txt = String::from_utf8_lossy(s);
                            match parse_internal_date(&txt) {
                                Some(d) if s.len() < 40 => date = d,
                                _ => data = s.clone(),
                            }
                        }
                        _ => {}
                    }
                }
                let res = {
                    let mut st = sh.lock();
                    st.modseq = st.modseq.saturating_add(1);
                    let ms = st.modseq;
                    st.mailboxes.get_mut(&mbox).map(|mb| (mb.uid_validity, add_message(mb, data, flags, date, ms)))
                };
                sh.bump();
                match res {
                    Some((v, uid)) if sh.caps.uidplus => {
                        send(&mut t, format!("{tag} OK [APPENDUID {v} {uid}] guardado\r\n").as_bytes()).await?;
                    }
                    Some(_) => send(&mut t, format!("{tag} OK guardado\r\n").as_bytes()).await?,
                    None => send(&mut t, format!("{tag} NO [TRYCREATE] no existe\r\n").as_bytes()).await?,
                }
            }
            "IDLE" if sh.caps.idle => {
                let Some(mbox) = sess.selected.clone() else {
                    send(&mut t, format!("{tag} BAD sin buzón\r\n").as_bytes()).await?;
                    continue;
                };
                let mut rx = sh.changed.subscribe();
                rx.borrow_and_update();
                let snapshot = |sh: &Shared| -> Vec<u32> {
                    sh.lock()
                        .mailboxes
                        .get(&mbox)
                        .map(|mb| mb.messages.iter().map(|m| m.uid).collect())
                        .unwrap_or_default()
                };
                let others = |sh: &Shared| -> BTreeMap<String, (u32, usize)> {
                    sh.lock()
                        .mailboxes
                        .iter()
                        .filter(|(n, _)| **n != mbox)
                        .map(|(n, mb)| (n.clone(), (mb.uid_next, mb.messages.len())))
                        .collect()
                };
                let mut known = snapshot(sh);
                let mut known_others = others(sh);
                send(&mut t, b"+ idling\r\n").await?;
                loop {
                    tokio::select! {
                        l = t.read_line(idle_wait) => {
                            let _ = l?;
                            break;
                        }
                        c = rx.changed() => {
                            if c.is_err() {
                                break;
                            }
                            let now_uids = snapshot(sh);
                            let gone: Vec<u32> = known.iter().copied().filter(|u| !now_uids.contains(u)).collect();
                            let mut out = Vec::new();
                            if !gone.is_empty() {
                                if sess.qresync {
                                    out.extend(format!("* VANISHED {}\r\n", SeqSet::from_numbers(gone)).into_bytes());
                                } else {
                                    for u in gone.iter().rev() {
                                        if let Some(pos) = known.iter().position(|k| k == u) {
                                            out.extend(format!("* {} EXPUNGE\r\n", pos.saturating_add(1)).into_bytes());
                                        }
                                    }
                                }
                            }
                            if now_uids.len() != known.len() || now_uids.last() != known.last() {
                                out.extend(format!("* {} EXISTS\r\n", now_uids.len()).into_bytes());
                            }
                            known = now_uids;
                            if sess.notify {
                                let now_others = others(sh);
                                for (n, (next, len)) in &now_others {
                                    if known_others.get(n) != Some(&(*next, *len)) {
                                        out.extend_from_slice(b"* STATUS ");
                                        out.extend(quote(n.as_bytes()));
                                        out.extend(format!(" (MESSAGES {len} UIDNEXT {next})\r\n").into_bytes());
                                    }
                                }
                                known_others = now_others;
                            }
                            if !out.is_empty() {
                                send(&mut t, &out).await?;
                            }
                        }
                    }
                }
                send(&mut t, format!("{tag} OK IDLE terminado\r\n").as_bytes()).await?;
            }
            "UID FETCH" | "UID STORE" | "UID COPY" | "UID MOVE" | "UID SEARCH" | "UID EXPUNGE" | "EXPUNGE" => {
                let Some(mbox) = sess.selected.clone() else {
                    send(&mut t, format!("{tag} BAD sin buzón\r\n").as_bytes()).await?;
                    continue;
                };
                let out = mailbox_command(sh, &mut sess, &mbox, &name, &args, &tag);
                sh.bump();
                send(&mut t, &out).await?;
            }
            _ => send(&mut t, format!("{tag} BAD orden desconocida {name}\r\n").as_bytes()).await?,
        }
    }
}

/// Órdenes sobre el buzón seleccionado (todo bajo el candado, sin esperas).
fn mailbox_command(sh: &Shared, sess: &mut Session, mbox: &str, name: &str, args: &[Item], tag: &str) -> Vec<u8> {
    let caps = &sh.caps;
    let mut st = sh.lock();
    let mut out = Vec::new();
    let text = |i: usize| args.get(i).and_then(Item::text).unwrap_or_default();
    let max_uid = |mb: &FakeMailbox| mb.messages.last().map_or(0, |m| m.uid);
    match name {
        "UID FETCH" => {
            let Some(mb) = st.mailboxes.get(mbox) else { return format!("{tag} NO sin buzón\r\n").into_bytes() };
            let ranges = resolve_set(&text(0), max_uid(mb));
            let attrs: Vec<String> = match args.get(1) {
                Some(Item::List(l)) => l.iter().filter_map(Item::text).collect(),
                Some(other) => other.text().into_iter().collect(),
                None => Vec::new(),
            };
            let mut since = None;
            let mut vanished = false;
            if let Some(Item::List(m)) = args.get(2) {
                for (i, x) in m.iter().enumerate() {
                    let tx = x.text().unwrap_or_default().to_ascii_uppercase();
                    if tx == "CHANGEDSINCE" {
                        since = m.get(i.saturating_add(1)).and_then(Item::number);
                    }
                    if tx == "VANISHED" {
                        vanished = true;
                    }
                }
            }
            if vanished
                && sess.qresync
                && let Some(s) = since
            {
                let gone: Vec<u32> =
                    mb.expunged.iter().filter(|(u, ms)| *ms > s && in_ranges(&ranges, *u)).map(|(u, _)| *u).collect();
                if !gone.is_empty() {
                    out.extend(format!("* VANISHED (EARLIER) {}\r\n", SeqSet::from_numbers(gone)).into_bytes());
                }
            }
            for (i, m) in mb.messages.iter().enumerate() {
                if in_ranges(&ranges, m.uid) && since.is_none_or(|s| m.modseq > s) {
                    out.extend(fetch_line(i.saturating_add(1), m, &attrs, since.is_some()));
                }
            }
            out.extend(format!("{tag} OK FETCH hecho\r\n").into_bytes());
        }
        "UID STORE" => {
            st.modseq = st.modseq.saturating_add(1);
            let ms = st.modseq;
            let Some(mb) = st.mailboxes.get_mut(mbox) else { return format!("{tag} NO sin buzón\r\n").into_bytes() };
            let ranges = resolve_set(&text(0), max_uid(mb));
            let mut k = 1usize;
            if matches!(args.get(1), Some(Item::List(_))) {
                k = 2;
            }
            let op = text(k).to_ascii_uppercase();
            let flags: BTreeSet<String> = args
                .get(k.saturating_add(1))
                .map(Item::list)
                .unwrap_or_default()
                .iter()
                .filter_map(Item::text)
                .collect();
            let silent = op.ends_with(".SILENT");
            if op.trim_start_matches(['+', '-']).starts_with("X-GM-LABELS") {
                if !caps.gmail {
                    return format!("{tag} BAD sin X-GM-EXT-1\r\n").into_bytes();
                }
                let picked: Vec<(u64, BTreeSet<String>, BTreeSet<String>)> = mb
                    .messages
                    .iter()
                    .filter(|m| in_ranges(&ranges, m.uid) && m.gm_msgid != 0)
                    .map(|m| (m.gm_msgid, m.labels.clone(), m.flags.clone()))
                    .collect();
                for (msgid, mut labels, flags) in picked {
                    if op.starts_with('+') {
                        labels.extend(flags_or_labels(args, k));
                    } else if op.starts_with('-') {
                        let rm = flags_or_labels(args, k);
                        labels.retain(|l| !rm.contains(l));
                    } else {
                        labels = flags_or_labels(args, k);
                    }
                    gmail_apply(&mut st, msgid, &labels, &flags);
                }
                out.extend(format!("{tag} OK STORE hecho\r\n").into_bytes());
                return out;
            }
            let mut gm_changed: Vec<(u64, BTreeSet<String>, BTreeSet<String>)> = Vec::new();
            for (i, m) in mb.messages.iter_mut().enumerate() {
                if !in_ranges(&ranges, m.uid) {
                    continue;
                }
                if op.starts_with('+') {
                    m.flags.extend(flags.iter().cloned());
                } else if op.starts_with('-') {
                    m.flags.retain(|f| !flags.contains(f));
                } else {
                    m.flags = flags.clone();
                }
                m.modseq = ms;
                if m.gm_msgid != 0 {
                    gm_changed.push((m.gm_msgid, m.labels.clone(), m.flags.clone()));
                }
                if !silent {
                    out.extend(fetch_line(i.saturating_add(1), m, &["FLAGS".to_string()], caps.condstore));
                }
            }
            // Gmail: las banderas son del mensaje, no de la copia.
            for (msgid, labels, flags) in gm_changed {
                gmail_apply(&mut st, msgid, &labels, &flags);
            }
            out.extend(format!("{tag} OK STORE hecho\r\n").into_bytes());
        }
        "UID COPY" | "UID MOVE" => {
            if name == "UID MOVE" && !caps.move_ {
                return format!("{tag} BAD MOVE no disponible\r\n").into_bytes();
            }
            let dest = text(1);
            if !st.mailboxes.contains_key(&dest) {
                return format!("{tag} NO [TRYCREATE] no existe\r\n").into_bytes();
            }
            st.modseq = st.modseq.saturating_add(1);
            let ms = st.modseq;
            let Some(src) = st.mailboxes.get(mbox).cloned() else {
                return format!("{tag} NO sin buzón\r\n").into_bytes();
            };
            let ranges = resolve_set(&text(0), max_uid(&src));
            let picked: Vec<FakeMessage> = src.messages.iter().filter(|m| in_ranges(&ranges, m.uid)).cloned().collect();
            let mut src_uids = Vec::new();
            let mut dst_uids = Vec::new();
            let mut dest_v = 0;
            if let Some(d) = st.mailboxes.get_mut(&dest) {
                dest_v = d.uid_validity;
                for m in &picked {
                    src_uids.push(m.uid);
                    let mut flags = m.flags.clone();
                    flags.remove("\\Deleted");
                    let uid = add_message(d, m.raw.clone(), flags, m.internal_date, ms);
                    if let Some(c) = d.messages.iter_mut().find(|c| c.uid == uid) {
                        c.gm_msgid = m.gm_msgid;
                        c.gm_thrid = m.gm_thrid;
                        c.labels = m.labels.clone();
                    }
                    dst_uids.push(uid);
                }
            }
            let copyuid = if caps.uidplus && !src_uids.is_empty() {
                format!(
                    "[COPYUID {dest_v} {} {}] ",
                    SeqSet::from_numbers(src_uids.iter().copied()),
                    SeqSet::from_numbers(dst_uids.iter().copied())
                )
            } else {
                String::new()
            };
            if name == "UID MOVE" {
                if !copyuid.is_empty() {
                    out.extend(format!("* OK {copyuid}movido\r\n").into_bytes());
                }
                if let Some(mb) = st.mailboxes.get_mut(mbox) {
                    expunge_where(mb, &mut out, sess.qresync, ms, |m| src_uids.contains(&m.uid));
                }
                // Gmail: a la Papelera o al Spam el mensaje pierde todas sus etiquetas.
                let to_bin = st
                    .mailboxes
                    .get(&dest)
                    .is_some_and(|d| matches!(d.special_use.as_deref(), Some("\\Trash" | "\\Junk")));
                if caps.gmail && to_bin {
                    let ids: Vec<u64> = picked.iter().map(|m| m.gm_msgid).filter(|g| *g != 0).collect();
                    for (n, mb) in st.mailboxes.iter_mut() {
                        if *n == dest || n == mbox {
                            continue;
                        }
                        let mut sink = Vec::new();
                        expunge_where(mb, &mut sink, true, ms, |m| ids.contains(&m.gm_msgid));
                    }
                }
                out.extend(format!("{tag} OK MOVE hecho\r\n").into_bytes());
            } else {
                out.extend(format!("{tag} OK {copyuid}COPY hecho\r\n").into_bytes());
            }
        }
        "UID EXPUNGE" | "EXPUNGE" => {
            if name == "UID EXPUNGE" && !caps.uidplus {
                return format!("{tag} BAD sin UIDPLUS\r\n").into_bytes();
            }
            st.modseq = st.modseq.saturating_add(1);
            let ms = st.modseq;
            let Some(mb) = st.mailboxes.get_mut(mbox) else { return format!("{tag} NO sin buzón\r\n").into_bytes() };
            let ranges = if name == "UID EXPUNGE" { resolve_set(&text(0), max_uid(mb)) } else { vec![(1, u32::MAX)] };
            expunge_where(mb, &mut out, sess.qresync, ms, |m| {
                m.flags.contains("\\Deleted") && in_ranges(&ranges, m.uid)
            });
            out.extend(format!("{tag} OK EXPUNGE hecho\r\n").into_bytes());
        }
        "UID SEARCH" => {
            let Some(mb) = st.mailboxes.get(mbox) else { return format!("{tag} NO sin buzón\r\n").into_bytes() };
            let mut i = 0usize;
            let mut esearch = false;
            if text(0).eq_ignore_ascii_case("RETURN") {
                esearch = caps.esearch;
                i = 2;
            }
            let mut uid_ranges: Option<Vec<(u32, u32)>> = None;
            let mut since: Option<NaiveDate> = None;
            let mut before: Option<NaiveDate> = None;
            let mut header_eq: Option<(String, String)> = None;
            let mut gm_msgid: Option<u64> = None;
            while let Some(a) = args.get(i) {
                let k = a.text().unwrap_or_default().to_ascii_uppercase();
                match k.as_str() {
                    "UID" => {
                        uid_ranges = Some(resolve_set(&text(i.saturating_add(1)), max_uid(mb)));
                        i = i.saturating_add(2);
                    }
                    "SINCE" => {
                        since = parse_date(&text(i.saturating_add(1)));
                        i = i.saturating_add(2);
                    }
                    "BEFORE" => {
                        before = parse_date(&text(i.saturating_add(1)));
                        i = i.saturating_add(2);
                    }
                    "X-GM-MSGID" => {
                        gm_msgid = args.get(i.saturating_add(1)).and_then(Item::number);
                        i = i.saturating_add(2);
                    }
                    "HEADER" => {
                        header_eq = Some((text(i.saturating_add(1)).to_ascii_lowercase(), text(i.saturating_add(2))));
                        i = i.saturating_add(3);
                    }
                    _ => i = i.saturating_add(1),
                }
            }
            let uids: Vec<u32> = mb
                .messages
                .iter()
                .filter(|m| uid_ranges.as_ref().is_none_or(|r| in_ranges(r, m.uid)))
                .filter(|m| since.is_none_or(|d| m.internal_date.date_naive() >= d))
                .filter(|m| before.is_none_or(|d| m.internal_date.date_naive() < d))
                .filter(|m| gm_msgid.is_none_or(|g| m.gm_msgid == g))
                .filter(|m| {
                    header_eq.as_ref().is_none_or(|(k, v)| {
                        headers(&m.raw).iter().any(|(hk, hv, _)| hk == k && hv.contains(v.as_str()))
                    })
                })
                .map(|m| m.uid)
                .collect();
            if esearch {
                out.extend(format!("* ESEARCH (TAG \"{tag}\") UID").into_bytes());
                if !uids.is_empty() {
                    out.extend(format!(" ALL {}", SeqSet::from_numbers(uids)).into_bytes());
                }
                out.extend_from_slice(b"\r\n");
            } else {
                out.extend_from_slice(b"* SEARCH");
                for u in uids {
                    out.extend(format!(" {u}").into_bytes());
                }
                out.extend_from_slice(b"\r\n");
            }
            out.extend(format!("{tag} OK SEARCH hecho\r\n").into_bytes());
        }
        _ => out.extend(format!("{tag} BAD orden desconocida\r\n").into_bytes()),
    }
    out
}

/// Lista de banderas o etiquetas tras la operación de `STORE` (posición `k`).
fn flags_or_labels(args: &[Item], k: usize) -> BTreeSet<String> {
    args.get(k.saturating_add(1)).map(Item::list).unwrap_or_default().iter().filter_map(Item::text).collect()
}

fn expunge_where(
    mb: &mut FakeMailbox,
    out: &mut Vec<u8>,
    qresync: bool,
    modseq: u64,
    pred: impl Fn(&FakeMessage) -> bool,
) {
    let mut gone = Vec::new();
    let mut i = 0usize;
    while i < mb.messages.len() {
        if mb.messages.get(i).is_some_and(&pred) {
            let m = mb.messages.remove(i);
            if !qresync {
                out.extend(format!("* {} EXPUNGE\r\n", i.saturating_add(1)).into_bytes());
            }
            mb.expunged.push((m.uid, modseq));
            gone.push(m.uid);
        } else {
            i = i.saturating_add(1);
        }
    }
    if qresync && !gone.is_empty() {
        out.extend(format!("* VANISHED {}\r\n", SeqSet::from_numbers(gone)).into_bytes());
    }
}

/// Construye un mensaje RFC 822 sencillo para pruebas.
#[allow(clippy::too_many_arguments)]
pub fn sample_message(
    from: &str,
    to: &str,
    subject: &str,
    message_id: &str,
    in_reply_to: Option<&str>,
    date: DateTime<FixedOffset>,
    body: &str,
    attachment: Option<&str>,
) -> Vec<u8> {
    let mut h = format!(
        "From: {from}\r\nTo: {to}\r\nSubject: {subject}\r\nDate: {}\r\nMessage-ID: <{message_id}>\r\n",
        date.to_rfc2822()
    );
    if let Some(r) = in_reply_to {
        h.push_str(&format!("In-Reply-To: <{r}>\r\nReferences: <{r}>\r\n"));
    }
    h.push_str("MIME-Version: 1.0\r\n");
    match attachment {
        None => h.push_str(&format!("Content-Type: text/plain; charset=utf-8\r\n\r\n{body}\r\n")),
        Some(name) => h.push_str(&format!(
            "Content-Type: multipart/mixed; boundary=\"b\"\r\n\r\n--b\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{body}\r\n--b\r\nContent-Type: application/pdf; name=\"{name}\"\r\nContent-Disposition: attachment; filename=\"{name}\"\r\nContent-Transfer-Encoding: base64\r\n\r\nJVBERi0xLjQK\r\n--b--\r\n"
        )),
    }
    h.into_bytes()
}

/// Error de E/S como [`ImapError`] (para quien arranca el servidor en pruebas).
pub fn io_error(e: std::io::Error) -> ImapError {
    ImapError::Protocol(e.to_string())
}

/// Atajo: `Result` del módulo.
pub type FakeResult<T> = Result<T>;
