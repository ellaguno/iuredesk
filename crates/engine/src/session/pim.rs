//! Calendario, contactos y tareas en vivo: arranca `iuredesk-pim-sync` junto a la sincronización
//! de correo para las cuentas con CalDAV/CardDAV o Microsoft Graph, convierte sus avisos (cambios,
//! conflictos, errores) en refrescos y avisos, y pasa a la bandeja de salida las respuestas iMIP
//! (`mail.imip_reply`) que deja al responder a una invitación.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use iuredesk_compose::{CalendarMethod, CalendarPart, Draft, RichBody};
use iuredesk_model::account::Identity;
use iuredesk_pim_sync::{
    ConflictResolution, ImipReply, OP_IMIP_REPLY, PimAccount, PimEvent, PimSyncEngine, PimSyncHandle, PimSyncOptions,
};

use super::{Notice, Session};
use crate::lang::Text;

/// Parte PIM de la sesión.
pub(crate) struct Pim {
    pub handle: Option<PimSyncHandle>,
    generation: u64,
    tx: Sender<(u64, PimEvent)>,
    rx: Receiver<(u64, PimEvent)>,
}

impl Default for Pim {
    fn default() -> Self {
        let (tx, rx) = channel();
        Pim { handle: None, generation: 0, tx, rx }
    }
}

impl Session {
    /// (Re)arranca la sincronización de calendario y contactos (lo llama `restart_sync`).
    pub(crate) fn restart_pim(&mut self) {
        if let Some(old) = self.pim.handle.take() {
            self.bg.handle().spawn(async move {
                let _ = tokio::time::timeout(Duration::from_secs(15), old.shutdown()).await;
            });
        }
        self.pim.generation = self.pim.generation.wrapping_add(1);
        if self.sync_options.is_none() {
            return;
        }
        let accounts: Vec<PimAccount> = self.accounts.iter().filter_map(PimAccount::from_account).collect();
        if accounts.is_empty() {
            return;
        }
        let opts = PimSyncOptions {
            user_tz: crate::time::user_tz(self.zone),
            spanish: crate::lang::spanish(),
            ..Default::default()
        };
        let handle = PimSyncEngine::start(
            Arc::clone(&self.store),
            accounts,
            self.live.tokens.clone(),
            Arc::clone(&self.secrets),
            opts,
        );
        let mut events = handle.events();
        let (tx, generation, wake) = (self.pim.tx.clone(), self.pim.generation, self.bg.waker());
        self.bg.handle().spawn(async move {
            loop {
                match events.recv().await {
                    Ok(ev) => {
                        if tx.send((generation, ev)).is_err() {
                            break;
                        }
                        wake();
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        self.pim.handle = Some(handle);
    }

    /// Avisos de la sincronización PIM (desde `poll`).
    pub(crate) fn poll_pim(&mut self) {
        let current = self.pim.generation;
        let mut changed = false;
        let mut n = 0usize;
        while let Ok((generation, ev)) = self.pim.rx.try_recv() {
            n += 1;
            if n > 10_000 {
                break;
            }
            if generation != current {
                continue;
            }
            changed |= self.on_pim_event(ev);
        }
        if changed {
            self.refresh_pim_views();
        }
    }

    /// Atiende un aviso PIM como si hubiera llegado del motor (pruebas). Devuelve si hay que
    /// refrescar las vistas.
    #[doc(hidden)]
    pub fn feed_pim_event(&mut self, ev: PimEvent) -> bool {
        let r = self.on_pim_event(ev);
        if r {
            self.refresh_pim_views();
        }
        r
    }

    fn on_pim_event(&mut self, ev: PimEvent) -> bool {
        match ev {
            PimEvent::CollectionsChanged { .. }
            | PimEvent::CalendarChanged { .. }
            | PimEvent::ContactsChanged { .. }
            | PimEvent::TasksChanged { .. } => true,
            PimEvent::Conflict { summary, resolution, .. } => {
                let (en, es) = match resolution {
                    ConflictResolution::Merged => (
                        format!("«{summary}» changed here and on the server; both changes were kept."),
                        format!("«{summary}» cambió aquí y en el servidor; se juntaron los dos cambios."),
                    ),
                    ConflictResolution::ServerWon => (
                        format!("«{summary}» changed on the server at the same time; the server's version was kept."),
                        format!("«{summary}» cambió en el servidor a la vez; se conservó la versión del servidor."),
                    ),
                };
                self.notices.push(Notice::info(en, es));
                true
            }
            PimEvent::Synced { .. } => false,
            PimEvent::Error(f) => {
                let text =
                    Text::new(format!("Calendar and contacts: {}", f.en), format!("Calendario y contactos: {}", f.es));
                if f.needs_reauth
                    && let Some(a) = f.account
                {
                    self.live.status.insert(a, super::AccountSync::Error { text, reauth: true });
                } else if !f.transient && self.pim_last_error.as_ref() != Some(&text) {
                    self.pim_last_error = Some(text.clone());
                    self.notices.push(Notice::error(text));
                }
                false
            }
        }
    }

    fn refresh_pim_views(&mut self) {
        if let Ok(c) = self.store.calendars() {
            self.calendars = c;
        }
        self.events_cache = None;
        self.contacts_cache = None;
        self.data_gen = self.data_gen.wrapping_add(1);
    }

    /// Pasa a la bandeja de salida las respuestas iMIP que encoló `pim-sync` (contrato
    /// `mail.imip_reply` en `crates/pim-sync/src/ops.rs`).
    pub(crate) fn poll_imip_replies(&mut self) {
        let Some(sender) = &self.sender else { return };
        let ops: Vec<_> = self
            .store
            .peek_ops(10_000)
            .unwrap_or_default()
            .into_iter()
            .filter(|o| o.kind == OP_IMIP_REPLY)
            .take(50)
            .collect();
        for op in ops {
            let Some(r) = ImipReply::decode(&op.payload) else {
                // Dañada: se descarta para no atascar la cola.
                let _ = self.store.ack_op(op.seq);
                continue;
            };
            let Ok(draft) = imip_draft(&r) else {
                let _ = self.store.ack_op(op.seq);
                continue;
            };
            if sender.outbox.queue(op.account, &draft, &iuredesk_compose::FsResolver).is_ok() {
                let _ = self.store.ack_op(op.seq);
            }
        }
    }
}

/// Borrador de una respuesta iMIP (`METHOD:REPLY`).
pub(crate) fn imip_draft(r: &ImipReply) -> iuredesk_compose::Result<Draft> {
    let mut d = Draft::new(Identity {
        name: r.from.name.clone().unwrap_or_default(),
        email: r.from.addr.clone(),
        signature: None,
        reply_to: None,
    });
    d.to = vec![r.to.clone()];
    d.subject = r.subject.clone();
    d.body = RichBody::from_plain(&r.body);
    d.calendar = Some(CalendarPart::new(CalendarMethod::Reply, r.ics.clone().into_bytes())?);
    Ok(d)
}
