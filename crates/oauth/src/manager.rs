//! Tokens de las cuentas OAuth: el de refresco en el llavero (propósito `oauth-refresh`), el de
//! acceso solo en memoria, refrescado con margen antes de caducar.
//!
//! IMAP, SMTP y Graph piden tokens con [`TokenProvider::access_token`]; si el servidor rechaza uno
//! (SASL `401`), llaman a [`TokenProvider::invalidate`] y lo piden de nuevo una vez.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iuredesk_accounts::{SecretPurpose, SecretStore};
use iuredesk_model::account::{Account, AuthMethod, OAuthProvider};

use crate::Error;
use crate::provider::{OAuthSettings, ProviderConfig, TokenAudience};
use crate::token::{TokenSet, refresh};

/// Futuro en caja, para que [`TokenProvider`] se pueda usar como `dyn`.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Margen por omisión: se refresca si el token caduca en menos de esto.
pub const DEFAULT_SKEW: Duration = Duration::from_secs(120);

/// Quien entrega tokens de acceso a los protocolos.
pub trait TokenProvider: Send + Sync {
    /// Token de acceso vigente para `account` y `audience` (refresca si hace falta).
    /// [`Error::needs_reconsent`] indica que hay que volver a iniciar sesión.
    fn access_token<'a>(
        &'a self,
        account: &'a Account,
        audience: TokenAudience,
    ) -> BoxFuture<'a, Result<String, Error>>;

    /// Olvida los tokens de acceso en caché de la cuenta (p. ej. tras un `401` del servidor).
    fn invalidate(&self, account_id: &str);
}

type Slot = Arc<tokio::sync::Mutex<Option<TokenSet>>>;

/// [`TokenProvider`] de producción.
pub struct TokenManager {
    http: reqwest::Client,
    secrets: Arc<dyn SecretStore>,
    providers: HashMap<OAuthProvider, ProviderConfig>,
    /// Un candado por cuenta y audiencia: dos peticiones a la vez refrescan una sola vez.
    slots: Mutex<HashMap<(String, TokenAudience), Slot>>,
    skew: Duration,
}

impl std::fmt::Debug for TokenManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenManager").field("providers", &self.providers.keys()).finish_non_exhaustive()
    }
}

/// Proveedor OAuth de la cuenta (servidor entrante o, si no, saliente).
pub fn account_provider(account: &Account) -> Result<OAuthProvider, Error> {
    [account.incoming.auth, account.outgoing.auth]
        .into_iter()
        .find_map(|a| match a {
            AuthMethod::OAuth2 { provider } => Some(provider),
            _ => None,
        })
        .ok_or(Error::NotOAuthAccount)
}

/// Llama al llavero (síncrono, puede tardar por D-Bus) fuera del hilo del runtime.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, iuredesk_accounts::Error> + Send + 'static,
) -> Result<T, Error> {
    tokio::task::spawn_blocking(f).await.map_err(|e| Error::Secrets(e.to_string()))?.map_err(Error::from)
}

impl TokenManager {
    pub fn new(http: reqwest::Client, secrets: Arc<dyn SecretStore>) -> Self {
        Self { http, secrets, providers: HashMap::new(), slots: Mutex::new(HashMap::new()), skew: DEFAULT_SKEW }
    }

    /// Con Google y Microsoft según los ajustes; un proveedor sin client id se omite.
    pub fn from_settings(http: reqwest::Client, secrets: Arc<dyn SecretStore>, settings: &OAuthSettings) -> Self {
        let mut m = Self::new(http, secrets);
        if let Ok(p) = settings.google() {
            m.providers.insert(OAuthProvider::Google, p);
        }
        if let Ok(p) = settings.microsoft() {
            m.providers.insert(OAuthProvider::Microsoft, p);
        }
        m
    }

    pub fn with_provider(mut self, which: OAuthProvider, config: ProviderConfig) -> Self {
        self.providers.insert(which, config);
        self
    }

    /// Cambia el margen de refresco.
    pub fn with_skew(mut self, skew: Duration) -> Self {
        self.skew = skew;
        self
    }

    /// Configuración del proveedor.
    pub fn provider(&self, which: OAuthProvider) -> Result<&ProviderConfig, Error> {
        self.providers.get(&which).ok_or_else(|| Error::UnknownProvider(format!("{which:?}")))
    }

    fn slot(&self, account_id: &str, audience: TokenAudience) -> Result<Slot, Error> {
        let mut map = self.slots.lock().map_err(|_| Error::Secrets("caché de tokens envenenada".into()))?;
        Ok(map.entry((account_id.to_string(), audience)).or_default().clone())
    }

    /// Guarda el resultado de un inicio de sesión: el token de refresco en el llavero y el de
    /// acceso en memoria (sirve para [`TokenAudience::Mail`], que es para el que se canjeó).
    pub async fn store_login(&self, account_id: &str, tokens: &TokenSet) -> Result<(), Error> {
        let refresh_token = tokens.refresh_token.clone().ok_or(Error::NoRefreshToken)?;
        let (secrets, id) = (self.secrets.clone(), account_id.to_string());
        blocking(move || secrets.set(&id, SecretPurpose::OAuthRefresh, &refresh_token)).await?;
        self.invalidate(account_id);
        *self.slot(account_id, TokenAudience::Mail)?.lock().await = Some(tokens.clone());
        Ok(())
    }

    /// Cierra la sesión OAuth de la cuenta: borra el token de refresco y la caché.
    pub async fn forget(&self, account_id: &str) -> Result<(), Error> {
        self.invalidate(account_id);
        let (secrets, id) = (self.secrets.clone(), account_id.to_string());
        blocking(move || {
            secrets.delete(&id, SecretPurpose::OAuthRefresh)?;
            secrets.delete(&id, SecretPurpose::OAuthAccess)
        })
        .await
    }

    async fn fetch(&self, account: &Account, audience: TokenAudience) -> Result<String, Error> {
        let config = self.provider(account_provider(account)?)?;
        let slot = self.slot(&account.id, audience)?;
        let mut cached = slot.lock().await;
        if let Some(t) = cached.as_ref()
            && !t.expires_within(self.skew)
        {
            return Ok(t.access_token.clone());
        }
        let (secrets, id) = (self.secrets.clone(), account.id.clone());
        let stored =
            blocking(move || secrets.get(&id, SecretPurpose::OAuthRefresh)).await?.ok_or(Error::NoRefreshToken)?;
        match refresh(&self.http, config, &stored, audience).await {
            Ok(fresh) => {
                // Microsoft rota el token de refresco: se guarda el nuevo.
                if let Some(new_rt) = fresh.refresh_token.clone().filter(|r| *r != stored) {
                    let (secrets, id) = (self.secrets.clone(), account.id.clone());
                    blocking(move || secrets.set(&id, SecretPurpose::OAuthRefresh, &new_rt)).await?;
                }
                let access = fresh.access_token.clone();
                *cached = Some(fresh);
                Ok(access)
            }
            Err(e @ Error::NeedsReconsent { .. }) => {
                // Revocado o caducado: no tiene sentido reintentarlo con el mismo token.
                *cached = None;
                let (secrets, id) = (self.secrets.clone(), account.id.clone());
                blocking(move || secrets.delete(&id, SecretPurpose::OAuthRefresh)).await?;
                Err(e)
            }
            Err(e) => Err(e),
        }
    }
}

impl TokenProvider for TokenManager {
    fn access_token<'a>(
        &'a self,
        account: &'a Account,
        audience: TokenAudience,
    ) -> BoxFuture<'a, Result<String, Error>> {
        Box::pin(self.fetch(account, audience))
    }

    fn invalidate(&self, account_id: &str) {
        if let Ok(mut map) = self.slots.lock() {
            map.retain(|(id, _), _| id != account_id);
        }
    }
}
