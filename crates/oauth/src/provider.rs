//! Configuración de los proveedores OAuth 2.
//!
//! Los *client id* **no** son secretos ni se escriben en el código: se leen, por orden, de
//! [`OAuthSettings`] (ajustes del usuario o del administrador), de la variable de entorno en tiempo
//! de ejecución y de la misma variable en tiempo de compilación (`option_env!`):
//!
//! | Proveedor | Client id | «Secreto» (opcional) |
//! |---|---|---|
//! | Google | `IUREDESK_GOOGLE_CLIENT_ID` | `IUREDESK_GOOGLE_CLIENT_SECRET` |
//! | Microsoft | `IUREDESK_MS_CLIENT_ID` | — (cliente público) |
//!
//! Google entrega a los clientes «de escritorio» un *client secret* que **no es secreto** (va
//! dentro de cada copia de la app; RFC 8252 §8.5); su servidor de tokens lo exige igualmente, por
//! eso se admite como opcional. Microsoft, con cliente público, no lo usa.
//!
//! Microsoft identity platform v2 emite cada token para **un solo recurso**: Outlook
//! (`https://outlook.office.com/`, para IMAP/POP/SMTP) o Microsoft Graph. El consentimiento se pide
//! una vez con todos los alcances y después se pide un token por [`TokenAudience`].
//! EWS se retira en Exchange Online: no se pide ningún alcance de EWS.

use serde::{Deserialize, Serialize};

use crate::Error;

/// Para qué se quiere el token de acceso.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TokenAudience {
    /// IMAP / POP3 / SMTP (SASL XOAUTH2 u OAUTHBEARER).
    Mail,
    /// API REST del proveedor: Microsoft Graph, Google Calendar/People/Tasks.
    Api,
}

/// Nombre de host del `redirect_uri` *loopback*. El receptor escucha siempre en `127.0.0.1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedirectHost {
    /// `http://127.0.0.1:<puerto>` (recomendado por RFC 8252 §7.3; Google).
    Ipv4Loopback,
    /// `http://localhost:<puerto>`: Microsoft Entra registra `http://localhost` y acepta cualquier
    /// puerto en tiempo de ejecución.
    Localhost,
}

impl RedirectHost {
    pub fn as_str(self) -> &'static str {
        match self {
            RedirectHost::Ipv4Loopback => "127.0.0.1",
            RedirectHost::Localhost => "localhost",
        }
    }
}

/// Identificador de la app ante el proveedor.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientCredentials {
    pub client_id: String,
    /// Solo Google escritorio (no es secreto, ver arriba).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
}

impl std::fmt::Debug for ClientCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientCredentials")
            .field("client_id", &self.client_id)
            .field("client_secret", &self.client_secret.as_ref().map(|_| "…"))
            .finish()
    }
}

impl ClientCredentials {
    pub fn new(client_id: impl Into<String>, client_secret: Option<String>) -> Self {
        Self { client_id: client_id.into(), client_secret }
    }
}

/// Inquilino de Microsoft Entra.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MicrosoftTenant {
    /// Cuentas de trabajo/escuela y personales (Outlook.com).
    #[default]
    Common,
    /// Solo cuentas de trabajo o escuela.
    Organizations,
    /// Solo cuentas personales.
    Consumers,
    /// Un inquilino concreto (GUID o dominio verificado).
    Id(String),
}

impl MicrosoftTenant {
    fn segment(&self) -> Result<&str, Error> {
        Ok(match self {
            MicrosoftTenant::Common => "common",
            MicrosoftTenant::Organizations => "organizations",
            MicrosoftTenant::Consumers => "consumers",
            MicrosoftTenant::Id(id) => {
                let ok = !id.is_empty()
                    && id.len() <= 128
                    && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.');
                if !ok {
                    return Err(Error::Config(format!("inquilino de Microsoft no válido: {id:?}")));
                }
                id
            }
        })
    }
}

/// Ajustes OAuth en tiempo de ejecución (archivo de configuración o política del administrador).
/// Lo que esté aquí manda sobre las variables de entorno y los valores de compilación.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OAuthSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub google: Option<ClientCredentials>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub microsoft: Option<ClientCredentials>,
    #[serde(default)]
    pub microsoft_tenant: MicrosoftTenant,
}

/// Valor no vacío, recortado.
fn non_empty(s: Option<String>) -> Option<String> {
    s.map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// Ajuste > entorno en ejecución > entorno al compilar.
fn resolve(
    provider: &str,
    from_settings: Option<&ClientCredentials>,
    id_var: &str,
    secret_var: Option<&str>,
    built_id: Option<&'static str>,
    built_secret: Option<&'static str>,
) -> Result<ClientCredentials, Error> {
    if let Some(c) = from_settings
        && !c.client_id.trim().is_empty()
    {
        return Ok(ClientCredentials::new(c.client_id.trim(), non_empty(c.client_secret.clone())));
    }
    let env_secret = secret_var.and_then(|v| non_empty(std::env::var(v).ok()));
    if let Some(id) = non_empty(std::env::var(id_var).ok()) {
        return Ok(ClientCredentials::new(id, env_secret.or_else(|| non_empty(built_secret.map(String::from)))));
    }
    if let Some(id) = non_empty(built_id.map(String::from)) {
        return Ok(ClientCredentials::new(id, non_empty(built_secret.map(String::from)).or(env_secret)));
    }
    Err(Error::MissingClientId { provider: provider.into() })
}

impl OAuthSettings {
    /// Credenciales efectivas de Google.
    pub fn google_credentials(&self) -> Result<ClientCredentials, Error> {
        resolve(
            "Google",
            self.google.as_ref(),
            "IUREDESK_GOOGLE_CLIENT_ID",
            Some("IUREDESK_GOOGLE_CLIENT_SECRET"),
            option_env!("IUREDESK_GOOGLE_CLIENT_ID"),
            option_env!("IUREDESK_GOOGLE_CLIENT_SECRET"),
        )
    }

    /// Credenciales efectivas de Microsoft.
    pub fn microsoft_credentials(&self) -> Result<ClientCredentials, Error> {
        resolve(
            "Microsoft",
            self.microsoft.as_ref(),
            "IUREDESK_MS_CLIENT_ID",
            None,
            option_env!("IUREDESK_MS_CLIENT_ID"),
            None,
        )
    }

    /// Configuración de Google con las credenciales efectivas.
    pub fn google(&self) -> Result<ProviderConfig, Error> {
        Ok(ProviderConfig::google(self.google_credentials()?))
    }

    /// Configuración de Microsoft con las credenciales y el inquilino efectivos.
    pub fn microsoft(&self) -> Result<ProviderConfig, Error> {
        ProviderConfig::microsoft(self.microsoft_credentials()?, &self.microsoft_tenant)
    }
}

/// Alcances de Google. `https://mail.google.com/` es **restringido**: la app publicada necesita
/// verificación de Google y auditoría CASA anual (PLAN.md §12); en modo «pruebas» basta con ≤ 100
/// usuarios de prueba.
pub const GOOGLE_SCOPES: &[&str] = &[
    "openid",
    "email",
    "profile",
    "https://mail.google.com/",
    "https://www.googleapis.com/auth/calendar",
    "https://www.googleapis.com/auth/contacts",
    "https://www.googleapis.com/auth/tasks",
];

/// Alcances de Outlook para IMAP/POP/SMTP (recurso `https://outlook.office.com/`).
pub const MICROSOFT_MAIL_SCOPES: &[&str] = &[
    "https://outlook.office.com/IMAP.AccessAsUser.All",
    "https://outlook.office.com/POP.AccessAsUser.All",
    "https://outlook.office.com/SMTP.Send",
];

/// Alcances de Microsoft Graph (correo, calendario, contactos, To Do, perfil).
pub const MICROSOFT_GRAPH_SCOPES: &[&str] = &[
    "https://graph.microsoft.com/Mail.ReadWrite",
    "https://graph.microsoft.com/Mail.Send",
    "https://graph.microsoft.com/Calendars.ReadWrite",
    "https://graph.microsoft.com/Contacts.ReadWrite",
    "https://graph.microsoft.com/Tasks.ReadWrite",
    "https://graph.microsoft.com/User.Read",
];

/// Reservado: alcances de la API de chat de Microsoft 365 Copilot para el asistente ✦ (PLAN.md §8).
/// Vacío hasta que Microsoft publique la API de forma estable y el dueño decida pedirlos (cada
/// alcance nuevo obliga a volver a pedir consentimiento a todos los usuarios).
pub const MICROSOFT_COPILOT_SCOPES: &[&str] = &[];

/// Alcances OpenID que se piden siempre a Microsoft (`offline_access` da el token de refresco).
const MICROSOFT_OIDC_SCOPES: &[&str] = &["openid", "email", "profile", "offline_access"];

/// Configuración completa de un proveedor. Los campos son públicos para poder describir
/// proveedores genéricos (Fastmail, Yahoo…) o servidores de prueba.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderConfig {
    /// Identificador corto (`google`, `microsoft`, …).
    pub id: String,
    /// Nombre para mensajes («Google», «Microsoft»).
    pub display_name: String,
    pub authorize_url: String,
    pub token_url: String,
    pub client: ClientCredentials,
    /// Alcances que se piden al usuario al iniciar sesión.
    pub consent_scopes: Vec<String>,
    /// Alcances que se mandan al pedir un token para correo. Vacío: no se manda `scope`.
    pub mail_scopes: Vec<String>,
    /// Alcances que se mandan al pedir un token para la API. Vacío: no se manda `scope`.
    pub api_scopes: Vec<String>,
    /// Parámetros adicionales de la URL de autorización.
    pub extra_authorize_params: Vec<(String, String)>,
    pub redirect_host: RedirectHost,
}

fn owned(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_string()).collect()
}

impl ProviderConfig {
    /// Google (Gmail, Calendar, People, Tasks). Un solo token sirve para todo.
    pub fn google(client: ClientCredentials) -> Self {
        Self {
            id: "google".into(),
            display_name: "Google".into(),
            authorize_url: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token_url: "https://oauth2.googleapis.com/token".into(),
            client,
            consent_scopes: owned(GOOGLE_SCOPES),
            mail_scopes: Vec::new(),
            api_scopes: Vec::new(),
            extra_authorize_params: vec![("access_type".into(), "offline".into()), ("prompt".into(), "consent".into())],
            redirect_host: RedirectHost::Ipv4Loopback,
        }
    }

    /// Microsoft identity platform v2 (Outlook.com y Microsoft 365), cliente público multiinquilino.
    pub fn microsoft(client: ClientCredentials, tenant: &MicrosoftTenant) -> Result<Self, Error> {
        let t = tenant.segment()?;
        let mut mail = owned(MICROSOFT_MAIL_SCOPES);
        mail.push("offline_access".into());
        let mut api = owned(MICROSOFT_GRAPH_SCOPES);
        api.extend(owned(MICROSOFT_COPILOT_SCOPES));
        api.push("offline_access".into());
        let mut consent = owned(MICROSOFT_OIDC_SCOPES);
        consent.extend(owned(MICROSOFT_MAIL_SCOPES));
        consent.extend(owned(MICROSOFT_GRAPH_SCOPES));
        consent.extend(owned(MICROSOFT_COPILOT_SCOPES));
        Ok(Self {
            id: "microsoft".into(),
            display_name: "Microsoft".into(),
            authorize_url: format!("https://login.microsoftonline.com/{t}/oauth2/v2.0/authorize"),
            token_url: format!("https://login.microsoftonline.com/{t}/oauth2/v2.0/token"),
            client,
            consent_scopes: consent,
            mail_scopes: mail,
            api_scopes: api,
            extra_authorize_params: vec![("prompt".into(), "select_account".into())],
            redirect_host: RedirectHost::Localhost,
        })
    }

    /// Proveedor genérico (p. ej. Fastmail o Yahoo cuando registremos la app): un solo token,
    /// `redirect_uri` en `127.0.0.1`.
    pub fn custom(
        id: impl Into<String>,
        display_name: impl Into<String>,
        authorize_url: impl Into<String>,
        token_url: impl Into<String>,
        client: ClientCredentials,
        scopes: &[&str],
    ) -> Self {
        Self {
            id: id.into(),
            display_name: display_name.into(),
            authorize_url: authorize_url.into(),
            token_url: token_url.into(),
            client,
            consent_scopes: owned(scopes),
            mail_scopes: Vec::new(),
            api_scopes: Vec::new(),
            extra_authorize_params: Vec::new(),
            redirect_host: RedirectHost::Ipv4Loopback,
        }
    }

    /// Alcances que se mandan al pedir un token para `audience` (vacío: no se manda `scope`).
    pub fn token_scopes(&self, audience: TokenAudience) -> &[String] {
        match audience {
            TokenAudience::Mail => &self.mail_scopes,
            TokenAudience::Api => &self.api_scopes,
        }
    }

    /// Comprueba lo mínimo antes de usar la configuración.
    pub fn validate(&self) -> Result<(), Error> {
        if self.client.client_id.trim().is_empty() {
            return Err(Error::MissingClientId { provider: self.display_name.clone() });
        }
        for u in [&self.authorize_url, &self.token_url] {
            let parsed = url::Url::parse(u).map_err(|e| Error::Config(format!("{u}: {e}")))?;
            let local = matches!(parsed.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
            if parsed.scheme() != "https" && !(parsed.scheme() == "http" && local) {
                return Err(Error::Config(format!("{u}: se exige https")));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn microsoft_tenants() {
        let c = ClientCredentials::new("id", None);
        let p = ProviderConfig::microsoft(c.clone(), &MicrosoftTenant::Organizations).unwrap();
        assert!(p.authorize_url.contains("/organizations/oauth2/v2.0/authorize"));
        assert!(p.consent_scopes.iter().any(|s| s == "offline_access"));
        assert!(p.mail_scopes.iter().all(|s| !s.contains("graph.microsoft.com")));
        assert!(p.api_scopes.iter().all(|s| !s.contains("outlook.office.com")));
        assert!(ProviderConfig::microsoft(c.clone(), &MicrosoftTenant::Id("a/b".into())).is_err());
        assert!(ProviderConfig::microsoft(c, &MicrosoftTenant::Id("contoso.com".into())).is_ok());
    }

    #[test]
    fn google_defaults() {
        let p = ProviderConfig::google(ClientCredentials::new("id", Some("s".into())));
        assert!(p.consent_scopes.iter().any(|s| s == "https://mail.google.com/"));
        assert!(p.token_scopes(TokenAudience::Mail).is_empty());
        assert!(p.extra_authorize_params.contains(&("access_type".into(), "offline".into())));
        assert!(p.validate().is_ok());
        assert!(!format!("{:?}", p.client).contains("\"s\""));
    }

    #[test]
    fn settings_override_wins() {
        let s = OAuthSettings {
            google: Some(ClientCredentials::new(" from-settings ", Some(" ".into()))),
            ..Default::default()
        };
        let c = s.google_credentials().unwrap();
        assert_eq!(c.client_id, "from-settings");
        assert_eq!(c.client_secret, None);
    }

    #[test]
    fn validate_rejects_plain_http() {
        let mut p = ProviderConfig::custom(
            "x",
            "X",
            "http://evil.example/a",
            "https://x/t",
            ClientCredentials::new("i", None),
            &[],
        );
        assert!(p.validate().is_err());
        p.authorize_url = "http://127.0.0.1:9/a".into();
        assert!(p.validate().is_ok());
        p.client.client_id = String::new();
        assert!(matches!(p.validate(), Err(Error::MissingClientId { .. })));
    }
}
