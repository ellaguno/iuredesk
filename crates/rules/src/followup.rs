//! «Recordarme si no responden en N días»: sobre un correo enviado; si pasa el plazo sin
//! respuesta de otra persona en la conversación, aparece en el panel «Hoy».

use chrono::{DateTime, Duration, Utc};
use iuredesk_model::{AccountId, MessageEnvelope, MessageId, ThreadId};
use iuredesk_store::Store;
use serde::{Deserialize, Serialize};

use crate::book;
use crate::error::{Result, RulesError};

/// Clave de los seguimientos.
pub const FOLLOWUP_KEY: &str = "rules/followups";
/// Seguimientos como mucho.
pub const MAX_FOLLOWUPS: usize = 2_000;
/// Plazo máximo en días.
pub const MAX_DAYS: u32 = 90;

/// Un seguimiento.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FollowUp {
    /// Mensaje enviado.
    pub msg: MessageId,
    /// Cuenta.
    pub account: AccountId,
    /// `Message-ID` del enviado.
    #[serde(default)]
    pub message_id: Option<String>,
    /// Conversación.
    #[serde(default)]
    pub thread: Option<ThreadId>,
    /// Asunto.
    pub subject: String,
    /// Destinatarios para mostrar.
    pub to: String,
    /// Cuándo se envió.
    pub sent: DateTime<Utc>,
    /// Cuándo avisar.
    pub due: DateTime<Utc>,
}

/// Estado de un seguimiento.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FollowState {
    /// Aún dentro del plazo, sin respuesta.
    Waiting,
    /// Venció sin respuesta: avisar.
    Due,
    /// Ya respondieron.
    Replied,
}

/// Seguimientos.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FollowUpBook {
    /// Entradas.
    pub items: Vec<FollowUp>,
}

impl FollowUpBook {
    /// Lee.
    pub fn load(store: &Store) -> FollowUpBook {
        book::load(store, FOLLOWUP_KEY).unwrap_or_default()
    }

    /// Guarda.
    pub fn save(&self, store: &Store) -> Result<()> {
        book::save(store, FOLLOWUP_KEY, self)
    }

    /// Agrega (o reemplaza) el seguimiento de un correo enviado.
    pub fn add(&mut self, env: &MessageEnvelope, days: u32, now: DateTime<Utc>) -> Result<FollowUp> {
        if days == 0 || days > MAX_DAYS {
            return Err(RulesError::Invalid(format!("entre 1 y {MAX_DAYS} días")));
        }
        let sent = if env.date > now { now } else { env.date };
        let f = FollowUp {
            msg: env.id,
            account: env.account,
            message_id: env.message_id.clone(),
            thread: env.thread,
            subject: env.subject.clone(),
            to: env.to.iter().map(|m| m.display().to_string()).collect::<Vec<_>>().join(", "),
            sent,
            due: now + Duration::days(i64::from(days)),
        };
        self.items.retain(|x| x.msg != env.id);
        if self.items.len() >= MAX_FOLLOWUPS {
            self.items.sort_by_key(|x| x.due);
            self.items.remove(0);
        }
        self.items.push(f.clone());
        Ok(f)
    }

    /// Quita el seguimiento de un mensaje.
    pub fn remove(&mut self, msg: MessageId) -> bool {
        let before = self.items.len();
        self.items.retain(|x| x.msg != msg);
        before != self.items.len()
    }

    /// ¿Tiene seguimiento?
    pub fn has(&self, msg: MessageId) -> bool {
        self.items.iter().any(|x| x.msg == msg)
    }

    /// Revisa todos: quita los respondidos y devuelve los vencidos (más antiguos primero).
    pub fn refresh(&mut self, store: &Store, mine: &[String], now: DateTime<Utc>) -> (Vec<FollowUp>, bool) {
        let mut due = Vec::new();
        let before = self.items.len();
        self.items.retain(|f| match state(store, f, mine, now) {
            FollowState::Replied => false,
            FollowState::Due => {
                due.push(f.clone());
                true
            }
            FollowState::Waiting => true,
        });
        due.sort_by_key(|f| f.due);
        (due, before != self.items.len())
    }
}

/// Estado: ¿respondió alguien que no soy yo, después de enviarlo, en la conversación?
pub fn state(store: &Store, f: &FollowUp, mine: &[String], now: DateTime<Utc>) -> FollowState {
    let mine: Vec<String> = mine.iter().map(|m| crate::norm_addr(m)).collect();
    let from_other = |e: &MessageEnvelope| {
        e.from.first().is_some_and(|m| !mine.contains(&crate::norm_addr(&m.addr))) && e.date > f.sent
    };
    let refers = |e: &MessageEnvelope| {
        f.message_id
            .as_ref()
            .is_some_and(|mid| e.in_reply_to.as_deref() == Some(mid.as_str()) || e.references.contains(mid))
    };
    // El hilo del mensaje (puede haber cambiado al reenhebrar: se lee del sobre actual).
    let thread = store.get_envelope(f.msg).ok().flatten().and_then(|e| e.thread).or(f.thread);
    if let Some(t) = thread {
        let members = store.thread_envelopes(t).unwrap_or_default();
        if members
            .iter()
            .take(500)
            .any(|e| e.id != f.msg && from_other(e) && (refers(e) || f.message_id.is_none() || e.thread == Some(t)))
        {
            return FollowState::Replied;
        }
    }
    if now >= f.due { FollowState::Due } else { FollowState::Waiting }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use iuredesk_model::{Folder, FolderRole, Mailbox};

    #[test]
    fn detects_replies_in_thread() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let sent_f = s
            .put_folder(&Folder {
                account: AccountId(1),
                name: "Sent".into(),
                role: FolderRole::Sent,
                ..Default::default()
            })
            .unwrap();
        let inbox = s
            .put_folder(&Folder {
                account: AccountId(1),
                name: "INBOX".into(),
                role: FolderRole::Inbox,
                ..Default::default()
            })
            .unwrap();
        let t0 = Utc.with_ymd_and_hms(2026, 10, 1, 10, 0, 0).unwrap();
        let sent = MessageEnvelope {
            account: AccountId(1),
            folder: sent_f,
            message_id: Some("p1@despacho.example".into()),
            from: vec![Mailbox::addr("yo@despacho.example")],
            to: vec![Mailbox::new("Carmen", "carmen@cliente.example")],
            subject: "Propuesta".into(),
            date: t0,
            ..Default::default()
        };
        let id = s.put_message(sent, b"Subject: Propuesta\r\n\r\nx").unwrap();
        let env = s.get_envelope(id).unwrap().unwrap();
        let mine = vec!["yo@despacho.example".to_string()];
        let mut book = FollowUpBook::default();
        let f = book.add(&env, 3, t0).unwrap();
        assert_eq!(f.to, "Carmen");
        assert!(book.add(&env, 0, t0).is_err());
        assert_eq!(state(&s, &f, &mine, t0 + Duration::days(1)), FollowState::Waiting);
        assert_eq!(state(&s, &f, &mine, t0 + Duration::days(4)), FollowState::Due);
        let (due, _) = book.refresh(&s, &mine, t0 + Duration::days(4));
        assert_eq!(due.len(), 1);

        // Mi propio seguimiento en el hilo no cuenta como respuesta.
        let mine_again = MessageEnvelope {
            account: AccountId(1),
            folder: sent_f,
            message_id: Some("p2@despacho.example".into()),
            in_reply_to: Some("p1@despacho.example".into()),
            references: vec!["p1@despacho.example".into()],
            from: vec![Mailbox::addr("yo@despacho.example")],
            subject: "Re: Propuesta".into(),
            date: t0 + Duration::days(1),
            ..Default::default()
        };
        s.put_message(mine_again, b"x").unwrap();
        assert_eq!(state(&s, &f, &mine, t0 + Duration::days(4)), FollowState::Due);

        // La respuesta de Carmen sí.
        let reply = MessageEnvelope {
            account: AccountId(1),
            folder: inbox,
            message_id: Some("r1@cliente.example".into()),
            in_reply_to: Some("p1@despacho.example".into()),
            references: vec!["p1@despacho.example".into()],
            from: vec![Mailbox::addr("carmen@cliente.example")],
            subject: "Re: Propuesta".into(),
            date: t0 + Duration::days(2),
            ..Default::default()
        };
        s.put_message(reply, b"x").unwrap();
        assert_eq!(state(&s, &f, &mine, t0 + Duration::days(4)), FollowState::Replied);
        let (due, changed) = book.refresh(&s, &mine, t0 + Duration::days(4));
        assert!(due.is_empty() && changed && book.items.is_empty());
    }
}
