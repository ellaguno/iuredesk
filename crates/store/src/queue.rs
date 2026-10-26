//! Cola de operaciones pendientes (offline-first) y estado de sincronización genérico.
//!
//! Toda acción del usuario (mover, marcar, enviar…) se aplica primero al almacén local y se
//! encola aquí; `sync` la replica cuando hay red (PLAN.md §4). La operación es opaca para el
//! almacén: `kind` es una etiqueta corta y `payload` lo serializa quien la crea.

use chrono::{DateTime, Utc};
use iuredesk_model::AccountId;
use redb::{ReadableDatabase, ReadableTable};
use serde::{Deserialize, Serialize};

use crate::codec::clip;
use crate::error::{Result, StoreError};
use crate::tables::{META, OPS_QUEUE, SYNC_STATE};
use crate::{Store, get_rec, next_id, put_rec};

/// Tamaño máximo de la carga de una operación encolada.
pub const MAX_OP_PAYLOAD: usize = 1024 * 1024;
/// Tamaño máximo de un valor de estado de sincronización.
const MAX_SYNC_STATE: usize = 1024 * 1024;
/// Longitud máxima de una clave de estado de sincronización.
const MAX_SYNC_KEY: usize = 512;

/// Operación pendiente.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedOp {
    /// Número de secuencia (orden FIFO).
    pub seq: u64,
    /// Cuenta a la que va dirigida.
    pub account: AccountId,
    /// Tipo de operación (`move`, `flag`, `send`…), como mucho 64 bytes.
    pub kind: String,
    /// Datos serializados por quien la encola.
    pub payload: Vec<u8>,
    /// Intentos fallidos.
    pub attempts: u32,
    /// Momento en que se encoló.
    pub created: DateTime<Utc>,
    /// Último error (texto para mostrar), como mucho 1000 bytes.
    pub last_error: Option<String>,
}

impl Store {
    /// Encola una operación y devuelve su número de secuencia.
    pub fn enqueue_op(&self, account: AccountId, kind: &str, payload: &[u8]) -> Result<u64> {
        if payload.len() > MAX_OP_PAYLOAD {
            return Err(StoreError::TooLarge {
                what: "operación",
                size: payload.len() as u64,
                max: MAX_OP_PAYLOAD as u64,
            });
        }
        self.write(|tx, _| {
            let mut meta = tx.open_table(META)?;
            let seq = next_id(&mut meta, "op")?;
            let op = QueuedOp {
                seq,
                account,
                kind: clip(kind, 64).to_string(),
                payload: payload.to_vec(),
                attempts: 0,
                created: Utc::now(),
                last_error: None,
            };
            put_rec(&mut tx.open_table(OPS_QUEUE)?, seq, &op)?;
            Ok(seq)
        })
    }

    /// Las `limit` operaciones más antiguas, sin sacarlas de la cola.
    pub fn peek_ops(&self, limit: usize) -> Result<Vec<QueuedOp>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(OPS_QUEUE)?;
        let mut out = Vec::new();
        for item in t.iter()?.take(limit.min(10_000)) {
            let (_, v) = item?;
            out.push(crate::codec::decode(v.value())?);
        }
        Ok(out)
    }

    /// Confirma una operación (ya replicada): la saca de la cola. No falla si no existía.
    pub fn ack_op(&self, seq: u64) -> Result<()> {
        self.write(|tx, _| {
            tx.open_table(OPS_QUEUE)?.remove(seq)?;
            Ok(())
        })
    }

    /// Registra un intento fallido; devuelve el número de intentos acumulados.
    pub fn fail_op(&self, seq: u64, error: &str) -> Result<u32> {
        self.write(|tx, _| {
            let mut t = tx.open_table(OPS_QUEUE)?;
            let mut op: QueuedOp = get_rec(&t, seq)?.ok_or_else(|| StoreError::not_found("operación", seq))?;
            op.attempts = op.attempts.saturating_add(1);
            op.last_error = Some(clip(error, 1_000).to_string());
            put_rec(&mut t, seq, &op)?;
            Ok(op.attempts)
        })
    }

    /// Operaciones pendientes.
    pub fn ops_len(&self) -> Result<u64> {
        use redb::ReadableTableMetadata;
        let tx = self.db.begin_read()?;
        Ok(tx.open_table(OPS_QUEUE)?.len()?)
    }

    /// Estado de sincronización guardado bajo `key` (p. ej. `imap/acct:1/fold:3`).
    pub fn sync_state(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(SYNC_STATE)?;
        let v = t.get(clip(key, MAX_SYNC_KEY))?.map(|g| g.value().to_vec());
        Ok(v)
    }

    /// Guarda un estado de sincronización.
    pub fn put_sync_state(&self, key: &str, value: &[u8]) -> Result<()> {
        if value.len() > MAX_SYNC_STATE {
            return Err(StoreError::TooLarge {
                what: "estado de sincronización",
                size: value.len() as u64,
                max: MAX_SYNC_STATE as u64,
            });
        }
        if key.len() > MAX_SYNC_KEY {
            return Err(StoreError::Invalid("clave de estado demasiado larga".into()));
        }
        self.write(|tx, _| {
            tx.open_table(SYNC_STATE)?.insert(key, value)?;
            Ok(())
        })
    }

    /// Borra un estado de sincronización.
    pub fn delete_sync_state(&self, key: &str) -> Result<()> {
        self.write(|tx, _| {
            tx.open_table(SYNC_STATE)?.remove(clip(key, MAX_SYNC_KEY))?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_fifo_ack_fail() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let a = s.enqueue_op(AccountId(1), "flag", b"m1").unwrap();
        let b = s.enqueue_op(AccountId(1), "move", b"m2").unwrap();
        assert!(a < b);
        let ops = s.peek_ops(10).unwrap();
        assert_eq!(ops.iter().map(|o| o.seq).collect::<Vec<_>>(), vec![a, b]);
        assert_eq!(s.fail_op(a, "sin red").unwrap(), 1);
        assert_eq!(s.fail_op(a, "sin red").unwrap(), 2);
        assert_eq!(s.peek_ops(1).unwrap()[0].last_error.as_deref(), Some("sin red"));
        s.ack_op(a).unwrap();
        s.ack_op(a).unwrap();
        assert_eq!(s.ops_len().unwrap(), 1);
        assert!(s.fail_op(a, "x").is_err());
        assert!(s.enqueue_op(AccountId(1), "big", &vec![0; MAX_OP_PAYLOAD + 1]).is_err());
        // Persistencia y secuencia monótona tras reabrir.
        drop(s);
        let s = Store::open(dir.path()).unwrap();
        let c = s.enqueue_op(AccountId(2), "send", b"").unwrap();
        assert!(c > b);
    }

    #[test]
    fn sync_state_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        assert_eq!(s.sync_state("imap/1/INBOX").unwrap(), None);
        s.put_sync_state("imap/1/INBOX", b"uidvalidity=1;modseq=9").unwrap();
        assert_eq!(s.sync_state("imap/1/INBOX").unwrap().unwrap(), b"uidvalidity=1;modseq=9");
        s.delete_sync_state("imap/1/INBOX").unwrap();
        assert_eq!(s.sync_state("imap/1/INBOX").unwrap(), None);
        assert!(s.put_sync_state(&"k".repeat(600), b"").is_err());
    }
}
