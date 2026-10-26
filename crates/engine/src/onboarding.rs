//! Asistente de alta de cuenta (PLAN.md §5): el usuario escribe su correo, detectamos el proveedor
//! en segundo plano y mostramos **un solo botón**: «Continuar con Google/Microsoft» (OAuth),
//! instrucciones de contraseña de app (iCloud) o contraseña + «Probar conexión» (genérico).
//!
//! Este módulo guarda el estado (sin egui); la red la lanza [`crate::Session`] en su runtime de
//! fondo y los resultados vuelven por [`crate::Session::poll`].

use iuredesk_autoconfig::{Candidate, Discovery};
use iuredesk_model::account::{
    Account, AccountKind, AuthMethod, Identity, OAuthProvider, Protocol, Security, ServerConfig,
};

use crate::background::JobId;
use crate::lang::Text;

/// Paso del asistente.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum WizardStep {
    /// Pide el correo (y el nombre).
    #[default]
    Email,
    /// Buscando el proveedor.
    Detecting,
    /// Muestra el proveedor y su único botón.
    Provider,
    /// Probando la conexión o esperando al navegador.
    Working,
    /// Cuenta guardada.
    Done,
}

/// Cómo entra esta cuenta.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Flow {
    /// «Continuar con Google/Microsoft».
    OAuth(OAuthProvider),
    /// Contraseña específica de app, con instrucciones.
    AppPassword { help_url: &'static str, steps: Text },
    /// Contraseña normal y «Probar conexión».
    Password,
}

/// Estado del asistente.
#[derive(Debug, Default)]
pub struct Wizard {
    pub step: WizardStep,
    pub email: String,
    pub name: String,
    pub password: String,
    pub discovery: Option<Discovery>,
    pub flow: Option<Flow>,
    /// Aviso o error para mostrar (en lenguaje humano).
    pub message: Option<crate::Notice>,
    /// Hay *client id* para el OAuth del proveedor detectado.
    pub oauth_ready: bool,
    /// La cuenta que se guardó.
    pub saved: Option<Account>,
    pub(crate) job: Option<JobId>,
    pub(crate) cancel: Option<iuredesk_oauth::CancellationToken>,
    /// Id de almacén reservado para la cuenta en curso.
    pub(crate) reserved: Option<iuredesk_model::AccountId>,
}

impl Wizard {
    /// Nombre del proveedor para la tarjeta («Gmail», «Fastmail», «example.com»).
    pub fn provider_name(&self) -> String {
        let Some(d) = &self.discovery else { return String::new() };
        if let Some(p) = d.best().and_then(|c| c.provider.clone()) {
            return p;
        }
        match d.kind {
            AccountKind::Gmail => "Google".into(),
            AccountKind::Microsoft365 => "Microsoft 365".into(),
            AccountKind::ICloud => "iCloud".into(),
            AccountKind::Iurefficient => "Iurefficient".into(),
            AccountKind::Generic => d.domain.clone(),
        }
    }

    /// Servidor entrante detectado («imap.gmail.com:993»).
    pub fn server_summary(&self) -> Option<String> {
        let c = self.discovery.as_ref()?.best()?;
        Some(format!("{} {}:{}", protocol_name(c.incoming.protocol), c.incoming.host, c.incoming.port))
    }

    /// `true` si el botón principal se puede pulsar.
    pub fn can_continue(&self) -> bool {
        match (&self.step, &self.flow) {
            (WizardStep::Email, _) => iuredesk_autoconfig::EmailAddress::parse(&self.email).is_ok(),
            (WizardStep::Provider, Some(Flow::OAuth(_))) => self.oauth_ready,
            (WizardStep::Provider, Some(_)) => !self.password.is_empty(),
            _ => false,
        }
    }
}

fn protocol_name(p: Protocol) -> &'static str {
    match p {
        Protocol::Imap => "IMAP",
        Protocol::Pop3 => "POP3",
        Protocol::Smtp => "SMTP",
    }
}

/// El flujo que corresponde a una familia de cuenta.
pub fn flow_for(kind: AccountKind) -> Flow {
    match kind {
        AccountKind::Gmail => Flow::OAuth(OAuthProvider::Google),
        AccountKind::Microsoft365 => Flow::OAuth(OAuthProvider::Microsoft),
        AccountKind::ICloud => Flow::AppPassword {
            help_url: "https://account.apple.com/account/manage",
            steps: Text::new(
                "1. Open your Apple Account page and sign in.\n2. Go to Sign-In and Security → App-Specific Passwords.\n3. Create one named «IureDesk» and paste it below.",
                "1. Abre la página de tu cuenta de Apple e inicia sesión.\n2. Ve a Inicio de sesión y seguridad → Contraseñas para apps.\n3. Crea una llamada «IureDesk» y pégala aquí abajo.",
            ),
        },
        AccountKind::Generic | AccountKind::Iurefficient => Flow::Password,
    }
}

/// ¿Hay *client id* para este proveedor en esta compilación/entorno?
pub fn oauth_available(p: OAuthProvider) -> bool {
    let s = iuredesk_oauth::OAuthSettings::default();
    match p {
        OAuthProvider::Google => s.google_credentials().is_ok(),
        OAuthProvider::Microsoft => s.microsoft_credentials().is_ok(),
    }
}

/// SMTP razonable cuando el descubrimiento no trajo uno.
fn fallback_outgoing(domain: &str, username: &str, auth: AuthMethod) -> ServerConfig {
    ServerConfig {
        protocol: Protocol::Smtp,
        host: format!("smtp.{domain}"),
        port: Protocol::Smtp.default_port(Security::StartTls),
        security: Security::StartTls,
        auth,
        username: username.to_string(),
    }
}

/// Arma la cuenta a partir del candidato elegido.
pub fn build_account(id: String, name: &str, email: &str, cand: &Candidate, auth: AuthMethod) -> Account {
    let mut incoming = cand.incoming.clone();
    incoming.auth = auth;
    let domain = email.rsplit_once('@').map_or("", |(_, d)| d);
    let mut outgoing =
        cand.outgoing.clone().unwrap_or_else(|| fallback_outgoing(domain, &incoming.username, auth));
    outgoing.auth = auth;
    let display = if name.trim().is_empty() {
        email.split('@').next().unwrap_or(email).to_string()
    } else {
        name.trim().to_string()
    };
    Account {
        id,
        display_name: display.clone(),
        email: email.to_string(),
        kind: cand.kind,
        incoming,
        outgoing,
        caldav_url: cand.caldav_url.clone(),
        carddav_url: cand.carddav_url.clone(),
        identities: vec![Identity { name: display, email: email.to_string(), signature: None, reply_to: None }],
    }
}

/// Texto humano de un fallo de prueba de conexión.
pub fn probe_text(e: &iuredesk_accounts::ProbeError) -> Text {
    Text::new(e.message(false), e.message(true))
}

/// Texto humano de un fallo de OAuth.
pub fn oauth_text(e: &iuredesk_oauth::Error) -> Text {
    Text::new(e.message(false), e.message(true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use iuredesk_autoconfig::Source;

    fn cand(kind: AccountKind) -> Candidate {
        Candidate {
            kind,
            provider: Some("Ejemplo".into()),
            incoming: ServerConfig {
                protocol: Protocol::Imap,
                host: "imap.example.com".into(),
                port: 993,
                security: Security::Tls,
                auth: AuthMethod::Password,
                username: "ana@example.com".into(),
            },
            outgoing: None,
            source: Source::KnownProvider,
            score: 90,
            caldav_url: None,
            carddav_url: None,
        }
    }

    #[test]
    fn flows_by_kind() {
        assert_eq!(flow_for(AccountKind::Gmail), Flow::OAuth(OAuthProvider::Google));
        assert_eq!(flow_for(AccountKind::Microsoft365), Flow::OAuth(OAuthProvider::Microsoft));
        assert!(matches!(flow_for(AccountKind::ICloud), Flow::AppPassword { .. }));
        assert_eq!(flow_for(AccountKind::Generic), Flow::Password);
    }

    #[test]
    fn account_from_candidate() {
        let a = build_account("acct7".into(), "", "ana@example.com", &cand(AccountKind::Generic), AuthMethod::AppPassword);
        assert_eq!(a.display_name, "ana");
        assert_eq!(a.incoming.auth, AuthMethod::AppPassword);
        assert_eq!(a.outgoing.host, "smtp.example.com");
        assert_eq!(a.outgoing.port, 587);
        assert_eq!(a.primary_identity().email, "ana@example.com");
    }

    #[test]
    fn continue_needs_valid_input() {
        let mut w = Wizard::default();
        assert!(!w.can_continue());
        w.email = "ana@example.com".into();
        assert!(w.can_continue());
        w.step = WizardStep::Provider;
        w.flow = Some(Flow::Password);
        assert!(!w.can_continue());
        w.password = "x".into();
        assert!(w.can_continue());
    }
}
