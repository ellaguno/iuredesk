//! Lista de cuentas (sin secretos) en `accounts.json`, dentro de la carpeta de configuración del
//! sistema (`~/.config/iuredesk`, `~/Library/Application Support/iuredesk`, `%APPDATA%\iuredesk`).
//!
//! Escritura atómica: se escribe un archivo temporal en la misma carpeta, se sincroniza y se
//! renombra encima del bueno. Un corte de luz deja el archivo viejo o el nuevo, nunca uno a medias.

use std::io::Write;
use std::path::{Path, PathBuf};

use iuredesk_model::account::Account;
use serde::{Deserialize, Serialize};

use crate::Error;

/// Tope del archivo (miles de cuentas caben de sobra).
const MAX_FILE: u64 = 4 * 1024 * 1024;
/// Versión del formato.
const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
struct FileFormat {
    version: u32,
    #[serde(default)]
    accounts: Vec<Account>,
}

/// Carpeta de configuración de IureDesk.
pub fn config_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("iuredesk"))
}

/// Almacén de la lista de cuentas.
#[derive(Debug, Clone)]
pub struct AccountStore {
    path: PathBuf,
}

impl AccountStore {
    /// En la carpeta de configuración del sistema.
    pub fn open_default() -> Result<Self, Error> {
        let dir = config_dir().ok_or(Error::NoConfigDir)?;
        Ok(Self::at(dir.join("accounts.json")))
    }

    /// En una ruta concreta (pruebas, perfiles portátiles).
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Lee las cuentas. Si el archivo no existe, la lista está vacía.
    pub fn load(&self) -> Result<Vec<Account>, Error> {
        let meta = match std::fs::metadata(&self.path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(Error::Io(e.to_string())),
        };
        if meta.len() > MAX_FILE {
            return Err(Error::TooLarge);
        }
        let text = std::fs::read_to_string(&self.path).map_err(|e| Error::Io(e.to_string()))?;
        let file: FileFormat = serde_json::from_str(&text).map_err(|e| Error::Json(e.to_string()))?;
        if file.version > FORMAT_VERSION {
            return Err(Error::Json(format!("versión {} del archivo no soportada", file.version)));
        }
        Ok(file.accounts)
    }

    /// Guarda la lista completa de forma atómica.
    pub fn save(&self, accounts: &[Account]) -> Result<(), Error> {
        let file = FileFormat { version: FORMAT_VERSION, accounts: accounts.to_vec() };
        let json = serde_json::to_vec_pretty(&file).map_err(|e| Error::Json(e.to_string()))?;
        write_atomic(&self.path, &json)
    }

    /// Agrega o reemplaza (por `id`) una cuenta.
    pub fn upsert(&self, account: Account) -> Result<(), Error> {
        let mut list = self.load()?;
        match list.iter_mut().find(|a| a.id == account.id) {
            Some(slot) => *slot = account,
            None => list.push(account),
        }
        self.save(&list)
    }

    /// Quita una cuenta. `false` si no estaba.
    pub fn remove(&self, id: &str) -> Result<bool, Error> {
        let mut list = self.load()?;
        let before = list.len();
        list.retain(|a| a.id != id);
        if list.len() == before {
            return Ok(false);
        }
        self.save(&list)?;
        Ok(true)
    }
}

/// Escribe `data` en `path` de forma atómica (temporal + fsync + rename).
pub fn write_atomic(path: &Path, data: &[u8]) -> Result<(), Error> {
    let io = |e: std::io::Error| Error::Io(e.to_string());
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).map_err(io)?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "data".into());
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    let result = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)?;
        // En Unix, sincronizar la carpeta hace durable el renombrado.
        #[cfg(unix)]
        if let Ok(d) = std::fs::File::open(dir) {
            d.sync_all().ok();
        }
        Ok::<(), std::io::Error>(())
    })();
    if result.is_err() {
        std::fs::remove_file(&tmp).ok();
    }
    result.map_err(io)
}

#[cfg(test)]
mod tests {
    use super::*;
    use iuredesk_model::account::{AccountKind, AuthMethod, Protocol, Security, ServerConfig};

    fn account(id: &str) -> Account {
        let server = |protocol, host: &str, port| ServerConfig {
            protocol,
            host: host.into(),
            port,
            security: Security::Tls,
            auth: AuthMethod::Password,
            username: "ana@example.com".into(),
        };
        Account {
            id: id.into(),
            display_name: "Ana".into(),
            email: "ana@example.com".into(),
            kind: AccountKind::Generic,
            incoming: server(Protocol::Imap, "imap.example.com", 993),
            outgoing: server(Protocol::Smtp, "smtp.example.com", 465),
            caldav_url: None,
            carddav_url: None,
            identities: vec![],
        }
    }

    #[test]
    fn missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let store = AccountStore::at(dir.path().join("sub/accounts.json"));
        assert!(store.load().unwrap().is_empty());
    }

    #[test]
    fn round_trip_upsert_remove() {
        let dir = tempfile::tempdir().unwrap();
        let store = AccountStore::at(dir.path().join("sub/accounts.json"));
        store.upsert(account("a")).unwrap();
        store.upsert(account("b")).unwrap();
        let mut changed = account("a");
        changed.display_name = "Ana P.".into();
        store.upsert(changed).unwrap();
        let list = store.load().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list.first().unwrap().display_name, "Ana P.");
        assert!(store.remove("b").unwrap());
        assert!(!store.remove("b").unwrap());
        assert_eq!(store.load().unwrap().len(), 1);
        // No quedan temporales.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path().join("sub"))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
        // Sin secretos en el archivo: solo lo que el modelo tiene.
        let text = std::fs::read_to_string(store.path()).unwrap();
        assert!(!text.to_lowercase().contains("password\":"));
    }

    #[test]
    fn corrupt_or_future_file_is_an_error_not_a_crash() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("accounts.json");
        std::fs::write(&path, "{nope").unwrap();
        assert!(matches!(AccountStore::at(&path).load(), Err(Error::Json(_))));
        std::fs::write(&path, r#"{"version":99,"accounts":[]}"#).unwrap();
        assert!(matches!(AccountStore::at(&path).load(), Err(Error::Json(_))));
    }
}
