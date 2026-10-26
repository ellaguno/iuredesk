//! Conexiones IMAP por cuenta: autenticación (contraseña u OAuth) y un *pool* con tope de
//! conexiones simultáneas (Gmail admite como mucho 15 por cuenta).

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use iuredesk_accounts::{SecretPurpose, SecretStore};
use iuredesk_imap::{ImapClient, ImapConfig};
use iuredesk_model::account::{Account, AccountKind, AuthMethod};
use iuredesk_oauth::{TokenAudience, TokenProvider, sasl};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::SyncOptions;
use crate::error::{Result, SyncError};

/// Una conexión inactiva más vieja que esto se cierra en vez de reutilizarse (los servidores
/// cortan a los 30 min; algunos cortafuegos antes).
const MAX_IDLE_AGE: Duration = Duration::from_secs(4 * 60);

/// Credenciales: llavero y tokens.
pub(crate) struct Auth {
    pub tokens: Option<Arc<dyn TokenProvider>>,
    pub secrets: Arc<dyn SecretStore>,
}

impl Auth {
    /// Contraseña del servidor entrante (el llavero puede bloquear: fuera del runtime).
    pub async fn password(&self, account: &Account) -> Result<String> {
        let secrets = Arc::clone(&self.secrets);
        let id = account.id.clone();
        let r = tokio::task::spawn_blocking(move || secrets.get(&id, SecretPurpose::Incoming))
            .await
            .map_err(|e| SyncError::Secrets(e.to_string()))?;
        r.map_err(|e| SyncError::Secrets(e.to_string()))?.ok_or(SyncError::NoPassword)
    }

    /// Token de acceso de correo.
    pub async fn token(&self, account: &Account) -> Result<String> {
        let tp = self.tokens.as_ref().ok_or(SyncError::NoTokenProvider)?;
        Ok(tp.access_token(account, TokenAudience::Mail).await?)
    }

    /// Olvida el token en caché (tras un rechazo).
    pub fn invalidate(&self, account: &Account) {
        if let Some(tp) = &self.tokens {
            tp.invalidate(&account.id);
        }
    }
}

/// Tope de conexiones simultáneas para la cuenta.
pub(crate) fn connection_limit(account: &Account, opts: &SyncOptions) -> usize {
    let host = account.incoming.host.to_ascii_lowercase();
    let cap = if account.kind == AccountKind::Gmail || host.ends_with("gmail.com") || host.ends_with("googlemail.com") {
        15
    } else if account.kind == AccountKind::ICloud || host.ends_with("mail.me.com") {
        10
    } else {
        16
    };
    opts.max_connections.clamp(1, cap)
}

/// Pool de conexiones IMAP de una cuenta.
pub(crate) struct ImapPool {
    account: Account,
    cfg: ImapConfig,
    auth: Arc<Auth>,
    sem: Arc<Semaphore>,
    idle: Mutex<Vec<(ImapClient, Instant)>>,
    compress: bool,
    pub limit: usize,
}

/// Conexión prestada por el pool. Si se suelta sin [`ImapPool::release`], se cierra.
pub(crate) struct Conn {
    pub c: ImapClient,
    _permit: OwnedSemaphorePermit,
}

impl ImapPool {
    pub fn new(account: Account, auth: Arc<Auth>, opts: &SyncOptions) -> Self {
        let mut cfg = ImapConfig::from_server(&account.incoming);
        cfg.io_timeout = opts.io_timeout;
        let limit = connection_limit(&account, opts);
        Self {
            account,
            cfg,
            auth,
            sem: Arc::new(Semaphore::new(limit)),
            idle: Mutex::new(Vec::new()),
            compress: opts.compress,
            limit,
        }
    }

    /// Presta una conexión autenticada (reutiliza una libre si es reciente).
    pub async fn get(&self) -> Result<Conn> {
        let permit = Arc::clone(&self.sem).acquire_owned().await.map_err(|_| SyncError::Shutdown)?;
        let reused = {
            let mut idle = self.idle.lock().unwrap_or_else(PoisonError::into_inner);
            let mut found = None;
            while let Some((c, at)) = idle.pop() {
                if at.elapsed() < MAX_IDLE_AGE {
                    found = Some(c);
                    break;
                }
            }
            found
        };
        let c = match reused {
            Some(c) => c,
            None => self.connect().await?,
        };
        Ok(Conn { c, _permit: permit })
    }

    /// Devuelve una conexión sana al pool.
    pub fn release(&self, conn: Conn) {
        let mut idle = self.idle.lock().unwrap_or_else(PoisonError::into_inner);
        if idle.len() < self.limit {
            idle.push((conn.c, Instant::now()));
        }
    }

    /// Vacía el pool (al apagar).
    pub fn drain(&self) -> Vec<ImapClient> {
        let mut idle = self.idle.lock().unwrap_or_else(PoisonError::into_inner);
        idle.drain(..).map(|(c, _)| c).collect()
    }

    async fn connect(&self) -> Result<ImapClient> {
        let mut c = ImapClient::connect(self.cfg.clone()).await?;
        if !c.is_authenticated() {
            self.authenticate(&mut c).await?;
        }
        let version = env!("CARGO_PKG_VERSION");
        if let Err(e) = c.id(&[("name", "IureDesk"), ("version", version), ("vendor", "Iurefficient")]).await
            && e.is_fatal_for_connection()
        {
            return Err(e.into());
        }
        let caps = c.capabilities().clone();
        let ext: &[&str] = if caps.has("QRESYNC") {
            &["QRESYNC"]
        } else if caps.has("CONDSTORE") {
            &["CONDSTORE"]
        } else {
            &[]
        };
        if !ext.is_empty()
            && let Err(e) = c.enable(ext).await
            && e.is_fatal_for_connection()
        {
            return Err(e.into());
        }
        if self.compress
            && let Err(e) = c.compress().await
            && e.is_fatal_for_connection()
        {
            return Err(e.into());
        }
        Ok(c)
    }

    async fn authenticate(&self, c: &mut ImapClient) -> Result<()> {
        let a = &self.account;
        let user = a.incoming.username.as_str();
        match a.incoming.auth {
            AuthMethod::OAuth2 { .. } => {
                let mut retried = false;
                loop {
                    let token = self.auth.token(a).await?;
                    let caps = c.capabilities();
                    let r = if caps.auth("XOAUTH2") || !caps.auth("OAUTHBEARER") {
                        c.authenticate("XOAUTH2", &sasl::xoauth2(user, &token)?).await
                    } else {
                        let ir = sasl::oauthbearer(user, &a.incoming.host, a.incoming.port, &token)?;
                        c.authenticate("OAUTHBEARER", &ir).await
                    };
                    match r {
                        Ok(()) => return Ok(()),
                        Err(e) if e.is_auth() && !retried => {
                            // Token caducado o revocado: se pide otro una vez.
                            self.auth.invalidate(a);
                            retried = true;
                        }
                        Err(e) => return Err(e.into()),
                    }
                }
            }
            AuthMethod::Password | AuthMethod::AppPassword => {
                let pass = self.auth.password(a).await?;
                let caps = c.capabilities();
                if caps.auth("PLAIN") && !pass.is_empty() {
                    c.authenticate_plain(user, &pass).await?;
                } else {
                    c.login(user, &pass).await?;
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iuredesk_model::account::{Protocol, Security, ServerConfig};

    #[test]
    fn provider_caps() {
        let server = |host: &str| ServerConfig {
            protocol: Protocol::Imap,
            host: host.into(),
            port: 993,
            security: Security::Tls,
            auth: AuthMethod::Password,
            username: "u".into(),
        };
        let mut a = Account {
            id: "a".into(),
            display_name: "A".into(),
            email: "a@gmail.com".into(),
            kind: AccountKind::Gmail,
            incoming: server("imap.gmail.com"),
            outgoing: server("smtp.gmail.com"),
            caldav_url: None,
            carddav_url: None,
            identities: vec![],
        };
        let opts = SyncOptions { max_connections: 50, ..Default::default() };
        assert_eq!(connection_limit(&a, &opts), 15);
        a.kind = AccountKind::Generic;
        a.incoming.host = "mail.example.com".into();
        assert_eq!(connection_limit(&a, &opts), 16);
        assert_eq!(connection_limit(&a, &SyncOptions { max_connections: 0, ..Default::default() }), 1);
        assert_eq!(connection_limit(&a, &SyncOptions::default()), 4);
    }
}
