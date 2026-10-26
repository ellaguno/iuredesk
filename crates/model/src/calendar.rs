//! Calendario: calendarios y eventos (modelo cercano a iCalendar, RFC 5545).

use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{AccountId, CalendarId, EventId};
use crate::mail::Mailbox;

/// Calendario de una cuenta (colección CalDAV, calendario de Graph/Google, o local).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Calendar {
    /// Id local.
    pub id: CalendarId,
    /// Cuenta.
    pub account: AccountId,
    /// Nombre visible.
    pub name: String,
    /// Color `0xRRGGBB`, si el servidor lo da.
    pub color: Option<u32>,
    /// Ruta o id remoto, opaco.
    pub remote_path: String,
    /// Solo lectura (suscripción, calendario compartido sin permiso de escritura).
    pub read_only: bool,
    /// Estado de sincronización opaco (`sync-token` de WebDAV, `ctag`, `deltaLink`…).
    pub sync_token: Option<String>,
}

/// Momento de inicio/fin de un evento: fecha y hora con zona, o día completo.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventTime {
    /// Instante concreto. `tzid` es la zona original (`Europe/Madrid`, o un nombre de Windows que
    /// aún no se pudo traducir); se conserva para mostrar y para expandir recurrencias.
    DateTime {
        /// Instante en UTC.
        utc: DateTime<Utc>,
        /// Zona IANA o nombre original; `None` = UTC o «flotante».
        tzid: Option<String>,
    },
    /// Día completo (`VALUE=DATE`). Para eventos de día completo el fin es exclusivo.
    Date(NaiveDate),
}

impl Default for EventTime {
    fn default() -> Self {
        Self::DateTime { utc: DateTime::<Utc>::default(), tzid: None }
    }
}

impl EventTime {
    /// Instante aproximado en UTC: para días completos, la medianoche UTC de ese día (el
    /// almacén lo usa para indexar; la vista convierte a la zona local).
    pub fn to_utc(&self) -> DateTime<Utc> {
        match self {
            Self::DateTime { utc, .. } => *utc,
            Self::Date(d) => d.and_time(NaiveTime::MIN).and_utc(),
        }
    }

    /// `true` si es de día completo.
    pub fn is_all_day(&self) -> bool {
        matches!(self, Self::Date(_))
    }

    /// Zona horaria IANA reconocida, si la hay.
    pub fn tz(&self) -> Option<chrono_tz::Tz> {
        match self {
            Self::DateTime { tzid: Some(t), .. } => t.parse().ok(),
            _ => None,
        }
    }
}

/// Estado de participación de un asistente (`PARTSTAT`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PartStat {
    /// Sin respuesta.
    #[default]
    NeedsAction,
    /// Aceptado.
    Accepted,
    /// Rechazado.
    Declined,
    /// Provisional.
    Tentative,
    /// Delegado.
    Delegated,
}

/// Papel de un asistente (`ROLE`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AttendeeRole {
    /// Obligatorio.
    #[default]
    Required,
    /// Opcional.
    Optional,
    /// Presidente.
    Chair,
    /// Solo informativo.
    NonParticipant,
}

/// Asistente a un evento.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attendee {
    /// Nombre y dirección.
    pub mailbox: Mailbox,
    /// Respuesta.
    pub partstat: PartStat,
    /// Papel.
    pub role: AttendeeRole,
    /// Se pidió confirmación (`RSVP=TRUE`).
    pub rsvp: bool,
}

/// Recordatorio (`VALARM`) relativo al inicio.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reminder {
    /// Minutos antes del inicio (negativo = después).
    pub minutes_before: i64,
}

/// Estado del evento (`STATUS`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EventStatus {
    /// Confirmado.
    #[default]
    Confirmed,
    /// Provisional.
    Tentative,
    /// Cancelado.
    Cancelled,
}

/// Transparencia (`TRANSP`): si ocupa tiempo en la disponibilidad.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Transparency {
    /// Ocupa (`OPAQUE`).
    #[default]
    Opaque,
    /// No ocupa (`TRANSPARENT`).
    Transparent,
}

/// Evento de calendario (`VEVENT`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    /// Id local.
    pub id: EventId,
    /// Calendario.
    pub calendar: CalendarId,
    /// `UID` de iCalendar.
    pub uid: String,
    /// Título.
    pub summary: String,
    /// Lugar.
    pub location: String,
    /// Descripción (texto plano).
    pub description: String,
    /// Inicio.
    pub start: EventTime,
    /// Fin (exclusivo).
    pub end: EventTime,
    /// `RRULE` tal cual (sin expandir; la expansión es trabajo de la capa superior).
    pub rrule: Option<String>,
    /// `EXDATE` tal cual (valores iCalendar).
    pub exdates: Vec<String>,
    /// Organizador.
    pub organizer: Option<Mailbox>,
    /// Asistentes.
    pub attendees: Vec<Attendee>,
    /// Recordatorios.
    pub reminders: Vec<Reminder>,
    /// Estado.
    pub status: EventStatus,
    /// Transparencia.
    pub transparency: Transparency,
    /// `SEQUENCE`.
    pub sequence: u32,
    /// Id o `href` remoto, opaco.
    pub remote_id: Option<String>,
    /// `ETag` remoto, opaco.
    pub etag: Option<String>,
    /// Última modificación.
    pub updated: Option<DateTime<Utc>>,
}

impl Event {
    /// `true` si se repite.
    pub fn is_recurring(&self) -> bool {
        self.rrule.as_deref().is_some_and(|r| !r.trim().is_empty())
    }
}
