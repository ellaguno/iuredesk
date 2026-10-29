//! Asistente de alta de cuenta (PLAN.md §5): el usuario escribe su correo, detectamos el proveedor
//! en segundo plano y mostramos **un solo botón**: «Continuar con Google/Microsoft» (OAuth),
//! instrucciones de contraseña de app (iCloud) o contraseña + «Probar conexión» (genérico).
//!
//! Ningún paso es un callejón sin salida: si el descubrimiento no encuentra nada (o solo adivina)
//! se pasa a «Configurar manualmente» con lo que haya; si la cuenta ya existe se dice y se ofrece
//! volver a iniciar sesión; si el navegador falla, se explica el motivo concreto y el siguiente paso.
//!
//! Este módulo guarda el estado (sin egui); la red la lanza [`crate::Session`] en su runtime de
//! fondo y los resultados vuelven por [`crate::Session::poll`].

use std::sync::{Arc, Mutex};

use iuredesk_autoconfig::{Candidate, Confidence, Discovery, Source};
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
    /// Servidores escritos a mano (IMAP/POP3, SMTP, CalDAV/CardDAV) con «Probar conexión».
    Manual,
    /// La dirección ya es de una cuenta de IureDesk.
    Exists,
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
    /// Volver a iniciar sesión en esta cuenta (en vez de dar de alta una nueva).
    pub reauth: Option<Account>,
    /// Formulario de «Configurar manualmente».
    pub manual: Option<ManualSetup>,
    /// Id de la cuenta que ya tiene esta dirección (paso [`WizardStep::Exists`]).
    pub existing: Option<String>,
    /// Por qué falló el último inicio de sesión con el navegador (decide los botones).
    pub oauth_issue: Option<OAuthIssue>,
    /// Enlace de autorización en curso (para copiarlo si el navegador no se abrió).
    pub(crate) oauth_link: Arc<Mutex<Option<String>>>,
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

    /// Enlace del inicio de sesión en curso, si ya está listo.
    pub fn oauth_link(&self) -> Option<String> {
        self.oauth_link.lock().ok().and_then(|g| g.clone())
    }

    /// `true` si el botón principal se puede pulsar.
    pub fn can_continue(&self) -> bool {
        match (&self.step, &self.flow) {
            (WizardStep::Email, _) => iuredesk_autoconfig::EmailAddress::parse(&self.email).is_ok(),
            (WizardStep::Provider, Some(Flow::OAuth(_))) => self.oauth_ready,
            (WizardStep::Provider, Some(_)) => !self.password.is_empty(),
            (WizardStep::Manual, _) => {
                !self.password.is_empty() && self.manual.as_ref().is_some_and(|m| !m.testing() && m.problem().is_none())
            }
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

/// Descubrimiento sin red: solo la tabla de proveedores conocidos (vista previa del asistente,
/// demos y pruebas). `None` si el dominio no es de un proveedor conocido.
pub fn known_provider_discovery(email: &str) -> Option<Discovery> {
    let addr = iuredesk_autoconfig::EmailAddress::parse(email).ok()?;
    let p = iuredesk_autoconfig::providers::by_domain(&addr.domain)?;
    let candidates = p.candidates(&addr, iuredesk_autoconfig::Source::KnownProvider, 95);
    let kind = candidates.first().map(|c| c.kind)?;
    Some(Discovery {
        email: addr.address.clone(),
        domain: addr.domain.clone(),
        kind,
        candidates,
        notes: p.notes(),
        caldav: None,
        carddav: None,
        timed_out: false,
    })
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
    let mut outgoing = cand.outgoing.clone().unwrap_or_else(|| fallback_outgoing(domain, &incoming.username, auth));
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

// ------------------------------------------------------------------ configurar manualmente

/// Estado de la prueba de un servidor del formulario manual.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProbeStatus {
    Running,
    Ok,
    Failed(Text),
}

/// Formulario de «Configurar manualmente».
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManualSetup {
    /// IMAP o POP3.
    pub incoming: ServerConfig,
    /// SMTP.
    pub outgoing: ServerConfig,
    /// El SMTP usa la misma contraseña que el entrante.
    pub same_password: bool,
    /// Contraseña del SMTP si es otra.
    pub outgoing_password: String,
    pub caldav_url: String,
    pub carddav_url: String,
    pub incoming_test: Option<ProbeStatus>,
    pub outgoing_test: Option<ProbeStatus>,
    /// De dónde salieron los datos propuestos (para el usuario).
    pub hint: Option<Text>,
}

impl ManualSetup {
    /// Cambia el tipo de cuenta (IMAP/POP3) y, si el puerto era el de siempre, también el puerto.
    pub fn set_protocol(&mut self, p: Protocol) {
        if matches!(p, Protocol::Smtp) || p == self.incoming.protocol {
            return;
        }
        self.incoming.port = port_after_change(
            (self.incoming.protocol, self.incoming.security),
            (p, self.incoming.security),
            self.incoming.port,
        );
        self.incoming.protocol = p;
        self.incoming_test = None;
    }

    /// ¿Se está probando algo?
    pub fn testing(&self) -> bool {
        matches!(self.incoming_test, Some(ProbeStatus::Running))
            || matches!(self.outgoing_test, Some(ProbeStatus::Running))
    }

    /// Lo que falta o está mal en el formulario, en lenguaje humano.
    pub fn problem(&self) -> Option<Text> {
        let a = account_from_manual(String::new(), "", "x@example.com", self);
        crate::session::validate_account(&a)
    }
}

/// Puerto tras cambiar protocolo o seguridad: si el usuario dejó el de siempre, pasa al nuevo de
/// siempre (993 ↔ 143, 995 ↔ 110, 465 ↔ 587); si escribió otro, se respeta.
pub fn port_after_change(old: (Protocol, Security), new: (Protocol, Security), port: u16) -> u16 {
    if port == 0 || port == old.0.default_port(old.1) { new.0.default_port(new.1) } else { port }
}

/// ¿El descubrimiento obliga a configurar a mano? (Nada, o solo propuestas de baja confianza.)
pub fn needs_manual(d: &Discovery) -> bool {
    d.best().is_none_or(|c| c.confidence() == Confidence::Low)
}

/// Formulario manual propuesto: el mejor candidato que haya (aunque sea una suposición o la
/// propuesta del hospedaje del MX) o, sin nada, `mail.<dominio>` con SSL/TLS.
pub fn manual_setup(email: &str, discovery: Option<&Discovery>) -> ManualSetup {
    let email = email.trim();
    let domain = email.rsplit_once('@').map_or("", |(_, d)| d).to_ascii_lowercase();
    let server = |protocol: Protocol, host: String, security| ServerConfig {
        protocol,
        port: protocol.default_port(security),
        host,
        security,
        auth: AuthMethod::Password,
        username: email.to_string(),
    };
    let best = discovery.and_then(|d| d.best()).filter(|c| !matches!(c.incoming.auth, AuthMethod::OAuth2 { .. }));
    let (mut incoming, mut outgoing, hint) = match best {
        Some(c) => {
            let out =
                c.outgoing.clone().unwrap_or_else(|| server(Protocol::Smtp, c.incoming.host.clone(), Security::Tls));
            let hint = match (c.source, &c.provider) {
                (Source::Guess, _) => Text::new(
                    "We guessed these servers from your domain. Check them with your provider before testing.",
                    "Propusimos estos servidores a partir de tu dominio. Confírmalos con tu proveedor antes de probar.",
                ),
                (_, Some(p)) => Text::new(
                    format!("Proposed servers for {p}. Change anything your provider tells you."),
                    format!("Servidores propuestos para {p}. Cambia lo que tu proveedor te indique."),
                ),
                _ => Text::new(
                    "We found these servers. Change anything your provider tells you.",
                    "Encontramos estos servidores. Cambia lo que tu proveedor te indique.",
                ),
            };
            (c.incoming.clone(), out, Some(hint))
        }
        None => (
            server(Protocol::Imap, format!("mail.{domain}"), Security::Tls),
            server(Protocol::Smtp, format!("mail.{domain}"), Security::Tls),
            Some(Text::new(
                format!(
                    "We couldn't find the servers of {domain}. Many providers use mail.{domain}; your provider can confirm the data."
                ),
                format!(
                    "No encontramos los servidores de {domain}. Muchos proveedores usan mail.{domain}; tu proveedor puede confirmarte los datos."
                ),
            )),
        ),
    };
    for s in [&mut incoming, &mut outgoing] {
        s.auth = AuthMethod::Password;
        if s.username.trim().is_empty() {
            s.username = email.to_string();
        }
    }
    // Solo direcciones DAV encontradas de verdad (no la suposición «well-known»).
    let dav = |h: Option<&iuredesk_autoconfig::DavHint>| {
        h.filter(|h| h.source != Source::Guess).map(|h| h.url.clone()).unwrap_or_default()
    };
    ManualSetup {
        incoming,
        outgoing,
        same_password: true,
        outgoing_password: String::new(),
        caldav_url: dav(discovery.and_then(|d| d.caldav.as_ref())),
        carddav_url: dav(discovery.and_then(|d| d.carddav.as_ref())),
        incoming_test: None,
        outgoing_test: None,
        hint,
    }
}

/// Formulario manual a partir de una cuenta existente (para no tener dos formularios).
pub fn manual_from_account(a: &Account) -> ManualSetup {
    ManualSetup {
        incoming: a.incoming.clone(),
        outgoing: a.outgoing.clone(),
        same_password: true,
        outgoing_password: String::new(),
        caldav_url: a.caldav_url.clone().unwrap_or_default(),
        carddav_url: a.carddav_url.clone().unwrap_or_default(),
        incoming_test: None,
        outgoing_test: None,
        hint: None,
    }
}

/// La cuenta del formulario manual.
pub fn account_from_manual(id: String, name: &str, email: &str, m: &ManualSetup) -> Account {
    let email = email.trim();
    let display = if name.trim().is_empty() {
        email.split('@').next().unwrap_or(email).to_string()
    } else {
        name.trim().to_string()
    };
    let clean = |s: &ServerConfig| ServerConfig {
        host: s.host.trim().to_string(),
        username: s.username.trim().to_string(),
        auth: AuthMethod::Password,
        ..s.clone()
    };
    let url = |u: &str| Some(u.trim().to_string()).filter(|u| !u.is_empty());
    Account {
        id,
        display_name: display.clone(),
        email: email.to_string(),
        kind: AccountKind::Generic,
        incoming: clean(&m.incoming),
        outgoing: clean(&m.outgoing),
        caldav_url: url(&m.caldav_url),
        carddav_url: url(&m.carddav_url),
        identities: vec![Identity { name: display, email: email.to_string(), signature: None, reply_to: None }],
    }
}

/// La cuenta que ya tiene esa dirección (sin distinguir mayúsculas ni espacios).
pub fn existing_account<'a>(accounts: &'a [Account], email: &str) -> Option<&'a Account> {
    let e = email.trim();
    accounts.iter().find(|a| a.email.trim().eq_ignore_ascii_case(e))
}

// ------------------------------------------------------------------ fallos del navegador

/// Por qué no se pudo iniciar sesión con el proveedor: decide el mensaje y los botones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OAuthIssue {
    /// Esta compilación no trae *client id*.
    NoClientId,
    /// El usuario canceló aquí.
    Cancelled,
    /// El usuario no dio permiso en la página del proveedor.
    Denied,
    /// App en pruebas y la cuenta no está en la lista de usuarios de prueba (Google, 403).
    TestUser,
    /// El administrador de la organización bloquea apps externas (`admin_policy_enforced`,
    /// consentimiento de administrador en Microsoft, acceso condicional).
    AdminBlocked,
    /// La configuración OAuth solo admite cuentas de otra organización (`org_internal`).
    OrgInternal,
    /// No llegó la respuesta del navegador a tiempo.
    TimedOut,
    /// No se pudo abrir el navegador.
    Browser,
    /// Sin red hacia el proveedor.
    Network,
    Other,
}

impl OAuthIssue {
    /// ¿Tiene sentido ofrecer la contraseña de app?
    pub fn offers_app_password(self) -> bool {
        !matches!(self, OAuthIssue::Cancelled | OAuthIssue::Network)
    }

    /// ¿Tiene sentido «Intentar de nuevo»?
    pub fn offers_retry(self) -> bool {
        !matches!(self, OAuthIssue::NoClientId | OAuthIssue::OrgInternal)
    }
}

/// Fallo del inicio de sesión con el navegador, ya explicado.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OAuthFailure {
    pub issue: OAuthIssue,
    pub text: Text,
}

/// Clasifica un error de OAuth.
pub fn oauth_issue(e: &iuredesk_oauth::Error) -> OAuthIssue {
    use iuredesk_oauth::Error as E;
    let by_text = |parts: &[&str]| {
        let t = parts.join(" ").to_ascii_lowercase();
        if t.contains("admin_policy_enforced")
            || t.contains("aadsts65001")
            || t.contains("aadsts90094")
            || t.contains("aadsts90095")
            || t.contains("aadsts53003")
            || t.contains("admin approval")
            || t.contains("administrator")
        {
            Some(OAuthIssue::AdminBlocked)
        } else if t.contains("org_internal") || t.contains("aadsts50020") {
            Some(OAuthIssue::OrgInternal)
        } else if t.contains("verification process")
            || t.contains("test user")
            || t.contains("testing")
            || t.contains("developer-approved")
        {
            Some(OAuthIssue::TestUser)
        } else {
            None
        }
    };
    match e {
        E::MissingClientId { .. } => OAuthIssue::NoClientId,
        E::Cancelled => OAuthIssue::Cancelled,
        E::Timeout => OAuthIssue::TimedOut,
        E::Browser(_) => OAuthIssue::Browser,
        E::Http(_) => OAuthIssue::Network,
        E::AccessDenied(d) => by_text(&[d]).unwrap_or(OAuthIssue::Denied),
        E::Authorization { error, description } | E::TokenEndpoint { error, description, .. } => {
            by_text(&[error, description.as_deref().unwrap_or("")]).unwrap_or(OAuthIssue::Other)
        }
        _ => OAuthIssue::Other,
    }
}

/// Mensaje humano (con el siguiente paso) de un fallo de OAuth.
pub fn oauth_failure(e: &iuredesk_oauth::Error, provider: OAuthProvider) -> OAuthFailure {
    let issue = oauth_issue(e);
    let (who, var) = match provider {
        OAuthProvider::Google => ("Google", "IUREDESK_GOOGLE_CLIENT_ID"),
        OAuthProvider::Microsoft => ("Microsoft", "IUREDESK_MS_CLIENT_ID"),
    };
    let text = match issue {
        OAuthIssue::NoClientId => Text::new(
            format!(
                "This build of IureDesk has no {who} sign-in configured (there is no client id; it is set with {var}). You can sign in with an app password instead."
            ),
            format!(
                "Esta versión de IureDesk no trae configurado el inicio de sesión con {who} (no tiene client id; se define con {var}). Mientras, puedes entrar con una contraseña de app."
            ),
        ),
        OAuthIssue::Cancelled => Text::new(
            "You cancelled the sign-in. Try again whenever you are ready.",
            "Cancelaste el inicio de sesión. Vuelve a intentarlo cuando quieras.",
        ),
        OAuthIssue::Denied => Text::new(
            format!(
                "You did not give IureDesk permission on the {who} page, so we cannot read your mail. Try again and choose «Allow», or use an app password."
            ),
            format!(
                "No diste permiso a IureDesk en la página de {who}, así que no podemos leer tu correo. Vuelve a intentarlo y elige «Permitir», o usa una contraseña de app."
            ),
        ),
        OAuthIssue::TestUser => Text::new(
            format!(
                "{who} blocked the access because this build of IureDesk is still in testing and your account is not on its list of test users (error 403). Use an app password, or ask for your account to be added as a test user."
            ),
            format!(
                "{who} bloqueó el acceso porque esta versión de IureDesk aún está en pruebas y tu cuenta no está en su lista de usuarios de prueba (error 403). Usa una contraseña de app o pide que agreguen tu cuenta como usuario de prueba."
            ),
        ),
        OAuthIssue::AdminBlocked => Text::new(
            "Your organization's administrator does not allow outside apps such as IureDesk to access your account. Ask them to approve IureDesk, or use an app password if your organization allows it.",
            "El administrador de tu organización no permite que apps externas como IureDesk entren a tu cuenta. Pídele que apruebe IureDesk, o usa una contraseña de app si tu organización lo permite.",
        ),
        OAuthIssue::OrgInternal => Text::new(
            format!(
                "The {who} sign-in of this build only accepts accounts of another organization, so it cannot be used with yours. Use an app password."
            ),
            format!(
                "El inicio de sesión con {who} de esta versión solo acepta cuentas de otra organización, así que no sirve para la tuya. Usa una contraseña de app."
            ),
        ),
        OAuthIssue::TimedOut => Text::new(
            format!(
                "We got no answer from the browser in time. If {who} showed «Access blocked» or «Error 403», your organization or this build does not allow the access: use an app password. If you just closed the window, try again."
            ),
            format!(
                "No recibimos respuesta del navegador a tiempo. Si {who} mostró «Acceso bloqueado» o «Error 403», tu organización o esta versión no permiten el acceso: usa una contraseña de app. Si solo cerraste la ventana, vuelve a intentarlo."
            ),
        ),
        OAuthIssue::Browser => Text::new(
            "Your browser could not be opened. Try again; while IureDesk waits you can copy the link and open it yourself.",
            "No se pudo abrir tu navegador. Vuelve a intentarlo; mientras IureDesk espera puedes copiar el enlace y abrirlo tú.",
        ),
        OAuthIssue::Network | OAuthIssue::Other => oauth_text(e),
    };
    OAuthFailure { issue, text }
}

/// Por qué no se ofrece «Continuar con Google/Microsoft» (no hay *client id* en esta compilación).
pub fn oauth_unavailable(p: OAuthProvider) -> Text {
    let who = match p {
        OAuthProvider::Google => "Google",
        OAuthProvider::Microsoft => "Microsoft",
    };
    oauth_failure(&iuredesk_oauth::Error::MissingClientId { provider: who.into() }, p).text
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
        let a =
            build_account("acct7".into(), "", "ana@example.com", &cand(AccountKind::Generic), AuthMethod::AppPassword);
        assert_eq!(a.display_name, "ana");
        assert_eq!(a.incoming.auth, AuthMethod::AppPassword);
        assert_eq!(a.outgoing.host, "smtp.example.com");
        assert_eq!(a.outgoing.port, 587);
        assert_eq!(a.primary_identity().email, "ana@example.com");
    }

    #[test]
    fn known_providers_without_network() {
        let d = known_provider_discovery("ana@gmail.com").unwrap();
        assert_eq!(d.kind, AccountKind::Gmail);
        assert!(d.best().is_some());
        assert!(known_provider_discovery("ana@despacho.example.org").is_none());
        assert!(known_provider_discovery("no es correo").is_none());
    }

    #[test]
    fn ports_follow_security_unless_custom() {
        let imap = |s| (Protocol::Imap, s);
        assert_eq!(port_after_change(imap(Security::Tls), imap(Security::StartTls), 993), 143);
        assert_eq!(port_after_change(imap(Security::Tls), imap(Security::StartTls), 1993), 1993);
        assert_eq!(port_after_change(imap(Security::Tls), (Protocol::Pop3, Security::Tls), 993), 995);
        assert_eq!(port_after_change((Protocol::Pop3, Security::Tls), (Protocol::Pop3, Security::Plain), 995), 110);
        let smtp = |s| (Protocol::Smtp, s);
        assert_eq!(port_after_change(smtp(Security::Tls), smtp(Security::StartTls), 465), 587);
        assert_eq!(port_after_change(smtp(Security::StartTls), smtp(Security::Tls), 0), 465);
    }

    #[test]
    fn manual_setup_without_discovery_proposes_mail_domain() {
        let mut m = manual_setup("Ellaguno@Confiabilidad.com.mx", None);
        assert_eq!(m.incoming.host, "mail.confiabilidad.com.mx");
        assert_eq!((m.incoming.protocol, m.incoming.port, m.incoming.security), (Protocol::Imap, 993, Security::Tls));
        assert_eq!((m.outgoing.port, m.outgoing.security), (465, Security::Tls));
        assert_eq!(m.incoming.username, "Ellaguno@Confiabilidad.com.mx");
        assert!(m.same_password);
        assert!(m.problem().is_none());
        m.set_protocol(Protocol::Pop3);
        assert_eq!(m.incoming.port, 995);
        m.incoming.host.clear();
        assert!(m.problem().is_some_and(|t| t.es.contains("servidor de entrada")));
        let a = account_from_manual("a".into(), "", "x@y.mx", &manual_setup("x@y.mx", None));
        assert_eq!(a.kind, AccountKind::Generic);
        assert_eq!(a.caldav_url, None);
        assert_eq!(manual_from_account(&a).incoming, a.incoming);
    }

    #[test]
    fn manual_setup_uses_low_confidence_candidates() {
        let mut c = cand(AccountKind::Generic);
        c.source = Source::Guess;
        c.score = 25;
        let d = Discovery {
            email: "ana@example.com".into(),
            domain: "example.com".into(),
            kind: AccountKind::Generic,
            candidates: vec![c],
            notes: Vec::new(),
            caldav: Some(iuredesk_autoconfig::DavHint {
                url: "https://example.com/.well-known/caldav".into(),
                source: Source::Guess,
            }),
            carddav: Some(iuredesk_autoconfig::DavHint {
                url: "https://dav.example.com/card".into(),
                source: Source::Srv,
            }),
            timed_out: false,
        };
        assert!(needs_manual(&d));
        let m = manual_setup("ana@example.com", Some(&d));
        assert_eq!(m.incoming.host, "imap.example.com");
        assert_eq!(m.outgoing.host, "imap.example.com");
        assert!(m.hint.as_ref().is_some_and(|t| t.es.contains("a partir de tu dominio")));
        assert_eq!(m.caldav_url, "");
        assert_eq!(m.carddav_url, "https://dav.example.com/card");
        assert!(known_provider_discovery("ana@gmail.com").is_some_and(|d| !needs_manual(&d)));
    }

    #[test]
    fn duplicate_detection_ignores_case() {
        let a = build_account("a1".into(), "", "Eduardo@Llaguno.com", &cand(AccountKind::Gmail), AuthMethod::Password);
        let list = vec![a];
        assert_eq!(existing_account(&list, " eduardo@llaguno.COM ").map(|a| a.id.as_str()), Some("a1"));
        assert!(existing_account(&list, "otro@llaguno.com").is_none());
    }

    #[test]
    fn oauth_errors_are_classified() {
        use iuredesk_oauth::Error as E;
        let auth = |e: &str, d: Option<&str>| E::Authorization { error: e.into(), description: d.map(str::to_string) };
        assert_eq!(oauth_issue(&E::MissingClientId { provider: "Google".into() }), OAuthIssue::NoClientId);
        assert_eq!(oauth_issue(&E::AccessDenied(String::new())), OAuthIssue::Denied);
        assert_eq!(
            oauth_issue(&E::AccessDenied("IureDesk has not completed the Google verification process".into())),
            OAuthIssue::TestUser
        );
        assert_eq!(oauth_issue(&auth("admin_policy_enforced", None)), OAuthIssue::AdminBlocked);
        assert_eq!(oauth_issue(&auth("org_internal", None)), OAuthIssue::OrgInternal);
        assert_eq!(
            oauth_issue(&auth("consent_required", Some("AADSTS65001: needs admin approval"))),
            OAuthIssue::AdminBlocked
        );
        assert_eq!(oauth_issue(&auth("invalid_request", None)), OAuthIssue::Other);
        assert_eq!(oauth_issue(&E::Timeout), OAuthIssue::TimedOut);
        assert_eq!(oauth_issue(&E::Cancelled), OAuthIssue::Cancelled);
        let f = oauth_failure(&E::MissingClientId { provider: "Google".into() }, OAuthProvider::Google);
        assert!(f.text.es.contains("IUREDESK_GOOGLE_CLIENT_ID"));
        assert!(!f.issue.offers_retry() && f.issue.offers_app_password());
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
