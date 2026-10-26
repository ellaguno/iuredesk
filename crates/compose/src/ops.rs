//! Operaciones que el redactor deja en la cola del almacén para la capa `sync`.
//!
//! Contrato con `sync` (la cola es FIFO y compartida):
//! - `outbox.send` ([`OP_SEND`]) la consume **solo** la bandeja de salida de este crate; `sync`
//!   debe saltarla.
//! - `mail.sync` ([`OP_SYNC`]) lleva un [`SyncOp`] en JSON y la consume `sync`: guardar el
//!   enviado en la carpeta Enviados del servidor (salvo proveedores que ya lo hacen, como Gmail o
//!   Microsoft 365) y marcar el original como respondido o reenviado (`\Answered`, `$Forwarded`).
//!   El estado local ya se actualizó antes de encolar (offline-first).

use iuredesk_model::{BlobHash, MessageId};
use serde::{Deserialize, Serialize};

/// Tipo de operación de la bandeja de salida.
pub const OP_SEND: &str = "outbox.send";
/// Tipo de operación para `sync`.
pub const OP_SYNC: &str = "mail.sync";

/// Operación para `sync`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "op")]
pub enum SyncOp {
    /// Añadir el mensaje enviado (crudo, en el almacén de blobs) a la carpeta Enviados con la
    /// marca `\Seen` (IMAP `APPEND`).
    AppendToSent {
        /// Blob con los bytes RFC 5322 tal como se enviaron.
        raw: BlobHash,
        /// Tamaño en bytes.
        size: u64,
        /// `Message-ID` (para no duplicar si ya está).
        message_id: String,
    },
    /// Marcar el original como respondido.
    MarkAnswered { message: MessageId },
    /// Marcar el original como reenviado.
    MarkForwarded { message: MessageId },
}

impl SyncOp {
    /// JSON para la cola.
    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// Lee una operación de la cola (`None` si no es nuestra o está dañada).
    pub fn decode(payload: &[u8]) -> Option<Self> {
        serde_json::from_slice(payload).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let op = SyncOp::AppendToSent { raw: BlobHash([7; 32]), size: 10, message_id: "<a@b>".into() };
        assert_eq!(SyncOp::decode(&op.encode()), Some(op));
        let op = SyncOp::MarkAnswered { message: MessageId(3) };
        assert_eq!(SyncOp::decode(&op.encode()), Some(op));
        assert_eq!(SyncOp::decode(b"basura"), None);
    }
}
