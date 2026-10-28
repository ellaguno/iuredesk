//! «Posponer»: el correo sale de la bandeja de entrada y vuelve a la hora elegida, sin leer y
//! arriba de la lista.
//!
//! Local primero. Si la cuenta tiene una carpeta «Pospuestos»/«Snoozed» en el servidor, el
//! mensaje se mueve allí (se ve igual en todos los equipos); si no, se oculta en local y se marca
//! con la palabra clave IMAP `$Snoozed` para que otros clientes lo sepan.

use chrono::{DateTime, Datelike, Duration, NaiveTime, TimeZone, Timelike, Utc, Weekday};
use iuredesk_model::{AccountId, Flags, Folder, FolderId, FolderRole, MessageId};
use iuredesk_store::Store;
use serde::{Deserialize, Serialize};

use crate::Bi;
use crate::book;
use crate::error::Result;

/// Clave de las posposiciones.
pub const SNOOZE_KEY: &str = "rules/snoozes";
/// Posposiciones como mucho.
pub const MAX_SNOOZED: usize = 10_000;
/// Palabra clave IMAP de lo pospuesto sin carpeta.
pub const SNOOZE_KEYWORD: &str = "$Snoozed";
/// Nombres de carpeta que se reconocen como «Pospuestos».
pub const SNOOZE_FOLDER_NAMES: [&str; 6] = ["Snoozed", "Pospuestos", "Pospuesto", "Snooze", "Reportés", "Später"];

/// Hora elegida.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnoozePreset {
    /// Más tarde hoy (18:00, o dentro de 3 h si ya es tarde).
    LaterToday,
    /// Mañana a las 8:00.
    Tomorrow,
    /// Este fin de semana (sábado 9:00).
    ThisWeekend,
    /// La semana que viene (lunes 8:00).
    NextWeek,
    /// Fecha y hora elegidas.
    At(DateTime<Utc>),
}

impl SnoozePreset {
    /// Las fijas, en el orden del menú.
    pub const MENU: [SnoozePreset; 4] =
        [SnoozePreset::LaterToday, SnoozePreset::Tomorrow, SnoozePreset::ThisWeekend, SnoozePreset::NextWeek];

    /// Id para comandos (`mail.snooze:tomorrow`).
    pub fn key(self) -> String {
        match self {
            SnoozePreset::LaterToday => "later_today".into(),
            SnoozePreset::Tomorrow => "tomorrow".into(),
            SnoozePreset::ThisWeekend => "weekend".into(),
            SnoozePreset::NextWeek => "next_week".into(),
            SnoozePreset::At(t) => t.to_rfc3339(),
        }
    }

    /// Inversa de [`SnoozePreset::key`].
    pub fn from_key(s: &str) -> Option<SnoozePreset> {
        Some(match s {
            "later_today" => SnoozePreset::LaterToday,
            "tomorrow" => SnoozePreset::Tomorrow,
            "weekend" => SnoozePreset::ThisWeekend,
            "next_week" => SnoozePreset::NextWeek,
            other => SnoozePreset::At(DateTime::parse_from_rfc3339(other).ok()?.with_timezone(&Utc)),
        })
    }

    /// Nombre en el menú.
    pub fn label(self) -> Bi {
        match self {
            SnoozePreset::LaterToday => Bi::new("Later today", "Más tarde hoy"),
            SnoozePreset::Tomorrow => Bi::new("Tomorrow", "Mañana"),
            SnoozePreset::ThisWeekend => Bi::new("This weekend", "Este fin de semana"),
            SnoozePreset::NextWeek => Bi::new("Next week", "La semana que viene"),
            SnoozePreset::At(_) => Bi::new("Pick date & time…", "Elegir fecha y hora…"),
        }
    }

    /// Momento en que vuelve, con `now` en la zona del usuario. `None` si no tiene sentido
    /// (p. ej. «Más tarde hoy» a las 22:00).
    pub fn resolve<Tz: TimeZone>(self, now: &DateTime<Tz>) -> Option<DateTime<Utc>> {
        let tz = now.timezone();
        let at = |date: chrono::NaiveDate, h: u32, m: u32| -> Option<DateTime<Utc>> {
            let t = date.and_time(NaiveTime::from_hms_opt(h, m, 0)?);
            tz.from_local_datetime(&t).earliest().map(|d| d.with_timezone(&Utc))
        };
        let today = now.date_naive();
        match self {
            SnoozePreset::LaterToday => {
                if now.hour() < 15 {
                    at(today, 18, 0)
                } else {
                    // Tres horas, redondeado a la hora siguiente; solo si sigue siendo hoy (≤ 22:00).
                    let round = u32::from(now.minute() > 0 || now.second() > 0);
                    let h = now.hour().saturating_add(3).saturating_add(round);
                    if h <= 22 { at(today, h, 0) } else { None }
                }
            }
            SnoozePreset::Tomorrow => at(today.succ_opt()?, 8, 0),
            SnoozePreset::ThisWeekend => {
                let wd = today.weekday();
                let days = match wd {
                    Weekday::Sat if now.hour() < 9 => 0,
                    // El sábado tarde y el domingo ya es fin de semana: el siguiente.
                    Weekday::Sat => 7,
                    Weekday::Sun => 6,
                    other => i64::from(Weekday::Sat.num_days_from_monday()) - i64::from(other.num_days_from_monday()),
                };
                at(today.checked_add_signed(Duration::days(days))?, 9, 0)
            }
            SnoozePreset::NextWeek => {
                let to_monday = 7 - i64::from(today.weekday().num_days_from_monday());
                at(today.checked_add_signed(Duration::days(to_monday))?, 8, 0)
            }
            SnoozePreset::At(t) => (t > now.with_timezone(&Utc)).then_some(t),
        }
    }
}

/// Dónde está el mensaje mientras tanto.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum SnoozeMode {
    /// Movido a la carpeta «Pospuestos» de la cuenta.
    Moved {
        /// Carpeta.
        to: FolderId,
    },
    /// Oculto en local (`$Snoozed` en el servidor).
    Hidden,
}

/// Un mensaje pospuesto.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snoozed {
    /// Mensaje.
    pub msg: MessageId,
    /// Cuenta.
    pub account: AccountId,
    /// Carpeta de donde salió (a donde vuelve).
    pub from_folder: FolderId,
    /// Vuelve a esta hora.
    pub until: DateTime<Utc>,
    /// Dónde espera.
    pub mode: SnoozeMode,
    /// Asunto (para la lista de pospuestos).
    #[serde(default)]
    pub subject: String,
    /// Remitente para mostrar.
    #[serde(default)]
    pub from: String,
}

/// Lo pospuesto.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnoozeBook {
    /// Entradas, por hora de vuelta.
    pub items: Vec<Snoozed>,
}

impl SnoozeBook {
    /// Lee.
    pub fn load(store: &Store) -> SnoozeBook {
        book::load(store, SNOOZE_KEY).unwrap_or_default()
    }

    /// Guarda.
    pub fn save(&self, store: &Store) -> Result<()> {
        book::save(store, SNOOZE_KEY, self)
    }

    /// ¿Está pospuesto y oculto en local?
    pub fn is_hidden(&self, msg: MessageId) -> bool {
        self.items.iter().any(|s| s.msg == msg && s.mode == SnoozeMode::Hidden)
    }

    /// Entrada de un mensaje.
    pub fn get(&self, msg: MessageId) -> Option<&Snoozed> {
        self.items.iter().find(|s| s.msg == msg)
    }

    /// Los que ya tienen que volver.
    pub fn due(&self, now: DateTime<Utc>) -> Vec<Snoozed> {
        self.items.iter().filter(|s| s.until <= now).cloned().collect()
    }

    /// Próxima vuelta (para despertar a la app).
    pub fn next_wake(&self) -> Option<DateTime<Utc>> {
        self.items.iter().map(|s| s.until).min()
    }

    fn insert(&mut self, s: Snoozed) {
        self.items.retain(|x| x.msg != s.msg);
        if self.items.len() >= MAX_SNOOZED {
            self.items.sort_by_key(|x| x.until);
            self.items.pop();
        }
        self.items.push(s);
        self.items.sort_by_key(|x| x.until);
    }

    fn take(&mut self, msg: MessageId) -> Option<Snoozed> {
        let pos = self.items.iter().position(|s| s.msg == msg)?;
        Some(self.items.remove(pos))
    }
}

/// Carpeta «Pospuestos» de una cuenta, si existe en el servidor.
pub fn snooze_folder(folders: &[Folder], account: AccountId) -> Option<FolderId> {
    folders
        .iter()
        .filter(|f| f.account == account && f.role == FolderRole::Custom)
        .find(|f| SNOOZE_FOLDER_NAMES.iter().any(|n| f.name.eq_ignore_ascii_case(n)))
        .map(|f| f.id)
}

/// Pospone un mensaje hasta `until`. Devuelve la entrada y la operación encolada (para
/// deshacer).
pub fn snooze(
    store: &Store,
    book: &mut SnoozeBook,
    folders: &[Folder],
    msg: MessageId,
    until: DateTime<Utc>,
) -> Result<(Snoozed, Option<u64>)> {
    let env = store.get_envelope(msg)?.ok_or_else(|| crate::RulesError::Invalid("el mensaje ya no existe".into()))?;
    // Volver a posponer algo pospuesto: conserva la carpeta de origen.
    let prev = book.take(msg);
    let from_folder = prev.as_ref().map_or(env.folder, |p| p.from_folder);
    let (mode, seq) = match snooze_folder(folders, env.account) {
        Some(to) if to != env.folder => (SnoozeMode::Moved { to }, iuredesk_sync::ops::move_message(store, msg, to)?),
        Some(to) => (SnoozeMode::Moved { to }, None),
        None => {
            let mut seq = None;
            if !env.keywords.iter().any(|k| k == SNOOZE_KEYWORD) {
                let mut e = env.clone();
                e.keywords.push(SNOOZE_KEYWORD.into());
                seq = crate::apply::put_keywords(store, e, true, SNOOZE_KEYWORD)?;
            }
            (SnoozeMode::Hidden, seq)
        }
    };
    let s = Snoozed {
        msg,
        account: env.account,
        from_folder,
        until,
        mode,
        subject: env.subject.clone(),
        from: env.sender_display().to_string(),
    };
    book.insert(s.clone());
    Ok((s, seq))
}

/// Cancela una posposición sin marcarlo como no leído (deshacer o «Quitar posposición»).
pub fn unsnooze(store: &Store, book: &mut SnoozeBook, msg: MessageId) -> Result<Option<Snoozed>> {
    let Some(s) = book.take(msg) else { return Ok(None) };
    restore(store, &s, false)?;
    Ok(Some(s))
}

/// Devuelve a la bandeja lo que ya toca: lo mueve de vuelta y lo marca como no leído.
pub fn wake_due(store: &Store, book: &mut SnoozeBook, now: DateTime<Utc>) -> Result<Vec<Snoozed>> {
    let due = book.due(now);
    let mut woke = Vec::new();
    for s in due.into_iter().take(500) {
        book.take(s.msg);
        if restore(store, &s, true)? {
            woke.push(s);
        }
    }
    Ok(woke)
}

/// `true` si el mensaje seguía existiendo.
fn restore(store: &Store, s: &Snoozed, unread: bool) -> Result<bool> {
    let Some(env) = store.get_envelope(s.msg)? else { return Ok(false) };
    if let SnoozeMode::Moved { to } = s.mode
        && env.folder == to
        && store.get_folder(s.from_folder)?.is_some()
    {
        iuredesk_sync::ops::move_message(store, s.msg, s.from_folder)?;
    }
    if let Some(mut e) = store.get_envelope(s.msg)? {
        if e.keywords.iter().any(|k| k == SNOOZE_KEYWORD) {
            e.keywords.retain(|k| k != SNOOZE_KEYWORD);
            crate::apply::put_keywords(store, e.clone(), false, SNOOZE_KEYWORD)?;
        }
        if unread && e.flags.seen {
            iuredesk_sync::ops::set_flags(store, s.msg, Flags { seen: false, ..e.flags })?;
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    fn at(d: u32, h: u32, m: u32) -> DateTime<FixedOffset> {
        // Ciudad de México (UTC−6). Octubre de 2026: el 21 es miércoles.
        FixedOffset::west_opt(6 * 3600).unwrap().with_ymd_and_hms(2026, 10, d, h, m, 0).unwrap()
    }

    fn local(t: DateTime<Utc>) -> String {
        t.with_timezone(&FixedOffset::west_opt(6 * 3600).unwrap()).format("%a %d %H:%M").to_string()
    }

    #[test]
    fn presets_with_fake_clock() {
        let wed_morning = at(21, 9, 30);
        assert_eq!(local(SnoozePreset::LaterToday.resolve(&wed_morning).unwrap()), "Wed 21 18:00");
        assert_eq!(local(SnoozePreset::Tomorrow.resolve(&wed_morning).unwrap()), "Thu 22 08:00");
        assert_eq!(local(SnoozePreset::ThisWeekend.resolve(&wed_morning).unwrap()), "Sat 24 09:00");
        assert_eq!(local(SnoozePreset::NextWeek.resolve(&wed_morning).unwrap()), "Mon 26 08:00");
        // Por la tarde: tres horas después, redondeado.
        assert_eq!(local(SnoozePreset::LaterToday.resolve(&at(21, 16, 10)).unwrap()), "Wed 21 20:00");
        // De noche ya no hay «más tarde hoy».
        assert_eq!(SnoozePreset::LaterToday.resolve(&at(21, 21, 0)), None);
        // Sábado temprano: hoy; sábado tarde o domingo: el siguiente.
        assert_eq!(local(SnoozePreset::ThisWeekend.resolve(&at(24, 7, 0)).unwrap()), "Sat 24 09:00");
        assert_eq!(local(SnoozePreset::ThisWeekend.resolve(&at(24, 12, 0)).unwrap()), "Sat 31 09:00");
        assert_eq!(local(SnoozePreset::ThisWeekend.resolve(&at(25, 12, 0)).unwrap()), "Sat 31 09:00");
        // El domingo, «la semana que viene» es mañana lunes.
        assert_eq!(local(SnoozePreset::NextWeek.resolve(&at(25, 12, 0)).unwrap()), "Mon 26 08:00");
        // Una hora pasada no vale.
        let past = SnoozePreset::At(wed_morning.with_timezone(&Utc) - Duration::hours(1));
        assert_eq!(past.resolve(&wed_morning), None);
        for p in SnoozePreset::MENU {
            assert_eq!(SnoozePreset::from_key(&p.key()), Some(p));
        }
        let t = SnoozePreset::At(Utc.with_ymd_and_hms(2026, 11, 2, 15, 0, 0).unwrap());
        assert_eq!(SnoozePreset::from_key(&t.key()), Some(t));
        assert_eq!(SnoozePreset::from_key("basura"), None);
    }

    #[test]
    fn book_due_and_order() {
        let base = Utc.with_ymd_and_hms(2026, 10, 21, 12, 0, 0).unwrap();
        let mut b = SnoozeBook::default();
        for (i, h) in [(1u64, 5i64), (2, 1), (3, 3)] {
            b.insert(Snoozed {
                msg: MessageId(i),
                account: AccountId(1),
                from_folder: FolderId(1),
                until: base + Duration::hours(h),
                mode: SnoozeMode::Hidden,
                subject: String::new(),
                from: String::new(),
            });
        }
        assert_eq!(b.next_wake(), Some(base + Duration::hours(1)));
        assert_eq!(b.due(base + Duration::hours(3)).iter().map(|s| s.msg.0).collect::<Vec<_>>(), vec![2, 3]);
        assert!(b.is_hidden(MessageId(1)));
    }
}
