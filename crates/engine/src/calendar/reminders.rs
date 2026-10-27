//! Recordatorios mientras la app está abierta: qué avisos tocan ahora, posponer y descartar.
//!
//! Un recordatorio de una aparición «toca» desde su hora (inicio − minutos) hasta que el evento
//! termina; se avisa una vez (notificación del escritorio) y queda en la ventana de
//! recordatorios hasta que se descarta o se pospone. Lo que se perdió con la app cerrada aparece
//! al abrirla si el evento no terminó.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Duration, Utc};
use iuredesk_model::{EventId, Reminder};

/// Opciones de «Posponer» (minutos).
pub const SNOOZE_CHOICES: [i64; 4] = [5, 10, 15, 60];

/// Identifica una aparición: evento y su inicio.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ReminderKey {
    pub event: EventId,
    pub start: i64,
}

/// Un recordatorio que toca ahora.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DueReminder {
    pub key: ReminderKey,
    pub summary: String,
    pub location: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub all_day: bool,
}

/// Una aparición candidata a avisar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub event: EventId,
    pub summary: String,
    pub location: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub all_day: bool,
    pub reminders: Vec<Reminder>,
}

/// Estado de los avisos (en memoria; al reabrir la app se vuelve a avisar lo pendiente).
#[derive(Debug, Default)]
pub struct ReminderState {
    dismissed: HashSet<ReminderKey>,
    snoozed: HashMap<ReminderKey, DateTime<Utc>>,
    notified: HashSet<ReminderKey>,
}

/// Primera hora de aviso de una aparición (el recordatorio más temprano).
pub fn first_fire(start: DateTime<Utc>, reminders: &[Reminder]) -> Option<DateTime<Utc>> {
    reminders
        .iter()
        .map(|r| start - Duration::minutes(r.minutes_before.clamp(-10_080, 60 * 24 * 60)))
        .min()
}

impl ReminderState {
    /// Avisos que tocan en `now` entre los candidatos, por hora de inicio.
    pub fn due(&self, candidates: &[Candidate], now: DateTime<Utc>) -> Vec<DueReminder> {
        let mut out: Vec<DueReminder> = candidates
            .iter()
            .filter_map(|c| {
                let key = ReminderKey { event: c.event, start: c.start.timestamp() };
                let fire = first_fire(c.start, &c.reminders)?;
                let until = c.end.max(c.start + Duration::minutes(1));
                if now < fire || now >= until || self.dismissed.contains(&key) {
                    return None;
                }
                if self.snoozed.get(&key).is_some_and(|s| now < *s) {
                    return None;
                }
                Some(DueReminder {
                    key,
                    summary: c.summary.clone(),
                    location: c.location.clone(),
                    start: c.start,
                    end: c.end,
                    all_day: c.all_day,
                })
            })
            .collect();
        out.sort_by_key(|d| (d.start, d.key));
        out.dedup_by_key(|d| d.key);
        out
    }

    /// Los que tocan y aún no se notificaron (se marcan como notificados).
    pub fn take_new(&mut self, due: &[DueReminder]) -> Vec<DueReminder> {
        due.iter().filter(|d| self.notified.insert(d.key)).cloned().collect()
    }

    /// Pospone `minutes` desde `now` (y se volverá a notificar).
    pub fn snooze(&mut self, key: ReminderKey, minutes: i64, now: DateTime<Utc>) {
        self.snoozed.insert(key, now + Duration::minutes(minutes.clamp(1, 7 * 24 * 60)));
        self.notified.remove(&key);
    }

    /// Descarta el aviso de esa aparición.
    pub fn dismiss(&mut self, key: ReminderKey) {
        self.dismissed.insert(key);
        self.snoozed.remove(&key);
    }

    /// Próximo momento en que algo puede empezar a tocar (para despertar la interfaz).
    pub fn next_fire(&self, candidates: &[Candidate], now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let fires = candidates.iter().filter_map(|c| first_fire(c.start, &c.reminders)).filter(|f| *f > now);
        let snoozes = self.snoozed.values().copied().filter(|s| *s > now);
        fires.chain(snoozes).min()
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn cand(start_h: u32, before: &[i64]) -> Candidate {
        let start = Utc.with_ymd_and_hms(2026, 10, 21, start_h, 0, 0).unwrap();
        Candidate {
            event: EventId(u64::from(start_h)),
            summary: "x".into(),
            location: String::new(),
            start,
            end: start + Duration::hours(1),
            all_day: false,
            reminders: before.iter().map(|m| Reminder { minutes_before: *m }).collect(),
        }
    }

    fn at(h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 21, h, m, 0).unwrap()
    }

    #[test]
    fn fires_from_reminder_until_end() {
        let st = ReminderState::default();
        let c = vec![cand(12, &[15, 60]), cand(14, &[]), cand(9, &[10])];
        assert!(st.due(&c, at(10, 59)).is_empty());
        let due = st.due(&c, at(11, 0));
        assert_eq!(due.len(), 1, "la de las 12 con aviso de 60 min");
        assert_eq!(st.due(&c, at(12, 59)).len(), 1);
        assert!(st.due(&c, at(13, 0)).is_empty(), "terminó");
        assert!(st.due(&c, at(13, 30)).iter().all(|d| d.key.event != EventId(14)), "sin recordatorios");
        assert_eq!(st.next_fire(&c, at(10, 0)), Some(at(11, 0)));
    }

    #[test]
    fn notify_once_snooze_and_dismiss() {
        let mut st = ReminderState::default();
        let c = vec![cand(12, &[15])];
        let due = st.due(&c, at(11, 50));
        assert_eq!(st.take_new(&due).len(), 1);
        assert!(st.take_new(&due).is_empty(), "una sola notificación");
        let key = due[0].key;
        st.snooze(key, 5, at(11, 50));
        assert!(st.due(&c, at(11, 54)).is_empty());
        let again = st.due(&c, at(11, 55));
        assert_eq!(st.take_new(&again).len(), 1, "vuelve a avisar tras posponer");
        st.dismiss(key);
        assert!(st.due(&c, at(11, 58)).is_empty());
    }
}
