//! Persistencia en el almacén local (`sync_state`, JSON): reglas y preferencias, registro de
//! envíos automáticos, posposiciones y seguimientos. Cada clave tiene su tope.

use std::collections::{BTreeMap, VecDeque};

use chrono::{DateTime, Duration, Utc};
use iuredesk_model::AccountId;
use iuredesk_store::Store;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::error::{Result, RulesError};
use crate::focus::Focus;
use crate::rule::{MAX_RULES, Rule, ensure_valid};
use crate::vacation::Vacation;

/// Clave del libro de reglas.
pub const BOOK_KEY: &str = "rules/book";
/// Clave del registro de envíos automáticos.
pub const LOG_KEY: &str = "rules/sendlog";
/// Reenvíos automáticos como mucho por hora (todas las reglas).
pub const MAX_SENDS_PER_HOUR: usize = 30;
/// Remitentes recordados para «una vez por remitente».
const MAX_REPLIED: usize = 5_000;
/// `Message-ID` recordados ya reenviados.
const MAX_FORWARDED: usize = 5_000;
/// Excepciones de Enfocados/Otros.
pub const MAX_OVERRIDES: usize = 5_000;

/// Lee un valor JSON (`None` si falta o está dañado: nunca impide arrancar).
pub fn load<T: DeserializeOwned>(store: &Store, key: &str) -> Option<T> {
    store.sync_state(key).ok().flatten().and_then(|b| serde_json::from_slice(&b).ok())
}

/// Guarda un valor JSON.
pub fn save<T: Serialize>(store: &Store, key: &str, value: &T) -> Result<()> {
    store.put_sync_state(key, &serde_json::to_vec(value)?)?;
    Ok(())
}

/// Estado de Sieve de una cuenta.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SieveAccount {
    /// El servidor admite ManageSieve.
    pub available: bool,
    /// Subir las reglas al servidor.
    pub enabled: bool,
    /// Servidor y puerto (`mail.example.com:4190`).
    #[serde(default)]
    pub host: String,
    /// Reglas que corren en el servidor (las demás, en local).
    #[serde(default)]
    pub server_rules: Vec<u64>,
    /// La respuesta de vacaciones corre en el servidor.
    #[serde(default)]
    pub server_vacation: bool,
    /// Última subida correcta.
    #[serde(default)]
    pub synced_at: Option<DateTime<Utc>>,
    /// Último error (es).
    #[serde(default)]
    pub last_error: Option<String>,
    /// Extensiones que anuncia el servidor (`fileinto`, `vacation`…).
    #[serde(default)]
    pub extensions: Vec<String>,
}

/// Preferencias de «Enfocados / Otros».
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FocusPrefs {
    /// Separar la bandeja de entrada en dos pestañas.
    pub enabled: bool,
    /// «Mover siempre a…» por dirección o dominio (en minúsculas).
    #[serde(default)]
    pub overrides: BTreeMap<String, Focus>,
}

impl Default for FocusPrefs {
    fn default() -> Self {
        Self { enabled: true, overrides: BTreeMap::new() }
    }
}

/// Preferencias de las categorías de la bandeja (Principal, Promociones…).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CategoryPrefs {
    /// Mostrar pestañas de categorías (si no, una sola bandeja).
    pub enabled: bool,
    /// «Mover siempre a…» por dirección o dominio (en minúsculas).
    #[serde(default)]
    pub overrides: BTreeMap<String, crate::category::Category>,
}

impl Default for CategoryPrefs {
    fn default() -> Self {
        Self { enabled: true, overrides: BTreeMap::new() }
    }
}

/// Colores de etiquetas por omisión (se elige por el nombre).
pub const LABEL_PALETTE: [u32; 10] =
    [0xD93025, 0xE37400, 0xF9AB00, 0x1E8E3E, 0x12B5CB, 0x1A73E8, 0x8E44AD, 0xC2185B, 0x795548, 0x5F6368];

/// Color por omisión de una etiqueta (estable: depende solo del nombre).
pub fn default_label_color(name: &str) -> u32 {
    let mut h: u32 = 2_166_136_261;
    for b in name.to_lowercase().bytes() {
        h = (h ^ u32::from(b)).wrapping_mul(16_777_619);
    }
    LABEL_PALETTE.get((h as usize) % LABEL_PALETTE.len()).copied().unwrap_or(0x5F6368)
}

/// Reglas y preferencias.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleBook {
    /// Siguiente id.
    #[serde(default)]
    pub next_id: u64,
    /// Reglas, en orden de aplicación.
    #[serde(default)]
    pub rules: Vec<Rule>,
    /// Enfocados / Otros.
    #[serde(default)]
    pub focus: FocusPrefs,
    /// Respuesta de vacaciones.
    #[serde(default)]
    pub vacation: Vacation,
    /// Sieve por cuenta (`AccountId` como texto).
    #[serde(default)]
    pub sieve: BTreeMap<String, SieveAccount>,
    /// Categorías de la bandeja.
    #[serde(default)]
    pub categories: CategoryPrefs,
    /// Color elegido por etiqueta (nombre en minúsculas → 0xRRGGBB).
    #[serde(default)]
    pub label_colors: BTreeMap<String, u32>,
}

impl RuleBook {
    /// Lee el libro (vacío si no hay).
    pub fn load(store: &Store) -> RuleBook {
        load(store, BOOK_KEY).unwrap_or_default()
    }

    /// Guarda el libro.
    pub fn save(&self, store: &Store) -> Result<()> {
        save(store, BOOK_KEY, self)
    }

    /// Agrega o reemplaza una regla (id 0 = nueva). Devuelve su id.
    pub fn upsert(&mut self, mut rule: Rule) -> Result<u64> {
        ensure_valid(&rule)?;
        if rule.name.trim().is_empty() {
            rule.name = rule.summary().es.chars().take(60).collect();
        }
        if rule.id != 0
            && let Some(slot) = self.rules.iter_mut().find(|r| r.id == rule.id)
        {
            *slot = rule.clone();
            return Ok(rule.id);
        }
        if self.rules.len() >= MAX_RULES {
            return Err(RulesError::Invalid("demasiadas reglas".into()));
        }
        self.next_id = self.next_id.max(self.rules.iter().map(|r| r.id).max().unwrap_or(0)).saturating_add(1);
        rule.id = self.next_id;
        self.rules.push(rule);
        Ok(self.next_id)
    }

    /// Quita una regla. Devuelve la quitada y su posición (para deshacer).
    pub fn remove(&mut self, id: u64) -> Option<(usize, Rule)> {
        let pos = self.rules.iter().position(|r| r.id == id)?;
        Some((pos, self.rules.remove(pos)))
    }

    /// Sube (`-1`) o baja (`+1`) una regla.
    pub fn shift(&mut self, id: u64, delta: i32) {
        let Some(pos) = self.rules.iter().position(|r| r.id == id) else { return };
        let to = if delta < 0 { pos.checked_sub(1) } else { pos.checked_add(1).filter(|p| *p < self.rules.len()) };
        if let Some(to) = to {
            self.rules.swap(pos, to);
        }
    }

    /// Una regla por id.
    pub fn get(&self, id: u64) -> Option<&Rule> {
        self.rules.iter().find(|r| r.id == id)
    }

    /// Estado de Sieve de una cuenta.
    pub fn sieve_of(&self, account: AccountId) -> Option<&SieveAccount> {
        self.sieve.get(&account.get().to_string())
    }

    /// Estado de Sieve de una cuenta, para cambiarlo.
    pub fn sieve_mut(&mut self, account: AccountId) -> &mut SieveAccount {
        self.sieve.entry(account.get().to_string()).or_default()
    }

    /// Reglas que en esta cuenta ya corren en el servidor (se omiten en local al llegar).
    pub fn server_side(&self, account: AccountId) -> std::collections::HashSet<u64> {
        self.sieve_of(account)
            .filter(|s| s.enabled && s.available && s.synced_at.is_some())
            .map(|s| s.server_rules.iter().copied().collect())
            .unwrap_or_default()
    }

    /// Color de una etiqueta (el elegido o el de omisión).
    pub fn label_color(&self, name: &str) -> u32 {
        self.label_colors.get(&name.to_lowercase()).copied().unwrap_or_else(|| default_label_color(name))
    }

    /// «Mover siempre a <categoría>» para una dirección o dominio (`None` = quitar).
    pub fn set_category_override(&mut self, key: &str, cat: Option<crate::category::Category>) {
        let k = crate::norm_addr(key);
        if k.is_empty() {
            return;
        }
        match cat {
            Some(c) => {
                if self.categories.overrides.len() < MAX_OVERRIDES || self.categories.overrides.contains_key(&k) {
                    self.categories.overrides.insert(k, c);
                }
            }
            None => {
                self.categories.overrides.remove(&k);
            }
        }
    }

    /// «Mover siempre a Enfocados/Otros» para una dirección o dominio (`None` = quitar).
    pub fn set_override(&mut self, key: &str, focus: Option<Focus>) {
        let k = crate::norm_addr(key);
        if k.is_empty() {
            return;
        }
        match focus {
            Some(f) => {
                if self.focus.overrides.len() < MAX_OVERRIDES || self.focus.overrides.contains_key(&k) {
                    self.focus.overrides.insert(k, f);
                }
            }
            None => {
                self.focus.overrides.remove(&k);
            }
        }
    }
}

/// Registro de lo enviado automáticamente (bucles, «una vez por remitente», tope por hora).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendLog {
    /// Remitente → última respuesta automática.
    #[serde(default)]
    pub replied: BTreeMap<String, DateTime<Utc>>,
    /// `Message-ID` ya reenviados (los más recientes al final).
    #[serde(default)]
    pub forwarded: VecDeque<String>,
    /// Momentos de los últimos envíos automáticos.
    #[serde(default)]
    pub recent: VecDeque<DateTime<Utc>>,
}

impl SendLog {
    /// Lee el registro.
    pub fn load(store: &Store) -> SendLog {
        load(store, LOG_KEY).unwrap_or_default()
    }

    /// Guarda el registro.
    pub fn save(&self, store: &Store) -> Result<()> {
        save(store, LOG_KEY, self)
    }

    /// Envíos automáticos en la última hora.
    pub fn sends_last_hour(&self, now: DateTime<Utc>) -> usize {
        self.recent.iter().filter(|t| now.signed_duration_since(**t) < Duration::hours(1)).count()
    }

    fn sent(&mut self, now: DateTime<Utc>) {
        self.recent.retain(|t| now.signed_duration_since(*t) < Duration::hours(1));
        self.recent.push_back(now);
        while self.recent.len() > MAX_SENDS_PER_HOUR.saturating_mul(2) {
            self.recent.pop_front();
        }
    }

    /// Anota un reenvío.
    pub fn record_forward(&mut self, message_id: Option<&str>, now: DateTime<Utc>) {
        if let Some(m) = message_id {
            self.forwarded.push_back(m.to_string());
            while self.forwarded.len() > MAX_FORWARDED {
                self.forwarded.pop_front();
            }
        }
        self.sent(now);
    }

    /// Anota una respuesta automática.
    pub fn record_reply(&mut self, sender: &str, now: DateTime<Utc>) {
        self.replied.insert(sender.to_string(), now);
        if self.replied.len() > MAX_REPLIED
            && let Some(oldest) = self.replied.iter().min_by_key(|(_, t)| **t).map(|(k, _)| k.clone())
        {
            self.replied.remove(&oldest);
        }
        self.sent(now);
    }

    /// Ya se reenvió este mensaje.
    pub fn was_forwarded(&self, message_id: Option<&str>) -> bool {
        message_id.is_some_and(|m| self.forwarded.iter().any(|x| x == m))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule::{Action, Condition, Field, TextOp};

    fn r(name: &str) -> Rule {
        Rule {
            name: name.into(),
            conditions: vec![Condition::Text { field: Field::From, op: TextOp::Contains, value: "x".into() }],
            actions: vec![Action::Flag],
            ..Rule::default()
        }
    }

    #[test]
    fn upsert_shift_remove_persist() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let mut b = RuleBook::load(&s);
        let a = b.upsert(r("a")).unwrap();
        let c = b.upsert(r("c")).unwrap();
        assert!(c > a);
        b.shift(c, -1);
        assert_eq!(b.rules.iter().map(|r| r.id).collect::<Vec<_>>(), vec![c, a]);
        b.shift(c, -1);
        let mut edited = b.get(a).unwrap().clone();
        edited.name = "A".into();
        assert_eq!(b.upsert(edited).unwrap(), a);
        assert_eq!(b.get(a).unwrap().name, "A");
        assert!(b.upsert(Rule::default()).is_err(), "regla vacía");
        b.set_override("News@Tienda.example", Some(Focus::Other));
        b.save(&s).unwrap();
        let mut b2 = RuleBook::load(&s);
        assert_eq!(b2, b);
        assert_eq!(b2.focus.overrides.get("news@tienda.example"), Some(&Focus::Other));
        assert_eq!(b2.remove(c).map(|(p, _)| p), Some(0));
        // Un id nuevo nunca reutiliza uno anterior.
        let d = b2.upsert(r("d")).unwrap();
        assert!(d > c);
    }

    #[test]
    fn send_log_limits() {
        let now = Utc::now();
        let mut l = SendLog::default();
        for i in 0..100 {
            l.record_forward(Some(&format!("m{i}")), now);
        }
        assert!(l.was_forwarded(Some("m99")));
        assert!(!l.was_forwarded(None));
        assert!(l.recent.len() <= MAX_SENDS_PER_HOUR * 2);
        assert_eq!(l.sends_last_hour(now + Duration::hours(2)), 0);
    }
}

#[cfg(test)]
mod label_tests {
    use super::*;

    #[test]
    fn label_colors_are_stable_and_editable() {
        let mut b = RuleBook::default();
        let c = b.label_color("Clientes");
        assert_eq!(c, b.label_color("clientes"));
        assert!(LABEL_PALETTE.contains(&c));
        b.label_colors.insert("clientes".into(), 0x123456);
        assert_eq!(b.label_color("Clientes"), 0x123456);
        b.set_category_override("Ofertas@Tienda.example", Some(crate::category::Category::Promotions));
        assert_eq!(b.categories.overrides.get("ofertas@tienda.example"), Some(&crate::category::Category::Promotions));
    }
}
