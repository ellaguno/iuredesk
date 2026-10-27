//! Disponibilidad de otras personas para el asistente de programación.
//!
//! - CalDAV con buzón de salida de agenda (RFC 6638: iCloud, Fastmail, Nextcloud, Stalwart): un
//!   `POST` `VFREEBUSY` con todos los asistentes a la vez.
//! - Sin buzón de salida, o en Microsoft 365 (`getSchedule` queda pendiente), la respuesta es
//!   «disponibilidad desconocida» (`busy: None`) y la interfaz lo dice así.

use chrono::{DateTime, Utc};
use iuredesk_model::CalendarId;

use super::PimSyncHandle;
use crate::account::PimSource;
use crate::caldav::{CalDav, discovered_outbox};
use crate::error::{PimError, Result};

/// Destinatarios máximos por consulta.
const MAX_ATTENDEES: usize = 50;

/// Un intervalo ocupado.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BusySlot {
    /// Inicio.
    pub start: DateTime<Utc>,
    /// Fin.
    pub end: DateTime<Utc>,
    /// Provisional (`BUSY-TENTATIVE`).
    pub tentative: bool,
}

/// Disponibilidad de una persona.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FreeBusy {
    /// Correo (en minúsculas).
    pub email: String,
    /// Intervalos ocupados; `None` = el servidor no lo sabe o no lo comparte.
    pub busy: Option<Vec<BusySlot>>,
}

fn unknown(attendees: &[String]) -> Vec<FreeBusy> {
    attendees.iter().map(|a| FreeBusy { email: a.trim().to_lowercase(), busy: None }).collect()
}

impl PimSyncHandle {
    /// Disponibilidad de `attendees` entre `from` y `to`, preguntando al servidor de la cuenta del
    /// calendario `calendar`. Nunca falla por un asistente desconocido: ese queda en `None`.
    pub async fn free_busy(
        &self,
        calendar: CalendarId,
        attendees: Vec<String>,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<FreeBusy>> {
        let attendees: Vec<String> = attendees.into_iter().take(MAX_ATTENDEES).collect();
        if attendees.is_empty() || to <= from {
            return Ok(Vec::new());
        }
        let acct = self.calendar_account(calendar).ok_or_else(|| PimError::NotFound(calendar.to_string()))?;
        let Some(slot) = self.slot_by_acct(acct) else { return Ok(unknown(&attendees)) };
        let ctx = std::sync::Arc::clone(&slot.ctx);
        let PimSource::Dav { caldav, auth, .. } = &ctx.account.source else { return Ok(unknown(&attendees)) };
        let Some(outbox) = discovered_outbox(&ctx.store, acct) else { return Ok(unknown(&attendees)) };
        let dav = CalDav::connect(&ctx, caldav.clone(), None, auth).await?;
        let organizer = ctx.account.account.email.clone();
        let refs: Vec<&str> = attendees.iter().map(String::as_str).collect();
        let replies = dav.schedule_free_busy(&outbox, &organizer, &refs, from, to).await?;
        Ok(attendees
            .iter()
            .map(|a| {
                let email = a.trim().to_lowercase();
                let busy = replies
                    .iter()
                    .find(|r| r.recipient.trim().eq_ignore_ascii_case(&email))
                    // `2.x` = éxito (RFC 5545 §3.8.8.3); lo demás, desconocido.
                    .filter(|r| r.request_status.trim().starts_with('2'))
                    .map(|r| {
                        r.busy
                            .iter()
                            .filter(|b| !b.kind.eq_ignore_ascii_case("FREE"))
                            .take(2_000)
                            .map(|b| BusySlot {
                                start: b.start,
                                end: b.end,
                                tentative: b.kind.eq_ignore_ascii_case("BUSY-TENTATIVE"),
                            })
                            .collect()
                    });
                FreeBusy { email, busy }
            })
            .collect())
    }
}
