//! Tabla de proveedores conocidos: la fuente más rápida y fiable (no toca la red).
//!
//! Cada proveedor se reconoce por el dominio del correo o, para dominios propios (Google
//! Workspace, Microsoft 365, iCloud+, Zoho, Fastmail, Proton), por el sufijo de su MX.

use iuredesk_model::account::{AccountKind, AuthMethod, OAuthProvider, Protocol, Security, ServerConfig};

use crate::email::EmailAddress;
use crate::xml::host_matches;
use crate::{Candidate, Note, Source};

/// Un servidor de la tabla.
#[derive(Debug, Clone, Copy)]
pub struct Host {
    pub host: &'static str,
    pub port: u16,
    pub security: Security,
}

const fn tls(host: &'static str, port: u16) -> Option<Host> {
    Some(Host { host, port, security: Security::Tls })
}

const fn starttls(host: &'static str, port: u16) -> Option<Host> {
    Some(Host { host, port, security: Security::StartTls })
}

/// Un proveedor conocido.
#[derive(Debug, Clone, Copy)]
pub struct KnownProvider {
    pub id: &'static str,
    pub name: &'static str,
    pub kind: AccountKind,
    /// Dominios de correo exactos.
    pub domains: &'static [&'static str],
    /// Primer nivel del dominio que vale con cualquier terminación de país (`hotmail.es`,
    /// `yahoo.com.mx`).
    pub domain_prefixes: &'static [&'static str],
    /// Sufijos de MX que delatan al proveedor en un dominio propio.
    pub mx_suffixes: &'static [&'static str],
    pub imap: Option<Host>,
    pub pop3: Option<Host>,
    pub smtp: Option<Host>,
    pub auth: AuthMethod,
    pub caldav: Option<&'static str>,
    pub carddav: Option<&'static str>,
    /// Avisos para el usuario (en, es).
    pub notes: &'static [(&'static str, &'static str)],
}

const GOOGLE: AuthMethod = AuthMethod::OAuth2 { provider: OAuthProvider::Google };
const MICROSOFT: AuthMethod = AuthMethod::OAuth2 { provider: OAuthProvider::Microsoft };

const NOTE_GOOGLE: (&str, &str) = (
    "Sign in with your Google account (OAuth). With a password you need an app password and 2-Step Verification.",
    "Inicia sesión con tu cuenta de Google (OAuth). Con contraseña necesitas una contraseña de app y la verificación en dos pasos.",
);
const NOTE_MICROSOFT: (&str, &str) = (
    "Microsoft turned off basic authentication: sign in with your Microsoft account (OAuth).",
    "Microsoft desactivó la autenticación básica: inicia sesión con tu cuenta de Microsoft (OAuth).",
);
const NOTE_ICLOUD: (&str, &str) = (
    "iCloud requires an app-specific password: create one at account.apple.com → Sign-In and Security → App-Specific Passwords.",
    "iCloud requiere una contraseña específica de app: créala en account.apple.com → Inicio de sesión y seguridad → Contraseñas específicas de app.",
);
const NOTE_APP_PASSWORD: (&str, &str) = (
    "This provider requires an app password (generate it in your account's security settings).",
    "Este proveedor requiere una contraseña de app (se genera en la configuración de seguridad de tu cuenta).",
);

/// Proveedores conocidos.
pub static PROVIDERS: &[KnownProvider] = &[
    KnownProvider {
        id: "gmail",
        name: "Gmail",
        kind: AccountKind::Gmail,
        domains: &["gmail.com", "googlemail.com"],
        domain_prefixes: &[],
        mx_suffixes: &["google.com", "googlemail.com"],
        imap: tls("imap.gmail.com", 993),
        pop3: tls("pop.gmail.com", 995),
        smtp: tls("smtp.gmail.com", 465),
        auth: GOOGLE,
        caldav: Some("https://apidata.googleusercontent.com/caldav/v2/"),
        carddav: Some("https://www.googleapis.com/carddav/v1/principals/"),
        notes: &[NOTE_GOOGLE],
    },
    KnownProvider {
        id: "outlook",
        name: "Outlook.com",
        kind: AccountKind::Microsoft365,
        domains: &["outlook.com", "hotmail.com", "live.com", "msn.com", "passport.com"],
        domain_prefixes: &["outlook", "hotmail", "live"],
        mx_suffixes: &[],
        imap: tls("outlook.office365.com", 993),
        pop3: tls("outlook.office365.com", 995),
        smtp: starttls("smtp-mail.outlook.com", 587),
        auth: MICROSOFT,
        caldav: None,
        carddav: None,
        notes: &[NOTE_MICROSOFT],
    },
    KnownProvider {
        id: "microsoft365",
        name: "Microsoft 365",
        kind: AccountKind::Microsoft365,
        domains: &[],
        domain_prefixes: &[],
        mx_suffixes: &["mail.protection.outlook.com", "mail.protection.outlook.de", "olc.protection.outlook.com"],
        imap: tls("outlook.office365.com", 993),
        pop3: tls("outlook.office365.com", 995),
        smtp: starttls("smtp.office365.com", 587),
        auth: MICROSOFT,
        caldav: None,
        carddav: None,
        notes: &[NOTE_MICROSOFT],
    },
    KnownProvider {
        id: "icloud",
        name: "iCloud",
        kind: AccountKind::ICloud,
        domains: &["icloud.com", "me.com", "mac.com"],
        domain_prefixes: &[],
        mx_suffixes: &["mail.icloud.com"],
        imap: tls("imap.mail.me.com", 993),
        pop3: None,
        smtp: starttls("smtp.mail.me.com", 587),
        auth: AuthMethod::AppPassword,
        caldav: Some("https://caldav.icloud.com/"),
        carddav: Some("https://contacts.icloud.com/"),
        notes: &[NOTE_ICLOUD],
    },
    KnownProvider {
        id: "yahoo",
        name: "Yahoo Mail",
        kind: AccountKind::Generic,
        domains: &["yahoo.com", "ymail.com", "rocketmail.com"],
        domain_prefixes: &["yahoo"],
        mx_suffixes: &["yahoodns.net"],
        imap: tls("imap.mail.yahoo.com", 993),
        pop3: tls("pop.mail.yahoo.com", 995),
        smtp: tls("smtp.mail.yahoo.com", 465),
        auth: AuthMethod::AppPassword,
        caldav: Some("https://caldav.calendar.yahoo.com/"),
        carddav: Some("https://carddav.address.yahoo.com/"),
        notes: &[NOTE_APP_PASSWORD],
    },
    KnownProvider {
        id: "aol",
        name: "AOL Mail",
        kind: AccountKind::Generic,
        domains: &["aol.com", "aim.com"],
        domain_prefixes: &[],
        mx_suffixes: &[],
        imap: tls("imap.aol.com", 993),
        pop3: tls("pop.aol.com", 995),
        smtp: tls("smtp.aol.com", 465),
        auth: AuthMethod::AppPassword,
        caldav: None,
        carddav: None,
        notes: &[NOTE_APP_PASSWORD],
    },
    KnownProvider {
        id: "gmx.com",
        name: "GMX",
        kind: AccountKind::Generic,
        domains: &["gmx.com", "gmx.us", "gmx.co.uk", "gmx.es", "gmx.fr"],
        domain_prefixes: &[],
        mx_suffixes: &[],
        imap: tls("imap.gmx.com", 993),
        pop3: tls("pop.gmx.com", 995),
        smtp: starttls("mail.gmx.com", 587),
        auth: AuthMethod::Password,
        caldav: None,
        carddav: None,
        notes: &[(
            "Enable POP3/IMAP access in GMX's web settings first.",
            "Primero activa el acceso POP3/IMAP en la configuración web de GMX.",
        )],
    },
    KnownProvider {
        id: "gmx.net",
        name: "GMX",
        kind: AccountKind::Generic,
        domains: &["gmx.net", "gmx.de", "gmx.at", "gmx.ch"],
        domain_prefixes: &[],
        mx_suffixes: &[],
        imap: tls("imap.gmx.net", 993),
        pop3: tls("pop.gmx.net", 995),
        smtp: starttls("mail.gmx.net", 587),
        auth: AuthMethod::Password,
        caldav: None,
        carddav: None,
        notes: &[(
            "Enable POP3/IMAP access in GMX's web settings first.",
            "Primero activa el acceso POP3/IMAP en la configuración web de GMX.",
        )],
    },
    KnownProvider {
        id: "web.de",
        name: "WEB.DE",
        kind: AccountKind::Generic,
        domains: &["web.de"],
        domain_prefixes: &[],
        mx_suffixes: &[],
        imap: tls("imap.web.de", 993),
        pop3: tls("pop3.web.de", 995),
        smtp: starttls("smtp.web.de", 587),
        auth: AuthMethod::Password,
        caldav: None,
        carddav: None,
        notes: &[],
    },
    KnownProvider {
        id: "zoho",
        name: "Zoho Mail",
        kind: AccountKind::Generic,
        domains: &["zoho.com", "zohomail.com"],
        domain_prefixes: &[],
        mx_suffixes: &["zoho.com"],
        imap: tls("imap.zoho.com", 993),
        pop3: tls("pop.zoho.com", 995),
        smtp: tls("smtp.zoho.com", 465),
        auth: AuthMethod::Password,
        caldav: Some("https://calendar.zoho.com/caldav/"),
        carddav: Some("https://contacts.zoho.com/carddav/"),
        notes: &[(
            "With two-factor authentication on, Zoho requires an app password. EU accounts use imap.zoho.eu / smtp.zoho.eu.",
            "Con verificación en dos pasos, Zoho requiere una contraseña de app. Las cuentas de la UE usan imap.zoho.eu / smtp.zoho.eu.",
        )],
    },
    KnownProvider {
        id: "zoho.eu",
        name: "Zoho Mail (EU)",
        kind: AccountKind::Generic,
        domains: &["zoho.eu", "zohomail.eu"],
        domain_prefixes: &[],
        mx_suffixes: &["zoho.eu"],
        imap: tls("imap.zoho.eu", 993),
        pop3: tls("pop.zoho.eu", 995),
        smtp: tls("smtp.zoho.eu", 465),
        auth: AuthMethod::Password,
        caldav: Some("https://calendar.zoho.eu/caldav/"),
        carddav: Some("https://contacts.zoho.eu/carddav/"),
        notes: &[],
    },
    KnownProvider {
        id: "fastmail",
        name: "Fastmail",
        kind: AccountKind::Generic,
        domains: &["fastmail.com", "fastmail.fm"],
        domain_prefixes: &[],
        mx_suffixes: &["messagingengine.com"],
        imap: tls("imap.fastmail.com", 993),
        pop3: tls("pop.fastmail.com", 995),
        smtp: tls("smtp.fastmail.com", 465),
        auth: AuthMethod::AppPassword,
        caldav: Some("https://caldav.fastmail.com/dav/"),
        carddav: Some("https://carddav.fastmail.com/dav/"),
        notes: &[NOTE_APP_PASSWORD],
    },
    KnownProvider {
        id: "yandex",
        name: "Yandex Mail",
        kind: AccountKind::Generic,
        domains: &["yandex.com", "yandex.ru", "ya.ru"],
        domain_prefixes: &[],
        mx_suffixes: &["yandex.net"],
        imap: tls("imap.yandex.com", 993),
        pop3: tls("pop.yandex.com", 995),
        smtp: tls("smtp.yandex.com", 465),
        auth: AuthMethod::AppPassword,
        caldav: Some("https://caldav.yandex.ru/"),
        carddav: Some("https://carddav.yandex.ru/"),
        notes: &[NOTE_APP_PASSWORD],
    },
    KnownProvider {
        id: "proton",
        name: "Proton Mail (Bridge)",
        kind: AccountKind::Generic,
        domains: &["proton.me", "protonmail.com", "protonmail.ch", "pm.me"],
        domain_prefixes: &[],
        mx_suffixes: &["protonmail.ch"],
        imap: starttls("127.0.0.1", 1143),
        pop3: None,
        smtp: starttls("127.0.0.1", 1025),
        auth: AuthMethod::Password,
        caldav: None,
        carddav: None,
        notes: &[(
            "Proton Mail only works through Proton Mail Bridge (paid plans): install it, sign in, and use the password Bridge shows you.",
            "Proton Mail solo funciona a través de Proton Mail Bridge (planes de pago): instálalo, inicia sesión y usa la contraseña que te muestra Bridge.",
        )],
    },
];

impl KnownProvider {
    /// ¿Atiende este dominio de correo?
    pub fn matches_domain(&self, domain: &str) -> bool {
        if self.domains.iter().any(|d| d.eq_ignore_ascii_case(domain)) {
            return true;
        }
        let Some((first, rest)) = domain.split_once('.') else { return false };
        self.domain_prefixes.iter().any(|p| p.eq_ignore_ascii_case(first)) && looks_like_public_suffix(rest)
    }

    /// ¿Este MX es suyo?
    pub fn matches_mx(&self, mx: &str) -> bool {
        self.mx_suffixes.iter().any(|s| host_matches(mx, s))
    }

    /// Avisos del proveedor.
    pub fn notes(&self) -> Vec<Note> {
        self.notes.iter().map(|(en, es)| Note::new(en, es)).collect()
    }

    fn server(&self, protocol: Protocol, h: Host, email: &EmailAddress) -> ServerConfig {
        ServerConfig {
            protocol,
            host: h.host.to_string(),
            port: h.port,
            security: h.security,
            auth: self.auth,
            username: email.address.clone(),
        }
    }

    /// Candidatos (IMAP primero; POP3 un poco abajo) para `email`.
    pub fn candidates(&self, email: &EmailAddress, source: Source, score: u8) -> Vec<Candidate> {
        let outgoing = self.smtp.map(|h| self.server(Protocol::Smtp, h, email));
        let mut out = Vec::new();
        for (protocol, host, penalty) in [(Protocol::Imap, self.imap, 0u8), (Protocol::Pop3, self.pop3, 10)] {
            let Some(h) = host else { continue };
            out.push(Candidate {
                kind: self.kind,
                provider: Some(self.name.to_string()),
                incoming: self.server(protocol, h, email),
                outgoing: outgoing.clone(),
                source,
                score: score.saturating_sub(penalty),
                caldav_url: self.caldav.map(str::to_string),
                carddav_url: self.carddav.map(str::to_string),
            });
        }
        out
    }
}

/// Hospedaje de correo reconocible por su MX pero sin ISPDB ni autoconfig: solo sirve para
/// **proponer** servidores en «Configurar manualmente» (confianza baja; el usuario los prueba).
/// `{domain}` en un nombre se cambia por el dominio del correo.
#[derive(Debug, Clone, Copy)]
pub struct Hosting {
    pub id: &'static str,
    pub name: &'static str,
    pub mx_suffixes: &'static [&'static str],
    pub imap: Host,
    pub smtp: Host,
}

/// Hospedajes conocidos (servidores según su documentación pública).
pub static HOSTINGS: &[Hosting] = &[
    // Network Solutions / Web.com / Register.com: el MX es `mx*.carrierzone.com`. `mail.<dominio>`
    // no siempre existe en el DNS (caso real: confiabilidad.com.mx) y su certificado es
    // `*.carrierzone.com`, así que se propone el servidor común: IMAP 993 SSL y SMTP 587 STARTTLS
    // (también responde en 1025).
    Hosting {
        id: "carrierzone",
        name: "Network Solutions / Web.com",
        mx_suffixes: &["carrierzone.com"],
        imap: Host { host: "mail.carrierzone.com", port: 993, security: Security::Tls },
        smtp: Host { host: "mail.carrierzone.com", port: 587, security: Security::StartTls },
    },
    Hosting {
        id: "godaddy",
        name: "GoDaddy",
        mx_suffixes: &["secureserver.net"],
        imap: Host { host: "imap.secureserver.net", port: 993, security: Security::Tls },
        smtp: Host { host: "smtpout.secureserver.net", port: 465, security: Security::Tls },
    },
    Hosting {
        id: "hostinger",
        name: "Hostinger",
        mx_suffixes: &["hostinger.com"],
        imap: Host { host: "imap.hostinger.com", port: 993, security: Security::Tls },
        smtp: Host { host: "smtp.hostinger.com", port: 465, security: Security::Tls },
    },
];

/// Puntuación de una propuesta de hospedaje (confianza baja: se ofrece en el formulario manual).
pub const HOSTING_SCORE: u8 = 40;

impl Hosting {
    fn server(&self, protocol: Protocol, h: Host, email: &EmailAddress) -> ServerConfig {
        ServerConfig {
            protocol,
            host: h.host.replace("{domain}", &email.domain),
            port: h.port,
            security: h.security,
            auth: AuthMethod::Password,
            username: email.address.clone(),
        }
    }

    /// La propuesta para `email`.
    pub fn candidate(&self, email: &EmailAddress) -> Candidate {
        Candidate {
            kind: AccountKind::Generic,
            provider: Some(self.name.to_string()),
            incoming: self.server(Protocol::Imap, self.imap, email),
            outgoing: Some(self.server(Protocol::Smtp, self.smtp, email)),
            source: Source::Mx,
            score: HOSTING_SCORE,
            caldav_url: None,
            carddav_url: None,
        }
    }

    /// Aviso para el usuario.
    pub fn note(&self) -> Note {
        Note {
            en: format!(
                "Your domain's mail is hosted by {}. We filled in its usual servers: check them with your provider.",
                self.name
            ),
            es: format!(
                "El correo de tu dominio lo aloja {}. Pusimos sus servidores habituales: confírmalos con tu proveedor.",
                self.name
            ),
        }
    }
}

/// Hospedaje por un nombre de MX.
pub fn hosting_by_mx(mx: &str) -> Option<&'static Hosting> {
    HOSTINGS.iter().find(|h| h.mx_suffixes.iter().any(|s| host_matches(mx, s)))
}

/// `es`, `com.mx`, `co.uk`: uno o dos niveles cortos.
fn looks_like_public_suffix(rest: &str) -> bool {
    let labels: Vec<&str> = rest.split('.').collect();
    (1..=2).contains(&labels.len())
        && labels.iter().all(|l| (2..=3).contains(&l.len()) && l.chars().all(|c| c.is_ascii_alphabetic()))
}

/// Proveedor por el dominio del correo.
pub fn by_domain(domain: &str) -> Option<&'static KnownProvider> {
    PROVIDERS.iter().find(|p| p.matches_domain(domain))
}

/// Proveedor por un nombre de MX.
pub fn by_mx(mx: &str) -> Option<&'static KnownProvider> {
    PROVIDERS.iter().find(|p| p.matches_mx(mx))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_domains() {
        let id = |d: &str| by_domain(d).map(|p| p.id);
        assert_eq!(id("gmail.com"), Some("gmail"));
        assert_eq!(id("googlemail.com"), Some("gmail"));
        assert_eq!(id("hotmail.com"), Some("outlook"));
        assert_eq!(id("hotmail.es"), Some("outlook"));
        assert_eq!(id("outlook.com.mx"), Some("outlook"));
        assert_eq!(id("live.com.mx"), Some("outlook"));
        assert_eq!(id("msn.com"), Some("outlook"));
        assert_eq!(id("icloud.com"), Some("icloud"));
        assert_eq!(id("me.com"), Some("icloud"));
        assert_eq!(id("mac.com"), Some("icloud"));
        assert_eq!(id("yahoo.com.mx"), Some("yahoo"));
        assert_eq!(id("aol.com"), Some("aol"));
        assert_eq!(id("gmx.de"), Some("gmx.net"));
        assert_eq!(id("pm.me"), Some("proton"));
        assert_eq!(id("fastmail.com"), Some("fastmail"));
        assert_eq!(id("llaguno.com"), None);
        // Un subdominio propio que empieza igual no es Microsoft.
        assert_eq!(id("outlook.acme-corp.com"), None);
        assert_eq!(id("live.example.org"), None);
    }

    #[test]
    fn known_mx() {
        assert_eq!(by_mx("aspmx.l.google.com").map(|p| p.id), Some("gmail"));
        assert_eq!(by_mx("acme-com.mail.protection.outlook.com").map(|p| p.id), Some("microsoft365"));
        assert_eq!(by_mx("mx01.mail.icloud.com").map(|p| p.id), Some("icloud"));
        assert_eq!(by_mx("in1-smtp.messagingengine.com").map(|p| p.id), Some("fastmail"));
        assert!(by_mx("mx.example.com").is_none());
        assert!(by_mx("fakegoogle.com").is_none());
    }

    #[test]
    fn hosting_by_mx_fills_the_domain() {
        let e = EmailAddress::parse("ellaguno@confiabilidad.com.mx").unwrap();
        let h = hosting_by_mx("mx1.carrierzone.com").unwrap();
        let c = h.candidate(&e);
        // Regresión (dueño): `mail.confiabilidad.com.mx` no existe en el DNS.
        assert_eq!(c.incoming.host, "mail.carrierzone.com");
        assert_eq!(c.incoming.port, 993);
        assert_eq!(c.incoming.username, "ellaguno@confiabilidad.com.mx");
        assert_eq!(c.outgoing.as_ref().map(|o| (o.host.as_str(), o.port)), Some(("mail.carrierzone.com", 587)));
        assert_eq!(c.confidence(), crate::Confidence::Low);
        assert!(hosting_by_mx("mx.example.com").is_none());
        assert!(hosting_by_mx("notcarrierzone.com").is_none());
        for h in HOSTINGS {
            for x in [h.imap, h.smtp] {
                assert!(crate::xml::valid_host(&x.host.replace("{domain}", "example.com")), "{}", x.host);
            }
        }
    }

    #[test]
    fn gmail_candidates_use_oauth() {
        let e = EmailAddress::parse("ana@gmail.com").unwrap();
        let c = by_domain("gmail.com").unwrap().candidates(&e, Source::KnownProvider, 100);
        assert_eq!(c.len(), 2);
        let first = c.first().unwrap();
        assert_eq!(first.incoming.protocol, Protocol::Imap);
        assert_eq!(first.incoming.auth, GOOGLE);
        assert_eq!(first.incoming.username, "ana@gmail.com");
        assert_eq!(first.kind, AccountKind::Gmail);
        assert!(c.get(1).unwrap().score < first.score);
    }

    #[test]
    fn icloud_has_app_password_note() {
        let p = by_domain("icloud.com").unwrap();
        assert_eq!(p.auth, AuthMethod::AppPassword);
        assert!(p.notes().iter().any(|n| n.es.contains("contraseña específica de app")));
    }

    #[test]
    fn table_is_consistent() {
        for p in PROVIDERS {
            assert!(p.imap.is_some() || p.pop3.is_some(), "{}", p.id);
            for h in [p.imap, p.pop3, p.smtp].into_iter().flatten() {
                assert!(crate::xml::valid_host(h.host), "{}", h.host);
                assert_ne!(h.port, 0);
            }
            for u in [p.caldav, p.carddav].into_iter().flatten() {
                assert!(u.starts_with("https://"), "{u}");
            }
        }
    }
}
