//! Pistas de categoría que da el proveedor (F5.3): Gmail (`X-GM-RAW "category:…"`) y Microsoft 365
//! (`inferenceClassification`). Se guardan por cuenta en `sync_state` (mensaje → texto) y las lee
//! el clasificador de la bandeja; las cuentas sin pistas usan solo las heurísticas locales.

use std::collections::BTreeMap;

use iuredesk_model::{AccountId, MessageId};
use iuredesk_store::Store;

use crate::error::Result;

/// Mensajes recordados por cuenta (se olvidan los de id más bajo).
pub const MAX_HINTS: usize = 40_000;

/// Categorías de Gmail que se consultan (Principal es «ninguna de estas»).
pub const GMAIL_CATEGORIES: [&str; 4] = ["promotions", "social", "updates", "forums"];

fn key(acct: AccountId) -> String {
    format!("cat-hint/{}", acct.get())
}

/// Pistas de una cuenta (id de mensaje → `promotions`, `social`, `updates`, `forums`,
/// `primary`, `focused`, `other`).
pub fn load(store: &Store, acct: AccountId) -> BTreeMap<u64, String> {
    store.sync_state(&key(acct)).ok().flatten().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// Agrega pistas.
pub fn record(store: &Store, acct: AccountId, items: &[(MessageId, &str)]) -> Result<()> {
    if items.is_empty() {
        return Ok(());
    }
    let mut map = load(store, acct);
    for (id, v) in items {
        map.insert(id.get(), (*v).chars().take(24).collect());
    }
    while map.len() > MAX_HINTS {
        let Some(first) = map.keys().next().copied() else { break };
        map.remove(&first);
    }
    let bytes = serde_json::to_vec(&map).map_err(|e| crate::SyncError::Store(e.to_string()))?;
    store.put_sync_state(&key(acct), &bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_and_bound() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        record(&s, AccountId(1), &[(MessageId(5), "promotions"), (MessageId(6), "other")]).unwrap();
        record(&s, AccountId(1), &[(MessageId(5), "social")]).unwrap();
        let m = load(&s, AccountId(1));
        assert_eq!(m.get(&5).map(String::as_str), Some("social"));
        assert_eq!(m.len(), 2);
        assert!(load(&s, AccountId(2)).is_empty());
    }
}
