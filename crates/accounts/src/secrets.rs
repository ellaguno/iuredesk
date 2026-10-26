//! Secretos de las cuentas en el llavero del sistema: servicio `iuredesk`, usuario
//! `<id-de-cuenta>:<propósito>`. Nunca en disco plano.
//!
//! En Linux, Secret Service por D-Bus con `zbus` (Rust puro; GNOME Keyring, KWallet); en Windows, el
//! administrador de credenciales; en macOS, el llavero del sistema. Igual que IureProject.
//!
//! Las llamadas son síncronas y pueden tardar (D-Bus): la UI las hace desde su runtime de fondo.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::Error;

/// Servicio del llavero de IureDesk.
pub const SERVICE: &str = "iuredesk";

/// Para qué es un secreto.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SecretPurpose {
    /// Contraseña (o contraseña de app) del servidor entrante.
    Incoming,
    /// Contraseña del SMTP, si difiere de la entrante.
    Outgoing,
    /// Token de refresco OAuth 2.
    OAuthRefresh,
    /// Token de acceso OAuth 2 (caché; caduca en una hora).
    OAuthAccess,
    /// Contraseña de CalDAV/CardDAV, si difiere.
    Dav,
}

impl SecretPurpose {
    pub fn as_str(self) -> &'static str {
        match self {
            SecretPurpose::Incoming => "incoming",
            SecretPurpose::Outgoing => "outgoing",
            SecretPurpose::OAuthRefresh => "oauth-refresh",
            SecretPurpose::OAuthAccess => "oauth-access",
            SecretPurpose::Dav => "dav",
        }
    }
}

/// Usuario de la entrada del llavero: `<id-de-cuenta>:<propósito>`.
pub fn keyring_user(account_id: &str, purpose: SecretPurpose) -> String {
    format!("{account_id}:{}", purpose.as_str())
}

/// Dónde se guardan los secretos. [`KeyringStore`] en producción; [`MemoryStore`] en pruebas.
pub trait SecretStore: Send + Sync {
    /// `Ok(None)` si no hay nada guardado (distinto de que el llavero falle).
    fn get(&self, account_id: &str, purpose: SecretPurpose) -> Result<Option<String>, Error>;
    fn set(&self, account_id: &str, purpose: SecretPurpose, secret: &str) -> Result<(), Error>;
    /// Borrar algo que no existe no es error.
    fn delete(&self, account_id: &str, purpose: SecretPurpose) -> Result<(), Error>;

    /// Borra todos los secretos de una cuenta (al quitarla).
    fn delete_all(&self, account_id: &str) -> Result<(), Error> {
        for p in [
            SecretPurpose::Incoming,
            SecretPurpose::Outgoing,
            SecretPurpose::OAuthRefresh,
            SecretPurpose::OAuthAccess,
            SecretPurpose::Dav,
        ] {
            self.delete(account_id, p)?;
        }
        Ok(())
    }
}

/// Llavero del sistema.
#[derive(Debug, Clone, Copy, Default)]
pub struct KeyringStore;

fn keyring_err(e: keyring::Error) -> Error {
    Error::Keyring(e.to_string())
}

impl KeyringStore {
    /// Entrada del llavero para una cuenta y propósito.
    pub fn entry(&self, account_id: &str, purpose: SecretPurpose) -> Result<keyring::Entry, Error> {
        if account_id.is_empty() || account_id.contains(':') {
            return Err(Error::Keyring("identificador de cuenta no válido".into()));
        }
        keyring::Entry::new(SERVICE, &keyring_user(account_id, purpose)).map_err(keyring_err)
    }
}

impl SecretStore for KeyringStore {
    fn get(&self, account_id: &str, purpose: SecretPurpose) -> Result<Option<String>, Error> {
        match self.entry(account_id, purpose)?.get_password() {
            Ok(p) => Ok(Some(p)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(keyring_err(e)),
        }
    }

    fn set(&self, account_id: &str, purpose: SecretPurpose, secret: &str) -> Result<(), Error> {
        self.entry(account_id, purpose)?.set_password(secret).map_err(keyring_err)
    }

    fn delete(&self, account_id: &str, purpose: SecretPurpose) -> Result<(), Error> {
        match self.entry(account_id, purpose)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(keyring_err(e)),
        }
    }
}

/// Secretos en memoria: para pruebas y para sesiones sin llavero (no persiste nada).
#[derive(Debug, Default)]
pub struct MemoryStore {
    map: Mutex<HashMap<String, String>>,
}

impl MemoryStore {
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, HashMap<String, String>>, Error> {
        self.map.lock().map_err(|_| Error::Keyring("almacén en memoria envenenado".into()))
    }
}

impl SecretStore for MemoryStore {
    fn get(&self, account_id: &str, purpose: SecretPurpose) -> Result<Option<String>, Error> {
        Ok(self.lock()?.get(&keyring_user(account_id, purpose)).cloned())
    }

    fn set(&self, account_id: &str, purpose: SecretPurpose, secret: &str) -> Result<(), Error> {
        self.lock()?.insert(keyring_user(account_id, purpose), secret.to_string());
        Ok(())
    }

    fn delete(&self, account_id: &str, purpose: SecretPurpose) -> Result<(), Error> {
        self.lock()?.remove(&keyring_user(account_id, purpose));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_names() {
        assert_eq!(keyring_user("acc-1", SecretPurpose::Incoming), "acc-1:incoming");
        assert_eq!(keyring_user("acc-1", SecretPurpose::OAuthRefresh), "acc-1:oauth-refresh");
        assert!(KeyringStore.entry("", SecretPurpose::Incoming).is_err());
        assert!(KeyringStore.entry("a:b", SecretPurpose::Incoming).is_err());
    }

    #[test]
    fn memory_round_trip() {
        let s = MemoryStore::default();
        assert_eq!(s.get("a", SecretPurpose::Incoming).unwrap(), None);
        s.set("a", SecretPurpose::Incoming, "secreto").unwrap();
        s.set("a", SecretPurpose::OAuthRefresh, "tok").unwrap();
        assert_eq!(s.get("a", SecretPurpose::Incoming).unwrap().as_deref(), Some("secreto"));
        s.delete("a", SecretPurpose::Incoming).unwrap();
        s.delete("a", SecretPurpose::Incoming).unwrap();
        assert_eq!(s.get("a", SecretPurpose::Incoming).unwrap(), None);
        s.delete_all("a").unwrap();
        assert_eq!(s.get("a", SecretPurpose::OAuthRefresh).unwrap(), None);
    }

    /// Con el backend simulado de `keyring` (nunca el llavero real): la entrada lleva el
    /// servicio y usuario correctos y guarda, lee y borra.
    #[test]
    fn keyring_mock_round_trip() {
        keyring::set_default_credential_builder(keyring::mock::default_credential_builder());
        let entry = KeyringStore.entry("acc-1", SecretPurpose::Incoming).unwrap();
        assert!(matches!(entry.get_password(), Err(keyring::Error::NoEntry)));
        entry.set_password("s3creto").unwrap();
        assert_eq!(entry.get_password().unwrap(), "s3creto");
        entry.delete_credential().unwrap();
        assert!(matches!(entry.get_password(), Err(keyring::Error::NoEntry)));
    }
}
