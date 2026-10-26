//! Sesión compartida de la suite Iurefficient.
//!
//! Si el usuario ya inició sesión en otra app (IureDav, IureTranscribe, IureProject, iureditor),
//! IureDesk entra sin pedir nada, igual que IureProject (`crates/connect`):
//!
//! - la cuenta activa está en `<config>/iurefficient/cuenta-activa.json`
//!   ([`iurefficient_connect::account::active`]);
//! - las cookies de la sesión están en el llavero, servicio `iurefficient`, usuario
//!   `host:correo:session`, valor `{"cookies": […]}`. Nunca la contraseña.
//!
//! Al restaurar, el servidor puede rotar la cookie de refresco: se reescribe la entrada compartida
//! para no dejar fuera a las otras apps.

use std::time::Duration;

use iurefficient_connect::Account;
use iurefficient_connect::rest::{Session, SessionExport};

use crate::Error;

/// Servicio compartido del llavero de la suite.
pub const SHARED_SERVICE: &str = "iurefficient";
/// Nombre con el que IureDesk se anuncia (User-Agent).
const APP: &str = "IureDesk";
/// Tiempo máximo para restaurar la sesión (renovar + `me`).
const RESTORE_TIMEOUT: Duration = Duration::from_secs(20);

/// Estado de la sesión compartida.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IurefficientStatus {
    /// Ninguna app de la suite ha iniciado sesión en este equipo (o no dejó sesión en el llavero).
    NotConfigured,
    /// Sesión válida.
    Ready { host: String, email: String, name: Option<String> },
    /// Había sesión pero el servidor ya no la acepta: hay que volver a entrar.
    Expired { host: String, email: String },
    /// No se pudo hablar con la instancia (sin red, servidor caído). La sesión puede seguir siendo
    /// válida; se reintenta más tarde.
    Offline { host: String, email: String, reason: String },
}

/// Estado y, si está lista, la sesión abierta para usarla con `iurefficient_connect::api`.
pub struct SharedSession {
    pub status: IurefficientStatus,
    pub session: Option<Session>,
}

impl std::fmt::Debug for SharedSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedSession").field("status", &self.status).finish_non_exhaustive()
    }
}

/// Lectura y escritura de la entrada compartida del llavero (rasgo para poder probarla).
pub trait SharedVault: Send + Sync {
    fn read(&self, acc: &Account) -> Result<Option<String>, Error>;
    fn write(&self, acc: &Account, secret: &str) -> Result<(), Error>;
}

/// Usuario de la entrada compartida, igual que `iurefficient_connect::secrets`.
pub fn shared_entry_user(acc: &Account) -> String {
    format!("{}:{}:session", acc.host(), acc.email)
}

/// La entrada compartida en el llavero del sistema.
#[derive(Debug, Clone, Copy, Default)]
pub struct KeyringVault;

impl KeyringVault {
    fn entry(acc: &Account) -> Result<keyring::Entry, Error> {
        keyring::Entry::new(SHARED_SERVICE, &shared_entry_user(acc)).map_err(|e| Error::Keyring(e.to_string()))
    }
}

impl SharedVault for KeyringVault {
    fn read(&self, acc: &Account) -> Result<Option<String>, Error> {
        match Self::entry(acc)?.get_password() {
            Ok(p) => Ok(Some(p)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(Error::Keyring(e.to_string())),
        }
    }

    fn write(&self, acc: &Account, secret: &str) -> Result<(), Error> {
        Self::entry(acc)?.set_password(secret).map_err(|e| Error::Keyring(e.to_string()))
    }
}

/// Retoma la sesión activa de la suite con el llavero del sistema.
pub async fn iurefficient_session() -> SharedSession {
    let acc = iurefficient_connect::account::active().and_then(|a| a.account().ok());
    iurefficient_session_with(acc, &KeyringVault).await
}

/// Igual que [`iurefficient_session`] con la cuenta y el llavero dados.
pub async fn iurefficient_session_with(acc: Option<Account>, vault: &dyn SharedVault) -> SharedSession {
    let not_configured = || SharedSession { status: IurefficientStatus::NotConfigured, session: None };
    let Some(acc) = acc else { return not_configured() };
    let (host, email) = (acc.host(), acc.email.clone());
    let saved = match vault.read(&acc) {
        Ok(Some(json)) => match serde_json::from_str::<SessionExport>(&json) {
            Ok(s) if !s.cookies.is_empty() => s,
            _ => return not_configured(),
        },
        Ok(None) => return not_configured(),
        // Un llavero bloqueado o ausente no es «sesión caducada»: no hay con qué entrar.
        Err(e) => {
            return SharedSession {
                status: IurefficientStatus::Offline { host, email, reason: e.to_string() },
                session: None,
            };
        }
    };
    let user_agent = iurefficient_connect::user_agent(APP, env!("CARGO_PKG_VERSION"));
    let session = match Session::new(acc.clone(), &user_agent) {
        Ok(s) => s,
        Err(e) => {
            return SharedSession {
                status: IurefficientStatus::Offline { host, email, reason: e.to_string() },
                session: None,
            };
        }
    };
    match tokio::time::timeout(RESTORE_TIMEOUT, session.import(&saved)).await {
        Ok(Ok(user)) => {
            // El refresco pudo rotar: se deja la sesión nueva para las demás apps. Si el llavero
            // falla aquí, la sesión sigue sirviendo en esta ejecución.
            if let Ok(json) = serde_json::to_string(&session.export()) {
                vault.write(&acc, &json).ok();
            }
            let name = user.name.filter(|n| !n.trim().is_empty());
            let email = if user.email.trim().is_empty() { email } else { user.email };
            SharedSession { status: IurefficientStatus::Ready { host, email, name }, session: Some(session) }
        }
        Ok(Err(e)) if is_network(&e) => SharedSession {
            status: IurefficientStatus::Offline { host, email, reason: format!("{e:#}") },
            session: None,
        },
        Ok(Err(_)) => SharedSession { status: IurefficientStatus::Expired { host, email }, session: None },
        Err(_) => SharedSession {
            status: IurefficientStatus::Offline { host, email, reason: "tiempo de espera agotado".into() },
            session: None,
        },
    }
}

/// ¿El fallo fue de red (conexión, DNS, TLS, tiempo) y no una respuesta del servidor?
fn is_network(e: &anyhow::Error) -> bool {
    e.chain()
        .any(|c| c.downcast_ref::<reqwest::Error>().is_some_and(|r| r.is_connect() || r.is_timeout() || r.is_request()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[derive(Default)]
    struct FakeVault {
        value: Mutex<Option<String>>,
        writes: Mutex<u32>,
    }

    impl SharedVault for FakeVault {
        fn read(&self, _: &Account) -> Result<Option<String>, Error> {
            Ok(self.value.lock().unwrap().clone())
        }
        fn write(&self, _: &Account, secret: &str) -> Result<(), Error> {
            *self.value.lock().unwrap() = Some(secret.to_string());
            *self.writes.lock().unwrap() += 1;
            Ok(())
        }
    }

    fn vault_with_session() -> FakeVault {
        let v = FakeVault::default();
        *v.value.lock().unwrap() =
            Some(r#"{"cookies":["refresh_token_cookie=r1","csrf_refresh_token=c1"]}"#.to_string());
        v
    }

    /// Servidor HTTP mínimo: responde `refresh_status` a `/api/auth/refresh` y un usuario a `me`.
    async fn fake_instance(refresh_status: u16) -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            for _ in 0..8 {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                let mut buf = vec![0u8; 8192];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let (status, body) = if req.starts_with("POST /api/auth/refresh") {
                    (refresh_status, String::new())
                } else if req.starts_with("GET /api/auth/me") {
                    (200, r#"{"user":{"id":7,"email":"ana@despacho.mx","name":"Ana Pérez"}}"#.to_string())
                } else {
                    (404, String::new())
                };
                let resp = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                sock.write_all(resp.as_bytes()).await.ok();
                sock.shutdown().await.ok();
            }
        });
        port
    }

    #[test]
    fn entry_is_shared_with_the_suite() {
        let acc = Account::new("https://despacho.iurefficient.com/webdav/", " ana@despacho.mx ").unwrap();
        assert_eq!(shared_entry_user(&acc), "despacho.iurefficient.com:ana@despacho.mx:session");
    }

    #[tokio::test]
    async fn no_active_account_or_no_session_is_not_configured() {
        let s = iurefficient_session_with(None, &FakeVault::default()).await;
        assert_eq!(s.status, IurefficientStatus::NotConfigured);
        let acc = Account::new("despacho.iurefficient.com", "ana@despacho.mx").unwrap();
        let s = iurefficient_session_with(Some(acc.clone()), &FakeVault::default()).await;
        assert_eq!(s.status, IurefficientStatus::NotConfigured);
        let v = FakeVault::default();
        *v.value.lock().unwrap() = Some("basura".into());
        let s = iurefficient_session_with(Some(acc), &v).await;
        assert_eq!(s.status, IurefficientStatus::NotConfigured);
    }

    #[tokio::test]
    async fn rejected_refresh_is_expired() {
        let port = fake_instance(401).await;
        let acc = Account::new(&format!("http://127.0.0.1:{port}"), "ana@despacho.mx").unwrap();
        let s = iurefficient_session_with(Some(acc), &vault_with_session()).await;
        assert!(matches!(s.status, IurefficientStatus::Expired { .. }), "{:?}", s.status);
        assert!(s.session.is_none());
    }

    #[tokio::test]
    async fn valid_session_is_ready_and_rewritten() {
        let port = fake_instance(200).await;
        let acc = Account::new(&format!("http://127.0.0.1:{port}"), "ana@despacho.mx").unwrap();
        let vault = vault_with_session();
        let s = iurefficient_session_with(Some(acc), &vault).await;
        assert_eq!(
            s.status,
            IurefficientStatus::Ready {
                host: format!("127.0.0.1:{port}"),
                email: "ana@despacho.mx".into(),
                name: Some("Ana Pérez".into())
            }
        );
        assert!(s.session.is_some());
        assert_eq!(*vault.writes.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn unreachable_instance_is_offline() {
        // Puerto cerrado: conexión rechazada.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let acc = Account::new(&format!("http://127.0.0.1:{port}"), "ana@despacho.mx").unwrap();
        let s = iurefficient_session_with(Some(acc), &vault_with_session()).await;
        assert!(matches!(s.status, IurefficientStatus::Offline { .. }), "{:?}", s.status);
    }
}
