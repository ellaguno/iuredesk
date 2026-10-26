//! Identificadores locales del dominio.
//!
//! **Decisión:** cada id es un `u64` envuelto en un tipo propio (*newtype*):
//! - **Compacto:** 8 bytes; las claves de redb (tuplas `(carpeta, fecha, mensaje)`) quedan pequeñas y
//!   las páginas de índice caben más entradas por bloque.
//! - **Estable:** lo asigna el almacén local con un contador monótono persistido por tipo; nunca se
//!   reutiliza. Los ids remotos (UID de IMAP, ids de Graph/Gmail, `href` de DAV) son otra cosa y se
//!   guardan aparte como texto opaco.
//! - **Ordenado:** el orden numérico es el orden de alta, útil como desempate estable.
//!
//! Un ULID (128 bits) solo aportaría generación distribuida sin coordinación, que aquí no hace
//! falta: hay un único almacén local por perfil y la sincronización se apoya en los ids remotos.
//!
//! El valor `0` está reservado: significa «sin asignar» y el almacén lo sustituye por uno nuevo al
//! guardar.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

macro_rules! define_id {
    ($(#[$doc:meta])* $name:ident, $prefix:literal) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub u64);

        impl $name {
            /// Id reservado «sin asignar»; el almacén asigna uno nuevo al guardar.
            pub const UNASSIGNED: Self = Self(0);

            /// Crea el id a partir de su valor numérico.
            pub const fn new(v: u64) -> Self {
                Self(v)
            }

            /// Valor numérico del id.
            pub const fn get(self) -> u64 {
                self.0
            }

            /// `true` si el id aún no ha sido asignado por el almacén.
            pub const fn is_unassigned(self) -> bool {
                self.0 == 0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!($prefix, "{}"), self.0)
            }
        }

        impl From<u64> for $name {
            fn from(v: u64) -> Self {
                Self(v)
            }
        }
    };
}

define_id!(
    /// Cuenta de usuario (los datos de la cuenta viven en `account`).
    AccountId,
    "acct:"
);
define_id!(
    /// Carpeta de correo.
    FolderId,
    "fold:"
);
define_id!(
    /// Mensaje en el almacén local (no es el `Message-ID` de RFC 5322).
    MessageId,
    "msg:"
);
define_id!(
    /// Hilo de conversación.
    ThreadId,
    "thr:"
);
define_id!(
    /// Evento de calendario.
    EventId,
    "evt:"
);
define_id!(
    /// Calendario.
    CalendarId,
    "cal:"
);
define_id!(
    /// Contacto.
    ContactId,
    "con:"
);
define_id!(
    /// Libreta de direcciones.
    AddressBookId,
    "abk:"
);
define_id!(
    /// Nota.
    NoteId,
    "note:"
);
define_id!(
    /// Tarea.
    TaskId,
    "task:"
);
define_id!(
    /// Lista de tareas (colección VTODO de CalDAV, lista de To Do…).
    TaskListId,
    "tlist:"
);

/// Huella SHA-256 del contenido de un blob (correo crudo, adjunto, `.ics`, `.vcf`, foto).
///
/// Direcciona el almacén de blobs: el mismo contenido siempre tiene la misma huella.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct BlobHash(pub [u8; 32]);

impl BlobHash {
    /// Representación hexadecimal en minúsculas (64 caracteres).
    pub fn to_hex(&self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut s = String::with_capacity(64);
        for b in self.0 {
            s.push(char::from(HEX.get(usize::from(b >> 4)).copied().unwrap_or(b'0')));
            s.push(char::from(HEX.get(usize::from(b & 0x0f)).copied().unwrap_or(b'0')));
        }
        s
    }
}

impl fmt::Debug for BlobHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BlobHash({})", self.to_hex())
    }
}

impl fmt::Display for BlobHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// Error al interpretar una huella hexadecimal.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("huella de blob inválida: se esperaban 64 dígitos hexadecimales")]
pub struct BlobHashParseError;

impl FromStr for BlobHash {
    type Err = BlobHashParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes = s.as_bytes();
        if bytes.len() != 64 {
            return Err(BlobHashParseError);
        }
        let mut out = [0u8; 32];
        for (i, pair) in bytes.chunks_exact(2).enumerate() {
            let hi = hex_val(pair.first().copied())?;
            let lo = hex_val(pair.get(1).copied())?;
            if let Some(slot) = out.get_mut(i) {
                *slot = (hi << 4) | lo;
            }
        }
        Ok(Self(out))
    }
}

fn hex_val(c: Option<u8>) -> Result<u8, BlobHashParseError> {
    match c {
        Some(c @ b'0'..=b'9') => Ok(c - b'0'),
        Some(c @ b'a'..=b'f') => Ok(c - b'a' + 10),
        Some(c @ b'A'..=b'F') => Ok(c - b'A' + 10),
        _ => Err(BlobHashParseError),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_order_and_display() {
        assert!(MessageId(1) < MessageId(2));
        assert_eq!(FolderId(7).to_string(), "fold:7");
        assert!(ThreadId::UNASSIGNED.is_unassigned());
        assert_eq!(serde_json::to_string(&EventId(42)).unwrap(), "42");
    }

    #[test]
    fn blob_hash_roundtrip() {
        let mut b = [0u8; 32];
        for (i, x) in b.iter_mut().enumerate() {
            *x = (i * 7) as u8;
        }
        let h = BlobHash(b);
        let hex = h.to_hex();
        assert_eq!(hex.len(), 64);
        assert_eq!(hex.parse::<BlobHash>().unwrap(), h);
        assert!("zz".parse::<BlobHash>().is_err());
        assert!("g".repeat(64).parse::<BlobHash>().is_err());
        assert!("é".repeat(32).parse::<BlobHash>().is_err());
    }

    proptest::proptest! {
        #[test]
        fn blob_hash_parse_never_panics(s in ".{0,80}") {
            let _ = s.parse::<BlobHash>();
        }
    }
}
