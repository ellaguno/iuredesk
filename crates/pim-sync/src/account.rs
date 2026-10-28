//! Qué sincronizar de cada cuenta y con qué credenciales.

use std::sync::Arc;

use iuredesk_accounts::{SecretPurpose, SecretStore};
use iuredesk_model::account::{Account, AccountKind, AuthMethod};
use iuredesk_oauth::{TokenAudience, TokenProvider};

use crate::error::{PimError, Result};

/// Raíz de Microsoft Graph.
pub const GRAPH_BASE: &str = "https://graph.microsoft.com/v1.0";

/// Autenticación de CalDAV/CardDAV.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DavAuth {
    /// HTTP Basic con este usuario; la contraseña sale del llavero (propósito `dav` y, si no hay,
    /// `incoming`: iCloud usa la misma contraseña de app para todo).
    Password {
        /// Usuario.
        username: String,
    },
    /// OAuth Bearer con un token de [`TokenAudience::Api`] (Google).
    OAuth,
}

/// Origen de calendarios, contactos y tareas de una cuenta.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PimSource {
    /// CalDAV y/o CardDAV.
    Dav {
        /// URL de arranque CalDAV (dominio, `.well-known`, principal o raíz DAV).
        caldav: Option<String>,
        /// URL de arranque CardDAV.
        carddav: Option<String>,
        /// Autenticación.
        auth: DavAuth,
    },
    /// Microsoft Graph.
    Graph {
        /// Raíz de la API (en pruebas, un servidor local).
        base_url: String,
    },
}

/// Una cuenta para la sincronización PIM.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PimAccount {
    /// La cuenta (sus `id`, correo e identidades).
    pub account: Account,
    /// De dónde salen sus calendarios y contactos.
    pub source: PimSource,
}

impl PimAccount {
    /// Configuración por omisión según el tipo de cuenta (PLAN.md §5). `None` si la cuenta no
    /// tiene calendario/contactos que sincronizar aquí (Iurefficient va por `connect`; una cuenta
    /// genérica sin URL CalDAV/CardDAV).
    pub fn from_account(a: &Account) -> Option<Self> {
        let oauth = matches!(a.incoming.auth, AuthMethod::OAuth2 { .. })
            || matches!(a.outgoing.auth, AuthMethod::OAuth2 { .. });
        let username =
            if a.incoming.username.trim().is_empty() { a.email.clone() } else { a.incoming.username.clone() };
        let source = match a.kind {
            AccountKind::Iurefficient => return None,
            AccountKind::Microsoft365 if oauth => PimSource::Graph { base_url: GRAPH_BASE.into() },
            AccountKind::ICloud => PimSource::Dav {
                caldav: Some(a.caldav_url.clone().unwrap_or_else(|| "https://caldav.icloud.com/".into())),
                carddav: Some(a.carddav_url.clone().unwrap_or_else(|| "https://contacts.icloud.com/".into())),
                auth: DavAuth::Password { username },
            },
            AccountKind::Gmail if oauth => PimSource::Dav {
                caldav: Some(
                    a.caldav_url
                        .clone()
                        .unwrap_or_else(|| iuredesk_dav::dav::quirks::google_caldav_principal(&a.email)),
                ),
                // La raíz genérica que guarda el autodescubrimiento
                // (`…/carddav/v1/principals/`) responde 404: Google solo sirve la libreta
                // por usuario. Una URL propia (servidor de pruebas, alias) se respeta.
                carddav: Some(
                    a.carddav_url
                        .clone()
                        .filter(|u| !is_generic_google_carddav(u))
                        .unwrap_or_else(|| iuredesk_dav::dav::quirks::google_carddav_principal(&a.email)),
                ),
                auth: DavAuth::OAuth,
            },
            _ => {
                if a.caldav_url.is_none() && a.carddav_url.is_none() {
                    return None;
                }
                PimSource::Dav {
                    caldav: a.caldav_url.clone(),
                    carddav: a.carddav_url.clone(),
                    auth: if oauth { DavAuth::OAuth } else { DavAuth::Password { username } },
                }
            }
        };
        Some(Self { account: a.clone(), source })
    }

    /// Nuestras direcciones (la de la cuenta y las de sus identidades), en minúsculas.
    pub fn emails(&self) -> Vec<String> {
        let mut v = vec![self.account.email.trim().to_lowercase()];
        for i in &self.account.identities {
            let e = i.email.trim().to_lowercase();
            if !e.is_empty() && !v.contains(&e) {
                v.push(e);
            }
        }
        v
    }
}

/// ¿Es la raíz CardDAV de Google sin el usuario (`https://www.googleapis.com/carddav/v1/principals/`)?
fn is_generic_google_carddav(url: &str) -> bool {
    let Ok(u) = reqwest::Url::parse(url.trim()) else { return true };
    let google = u.host_str().is_some_and(|h| h.eq_ignore_ascii_case("www.googleapis.com"));
    let path = u.path().trim_end_matches('/');
    google && (path.is_empty() || path == "/carddav" || path == "/carddav/v1" || path == "/carddav/v1/principals")
}

/// Llavero y tokens.
pub(crate) struct Creds {
    pub secrets: Arc<dyn SecretStore>,
    pub tokens: Option<Arc<dyn TokenProvider>>,
}

impl Creds {
    /// Contraseña DAV (propósito `dav`, si no `incoming`). El llavero puede bloquear: fuera del
    /// runtime.
    pub async fn password(&self, account: &Account) -> Result<String> {
        let secrets = Arc::clone(&self.secrets);
        let id = account.id.clone();
        let r = tokio::task::spawn_blocking(move || -> std::result::Result<Option<String>, String> {
            match secrets.get(&id, SecretPurpose::Dav).map_err(|e| e.to_string())? {
                Some(p) => Ok(Some(p)),
                None => secrets.get(&id, SecretPurpose::Incoming).map_err(|e| e.to_string()),
            }
        })
        .await
        .map_err(|e| PimError::Secrets(e.to_string()))?;
        r.map_err(PimError::Secrets)?.ok_or(PimError::NoPassword)
    }

    /// Proveedor de tokens (error si la cuenta es OAuth y no hay).
    pub fn tokens(&self) -> Result<Arc<dyn TokenProvider>> {
        self.tokens.clone().ok_or(PimError::NoTokenProvider)
    }
}

/// [`iuredesk_dav::AuthProvider`] con tokens OAuth renovables (audiencia API).
pub(crate) struct TokenAuth {
    pub tokens: Arc<dyn TokenProvider>,
    pub account: Account,
}

impl iuredesk_dav::AuthProvider for TokenAuth {
    async fn auth_header(&self) -> iuredesk_dav::Result<String> {
        self.tokens
            .access_token(&self.account, TokenAudience::Api)
            .await
            .map(|t| format!("Bearer {t}"))
            .map_err(|e| iuredesk_dav::Error::Auth(e.to_string()))
    }

    fn invalidate(&self) -> bool {
        self.tokens.invalidate(&self.account.id);
        true
    }
}

#[cfg(test)]
mod tests {
    use iuredesk_model::account::{OAuthProvider, Protocol, Security, ServerConfig};

    use super::*;

    fn acct(kind: AccountKind, auth: AuthMethod) -> Account {
        let srv = |p| ServerConfig {
            protocol: p,
            host: "h".into(),
            port: 993,
            security: Security::Tls,
            auth,
            username: String::new(),
        };
        Account {
            id: "a".into(),
            display_name: "Ana".into(),
            email: "Ana@Example.com".into(),
            kind,
            incoming: srv(Protocol::Imap),
            outgoing: srv(Protocol::Smtp),
            caldav_url: None,
            carddav_url: None,
            identities: vec![],
        }
    }

    #[test]
    fn defaults_by_kind() {
        let p = PimAccount::from_account(&acct(AccountKind::ICloud, AuthMethod::AppPassword)).unwrap();
        assert!(matches!(&p.source, PimSource::Dav { caldav: Some(c), auth: DavAuth::Password { username }, .. }
            if c.contains("caldav.icloud.com") && username == "Ana@Example.com"));
        let g =
            PimAccount::from_account(&acct(AccountKind::Gmail, AuthMethod::OAuth2 { provider: OAuthProvider::Google }))
                .unwrap();
        assert!(matches!(g.source, PimSource::Dav { auth: DavAuth::OAuth, .. }));
        // BUG-GCAL: la raíz genérica que guarda el autodescubrimiento da 404; se usa la libreta
        // del usuario. La raíz CalDAV genérica sí descubre y se respeta.
        let mut gs = acct(AccountKind::Gmail, AuthMethod::OAuth2 { provider: OAuthProvider::Google });
        gs.caldav_url = Some("https://apidata.googleusercontent.com/caldav/v2/".into());
        gs.carddav_url = Some("https://www.googleapis.com/carddav/v1/principals/".into());
        let PimSource::Dav { caldav, carddav, .. } = PimAccount::from_account(&gs).unwrap().source else {
            panic!("dav")
        };
        assert_eq!(caldav.as_deref(), Some("https://apidata.googleusercontent.com/caldav/v2/"));
        assert_eq!(
            carddav.as_deref(),
            Some("https://www.googleapis.com/carddav/v1/principals/Ana@Example.com/lists/default/")
        );
        gs.carddav_url = Some("http://127.0.0.1:9/carddav/".into());
        let PimSource::Dav { carddav, .. } = PimAccount::from_account(&gs).unwrap().source else { panic!("dav") };
        assert_eq!(carddav.as_deref(), Some("http://127.0.0.1:9/carddav/"));
        let m = PimAccount::from_account(&acct(
            AccountKind::Microsoft365,
            AuthMethod::OAuth2 { provider: OAuthProvider::Microsoft },
        ))
        .unwrap();
        assert!(matches!(m.source, PimSource::Graph { .. }));
        assert!(PimAccount::from_account(&acct(AccountKind::Generic, AuthMethod::Password)).is_none());
        let mut fm = acct(AccountKind::Generic, AuthMethod::Password);
        fm.caldav_url = Some("https://caldav.fastmail.com/dav/".into());
        assert!(PimAccount::from_account(&fm).is_some());
        assert!(PimAccount::from_account(&acct(AccountKind::Iurefficient, AuthMethod::Password)).is_none());
        assert_eq!(p.emails(), vec!["ana@example.com".to_string()]);
    }
}
