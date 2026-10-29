//! F5.3 en la sesión: reglas (al llegar y «Aplicar a la carpeta» con deshacer), Sieve en el
//! servidor, «Posponer», «Recordarme si no responden», Enfocados/Otros, plantillas, baja de listas
//! y respuesta de vacaciones. El motor de todo esto vive en `iuredesk-rules`; aquí solo se cablea
//! con el estado de la sesión, la cola de `sync` y la bandeja de salida.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use iuredesk_compose::TemplateStore;
use iuredesk_model::{AccountId, FolderId, FolderRole, MessageEnvelope, MessageId};
use iuredesk_rules::book::{self, SendLog};
use iuredesk_rules::category::{self, Category, CategoryReason};
use iuredesk_rules::facts::{Facts, Headers};
use iuredesk_rules::focus::Signals;
use iuredesk_rules::followup::{FollowUp, FollowUpBook};
use iuredesk_rules::managesieve;
use iuredesk_rules::sieve::{self, GenCtx};
use iuredesk_rules::snooze::{self, SnoozeBook, SnoozePreset, Snoozed};
use iuredesk_rules::unsubscribe::{self, Method};
use iuredesk_rules::vacation::Vacation;
use iuredesk_rules::{Action, ApplyCtx, Bi, CompiledRules, Effect, FolderRef, Rule, RuleBook, UndoStep};
use iuredesk_sync::NewMailSample;

use super::{Event, FolderSel, Inverse, Notice, NoticeKind, Outcome, Session, store_err};
use crate::compose::ComposeDraft;
use crate::lang::Text;
use crate::{EngineError, JobOutput, Result};

/// Mensajes por llegada que se procesan (el resto, en la siguiente).
const MAX_ARRIVAL: usize = 500;
/// Mensajes por «Aplicar a la carpeta».
const MAX_APPLY: usize = 5_000;
/// Clasificaciones recordadas (con cabeceras) entre sesiones.
const MAX_FOCUS_CACHE: usize = 20_000;
/// Clave de las clasificaciones recordadas.
const FOCUS_KEY: &str = "rules/category";
/// Espera máxima de un aviso de correo nuevo mientras se aplican las reglas.
const HOLD_NOTICE: Duration = Duration::from_secs(20);
/// Cada cuánto se revisan los seguimientos.
const FOLLOWUP_EVERY: Duration = Duration::from_secs(300);
/// Mensajes que «suben» al volver de posponer.
const MAX_WOKE: usize = 200;

fn t(b: Bi) -> Text {
    Text::new(b.en, b.es)
}

/// Comandos de este módulo (`Session::run` los despacha aquí).
pub(crate) fn handles(base: &str) -> bool {
    matches!(
        base,
        "mail.snooze"
            | "mail.unsnooze"
            | "mail.followup"
            | "mail.rule_from_message"
            | "mail.apply_rules"
            | "mail.unsubscribe"
            | "mail.category"
            | "mail.always_category"
            | "mail.iure_only"
            | "app.rules"
            | "app.vacation"
            | "app.labels"
    )
}

/// Los que necesitan un mensaje seleccionado.
pub(crate) fn needs_target(base: &str) -> bool {
    matches!(
        base,
        "mail.snooze"
            | "mail.unsnooze"
            | "mail.followup"
            | "mail.rule_from_message"
            | "mail.unsubscribe"
            | "mail.always_category"
    )
}

/// Tipo de entidad de Iurefficient de una insignia.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IureKind {
    Contact,
    Client,
    Case,
    Opportunity,
    Lead,
    Ticket,
}

impl IureKind {
    /// Nombre para cualquier usuario.
    pub fn label(self) -> Text {
        match self {
            IureKind::Contact => Text::new("Contact", "Contacto"),
            IureKind::Client => Text::new("Client", "Cliente"),
            IureKind::Case => Text::new("Case", "Caso"),
            IureKind::Opportunity => Text::new("Opportunity", "Oportunidad"),
            IureKind::Lead => Text::new("Lead", "Lead"),
            IureKind::Ticket => Text::new("Ticket", "Ticket"),
        }
    }
}

/// Insignia Iurefficient de una fila de la lista.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IureBadge {
    pub kind: IureKind,
    /// «Lead: ACME · $120k».
    pub tip: Text,
}

impl IureBadge {
    fn new(kind: IureKind, what: &str) -> Self {
        let k = kind.label();
        let what: String = what.chars().take(120).collect();
        IureBadge { kind, tip: Text::new(format!("{}: {what}", k.en), format!("{}: {what}", k.es)) }
    }
}

/// «$120k», «$1.5M».
fn money(a: f64) -> String {
    if a >= 1_000_000.0 {
        format!("${:.1}M", a / 1_000_000.0)
    } else if a >= 1_000.0 {
        format!("${:.0}k", a / 1_000.0)
    } else {
        format!("${a:.0}")
    }
}

/// Etiqueta del usuario (no una marca del sistema como `$Forwarded`, `\\Important` o `Junk`).
fn is_user_label(k: &str) -> bool {
    let l = k.to_ascii_lowercase();
    !(k.is_empty()
        || k.starts_with('$')
        || k.starts_with('\\')
        || k.starts_with('[')
        || matches!(
            l.as_str(),
            "junk" | "nonjunk" | "notjunk" | "forwarded" | "inbox" | "important" | "starred" | "sent"
        ))
}

/// Paso inverso propio de este módulo.
#[derive(Clone, Debug)]
pub(crate) enum RulesUndo {
    Keywords { id: MessageId, keywords: Vec<String>, added: String },
    Unsnooze(Vec<MessageId>),
    RestoreRule { pos: usize, rule: Box<Rule> },
    Followup(MessageId),
}

/// Dónde corre una regla.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RulePlace {
    /// En el servidor (aunque la app esté cerrada).
    Server,
    /// En este equipo, mientras IureDesk está abierto (con el motivo, si lo hay).
    Local(Option<Text>),
}

/// Sieve de una cuenta, para la interfaz.
#[derive(Clone, Debug)]
pub struct SieveRow {
    pub account: AccountId,
    pub email: String,
    pub state: iuredesk_rules::SieveAccount,
    pub busy: bool,
}

/// Un aviso de correo nuevo retenido hasta aplicar las reglas.
struct Held {
    account: AccountId,
    count: usize,
    sample: Vec<NewMailSample>,
    since: Instant,
}

enum BgDone {
    Sieve { account: AccountId, result: std::result::Result<SieveResult, Text> },
    Unsubscribed { sender: String, result: std::result::Result<(), Text> },
}

struct SieveResult {
    caps: managesieve::Capabilities,
    host: String,
    uploaded: Option<(Vec<u64>, bool, Option<String>)>,
}

/// Estado de F5.3 en la sesión.
pub(crate) struct RulesState {
    loaded: bool,
    pub book: RuleBook,
    compiled: CompiledRules,
    snoozes: SnoozeBook,
    followups: FollowUpBook,
    due_followups: Vec<FollowUp>,
    last_followup_check: Option<Instant>,
    log: SendLog,
    tab: Category,
    focus_cache: HashMap<MessageId, Category>,
    /// Clasificaciones hechas con cabeceras (se guardan).
    focus_known: Vec<(u64, Category)>,
    focus_dirty: bool,
    /// (total, no leídos) por categoría, en el orden de [`Category::ALL`].
    tab_counts: [(usize, usize); 5],
    /// Pistas del proveedor por cuenta (Gmail, Microsoft 365).
    hints: HashMap<AccountId, std::collections::BTreeMap<u64, String>>,
    hints_at: Option<Instant>,
    /// Filtro rápido «Solo Iurefficient».
    iure_only: bool,
    badges: HashMap<MessageId, Option<IureBadge>>,
    badge_gen: (u64, usize),
    /// Etiquetas vistas en la lista (para Ajustes → Etiquetas).
    seen_labels: std::collections::BTreeSet<String>,
    signals: Option<Signals>,
    woke: Vec<MessageId>,
    held: Vec<Held>,
    silenced: HashSet<MessageId>,
    tx: Sender<BgDone>,
    rx: Receiver<BgDone>,
    sieve_busy: HashSet<AccountId>,
    detected: bool,
}

impl Default for RulesState {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self {
            loaded: false,
            book: RuleBook::default(),
            compiled: CompiledRules::default(),
            snoozes: SnoozeBook::default(),
            followups: FollowUpBook::default(),
            due_followups: Vec::new(),
            last_followup_check: None,
            log: SendLog::default(),
            tab: Category::Primary,
            focus_cache: HashMap::new(),
            focus_known: Vec::new(),
            focus_dirty: false,
            tab_counts: [(0, 0); 5],
            hints: HashMap::new(),
            hints_at: None,
            iure_only: false,
            badges: HashMap::new(),
            badge_gen: (u64::MAX, 0),
            seen_labels: std::collections::BTreeSet::new(),
            signals: None,
            woke: Vec::new(),
            held: Vec::new(),
            silenced: HashSet::new(),
            tx,
            rx,
            sieve_busy: HashSet::new(),
            detected: false,
        }
    }
}

impl Session {
    // ------------------------------------------------------------------ carga y guardado

    fn rules_ensure(&mut self) {
        if self.rules.loaded {
            return;
        }
        let r = &mut self.rules;
        r.loaded = true;
        r.book = RuleBook::load(&self.store);
        r.compiled = CompiledRules::new(&r.book.rules);
        r.snoozes = SnoozeBook::load(&self.store);
        r.followups = FollowUpBook::load(&self.store);
        r.log = SendLog::load(&self.store);
        r.focus_known = book::load(&self.store, FOCUS_KEY).unwrap_or_default();
        r.focus_cache = r.focus_known.iter().map(|(id, f)| (MessageId(*id), *f)).collect();
    }

    fn rules_save(&mut self) {
        let r = self.rules.book.save(&self.store);
        self.rules.compiled = CompiledRules::new(&self.rules.book.rules);
        if let Err(e) = r {
            self.notices.push(Notice::error(t(e.text())));
        }
        self.sieve_resync();
    }

    fn rules_err(e: iuredesk_rules::RulesError) -> EngineError {
        EngineError::Invalid(if crate::lang::spanish() { e.text().es } else { e.text().en })
    }

    // ------------------------------------------------------------------ reglas

    /// Reglas y preferencias.
    pub fn rule_book(&mut self) -> &RuleBook {
        self.rules_ensure();
        &self.rules.book
    }

    /// Guarda (o crea) una regla; devuelve su id.
    pub fn save_rule(&mut self, rule: Rule) -> Result<u64> {
        self.rules_ensure();
        let id = self.rules.book.upsert(rule).map_err(Self::rules_err)?;
        self.rules_save();
        Ok(id)
    }

    /// Quita una regla (con «Deshacer»).
    pub fn delete_rule(&mut self, id: u64) -> Outcome {
        self.rules_ensure();
        let Some((pos, rule)) = self.rules.book.remove(id) else { return Outcome::Done };
        self.rules_save();
        let text = Text::new(format!("Rule “{}” deleted", rule.name), format!("Regla «{}» eliminada", rule.name));
        self.push_undo(
            text.clone(),
            vec![(Inverse::Rules(Box::new(RulesUndo::RestoreRule { pos, rule: Box::new(rule) })), None)],
        );
        Outcome::Notice(Notice { kind: NoticeKind::Success, text, undo: true, secs: None })
    }

    /// Sube o baja una regla en el orden.
    pub fn move_rule(&mut self, id: u64, delta: i32) {
        self.rules_ensure();
        self.rules.book.shift(id, delta);
        self.rules_save();
    }

    /// Activa o desactiva una regla.
    pub fn set_rule_enabled(&mut self, id: u64, on: bool) {
        self.rules_ensure();
        if let Some(r) = self.rules.book.rules.iter_mut().find(|r| r.id == id) {
            r.enabled = on;
        }
        self.rules_save();
    }

    /// Regla propuesta a partir del mensaje enfocado.
    pub fn rule_from_message(&mut self, id: MessageId) -> Option<Rule> {
        let env = self.envelope(id)?.clone();
        let list = self
            .store
            .get_raw(id)
            .ok()
            .map(|raw| Headers::parse(&raw))
            .and_then(|h| h.get("list-id").map(str::to_string));
        Some(Rule::from_message(&env, list.as_deref()))
    }

    /// Carpetas para «Moverlo a…» (de todas las cuentas, con su cuenta).
    pub fn rule_folders(&self) -> Vec<(String, FolderRef)> {
        let mut out = Vec::new();
        for l in &self.labels {
            let mut fs: Vec<&iuredesk_model::Folder> = self.folders.iter().filter(|f| f.account == l.id).collect();
            fs.sort_by(|a, b| {
                super::role_rank(a.role).cmp(&super::role_rank(b.role)).then_with(|| a.name.cmp(&b.name))
            });
            for f in fs {
                out.push((
                    l.email.clone(),
                    FolderRef { id: f.id, account: f.account, path: f.remote_path.clone(), name: f.name.clone() },
                ));
            }
        }
        out
    }

    /// Dónde corre una regla.
    pub fn rule_place(&mut self, rule: &Rule) -> RulePlace {
        self.rules_ensure();
        let accounts: Vec<AccountId> = match rule.account {
            Some(a) => vec![a],
            None => self.labels.iter().map(|l| l.id).collect(),
        };
        if accounts.is_empty() {
            return RulePlace::Local(None);
        }
        let on_server = accounts.iter().all(|a| self.rules.book.server_side(*a).contains(&rule.id));
        if on_server {
            return RulePlace::Server;
        }
        // Motivo de la primera cuenta con Sieve activo que no la pudo subir.
        for a in accounts {
            if let Some(s) = self.rules.book.sieve_of(a).filter(|s| s.enabled && s.available).cloned() {
                let ctx = self.gen_ctx(a, &s.extensions);
                let script = sieve::to_sieve(std::slice::from_ref(rule), None, &ctx);
                if let Some((_, why)) = script.local.into_iter().next() {
                    return RulePlace::Local(Some(t(why)));
                }
            }
        }
        RulePlace::Local(None)
    }

    /// Aplica las reglas a una carpeta ahora («Aplicar a la carpeta»), con «Deshacer». No envía
    /// correo (reenviar o responder solo actúa sobre lo que llega).
    pub fn apply_rules_to_folder(&mut self, folder: FolderId, only: Option<u64>) -> Result<Outcome> {
        self.rules_ensure();
        let rules: Vec<Rule> =
            self.rules.book.rules.iter().filter(|r| only.is_none_or(|o| o == r.id)).cloned().collect();
        let compiled = CompiledRules::new(&rules);
        if compiled.is_empty() {
            return Ok(Outcome::Notice(Notice::info("There are no active rules.", "No hay reglas activas.")));
        }
        let ids = self.store.list_ids(folder).map_err(store_err)?;
        let signals = self.focus_signals();
        let my = self.my_addresses();
        let idents = self.identities();
        let mut log = std::mem::take(&mut self.rules.log);
        let mut steps = Vec::new();
        let mut changed = 0usize;
        let mut kick = HashSet::new();
        let none = HashSet::new();
        for id in ids.into_iter().take(MAX_APPLY) {
            let Some(env) = self.store.get_envelope(id).map_err(store_err)? else { continue };
            let mut facts = self.rules_facts(env);
            facts.known_sender = signals.contacts.contains(&facts.sender());
            let plan = compiled.plan(&facts, &none);
            if plan.is_empty() {
                continue;
            }
            let ctx = ApplyCtx {
                store: &self.store,
                folders: &self.folders,
                my_addresses: &my,
                identities: &idents,
                now: self.now(),
                allow_send: false,
                spanish: crate::lang::spanish(),
            };
            match iuredesk_rules::apply_plan(&ctx, &facts, &plan, &mut log) {
                Ok(a) => {
                    if a.changed {
                        changed += 1;
                        kick.insert(facts.env.account);
                    }
                    steps.extend(a.undo.into_iter().map(undo_step));
                }
                Err(e) => self.notices.push(Notice::error(t(e.text()))),
            }
        }
        self.rules.log = log;
        for a in kick {
            self.kick(a);
        }
        self.envelopes.clear();
        self.counts.clear();
        self.rebuild_rows();
        if changed == 0 {
            return Ok(Outcome::Notice(Notice::info(
                "No message in this folder matches your rules.",
                "Ningún mensaje de esta carpeta cumple tus reglas.",
            )));
        }
        let text = Text::new(
            if changed == 1 {
                "Rules applied to 1 message".to_string()
            } else {
                format!("Rules applied to {changed} messages")
            },
            if changed == 1 {
                "Reglas aplicadas a 1 mensaje".to_string()
            } else {
                format!("Reglas aplicadas a {changed} mensajes")
            },
        );
        self.push_undo(text.clone(), steps);
        Ok(Outcome::Notice(Notice { kind: NoticeKind::Success, text, undo: true, secs: None }))
    }

    /// Hechos de un mensaje (con el crudo si ya está en el equipo).
    fn rules_facts(&self, env: MessageEnvelope) -> Facts {
        match self.store.get_raw(env.id) {
            Ok(raw) => Facts::from_raw(env, &raw),
            Err(_) => Facts::from_envelope(env),
        }
    }

    pub(crate) fn undo_rules(&mut self, u: RulesUndo) -> Result<()> {
        self.rules_ensure();
        match u {
            RulesUndo::Keywords { id, keywords, added } => {
                iuredesk_rules::apply::undo_keywords(&self.store, id, keywords, &added).map_err(Self::rules_err)?;
                self.envelopes.remove(&id);
            }
            RulesUndo::Unsnooze(ids) => {
                for id in ids {
                    if let Some(s) =
                        snooze::unsnooze(&self.store, &mut self.rules.snoozes, id).map_err(Self::rules_err)?
                    {
                        self.kick(s.account);
                    }
                    self.envelopes.remove(&id);
                }
                let _ = self.rules.snoozes.save(&self.store);
            }
            RulesUndo::RestoreRule { pos, rule } => {
                let mut rules = std::mem::take(&mut self.rules.book.rules);
                rules.insert(pos.min(rules.len()), *rule);
                self.rules.book.rules = rules;
                self.rules_save();
            }
            RulesUndo::Followup(id) => {
                self.rules.followups.remove(id);
                let _ = self.rules.followups.save(&self.store);
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------------ llegada de correo

    /// ¿Retener el aviso de correo nuevo hasta aplicar reglas y Enfocados/Otros?
    pub(crate) fn rules_hold_newmail(&mut self, account: AccountId, count: usize, sample: Vec<NewMailSample>) -> bool {
        self.rules_ensure();
        if self.rules.compiled.is_empty() && !self.rules.book.categories.enabled {
            return false;
        }
        self.rules.held.push(Held { account, count, sample, since: Instant::now() });
        true
    }

    fn newmail_event(&self, account: AccountId, count: usize, sample: &[NewMailSample]) -> Event {
        let who = self.labels.iter().find(|l| l.id == account).map(|l| l.email.clone()).unwrap_or_default();
        let summary = if count == 1 {
            Text::new("New message", "Correo nuevo")
        } else {
            Text::new(format!("{count} new messages"), format!("{count} correos nuevos"))
        };
        let detail =
            sample.iter().take(3).map(|s| format!("{} — {}", s.from, s.subject)).collect::<Vec<_>>().join("\n");
        Event::NewMail { summary, detail, account: who }
    }

    /// Correo recién llegado: reglas, respuesta de vacaciones, Enfocados/Otros y el aviso.
    pub(crate) fn on_arrived(&mut self, account: AccountId, ids: Vec<MessageId>, out: &mut Vec<Event>) {
        self.rules_ensure();
        self.rules.hints_at = None;
        self.reload_hints();
        let signals = self.focus_signals();
        let my = self.my_addresses();
        let idents = self.identities();
        let skip = self.rules.book.server_side(account);
        let vacation = self.rules.book.vacation.clone();
        let local_vacation = vacation.active_on(self.today())
            && !self
                .rules
                .book
                .sieve_of(account)
                .is_some_and(|s| s.enabled && s.server_vacation && s.synced_at.is_some());
        let mut log = std::mem::take(&mut self.rules.log);
        let mut quiet: HashSet<MessageId> = HashSet::new();
        let mut loud: HashSet<MessageId> = HashSet::new();
        let mut sends: Vec<(AccountId, iuredesk_compose::Draft, Bi)> = Vec::new();
        let mut kick = false;
        for id in ids.iter().copied().take(MAX_ARRIVAL) {
            let Ok(Some(env)) = self.store.get_envelope(id) else { continue };
            let mut facts = self.rules_facts(env);
            facts.known_sender = signals.contacts.contains(&facts.sender());
            let hint = self.category_hint(account, id);
            let (f, _) = category::classify(
                &facts.env,
                &facts.headers,
                &signals,
                &self.rules.book.categories.overrides,
                hint.as_deref(),
            );
            self.rules.focus_cache.insert(id, f);
            if !facts.headers.is_empty() || hint.is_some() {
                self.rules.focus_known.push((id.get(), f));
                self.rules.focus_dirty = true;
            }
            // Como en Gmail: solo avisa lo de Principal.
            if f != Category::Primary && self.rules.book.categories.enabled {
                quiet.insert(id);
            }
            let mut plan = self.rules.compiled.plan(&facts, &skip);
            if local_vacation && (!vacation.contacts_only || facts.known_sender) {
                plan.actions.push((
                    0,
                    Action::AutoReply {
                        subject: vacation.subject.clone(),
                        body: vacation.body.clone(),
                        days: vacation.days,
                    },
                ));
            }
            if plan.is_empty() {
                continue;
            }
            let ctx = ApplyCtx {
                store: &self.store,
                folders: &self.folders,
                my_addresses: &my,
                identities: &idents,
                now: self.now(),
                allow_send: true,
                spanish: crate::lang::spanish(),
            };
            match iuredesk_rules::apply_plan(&ctx, &facts, &plan, &mut log) {
                Ok(a) => {
                    kick |= a.changed;
                    if a.moved {
                        quiet.insert(id);
                    }
                    for e in a.effects {
                        match e {
                            Effect::Send { account, draft, what } => sends.push((account, *draft, what)),
                            Effect::Notify(m) => {
                                loud.insert(m);
                            }
                            Effect::Silence(m) => {
                                quiet.insert(m);
                                self.rules.silenced.insert(m);
                            }
                        }
                    }
                }
                Err(e) => self.notices.push(Notice::error(t(e.text()))),
            }
        }
        self.rules.log = log;
        let _ = self.rules.log.save(&self.store);
        self.save_focus_known();
        for (acc, draft, what) in sends {
            self.queue_auto(acc, draft, what);
        }
        if kick {
            self.kick(account);
        }
        // El aviso retenido, sin lo que las reglas apartaron (salvo «Avisarme siempre»).
        if let Some(pos) = self.rules.held.iter().position(|h| h.account == account) {
            let h = self.rules.held.remove(pos);
            let arrived: HashSet<MessageId> = ids.iter().copied().collect();
            let dropped = arrived.iter().filter(|m| quiet.contains(m) && !loud.contains(m)).count();
            let count = h.count.saturating_sub(dropped);
            let sample: Vec<NewMailSample> =
                h.sample.into_iter().filter(|s| !quiet.contains(&s.message) || loud.contains(&s.message)).collect();
            if count > 0 {
                out.push(self.newmail_event(account, count, &sample));
            }
        }
        self.live.dirty = true;
        self.counts.clear();
    }

    fn save_focus_known(&mut self) {
        if !self.rules.focus_dirty {
            return;
        }
        self.rules.focus_dirty = false;
        let k = &mut self.rules.focus_known;
        if k.len() > MAX_FOCUS_CACHE {
            let extra = k.len() - MAX_FOCUS_CACHE;
            k.drain(..extra);
        }
        let _ = book::save(&self.store, FOCUS_KEY, k);
    }

    fn queue_auto(&mut self, account: AccountId, draft: iuredesk_compose::Draft, what: Bi) {
        let Some(sender) = self.sender.as_ref() else { return };
        match sender.outbox.queue(account, &draft, &iuredesk_compose::FsResolver) {
            Ok(_) => self.notices.push(Notice::info(what.en, what.es)),
            Err(e) => self.notices.push(Notice::error(Text::new(
                format!("An automatic message could not be queued: {e}"),
                format!("No se pudo preparar un envío automático: {e}"),
            ))),
        }
    }

    // ------------------------------------------------------------------ Enfocados / Otros

    fn focus_signals(&mut self) -> Signals {
        if let Some(s) = &self.rules.signals {
            return s.clone();
        }
        let mut s = Signals::default();
        for a in self.my_addresses() {
            s.add_mine(&a);
        }
        // Directo del almacén: no se llena la caché de contactos de la sesión antes de tiempo.
        let mut contacts: Vec<String> = Vec::new();
        for b in self.store.address_books().unwrap_or_default() {
            for c in self.store.contacts(b.id).unwrap_or_default() {
                contacts.extend(c.emails.iter().map(|e| iuredesk_rules::norm_addr(&e.value)));
            }
        }
        s.contacts.extend(contacts);
        let sent: Vec<FolderId> = self.folders.iter().filter(|f| f.role == FolderRole::Sent).map(|f| f.id).collect();
        for f in sent {
            for e in self.store.list_envelopes(f, 0, 2_000).unwrap_or_default() {
                for m in e.to.iter().chain(e.cc.iter()) {
                    s.replied.insert(iuredesk_rules::norm_addr(&m.addr));
                }
            }
        }
        self.rules.signals = Some(s.clone());
        s
    }

    /// La vista actual es una bandeja de entrada (donde hay pestañas).
    pub fn focus_view(&self) -> bool {
        self.search.trim().is_empty()
            && match self.folder {
                FolderSel::Unified(FolderRole::Inbox) => true,
                FolderSel::Folder(f) => self.folder(f).is_some_and(|f| f.role == FolderRole::Inbox),
                _ => false,
            }
    }

    /// Relee las pistas de categoría del proveedor (como mucho cada 30 s).
    fn reload_hints(&mut self) {
        if self.rules.hints_at.is_some_and(|t| t.elapsed() < Duration::from_secs(30)) {
            return;
        }
        self.rules.hints_at = Some(Instant::now());
        let mut changed = false;
        let ids: Vec<AccountId> = self.labels.iter().map(|l| l.id).collect();
        for a in ids {
            let m = iuredesk_sync::category_hint::load(&self.store, a);
            if self.rules.hints.get(&a).is_none_or(|old| old.len() != m.len()) {
                changed = true;
            }
            self.rules.hints.insert(a, m);
        }
        if changed {
            let known = &self.rules.focus_known;
            self.rules.focus_cache = known.iter().map(|(id, f)| (MessageId(*id), *f)).collect();
        }
    }

    fn category_hint(&self, account: AccountId, id: MessageId) -> Option<String> {
        self.rules.hints.get(&account).and_then(|m| m.get(&id.get()).cloned())
    }

    /// Las pestañas de categorías están activas en esta vista.
    pub fn category_tabs_shown(&mut self) -> bool {
        self.rules_ensure();
        self.rules.book.categories.enabled && self.focus_view()
    }

    /// Pestaña actual.
    pub fn category_tab(&self) -> Category {
        self.rules.tab
    }

    /// (total, no leídos) por categoría, en el orden de [`Category::ALL`].
    pub fn category_counts(&self) -> [(usize, usize); 5] {
        self.rules.tab_counts
    }

    /// Cambia de pestaña.
    pub fn set_category_tab(&mut self, c: Category) {
        if self.rules.tab != c {
            self.rules.tab = c;
            self.selection.clear();
            self.focused = None;
            self.rebuild_rows();
        }
    }

    /// Activa o desactiva las categorías (desactivadas: una sola bandeja).
    pub fn set_categories_enabled(&mut self, on: bool) {
        self.rules_ensure();
        self.rules.book.categories.enabled = on;
        if let Err(e) = self.rules.book.save(&self.store) {
            self.notices.push(Notice::error(t(e.text())));
        }
        self.rebuild_rows();
    }

    /// Categoría de un mensaje y por qué.
    pub fn category_of(&mut self, id: MessageId) -> Option<(Category, CategoryReason)> {
        self.rules_ensure();
        self.reload_hints();
        let env = self.store.get_envelope(id).ok().flatten()?;
        let signals = self.focus_signals();
        let hint = self.category_hint(env.account, id);
        let facts = self.rules_facts(env);
        Some(category::classify(
            &facts.env,
            &facts.headers,
            &signals,
            &self.rules.book.categories.overrides,
            hint.as_deref(),
        ))
    }

    /// Categoría ya calculada (sin E/S; para chips de la lista).
    pub fn category_cached(&self, id: MessageId) -> Option<Category> {
        self.rules.focus_cache.get(&id).copied()
    }

    /// «Mover siempre a <categoría>» para el remitente del mensaje enfocado.
    fn always(&mut self, c: Category) -> Result<Outcome> {
        self.rules_ensure();
        let id = self.focused.or_else(|| self.selection.first().copied()).ok_or(EngineError::NoSelection)?;
        let sender =
            self.envelope(id).and_then(|e| e.from.first().map(|m| m.addr.clone())).ok_or(EngineError::NoSelection)?;
        self.rules.book.set_category_override(&sender, Some(c));
        self.rules.focus_cache.clear();
        self.rules.focus_known.clear();
        self.rules.focus_dirty = true;
        self.save_focus_known();
        if let Err(e) = self.rules.book.save(&self.store) {
            self.notices.push(Notice::error(t(e.text())));
        }
        self.rebuild_rows();
        let tab = t(c.label());
        Ok(Outcome::Notice(Notice::success(
            format!("Mail from {sender} will always go to {}", tab.en),
            format!("El correo de {sender} irá siempre a {}", tab.es),
        )))
    }

    /// Hay sesión de Iurefficient (insignias y «Solo Iurefficient»).
    pub fn iure_panel_visible(&self) -> bool {
        self.iure_visible()
    }

    /// ¿Activo «Solo Iurefficient»?
    pub fn iure_only(&self) -> bool {
        self.rules.iure_only
    }

    /// Activa o quita «Solo Iurefficient».
    pub fn set_iure_only(&mut self, on: bool) {
        if self.rules.iure_only != on {
            self.rules.iure_only = on;
            self.selection.clear();
            self.focused = None;
            self.rebuild_rows();
        }
    }

    /// Filtro de la lista: oculta lo pospuesto, separa por categorías, «Solo Iurefficient» y sube
    /// lo que volvió de posponer. Todo local (sin red).
    pub(crate) fn rules_filter_order(&mut self, order: Vec<MessageId>) -> Vec<MessageId> {
        self.rules_ensure();
        let hidden: HashSet<MessageId> =
            self.rules.snoozes.items.iter().filter(|s| s.mode == snooze::SnoozeMode::Hidden).map(|s| s.msg).collect();
        let mut out: Vec<MessageId> =
            if hidden.is_empty() { order } else { order.into_iter().filter(|m| !hidden.contains(m)).collect() };
        if self.rules.iure_only {
            let mut keep = Vec::new();
            for id in out.into_iter().take(MAX_APPLY) {
                if self.iure_badge(id).is_some() {
                    keep.push(id);
                }
            }
            out = keep;
        }
        if !self.focus_view() {
            return out;
        }
        if self.rules.book.categories.enabled {
            self.reload_hints();
            let signals = self.focus_signals();
            let overrides = self.rules.book.categories.overrides.clone();
            let mut counts = [(0usize, 0usize); 5];
            let tab = self.rules.tab;
            let mut keep = Vec::with_capacity(out.len());
            for (i, id) in out.into_iter().enumerate() {
                // Más allá de este tope no se leen sobres: cuenta como Principal si no se sabe.
                let env = if i < 5_000 { self.store.get_envelope(id).ok().flatten() } else { None };
                let c = match (self.rules.focus_cache.get(&id), &env) {
                    (Some(c), _) => *c,
                    (None, Some(e)) => {
                        let hint = self.category_hint(e.account, id);
                        let c = category::classify(e, &Headers::default(), &signals, &overrides, hint.as_deref()).0;
                        self.rules.focus_cache.insert(id, c);
                        c
                    }
                    (None, None) => Category::Primary,
                };
                let k = Category::ALL.iter().position(|x| *x == c).unwrap_or(0);
                if let Some(slot) = counts.get_mut(k) {
                    slot.0 += 1;
                    if env.as_ref().is_some_and(|e| !e.flags.seen) {
                        slot.1 += 1;
                    }
                }
                if c == tab {
                    keep.push(id);
                }
            }
            self.rules.tab_counts = counts;
            out = keep;
        }
        // Lo que volvió de posponer va arriba hasta que se lea.
        if !self.rules.woke.is_empty() {
            let store = &self.store;
            self.rules.woke.retain(|m| store.get_envelope(*m).ok().flatten().is_some_and(|e| !e.flags.seen));
            let woke: Vec<MessageId> = self.rules.woke.iter().copied().filter(|m| out.contains(m)).collect();
            if !woke.is_empty() {
                out.retain(|m| !woke.contains(m));
                let mut v = woke;
                v.extend(out);
                out = v;
            }
        }
        out
    }

    // ------------------------------------------------------------------ etiquetas

    /// Etiquetas de un mensaje con su color (las de Gmail, las categorías de Outlook y las
    /// propias), más la categoría si no es Principal. Sin E/S.
    pub fn message_labels(&mut self, env: &MessageEnvelope) -> Vec<(String, u32)> {
        self.rules_ensure();
        let mut out: Vec<(String, u32)> = Vec::new();
        if let Some(c) = self.rules.focus_cache.get(&env.id).copied().filter(|c| *c != Category::Primary) {
            out.push((t(c.label()).get().to_string(), c.color()));
        }
        for k in env.keywords.iter().take(12) {
            if !is_user_label(k) {
                continue;
            }
            let name = k.rsplit('/').next().unwrap_or(k).to_string();
            if self.rules.seen_labels.len() < 2_000 {
                self.rules.seen_labels.insert(k.clone());
            }
            let color = self.rules.book.label_color(k);
            out.push((name, color));
        }
        out
    }

    /// Pone una etiqueta a un mensaje (local primero; palabra clave IMAP en el servidor).
    pub fn add_label(&mut self, id: MessageId, name: &str) -> Result<()> {
        let mut env = self.store.get_envelope(id).map_err(store_err)?.ok_or(EngineError::NoSelection)?;
        let name = name.trim();
        if name.is_empty() || env.keywords.iter().any(|k| k.eq_ignore_ascii_case(name)) {
            return Ok(());
        }
        env.keywords.push(name.to_string());
        let account = env.account;
        let seq = iuredesk_rules::apply::put_keywords(&self.store, env, true, name).map_err(Self::rules_err)?;
        if seq.is_some() {
            self.kick(account);
        }
        self.envelopes.remove(&id);
        Ok(())
    }

    /// Etiquetas conocidas (vistas en la lista o con color elegido), con su color.
    pub fn known_labels(&mut self) -> Vec<(String, u32)> {
        self.rules_ensure();
        let mut names: std::collections::BTreeSet<String> = self.rules.seen_labels.clone();
        names.extend(self.rules.book.label_colors.keys().cloned());
        let mut v: Vec<(String, u32)> = names
            .into_iter()
            .map(|n| {
                let c = self.rules.book.label_color(&n);
                (n, c)
            })
            .collect();
        v.dedup_by(|a, b| a.0.eq_ignore_ascii_case(&b.0));
        v
    }

    /// Cambia el color de una etiqueta.
    pub fn set_label_color(&mut self, name: &str, color: u32) {
        self.rules_ensure();
        self.rules.book.label_colors.insert(name.to_lowercase(), color & 0xFF_FFFF);
        if let Err(e) = self.rules.book.save(&self.store) {
            self.notices.push(Notice::error(t(e.text())));
        }
    }

    // ------------------------------------------------------------------ insignia Iurefficient

    /// Insignia Iurefficient de un mensaje (vínculo guardado, contexto ya conocido de la
    /// instancia o número de caso en el asunto). En caché; nunca consulta la red.
    pub fn iure_badge(&mut self, id: MessageId) -> Option<IureBadge> {
        let current = self.iure_badge_gen();
        if current != self.rules.badge_gen {
            self.rules.badges.clear();
            self.rules.badge_gen = current;
        }
        if let Some(b) = self.rules.badges.get(&id) {
            return b.clone();
        }
        let b = self.compute_badge(id);
        if self.rules.badges.len() > 50_000 {
            self.rules.badges.clear();
        }
        self.rules.badges.insert(id, b.clone());
        b
    }

    fn compute_badge(&mut self, id: MessageId) -> Option<IureBadge> {
        if !self.iure_visible() {
            return None;
        }
        let env = self.store.get_envelope(id).ok().flatten()?;
        if let Some(mid) = env.message_id.as_deref().filter(|m| !m.trim().is_empty())
            && let Some(l) = self.store.email_links(mid).ok().and_then(|v| v.into_iter().last())
        {
            let kind = match l.kind.as_str() {
                "case" | "task" => IureKind::Case,
                "client" => IureKind::Client,
                "lead" => IureKind::Lead,
                "opportunity" => IureKind::Opportunity,
                "ticket" => IureKind::Ticket,
                _ => IureKind::Contact,
            };
            return Some(IureBadge::new(kind, &l.label));
        }
        if let Some(ctx) = self.iure_known_context(&env) {
            if let Some(d) = ctx.deals.first() {
                let kind = if d.kind == crate::iure::TargetKind::Lead { IureKind::Lead } else { IureKind::Opportunity };
                let mut what = d.organization.clone().unwrap_or_else(|| d.name.clone());
                if let Some(a) = d.amount {
                    what.push_str(&format!(" · {}", money(a)));
                }
                return Some(IureBadge::new(kind, &what));
            }
            if let Some(c) = ctx.cases.first() {
                return Some(IureBadge::new(IureKind::Case, &c.title));
            }
            if let Some(tk) = ctx.tickets.first() {
                return Some(IureBadge::new(IureKind::Ticket, &format!("#{} {}", tk.number, tk.title)));
            }
            if let Some(p) = ctx.people.first() {
                let kind = match p.kind {
                    crate::iure::PersonKind::Client => IureKind::Client,
                    crate::iure::PersonKind::Lead => IureKind::Lead,
                };
                return Some(IureBadge::new(kind, &p.name));
            }
        }
        crate::context::case_refs(&env.subject).into_iter().next().map(|r| IureBadge::new(IureKind::Case, &r))
    }

    // ------------------------------------------------------------------ posponer

    /// Opciones del menú «Posponer» con la hora a la que vuelve («mañana 8:00»).
    pub fn snooze_menu(&self) -> Vec<(String, Text, String)> {
        let tz = crate::time::user_tz(self.zone);
        let now = self.now().with_timezone(&tz);
        SnoozePreset::MENU
            .iter()
            .filter_map(|p| {
                let at = p.resolve(&now)?;
                Some((p.key(), t(p.label()), self.snooze_when(at)))
            })
            .collect()
    }

    /// «hoy 18:00», «sáb 9:00», «lun 26 oct 8:00».
    pub fn snooze_when(&self, at: DateTime<Utc>) -> String {
        let local = self.zone.local(at);
        let today = self.today();
        let d = local.date();
        let hm = local.format("%H:%M").to_string();
        if d == today {
            format!("{} {hm}", crate::lang::pick("today", "hoy"))
        } else if d.signed_duration_since(today).num_days() < 7 {
            format!("{} {hm}", crate::time::weekday_short(d))
        } else {
            format!("{} {} {hm}", crate::time::weekday_short(d), d.format("%d/%m"))
        }
    }

    /// Pospone la selección.
    pub fn snooze_selected(&mut self, preset: SnoozePreset) -> Result<Outcome> {
        self.rules_ensure();
        let tz = crate::time::user_tz(self.zone);
        let Some(until) = preset.resolve(&self.now().with_timezone(&tz)) else {
            return Ok(Outcome::Notice(Notice::info("Pick a later time.", "Elige una hora posterior.")));
        };
        let ids = self.targets();
        // F7.1: «Pospuestos» en el servidor (se crea si falta) para que se vea en otros equipos.
        let mut seen = HashSet::new();
        for id in ids.iter().copied().take(super::MAX_BATCH) {
            if let Some(a) = self.store.get_envelope(id).ok().flatten().map(|e| e.account)
                && seen.insert(a)
            {
                self.ensure_snooze_folder(a);
            }
        }
        let mut done = Vec::new();
        let mut accounts = HashSet::new();
        for id in ids.iter().copied().take(super::MAX_BATCH) {
            match snooze::snooze(&self.store, &mut self.rules.snoozes, &self.folders, id, until) {
                Ok((s, _)) => {
                    accounts.insert(s.account);
                    done.push(id);
                    self.rules.woke.retain(|m| *m != id);
                }
                Err(e) => self.notices.push(Notice::error(t(e.text()))),
            }
        }
        if let Err(e) = self.rules.snoozes.save(&self.store) {
            self.notices.push(Notice::error(t(e.text())));
        }
        for a in accounts {
            self.kick(a);
        }
        if done.is_empty() {
            return Ok(Outcome::Done);
        }
        let when = self.snooze_when(until);
        let text = Text::new(
            if done.len() == 1 {
                format!("Snoozed until {when}")
            } else {
                format!("{} snoozed until {when}", done.len())
            },
            if done.len() == 1 {
                format!("Pospuesto hasta {when}")
            } else {
                format!("{} pospuestos hasta {when}", done.len())
            },
        );
        self.push_undo(text.clone(), vec![(Inverse::Rules(Box::new(RulesUndo::Unsnooze(done.clone()))), None)]);
        self.after_removal(&done);
        Ok(Outcome::Notice(Notice { kind: NoticeKind::Success, text, undo: true, secs: None }))
    }

    /// Lo pospuesto, por hora de vuelta.
    pub fn snoozed(&mut self) -> Vec<Snoozed> {
        self.rules_ensure();
        self.rules.snoozes.items.clone()
    }

    /// ¿Está pospuesto?
    pub fn is_snoozed(&self, id: MessageId) -> bool {
        self.rules.snoozes.get(id).is_some()
    }

    /// Quita la posposición de la selección.
    fn unsnooze_selected(&mut self) -> Result<Outcome> {
        self.rules_ensure();
        let ids = self.targets();
        let mut n = 0;
        for id in ids {
            if let Some(s) = snooze::unsnooze(&self.store, &mut self.rules.snoozes, id).map_err(Self::rules_err)? {
                self.kick(s.account);
                n += 1;
            }
        }
        let _ = self.rules.snoozes.save(&self.store);
        self.envelopes.clear();
        self.rebuild_rows();
        Ok(Outcome::Notice(if n == 0 {
            Notice::info("This message is not snoozed.", "Este mensaje no está pospuesto.")
        } else {
            Notice::success("Back in your inbox", "De vuelta en tu bandeja")
        }))
    }

    // ------------------------------------------------------------------ seguimientos

    /// «Recordarme si no responden en N días» sobre el mensaje enfocado (enviado por mí).
    pub fn follow_up(&mut self, days: u32) -> Result<Outcome> {
        self.rules_ensure();
        let id = self.focused.ok_or(EngineError::NoSelection)?;
        let env = self.envelope(id).cloned().ok_or(EngineError::NoSelection)?;
        let my = self.my_addresses();
        let mine = env.from.first().is_some_and(|m| my.iter().any(|a| a.eq_ignore_ascii_case(&m.addr)));
        let in_sent = self.folder(env.folder).is_some_and(|f| f.role == FolderRole::Sent);
        if !mine && !in_sent {
            return Ok(Outcome::Notice(Notice::info(
                "Follow-up reminders are for messages you sent.",
                "Los recordatorios de seguimiento son para mensajes que enviaste.",
            )));
        }
        let now = self.now();
        self.rules.followups.add(&env, days, now).map_err(Self::rules_err)?;
        let _ = self.rules.followups.save(&self.store);
        let text = Text::new(
            format!("I'll remind you if there's no reply in {days} days"),
            format!("Te recordaré si no responden en {days} días"),
        );
        self.push_undo(text.clone(), vec![(Inverse::Rules(Box::new(RulesUndo::Followup(id))), None)]);
        Ok(Outcome::Notice(Notice { kind: NoticeKind::Success, text, undo: true, secs: None }))
    }

    /// Seguimientos vencidos sin respuesta (para «Hoy»).
    pub fn due_followups(&self) -> &[FollowUp] {
        &self.rules.due_followups
    }

    /// Descarta un seguimiento.
    pub fn dismiss_followup(&mut self, msg: MessageId) {
        self.rules_ensure();
        self.rules.followups.remove(msg);
        self.rules.due_followups.retain(|f| f.msg != msg);
        let _ = self.rules.followups.save(&self.store);
    }

    /// Abre el mensaje de un seguimiento (en su carpeta).
    pub fn open_followup(&mut self, msg: MessageId) {
        if let Some(env) = self.store.get_envelope(msg).ok().flatten() {
            self.module = super::Module::Mail;
            self.select_folder(FolderSel::Folder(env.folder));
            self.request_focus(msg);
        }
    }

    // ------------------------------------------------------------------ baja de listas

    /// Cómo darse de baja del remitente del mensaje (o `None`).
    pub fn unsubscribe_method(&mut self, id: MessageId) -> Option<(Method, Text)> {
        let raw = self.store.get_raw(id).ok()?;
        let h = Headers::parse(&raw);
        let m = unsubscribe::methods(&h).into_iter().next()?;
        let sender = self.envelope(id).map(|e| e.sender_display().to_string()).unwrap_or_default();
        let why = t(m.explain(&sender));
        Some((m, why))
    }

    /// Se da de baja (tras la confirmación de la interfaz). La página web la abre la interfaz.
    pub fn unsubscribe(&mut self, id: MessageId) -> Result<Outcome> {
        let Some((m, _)) = self.unsubscribe_method(id) else {
            return Ok(Outcome::Notice(Notice::info(
                "This sender doesn't offer a way to unsubscribe.",
                "Este remitente no ofrece forma de darse de baja.",
            )));
        };
        let env = self.envelope(id).cloned().ok_or(EngineError::NoSelection)?;
        let sender = env.sender_display().to_string();
        match m {
            Method::OneClick { url } => {
                let tx = self.rules.tx.clone();
                let who = sender.clone();
                self.bg.spawn(async move {
                    let r = unsubscribe::one_click(&url, &unsubscribe::HttpOptions::default()).await;
                    let _ =
                        tx.send(BgDone::Unsubscribed { sender: who, result: r.map(|_| ()).map_err(|e| t(e.text())) });
                    JobOutput::Custom(Box::new(()))
                });
                Ok(Outcome::Notice(Notice::info(
                    format!("Unsubscribing from {sender}…"),
                    format!("Dando de baja de {sender}…"),
                )))
            }
            Method::Mailto { to, subject, body } => {
                let Some((_, ident)) = self.identities().into_iter().find(|(a, _)| *a == env.account) else {
                    return Err(EngineError::NoAccount);
                };
                let d = unsubscribe::mailto_draft(ident, &to, &subject, &body);
                self.queue_auto(
                    env.account,
                    d,
                    Bi::new(format!("Unsubscribe request sent to {to}"), format!("Solicitud de baja enviada a {to}")),
                );
                Ok(Outcome::Done)
            }
            Method::Web { url } => Ok(Outcome::Rules(format!("open_url:{url}"))),
        }
    }

    // ------------------------------------------------------------------ plantillas

    /// Nombres de las plantillas.
    pub fn templates(&self) -> Vec<String> {
        TemplateStore::new(&self.store).list().unwrap_or_default()
    }

    /// Guarda el borrador como plantilla.
    pub fn save_template(&mut self, name: &str, cd: &ComposeDraft) -> Result<Outcome> {
        TemplateStore::new(&self.store)
            .save(name, &cd.draft)
            .map_err(|e| EngineError::Invalid(e.message(compose_lang())))?;
        Ok(Outcome::Notice(Notice::success(
            format!("Template “{}” saved", name.trim()),
            format!("Plantilla «{}» guardada", name.trim()),
        )))
    }

    /// Borra una plantilla.
    pub fn delete_template(&mut self, name: &str) -> Result<()> {
        TemplateStore::new(&self.store).delete(name).map_err(|e| EngineError::Invalid(e.message(compose_lang())))
    }

    /// Inserta una plantilla en el borrador: asunto y destinatarios si faltan, el texto antes de la
    /// firma o la cita, y sus adjuntos.
    pub fn apply_template(&self, cd: &mut ComposeDraft, name: &str) -> Result<()> {
        let tpl =
            TemplateStore::new(&self.store).load(name).map_err(|e| EngineError::Invalid(e.message(compose_lang())))?;
        let d = &mut cd.draft;
        if d.subject.trim().is_empty() {
            d.subject = tpl.subject.clone();
        }
        if d.to.is_empty() {
            d.to = tpl.to.clone();
        }
        if d.cc.is_empty() {
            d.cc = tpl.cc.clone();
        }
        let mut blocks = tpl.body.blocks.clone();
        blocks.append(&mut d.body.blocks);
        d.body.blocks = blocks;
        d.attachments.extend(tpl.attachments);
        Ok(())
    }

    // ------------------------------------------------------------------ vacaciones

    /// Respuesta de vacaciones.
    pub fn vacation(&mut self) -> Vacation {
        self.rules_ensure();
        self.rules.book.vacation.clone()
    }

    /// ¿Corre en el servidor (alguna cuenta)?
    pub fn vacation_on_server(&mut self) -> bool {
        self.rules_ensure();
        self.rules.book.sieve.values().any(|s| s.enabled && s.server_vacation && s.synced_at.is_some())
    }

    /// Guarda la respuesta de vacaciones.
    pub fn set_vacation(&mut self, v: Vacation) -> Outcome {
        self.rules_ensure();
        let on = v.enabled;
        self.rules.book.vacation = v;
        self.rules_save();
        if !on {
            return Outcome::Notice(Notice::success(
                "Automatic replies are off",
                "Respuestas automáticas desactivadas",
            ));
        }
        let any_sieve = self.rules.book.sieve.values().any(|s| s.enabled && s.available);
        Outcome::Notice(if any_sieve {
            Notice::success(
                "Automatic replies are on. Your server sends them even with IureDesk closed.",
                "Respuestas automáticas activadas. Tu servidor las envía aunque IureDesk esté cerrado.",
            )
        } else {
            Notice::success(
                "Automatic replies are on — only while IureDesk is open on this computer.",
                "Respuestas automáticas activadas: solo mientras IureDesk esté abierto en este equipo.",
            )
        })
    }

    // ------------------------------------------------------------------ Sieve

    fn gen_ctx(&self, account: AccountId, exts: &[String]) -> GenCtx {
        let trash = self
            .folders
            .iter()
            .find(|f| f.account == account && f.role == FolderRole::Trash)
            .map(|f| f.remote_path.clone());
        let my = self.labels.iter().filter(|l| l.id == account).map(|l| l.email.clone()).collect();
        GenCtx { account, extensions: exts.iter().cloned().collect(), trash_path: trash, my_addresses: my }
    }

    /// Cuentas y su Sieve.
    pub fn sieve_rows(&mut self) -> Vec<SieveRow> {
        self.rules_ensure();
        self.labels
            .iter()
            .map(|l| SieveRow {
                account: l.id,
                email: l.email.clone(),
                state: self.rules.book.sieve_of(l.id).cloned().unwrap_or_default(),
                busy: self.rules.sieve_busy.contains(&l.id),
            })
            .collect()
    }

    /// Busca ManageSieve en las cuentas IMAP (una vez por sesión; sin red no hace nada).
    pub fn sieve_detect(&mut self) {
        self.rules_ensure();
        if self.rules.detected || !self.is_live() {
            return;
        }
        self.rules.detected = true;
        let ids: Vec<AccountId> = self.labels.iter().map(|l| l.id).collect();
        for a in ids {
            let known = self.rules.book.sieve_of(a).is_some_and(|s| s.available || s.last_error.is_some());
            if !known {
                self.sieve_job(a, false);
            }
        }
    }

    /// Activa o desactiva subir las reglas al servidor de una cuenta.
    pub fn sieve_set_enabled(&mut self, account: AccountId, on: bool) {
        self.rules_ensure();
        self.rules.book.sieve_mut(account).enabled = on;
        let _ = self.rules.book.save(&self.store);
        self.sieve_job(account, on);
    }

    /// Vuelve a subir el script a las cuentas con Sieve activo (tras cambiar reglas).
    fn sieve_resync(&mut self) {
        let ids: Vec<AccountId> = self
            .rules
            .book
            .sieve
            .iter()
            .filter(|(_, s)| s.enabled && s.available)
            .filter_map(|(k, _)| k.parse::<u64>().ok().map(AccountId))
            .collect();
        for a in ids {
            self.sieve_job(a, true);
        }
    }

    fn sieve_job(&mut self, account: AccountId, upload: bool) {
        if self.rules.sieve_busy.contains(&account) || !self.is_live() {
            return;
        }
        let Some(acc) = self.accounts.iter().find(|a| self.account_ids.get(&a.id) == Some(&account)).cloned() else {
            return;
        };
        if acc.incoming.protocol != iuredesk_model::account::Protocol::Imap
            || managesieve::known_without_sieve(&acc.incoming.host)
        {
            self.rules.book.sieve_mut(account).available = false;
            return;
        }
        let secrets = std::sync::Arc::clone(&self.secrets);
        let rules = self.rules.book.rules.clone();
        let vacation = self.rules.book.vacation.clone();
        let mut ctx = self.gen_ctx(account, &[]);
        let tx = self.rules.tx.clone();
        let host = acc.incoming.host.clone();
        let user = if acc.incoming.username.is_empty() { acc.email.clone() } else { acc.incoming.username.clone() };
        let id = acc.id.clone();
        self.rules.sieve_busy.insert(account);
        self.bg.spawn(async move {
            let cfg = managesieve::Config::new(host.clone());
            let result = async {
                let caps = managesieve::probe(&cfg).await.map_err(|e| t(e.text()))?;
                if !upload {
                    return Ok(SieveResult { caps, host, uploaded: None });
                }
                let password = secrets
                    .get(&id, iuredesk_accounts::SecretPurpose::Incoming)
                    .ok()
                    .flatten()
                    .ok_or_else(|| Text::new("Your password is not saved.", "Tu contraseña no está guardada."))?;
                ctx.extensions = caps.sieve.iter().cloned().collect();
                let script = sieve::to_sieve(&rules, Some(&vacation), &ctx);
                let auth = managesieve::Auth::Password { user, password };
                let up = managesieve::upload(&cfg, &auth, &script.text).await.map_err(|e| t(e.text()))?;
                let server = if up.other_active.is_some() { Vec::new() } else { script.server };
                let vac = up.other_active.is_none() && script.vacation;
                Ok(SieveResult { caps: up.capabilities, host, uploaded: Some((server, vac, up.other_active)) })
            }
            .await;
            let _ = tx.send(BgDone::Sieve { account, result });
            JobOutput::Custom(Box::new(()))
        });
    }

    // ------------------------------------------------------------------ cada cuadro

    /// Despierta lo pospuesto, revisa seguimientos y atiende lo que terminó en segundo plano.
    pub(crate) fn poll_rules(&mut self, events: &mut Vec<Event>) {
        if !self.rules.loaded {
            // Nada cargado aún: solo hay trabajo si existen posposiciones o seguimientos.
            self.rules_ensure();
        }
        let now = self.now();
        if self.rules.snoozes.next_wake().is_some_and(|t| t <= now) {
            match snooze::wake_due(&self.store, &mut self.rules.snoozes, now) {
                Ok(woke) => {
                    let _ = self.rules.snoozes.save(&self.store);
                    for s in &woke {
                        self.kick(s.account);
                        self.rules.woke.insert(0, s.msg);
                        self.notices.push(Notice::info(
                            format!("Back from snooze: {}", s.subject),
                            format!("Ha vuelto: {}", s.subject),
                        ));
                    }
                    self.rules.woke.truncate(MAX_WOKE);
                    if !woke.is_empty() {
                        self.envelopes.clear();
                        self.rebuild_rows();
                    }
                }
                Err(e) => self.notices.push(Notice::error(t(e.text()))),
            }
        }
        if self.rules.last_followup_check.is_none_or(|t| t.elapsed() >= FOLLOWUP_EVERY)
            && !self.rules.followups.items.is_empty()
        {
            self.rules.last_followup_check = Some(Instant::now());
            let my = self.my_addresses();
            let (due, changed) = self.rules.followups.refresh(&self.store, &my, now);
            self.rules.due_followups = due;
            if changed {
                let _ = self.rules.followups.save(&self.store);
            }
        }
        // Avisos retenidos que no recibieron su llegada.
        let mut late = Vec::new();
        self.rules.held.retain(|h| {
            if h.since.elapsed() >= HOLD_NOTICE {
                late.push((h.account, h.count, h.sample.clone()));
                false
            } else {
                true
            }
        });
        for (a, c, s) in late {
            events.push(self.newmail_event(a, c, &s));
        }
        while let Ok(done) = self.rules.rx.try_recv() {
            match done {
                BgDone::Unsubscribed { sender, result } => self.notices.push(match result {
                    Ok(()) => {
                        Notice::success(format!("Unsubscribed from {sender}"), format!("Te diste de baja de {sender}"))
                    }
                    Err(e) => Notice::error(e),
                }),
                BgDone::Sieve { account, result } => {
                    self.rules.sieve_busy.remove(&account);
                    let s = self.rules.book.sieve_mut(account);
                    match result {
                        Ok(r) => {
                            s.available = true;
                            s.host = r.host;
                            s.extensions = r.caps.sieve;
                            s.last_error = None;
                            if let Some((server, vac, other)) = r.uploaded {
                                s.server_rules = server;
                                s.server_vacation = vac;
                                s.synced_at = Some(Utc::now());
                                s.last_error = other.map(|o| {
                                    format!("Tu servidor ya tiene otro filtro activo («{o}»); tus reglas se ejecutan en este equipo.")
                                });
                            } else if !s.enabled {
                                // Primera detección: se ofrece activarlo.
                                s.enabled = true;
                            }
                        }
                        Err(e) => {
                            s.last_error = Some(e.es.clone());
                            if !s.available {
                                s.enabled = false;
                            }
                        }
                    }
                    let _ = self.rules.book.save(&self.store);
                }
            }
        }
    }

    /// Falta para la próxima vuelta de algo pospuesto.
    pub(crate) fn rules_deadline(&self) -> Option<Duration> {
        let next = self.rules.snoozes.next_wake()?;
        let left = (next - self.now()).to_std().unwrap_or(Duration::ZERO);
        Some(left.min(Duration::from_secs(60)))
    }

    // ------------------------------------------------------------------ comandos

    pub(crate) fn run_rules(&mut self, base: &str, arg: Option<&str>) -> Result<Outcome> {
        match base {
            "mail.snooze" => match arg.and_then(SnoozePreset::from_key) {
                Some(p) => self.snooze_selected(p),
                None => Ok(Outcome::Rules("snooze".into())),
            },
            "mail.unsnooze" => self.unsnooze_selected(),
            "mail.followup" => match arg.and_then(|a| a.parse::<u32>().ok()) {
                Some(d) => self.follow_up(d),
                None => Ok(Outcome::Rules("followup".into())),
            },
            "mail.rule_from_message" => Ok(Outcome::Rules("rule_from_message".into())),
            "mail.apply_rules" => match self.folder {
                FolderSel::Folder(f) => self.apply_rules_to_folder(f, None),
                FolderSel::Unified(role) => {
                    let fs: Vec<FolderId> = self.folders.iter().filter(|f| f.role == role).map(|f| f.id).collect();
                    let mut last = Outcome::Done;
                    for f in fs {
                        last = self.apply_rules_to_folder(f, None)?;
                    }
                    Ok(last)
                }
                _ => Ok(Outcome::Notice(Notice::info("Open a folder first.", "Primero abre una carpeta."))),
            },
            "mail.unsubscribe" => Ok(Outcome::Rules("unsubscribe".into())),
            "mail.category" => match arg.and_then(Category::from_key) {
                Some(c) => {
                    self.set_category_tab(c);
                    Ok(Outcome::Done)
                }
                // Sin argumento: la pestaña siguiente.
                None => {
                    let k = Category::ALL.iter().position(|c| *c == self.rules.tab).unwrap_or(0);
                    let next = Category::ALL.get((k + 1) % Category::ALL.len()).copied().unwrap_or_default();
                    self.set_category_tab(next);
                    Ok(Outcome::Done)
                }
            },
            "mail.always_category" => match arg.and_then(Category::from_key) {
                Some(c) => self.always(c),
                None => Ok(Outcome::Rules("always_category".into())),
            },
            "mail.iure_only" => {
                self.set_iure_only(!self.rules.iure_only);
                Ok(Outcome::Done)
            }
            "app.labels" => Ok(Outcome::Rules("labels".into())),
            "app.rules" => Ok(Outcome::Rules("rules".into())),
            "app.vacation" => Ok(Outcome::Rules("vacation".into())),
            _ => Err(EngineError::UnknownCommand(base.to_string())),
        }
    }
}

fn compose_lang() -> iuredesk_compose::Lang {
    if crate::lang::spanish() { iuredesk_compose::Lang::Es } else { iuredesk_compose::Lang::En }
}

fn undo_step(u: UndoStep) -> (Inverse, Option<u64>) {
    match u {
        UndoStep::Flags { id, account, flags, seq } => (Inverse::Flags { id, account, flags }, seq),
        UndoStep::Move { id, account, folder, remote, seq } => (Inverse::Move { id, account, folder, remote }, seq),
        UndoStep::Keywords { id, keywords, added } => {
            (Inverse::Rules(Box::new(RulesUndo::Keywords { id, keywords, added })), None)
        }
    }
}
