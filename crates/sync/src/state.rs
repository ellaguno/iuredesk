//! Estado de sincronización persistido en el almacén (`sync_state`), en JSON.
//!
//! Claves:
//! - `acct-map/<id de cuenta>` → `AccountId` local (u64 LE).
//! - `imap/<cuenta>/<carpeta>` → [`FolderState`] (lo incremental: UIDVALIDITY, UIDNEXT, MODSEQ).
//! - `imap-bf/<cuenta>/<carpeta>` → [`BackfillState`] (cursor de la descarga de lo antiguo).
//! - `adopt/<carpeta>` → mensajes locales que esperan su UID (movidos/añadidos sin `COPYUID`).
//! - `pop3/<cuenta>` → UIDLs ya descargados y cuándo.

use std::collections::HashMap;

use iuredesk_model::account::Account;
use iuredesk_model::{AccountId, FolderId, MessageId};
use iuredesk_store::{AccountLabel, Store};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::error::Result;

/// Mensajes máximos en espera de UID por carpeta.
const MAX_ADOPT: usize = 2_000;

/// Estado incremental de una carpeta IMAP.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderState {
    /// UIDVALIDITY con el que se sincronizó.
    pub uid_validity: u32,
    /// Siguiente UID por descargar (todo lo de debajo, hasta el cursor de antiguos, está local).
    pub uid_next: u32,
    /// HIGHESTMODSEQ visto (CONDSTORE/QRESYNC).
    pub modseq: Option<u64>,
    /// `EXISTS` tras la última pasada (detecta borrados sin QRESYNC).
    pub exists: u32,
    /// Último repaso completo de banderas (segundos Unix).
    pub last_full_scan: i64,
}

/// Cursor de la descarga de mensajes antiguos.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackfillState {
    /// UIDVALIDITY al que pertenece el cursor.
    pub uid_validity: u32,
    /// Los UID menores que este aún no están en local.
    pub below: u32,
    /// Terminado.
    pub done: bool,
}

/// Clave del estado incremental.
pub fn folder_key(acct: AccountId, folder: FolderId) -> String {
    format!("imap/{}/{}", acct.get(), folder.get())
}

/// Clave del cursor de antiguos.
pub fn backfill_key(acct: AccountId, folder: FolderId) -> String {
    format!("imap-bf/{}/{}", acct.get(), folder.get())
}

/// Clave de los UIDL de POP3.
pub fn pop3_key(acct: AccountId) -> String {
    format!("pop3/{}", acct.get())
}

/// Lee un valor JSON; si falta o está dañado, `None` (se resincroniza desde cero).
pub fn load<T: DeserializeOwned>(store: &Store, key: &str) -> Option<T> {
    let bytes = store.sync_state(key).ok()??;
    serde_json::from_slice(&bytes).ok()
}

/// Guarda un valor JSON.
pub fn save<T: Serialize>(store: &Store, key: &str, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec(value).map_err(|e| crate::SyncError::Store(e.to_string()))?;
    store.put_sync_state(key, &bytes)?;
    Ok(())
}

/// Id local de una cuenta (se crea y se guarda la etiqueta la primera vez).
pub fn account_id(store: &Store, account: &Account) -> Result<AccountId> {
    let key = format!("acct-map/{}", account.id.chars().take(200).collect::<String>());
    let existing = store.sync_state(&key)?.and_then(|b| <[u8; 8]>::try_from(b.as_slice()).ok()).map(u64::from_le_bytes);
    let id = match existing {
        Some(v) if v != 0 => AccountId(v),
        _ => {
            let id = store.new_account_id()?;
            store.put_sync_state(&key, &id.get().to_le_bytes())?;
            id
        }
    };
    let label = AccountLabel { id, name: account.display_name.clone(), email: account.email.clone() };
    if !store.account_labels()?.contains(&label) {
        store.put_account_label(&label)?;
    }
    Ok(id)
}

/// Apunta un mensaje local que espera su UID en `folder`.
pub fn add_adopt(store: &Store, folder: FolderId, msg: MessageId) -> Result<()> {
    let key = format!("adopt/{}", folder.get());
    let mut v: Vec<u64> = load(store, &key).unwrap_or_default();
    if !v.contains(&msg.get()) {
        v.push(msg.get());
    }
    if v.len() > MAX_ADOPT {
        let extra = v.len().saturating_sub(MAX_ADOPT);
        v.drain(..extra);
    }
    save(store, &key, &v)
}

/// Mensajes en espera de UID en `folder`.
pub fn adopt_list(store: &Store, folder: FolderId) -> Vec<MessageId> {
    let v: Vec<u64> = load(store, &format!("adopt/{}", folder.get())).unwrap_or_default();
    v.into_iter().map(MessageId).collect()
}

/// Reemplaza la lista de espera de `folder`.
pub fn set_adopt(store: &Store, folder: FolderId, list: &[MessageId]) -> Result<()> {
    let key = format!("adopt/{}", folder.get());
    if list.is_empty() {
        store.delete_sync_state(&key)?;
        return Ok(());
    }
    save(store, &key, &list.iter().map(|m| m.get()).collect::<Vec<_>>())
}

/// UIDL de POP3 → segundo Unix de descarga.
pub type Pop3Known = HashMap<String, i64>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_mapping_is_stable() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let mut a: Account = serde_json::from_str(
            r#"{"id":"x1","display_name":"Ana","email":"ana@example.com",
            "incoming":{"protocol":"imap","host":"h","port":993,"security":"tls","auth":{"method":"password"},"username":"ana"},
            "outgoing":{"protocol":"smtp","host":"h","port":465,"security":"tls","auth":{"method":"password"},"username":"ana"}}"#,
        )
        .unwrap();
        let id = account_id(&s, &a).unwrap();
        assert_eq!(account_id(&s, &a).unwrap(), id);
        a.id = "x2".into();
        assert_ne!(account_id(&s, &a).unwrap(), id);
        assert_eq!(s.account_labels().unwrap().len(), 2);
        add_adopt(&s, FolderId(3), MessageId(9)).unwrap();
        add_adopt(&s, FolderId(3), MessageId(9)).unwrap();
        assert_eq!(adopt_list(&s, FolderId(3)), vec![MessageId(9)]);
        set_adopt(&s, FolderId(3), &[]).unwrap();
        assert!(adopt_list(&s, FolderId(3)).is_empty());
    }
}
