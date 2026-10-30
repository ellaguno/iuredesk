//! Sincronización de una cuenta POP3: descarga incremental por `UIDL` a la bandeja de entrada.

use std::sync::Arc;
use std::time::Instant;

use chrono::Utc;
use iuredesk_model::account::AuthMethod;
use iuredesk_model::{Folder, FolderRole};
use iuredesk_oauth::sasl;
use iuredesk_pop3::{Pop3Client, Pop3Config, Pop3Error, Pop3Policy, plan_download};
use iuredesk_store::NewMessage;

use crate::backend::{BoxFuture, MailBackend};
use crate::engine::Ctx;
use crate::error::{Result, SyncError};
use crate::imap_sync::CycleReport;
use crate::ops;
use crate::parse::envelope_from_raw;
use crate::pool::Auth;
use crate::state::{self, Pop3Known, pop3_key};
use crate::{NewMailSample, Phase, SyncEvent};

/// Mensajes por transacción del almacén.
const BATCH: usize = 50;

/// Una cuenta POP3.
pub(crate) struct Pop3Account {
    pub ctx: Arc<Ctx>,
    pub auth: Arc<Auth>,
    pub policy: Pop3Policy,
}

impl Pop3Account {
    async fn connect(&self) -> Result<Pop3Client> {
        let a = &self.ctx.account;
        let mut cfg = Pop3Config::from_server(&a.incoming);
        cfg.io_timeout = self.ctx.opts.io_timeout;
        let mut c = Pop3Client::connect(cfg).await?;
        let user = a.incoming.username.as_str();
        match a.incoming.auth {
            AuthMethod::OAuth2 { .. } => {
                let mut retried = false;
                loop {
                    let token = self.auth.token(a).await?;
                    match c.auth_sasl("XOAUTH2", &sasl::xoauth2(user, &token)?).await {
                        Ok(()) => break,
                        Err(e) if e.is_auth() && !retried => {
                            self.auth.invalidate(a);
                            retried = true;
                        }
                        Err(e) => return Err(e.into()),
                    }
                }
            }
            AuthMethod::Password | AuthMethod::AppPassword => {
                let pass = self.auth.password(a).await?;
                if c.has_capa("USER") || c.capabilities().is_empty() || !c.has_apop() {
                    c.login(user, &pass).await?;
                } else {
                    c.apop(user, &pass).await?;
                }
            }
        }
        Ok(c)
    }

    fn inbox(&self) -> Result<Folder> {
        let ctx = &self.ctx;
        if let Some(f) = ctx.store.folders(ctx.acct)?.into_iter().find(|f| f.role == FolderRole::Inbox) {
            return Ok(f);
        }
        let mut f = Folder {
            account: ctx.acct,
            name: "INBOX".into(),
            remote_path: "INBOX".into(),
            role: FolderRole::Inbox,
            ..Default::default()
        };
        f.id = ctx.store.put_folder(&f)?;
        ctx.emit(SyncEvent::FolderUpdated { account: ctx.acct, folder: f.id });
        Ok(f)
    }

    /// Una pasada: descarga lo nuevo (lo más reciente primero) y aplica la política de borrado.
    pub async fn cycle(&self) -> Result<CycleReport> {
        let started = Instant::now();
        let ctx = &self.ctx;
        let inbox = self.inbox()?;
        let key = pop3_key(ctx.acct);
        let mut known: Pop3Known = state::load(&ctx.store, &key).unwrap_or_default();
        // «Primera descarga» = ni UIDL conocidos ni correo en la bandeja. Sin copia en el servidor
        // los UIDL se olvidan al borrar, así que `known` vacío no basta: antes cada pasada parecía
        // la primera y nunca se avisaba de correo nuevo.
        let initial = known.is_empty() && ctx.store.count(inbox.id).map(|c| c.total == 0).unwrap_or(true);
        let mut c = self.connect().await?;
        let uidl = c.uidl().await?;
        let now = Utc::now().timestamp();
        let max = ctx.opts.pop3_max_per_cycle.max(1);
        let plan = plan_download(&uidl, &known, self.policy, now, max);
        let fresh_total = uidl.iter().filter(|(_, u)| !known.contains_key(u)).count();
        let total = plan.download.len() as u64;
        let mut done = 0u64;
        let mut samples = Vec::new();
        let mut inserted = 0usize;
        let mut arrived = Vec::new();
        for chunk in plan.download.chunks(BATCH) {
            let mut batch = Vec::new();
            let mut got = Vec::new();
            for (n, uid) in chunk {
                let raw = match c.retr(*n).await {
                    Ok(r) => r,
                    Err(Pop3Error::TooLarge(w)) => {
                        // Se recuerda para no reintentarlo en cada pasada.
                        ctx.emit_error(&SyncError::Pop3(Pop3Error::TooLarge(w)));
                        known.insert(uid.clone(), now);
                        continue;
                    }
                    Err(e) => return Err(e.into()),
                };
                let (mut env, text) = envelope_from_raw(&raw, inbox.id, Utc::now());
                env.remote_id = Some(uid.clone());
                if ctx.store.message_by_remote(inbox.id, uid)?.is_some() {
                    known.insert(uid.clone(), now);
                    continue;
                }
                got.push((*n, uid.clone(), env.sender_display().to_string(), env.subject.clone()));
                batch.push(NewMessage { envelope: env, raw: Some(raw), body_text: Some(text) });
            }
            let ids = if batch.is_empty() { Vec::new() } else { ctx.store.put_messages(batch)? };
            for (id, (n, uid, from, subject)) in ids.into_iter().zip(got) {
                known.insert(uid, now);
                inserted = inserted.saturating_add(1);
                arrived.push(id);
                if !initial && samples.len() < 3 {
                    samples.push(NewMailSample { message: id, from, subject });
                }
                if !self.policy.leave_on_server {
                    c.dele(n).await?;
                }
            }
            state::save(&ctx.store, &key, &known)?;
            done = done.saturating_add(chunk.len() as u64);
            ctx.emit(SyncEvent::Progress {
                account: ctx.acct,
                phase: Phase::Headers,
                folder: Some(inbox.id),
                done,
                total,
            });
        }
        for (n, _) in &plan.delete {
            c.dele(*n).await?;
        }
        for f in &plan.forget {
            known.remove(f);
        }
        state::save(&ctx.store, &key, &known)?;
        c.quit().await?;
        if inserted > 0 {
            ctx.store.rethread_account(ctx.acct)?;
            ctx.emit(SyncEvent::FolderUpdated { account: ctx.acct, folder: inbox.id });
            if !initial {
                ctx.emit(SyncEvent::NewMail { account: ctx.acct, count: inserted, sample: samples });
                ctx.emit(SyncEvent::Arrived { account: ctx.acct, folder: inbox.id, messages: arrived });
            }
        }
        let backfill_pending = fresh_total > plan.download.len();
        ctx.emit(SyncEvent::Synced { account: ctx.acct, took: started.elapsed(), backfill_pending });
        Ok(CycleReport { backfill_pending })
    }
}

impl MailBackend for Pop3Account {
    fn ctx(&self) -> &Arc<Ctx> {
        &self.ctx
    }

    fn cycle(&self, _inbox_only: bool) -> BoxFuture<'_, Result<CycleReport>> {
        Box::pin(Pop3Account::cycle(self))
    }

    fn backfill(&self) -> BoxFuture<'_, Result<bool>> {
        Box::pin(async move { Pop3Account::cycle(self).await.map(|r| r.backfill_pending) })
    }

    /// En POP3 no hay estado remoto que tocar: las operaciones son solo locales.
    fn replay(&self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            let ctx = &self.ctx;
            for op in ctx.store.peek_ops(10_000)?.into_iter().filter(|o| o.account == ctx.acct) {
                // `outbox.send` es de la bandeja de salida: no se toca.
                if ops::classify(&op.kind, &op.payload).is_none() {
                    continue;
                }
                ctx.store.ack_op(op.seq)?;
            }
            Ok(())
        })
    }

    fn fetch_body(&self, msg: iuredesk_model::MessageId) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            let env = self.ctx.store.get_envelope(msg)?.ok_or_else(|| SyncError::MessageNotFound(msg.to_string()))?;
            if env.blob.is_some() { Ok(()) } else { Err(SyncError::MessageNotFound(msg.to_string())) }
        })
    }
}
