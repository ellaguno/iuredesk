//! Lo que el asistente ✦, la suite y la importación (F4.2) necesitan de la sesión: correos como
//! [`EmailInput`], contexto de proveedores, runtime de fondo, y crear tareas y notas locales a
//! partir de una propuesta ya confirmada por el usuario.

use std::sync::Arc;

use iuredesk_assistant::{Addr, EmailInput, ProviderContext};
use iuredesk_model::account::{AuthMethod, OAuthProvider};
use iuredesk_model::{EventTime, FolderRole, Mailbox, MessageEnvelope, MessageId, Note, Task, TaskList, TaskListId};

use super::{IURE_TASKS_PATH, Notice, Outcome, Session, store_err};
use crate::background::Waker;
use crate::{EngineError, Result};

/// Correos sin leer que entran en el triage.
const TRIAGE_MAX: usize = 40;
/// Mensajes de un hilo que se envían.
const THREAD_MAX: usize = 20;

fn addr(m: &Mailbox) -> Addr {
    Addr::new(m.name.as_deref().filter(|n| !n.is_empty()), &m.addr)
}

impl Session {
    /// Manejador del runtime de fondo.
    pub fn runtime(&self) -> tokio::runtime::Handle {
        self.bg.handle()
    }

    /// Despierta a la UI.
    pub fn waker(&self) -> Waker {
        self.bg.waker()
    }

    /// El llavero.
    pub fn secrets(&self) -> &Arc<dyn iuredesk_accounts::SecretStore> {
        &self.secrets
    }

    /// Sesión de Iurefficient y cuenta de Microsoft 365 para construir proveedores.
    pub fn ai_provider_context(&self) -> ProviderContext {
        let microsoft = self
            .accounts
            .iter()
            .find(|a| matches!(a.incoming.auth, AuthMethod::OAuth2 { provider: OAuthProvider::Microsoft }))
            .cloned()
            .zip(self.live.tokens.clone());
        ProviderContext {
            iurefficient: self.iure_remote(),
            microsoft,
            time_zone: crate::time::user_tz(self.zone).name().to_string(),
        }
    }

    /// «Nombre <correo>» del usuario.
    pub fn ai_me(&self) -> String {
        match self.identities().first() {
            Some((_, i)) if !i.name.is_empty() => format!("{} <{}>", i.name, i.email),
            Some((_, i)) => i.email.clone(),
            None => self.labels.first().map(|l| l.email.clone()).unwrap_or_default(),
        }
    }

    fn ai_input(&mut self, env: &MessageEnvelope, with_body: bool) -> EmailInput {
        let body = if with_body {
            match self.bodies.get(&env.id) {
                Some(b) => b.text.clone(),
                None => match self.store.get_raw(env.id) {
                    Ok(raw) => crate::body::parse(&raw).text,
                    Err(_) => env.preview.clone(),
                },
            }
        } else {
            env.preview.clone()
        };
        EmailInput {
            id: env.id.get().to_string(),
            from: env.from.first().map(addr).unwrap_or_default(),
            to: env.to.iter().take(20).map(addr).collect(),
            cc: env.cc.iter().take(20).map(addr).collect(),
            subject: env.subject.clone(),
            date: Some(self.zone.local(env.date).format("%Y-%m-%d %H:%M").to_string()),
            body_text: body,
            attachments: env.attachments.iter().take(20).map(|a| a.filename.clone()).collect(),
            unread: !env.flags.seen,
        }
    }

    /// Un correo para el asistente.
    pub fn ai_email(&mut self, id: MessageId) -> Option<EmailInput> {
        let env = self.envelope(id).cloned()?;
        Some(self.ai_input(&env, true))
    }

    /// El hilo (los últimos mensajes) para el asistente, en orden cronológico.
    pub fn ai_thread(&mut self, id: MessageId) -> Vec<EmailInput> {
        let mut conv = self.conversation(id);
        if conv.len() > THREAD_MAX {
            conv.drain(..conv.len() - THREAD_MAX);
        }
        conv.iter().map(|e| self.ai_input(e, true)).collect()
    }

    /// Caracteres del hilo (para ofrecer «Resumir» en los largos).
    pub fn thread_chars(&mut self, id: MessageId) -> usize {
        let conv = self.conversation(id);
        if conv.len() > 2 {
            return usize::MAX;
        }
        conv.iter()
            .map(|e| self.bodies.get(&e.id).map_or(e.preview.chars().count(), |b| b.text.chars().count()))
            .fold(0usize, usize::saturating_add)
    }

    /// No leídos de las bandejas de entrada (más recientes primero) con su extracto.
    pub fn ai_unread(&mut self) -> Vec<(MessageId, EmailInput)> {
        let folders: Vec<_> = self.folders.iter().filter(|f| f.role == FolderRole::Inbox).map(|f| f.id).collect();
        let mut envs: Vec<MessageEnvelope> = Vec::new();
        for f in folders {
            let Ok(order) = self.store.list_order(f) else { continue };
            for &(_, id) in order.iter().take(400) {
                if let Some(e) = self.envelope(MessageId(id)).cloned()
                    && !e.flags.seen
                {
                    envs.push(e);
                }
            }
        }
        envs.sort_by_key(|e| std::cmp::Reverse(e.date));
        envs.truncate(TRIAGE_MAX);
        envs.iter().map(|e| (e.id, self.ai_input(e, false))).collect()
    }

    /// Crea una tarea local (propuesta confirmada por el usuario).
    pub fn ai_create_task(&mut self, title: &str, due: Option<chrono::NaiveDate>, notes: &str) -> Result<Outcome> {
        let title: String = title.trim().chars().take(300).collect();
        if title.is_empty() {
            return Err(EngineError::Invalid("título vacío".into()));
        }
        let lists = self.task_lists();
        let list = match lists.iter().find(|l| l.remote_path != IURE_TASKS_PATH) {
            Some(l) => l.id,
            None => self
                .store
                .put_task_list(&TaskList {
                    id: TaskListId(0),
                    name: crate::lang::pick("Tasks", "Tareas").into(),
                    ..Default::default()
                })
                .map_err(store_err)?,
        };
        let task = Task {
            list,
            summary: title.clone(),
            description: notes.chars().take(4_000).collect(),
            due: due.map(EventTime::Date),
            ..Default::default()
        };
        iuredesk_pim_sync::ops::save_task(&self.store, &task).map_err(|e| EngineError::Store(e.to_string()))?;
        self.data_gen = self.data_gen.wrapping_add(1);
        Ok(Outcome::Notice(Notice::success(format!("Task created: {title}"), format!("Tarea creada: {title}"))))
    }

    /// Guarda una nota (minuta, resumen…).
    pub fn ai_save_note(&mut self, title: &str, markdown: &str) -> Result<Outcome> {
        let now = self.now();
        let note = Note {
            title: title.chars().take(300).collect(),
            body: markdown.chars().take(200_000).collect(),
            created: now,
            updated: now,
            ..Default::default()
        };
        self.store.put_note(&note).map_err(store_err)?;
        self.data_gen = self.data_gen.wrapping_add(1);
        Ok(Outcome::Notice(Notice::success("Saved in Notes", "Guardado en Notas")))
    }

    /// Algo cambió fuera de la ventana (MCP, importación): recargar cuentas, carpetas y lista.
    pub fn reload_external(&mut self) {
        let _ = self.reload_accounts();
        self.contacts_cache = None;
        self.events_cache = None;
        self.envelopes.clear();
        self.rebuild_rows();
    }

    /// Comandos del asistente, la suite y la importación: los atiende la UI (necesitan sus
    /// ventanas); aquí solo se valida lo que requieren.
    pub(super) fn run_assist(&mut self, id: &str) -> Result<Outcome> {
        let needs_msg = matches!(id, "ai.summarize" | "ai.replies" | "ai.analyze" | "suite.read_aloud");
        if needs_msg && self.focused.is_none() {
            return Ok(Outcome::Notice(Notice::info("Select a message first.", "Primero selecciona un mensaje.")));
        }
        if matches!(id, "ai.triage" | "ai.minutes" | "ai.chat") {
            self.module = super::Module::Assistant;
        }
        Ok(Outcome::Assist(id.to_string()))
    }
}
