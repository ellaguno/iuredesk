//! Cuentas de correo, calendario y contactos (tipos puros, sin E/S).
//!
//! Aquí **nunca** viven secretos: contraseñas, contraseñas de app y tokens OAuth van al llavero del
//! sistema (crate `iuredesk-accounts`). Estos tipos se guardan tal cual en `accounts.json`.
//!
//! Nota de integración: el identificador de la cuenta es por ahora un `String` (`Account::id`).
//! Cuando `ids.rs` defina `AccountId`, se unifica aquí sin cambiar el formato JSON (un newtype
//! `#[serde(transparent)]` serializa igual que la cadena).

use serde::{Deserialize, Serialize};

/// Familia de la cuenta: decide el asistente de alta y los protocolos de calendario/contactos.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AccountKind {
    /// IMAP/POP3 + SMTP de cualquier proveedor.
    #[default]
    Generic,
    /// Gmail o Google Workspace.
    Gmail,
    /// Microsoft 365 / Outlook.com.
    Microsoft365,
    /// Apple iCloud (contraseña específica de app).
    #[serde(rename = "icloud")]
    ICloud,
    /// Instancia de Iurefficient (no provee buzones; agenda, contactos y vínculos).
    Iurefficient,
}

/// Protocolo de un servidor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    Imap,
    Pop3,
    Smtp,
}

impl Protocol {
    /// ¿Sirve para recibir correo?
    pub fn is_incoming(self) -> bool {
        matches!(self, Protocol::Imap | Protocol::Pop3)
    }

    /// Puerto estándar para una seguridad dada.
    pub fn default_port(self, security: Security) -> u16 {
        match (self, security) {
            (Protocol::Imap, Security::Tls) => 993,
            (Protocol::Imap, _) => 143,
            (Protocol::Pop3, Security::Tls) => 995,
            (Protocol::Pop3, _) => 110,
            (Protocol::Smtp, Security::Tls) => 465,
            (Protocol::Smtp, Security::StartTls) => 587,
            (Protocol::Smtp, Security::Plain) => 25,
        }
    }
}

/// Seguridad del transporte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Security {
    /// TLS desde el primer byte (puertos 993, 995, 465).
    Tls,
    /// Texto plano que se cifra con STARTTLS/STLS.
    StartTls,
    /// Sin cifrar. Solo para servidores locales (p. ej. Proton Bridge) o pruebas.
    Plain,
}

/// Proveedor OAuth 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OAuthProvider {
    Google,
    Microsoft,
}

/// Cómo se autentica. El secreto correspondiente vive en el llavero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "method")]
pub enum AuthMethod {
    /// Contraseña normal de la cuenta.
    Password,
    /// Contraseña específica de app (iCloud, Yahoo, Gmail con 2FA sin OAuth…).
    AppPassword,
    /// Token OAuth 2 (XOAUTH2 / OAUTHBEARER).
    #[serde(rename = "oauth2")]
    OAuth2 { provider: OAuthProvider },
}

/// Un servidor de correo entrante o saliente.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ServerConfig {
    pub protocol: Protocol,
    pub host: String,
    pub port: u16,
    pub security: Security,
    pub auth: AuthMethod,
    /// Usuario de inicio de sesión (casi siempre el correo completo).
    pub username: String,
}

/// Una identidad de envío («De:») de la cuenta.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub name: String,
    pub email: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
}

/// Una cuenta configurada. Sin secretos.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    /// Identificador estable (se unificará con `ids::AccountId`). También es la clave del llavero:
    /// `iuredesk` / `<id>:<propósito>`.
    pub id: String,
    pub display_name: String,
    pub email: String,
    #[serde(default)]
    pub kind: AccountKind,
    /// IMAP o POP3.
    pub incoming: ServerConfig,
    /// SMTP.
    pub outgoing: ServerConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caldav_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carddav_url: Option<String>,
    #[serde(default)]
    pub identities: Vec<Identity>,
}

impl Account {
    /// Identidad principal: la primera de la lista o, si no hay, el nombre y correo de la cuenta.
    pub fn primary_identity(&self) -> Identity {
        self.identities.first().cloned().unwrap_or_else(|| Identity {
            name: self.display_name.clone(),
            email: self.email.clone(),
            signature: None,
            reply_to: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Account {
        Account {
            id: "a1".into(),
            display_name: "Ana".into(),
            email: "ana@example.com".into(),
            kind: AccountKind::Gmail,
            incoming: ServerConfig {
                protocol: Protocol::Imap,
                host: "imap.gmail.com".into(),
                port: 993,
                security: Security::Tls,
                auth: AuthMethod::OAuth2 { provider: OAuthProvider::Google },
                username: "ana@example.com".into(),
            },
            outgoing: ServerConfig {
                protocol: Protocol::Smtp,
                host: "smtp.gmail.com".into(),
                port: 465,
                security: Security::Tls,
                auth: AuthMethod::OAuth2 { provider: OAuthProvider::Google },
                username: "ana@example.com".into(),
            },
            caldav_url: None,
            carddav_url: None,
            identities: vec![],
        }
    }

    #[test]
    fn json_round_trip() {
        let a = sample();
        let json = serde_json::to_string(&a).unwrap();
        assert!(json.contains(r#""method":"oauth2","provider":"google""#));
        let b: Account = serde_json::from_str(&json).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn primary_identity_falls_back_to_account() {
        let a = sample();
        assert_eq!(a.primary_identity().email, "ana@example.com");
    }

    #[test]
    fn default_ports() {
        assert_eq!(Protocol::Imap.default_port(Security::Tls), 993);
        assert_eq!(Protocol::Smtp.default_port(Security::StartTls), 587);
        assert_eq!(Protocol::Pop3.default_port(Security::Plain), 110);
    }
}
