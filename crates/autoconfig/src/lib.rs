//! Descubrimiento de servidores: ISPDB, Autodiscover, SRV, MX (capa L2).
//!
//! A partir de un correo propone servidores como lo hace Thunderbird, en este orden de confianza:
//!
//! 1. Tabla de proveedores conocidos (Gmail, Microsoft, iCloud, Yahoo, AOL, GMX, Zoho, Fastmail,
//!    Proton Bridge…). Si el dominio está en la tabla no se toca la red.
//! 2. Autoconfig del propio dominio (`https://autoconfig.<dominio>/mail/config-v1.1.xml` y
//!    `https://<dominio>/.well-known/autoconfig/mail/config-v1.1.xml`).
//! 3. ISPDB de Thunderbird (`https://autoconfig.thunderbird.net/v1.1/<dominio>`).
//! 4. MX: si apunta a Google o Microsoft (u otro proveedor de la tabla) se clasifica como Google
//!    Workspace / Microsoft 365; si no, se consulta la ISPDB con el dominio base del MX.
//! 5. Autodiscover v2 de Microsoft (solo clasifica: ¿el dominio es de un inquilino de M365?) y el
//!    del propio dominio para Exchange Server local ([`exchange`]): v2 da la URL de EWS; POX que
//!    pide NTLM/Negotiate dice «hay un Exchange» (la URL se pide luego con la contraseña).
//! 6. SRV de RFC 6186 (`_imaps._tcp`, `_submission._tcp`, `_pop3s._tcp`…) y RFC 6764
//!    (`_caldavs._tcp`, `_carddavs._tcp`).
//! 7. Adivinanza (`imap.`, `mail.`, `smtp.`): solo con confianza baja.
//!
//! Las fuentes de red corren a la vez con un tiempo total (8 s por defecto); lo que no respondió a
//! tiempo se descarta y lo demás se usa. Toda la red pasa por los rasgos [`net::Http`] y
//! [`net::Dns`], así las pruebas corren sin red.
//!
//! Privacidad: el autoconfig del dominio recibe la dirección completa (es el dueño del dominio);
//! la ISPDB solo el dominio; a Autodiscover se le manda `autodiscover@<dominio>`, no la dirección
//! del usuario.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

pub mod email;
pub mod exchange;
pub mod net;
pub mod providers;
pub mod srv;
pub mod xml;

use std::time::Duration;

use iuredesk_model::account::{AccountKind, AuthMethod, OAuthProvider, Protocol, Security, ServerConfig};
use serde::{Deserialize, Serialize};

pub use email::{EmailAddress, EmailError};
pub use exchange::ExchangeHint;
use net::{Dns, HickoryDns, Http, NetError, ReqwestHttp};

/// Tiempo total por defecto del descubrimiento.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(8);
/// Tope de candidatos que se devuelven.
const MAX_CANDIDATES: usize = 12;
/// Tope de redirecciones que se siguen (solo `https`).
const MAX_REDIRECTS: usize = 3;

const ISPDB: &str = "https://autoconfig.thunderbird.net/v1.1/";
const AUTODISCOVER: &str = "https://autodiscover-s.outlook.com/autodiscover/autodiscover.json";

/// De dónde salió un candidato.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    KnownProvider,
    DomainAutoconfig,
    Ispdb,
    /// El MX apunta a un proveedor de la tabla.
    Mx,
    /// ISPDB consultada con el dominio base del MX.
    MxIspdb,
    Autodiscover,
    Srv,
    Guess,
    /// Autodiscover del dominio (Exchange Server local).
    ExchangeAutodiscover,
}

/// Confianza, derivada de la puntuación.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Low,
    Medium,
    High,
}

/// Un aviso para el usuario en los dos idiomas.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    pub en: String,
    pub es: String,
}

impl Note {
    pub fn new(en: &str, es: &str) -> Self {
        Self { en: en.to_string(), es: es.to_string() }
    }

    /// El texto en español (`true`) o en inglés.
    pub fn text(&self, spanish: bool) -> &str {
        if spanish { &self.es } else { &self.en }
    }
}

/// Una configuración propuesta.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub kind: AccountKind,
    /// Nombre del proveedor para mostrar («Gmail», «Fastmail»), si se conoce.
    pub provider: Option<String>,
    pub incoming: ServerConfig,
    pub outgoing: Option<ServerConfig>,
    pub source: Source,
    /// 0–100; ordena los candidatos.
    pub score: u8,
    pub caldav_url: Option<String>,
    pub carddav_url: Option<String>,
}

impl Candidate {
    pub fn confidence(&self) -> Confidence {
        match self.score {
            80.. => Confidence::High,
            50..=79 => Confidence::Medium,
            _ => Confidence::Low,
        }
    }
}

/// URL de CalDAV/CardDAV propuesta y de dónde salió.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DavHint {
    pub url: String,
    pub source: Source,
}

/// Resultado del descubrimiento.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Discovery {
    pub email: String,
    pub domain: String,
    /// Familia más probable (la del mejor candidato).
    pub kind: AccountKind,
    /// De mejor a peor.
    pub candidates: Vec<Candidate>,
    pub notes: Vec<Note>,
    pub caldav: Option<DavHint>,
    pub carddav: Option<DavHint>,
    /// Alguna fuente no respondió dentro del tiempo total.
    pub timed_out: bool,
    /// Exchange Server local encontrado por Autodiscover (aunque falte la URL de EWS).
    #[serde(default)]
    pub exchange: Option<ExchangeHint>,
}

impl Discovery {
    pub fn best(&self) -> Option<&Candidate> {
        self.candidates.first()
    }
}

/// Error del descubrimiento. Los fallos de red de cada fuente no son error: solo restan candidatos.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("correo no válido: {0}")]
    InvalidEmail(#[from] EmailError),
    #[error("no se pudo preparar la red: {0}")]
    Network(#[from] NetError),
}

/// Lo que aporta una fuente.
#[derive(Debug, Default)]
struct Partial {
    candidates: Vec<Candidate>,
    notes: Vec<Note>,
    caldav: Option<DavHint>,
    carddav: Option<DavHint>,
    exchange: Option<ExchangeHint>,
}

/// Descubre con la red real (reqwest + hickory) y el tiempo total por defecto.
pub async fn discover(email: &str) -> Result<Discovery, Error> {
    Discoverer::system()?.discover(email).await
}

/// Motor de descubrimiento sobre un cliente HTTP y un resolutor DNS cualesquiera.
pub struct Discoverer<H, D> {
    http: H,
    dns: D,
    timeout: Duration,
}

impl Discoverer<ReqwestHttp, HickoryDns> {
    /// Con la red del sistema.
    pub fn system() -> Result<Self, NetError> {
        Ok(Self::new(ReqwestHttp::new()?, HickoryDns::new()?))
    }
}

impl<H: Http + Sync, D: Dns + Sync> Discoverer<H, D> {
    pub fn new(http: H, dns: D) -> Self {
        Self { http, dns, timeout: DEFAULT_TIMEOUT }
    }

    /// Cambia el tiempo total.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Descubre la configuración de `email`.
    pub async fn discover(&self, email: &str) -> Result<Discovery, Error> {
        let email = EmailAddress::parse(email)?;
        let mut parts: Vec<Partial> = Vec::new();
        let mut timed_out = false;

        if let Some(p) = providers::by_domain(&email.domain) {
            // Proveedor conocido: respuesta inmediata, sin red.
            parts.push(Partial {
                candidates: p.candidates(&email, Source::KnownProvider, 100),
                notes: p.notes(),
                ..Default::default()
            });
        } else {
            let deadline = tokio::time::Instant::now() + self.timeout;
            use tokio::time::timeout_at as at;
            let results = tokio::join!(
                at(deadline, self.domain_autoconfig(&email)),
                at(deadline, self.ispdb(&email.domain, &email, Source::Ispdb, 85)),
                at(deadline, self.mx(&email)),
                at(deadline, self.autodiscover(&email)),
                at(deadline, self.srv_mail(&email)),
                at(deadline, self.srv_dav(&email)),
                at(deadline, self.guess(&email)),
                at(deadline, self.exchange(&email)),
            );
            let (a, b, c, d, e, f, g, h) = results;
            for r in [a, b, c, d, e, f, g, h] {
                match r {
                    Ok(p) => parts.push(p),
                    Err(_) => timed_out = true,
                }
            }
        }
        Ok(finish(&email, parts, timed_out))
    }

    /// GET que sigue hasta [`MAX_REDIRECTS`] redirecciones `https` y devuelve el cuerpo de un 200.
    async fn fetch(&self, url: &str) -> Option<String> {
        let mut url = url.to_string();
        for _ in 0..=MAX_REDIRECTS {
            let resp = self.http.get(&url).await.ok()?;
            match resp.status {
                200 => return Some(resp.body),
                301 | 302 | 303 | 307 | 308 => {
                    url = resp.location.filter(|l| l.starts_with("https://"))?;
                }
                _ => return None,
            }
        }
        None
    }

    async fn domain_autoconfig(&self, email: &EmailAddress) -> Partial {
        let d = &email.domain;
        let a = format!("https://autoconfig.{d}/mail/config-v1.1.xml?emailaddress={}", email.url_encoded());
        let b = format!("https://{d}/.well-known/autoconfig/mail/config-v1.1.xml");
        let (ra, rb) = tokio::join!(self.fetch(&a), self.fetch(&b));
        for body in [ra, rb].into_iter().flatten() {
            if let Ok(cfg) = xml::parse_config(&body, email) {
                return from_config(&cfg, Source::DomainAutoconfig, 90);
            }
        }
        Partial::default()
    }

    async fn ispdb(&self, domain: &str, email: &EmailAddress, source: Source, score: u8) -> Partial {
        let Some(body) = self.fetch(&format!("{ISPDB}{}", email::url_encode(domain))).await else {
            return Partial::default();
        };
        match xml::parse_config(&body, email) {
            Ok(cfg) => from_config(&cfg, source, score),
            Err(_) => Partial::default(),
        }
    }

    async fn mx(&self, email: &EmailAddress) -> Partial {
        let Ok(mut records) = self.dns.mx(&email.domain).await else {
            return Partial::default();
        };
        records.sort_by_key(|r| r.preference);
        let Some(first) = records.into_iter().map(|r| r.exchange).find(|h| xml::valid_host(h)) else {
            return Partial::default();
        };
        if let Some(p) = providers::by_mx(&first) {
            // Un dominio propio en Google es Google Workspace, no «Gmail».
            let name = if p.id == "gmail" { "Google Workspace" } else { p.name };
            let mut notes = vec![Note {
                en: format!("Your domain's mail is hosted by {name}."),
                es: format!("El correo de tu dominio lo atiende {name}."),
            }];
            notes.extend(p.notes());
            let mut candidates = p.candidates(email, Source::Mx, 80);
            for c in &mut candidates {
                c.provider = Some(name.to_string());
            }
            return Partial { candidates, notes, ..Default::default() };
        }
        let mut found = match base_domain(&first) {
            Some(base) if base != email.domain => self.ispdb(&base, email, Source::MxIspdb, 70).await,
            _ => Partial::default(),
        };
        // Sin ISPDB: un hospedaje conocido por su MX deja al menos una propuesta (confianza baja).
        if found.candidates.is_empty()
            && let Some(h) = providers::hosting_by_mx(&first)
        {
            found.candidates.push(h.candidate(email));
            found.notes.push(h.note());
        }
        found
    }

    async fn autodiscover(&self, email: &EmailAddress) -> Partial {
        let probe = email::url_encode(&format!("autodiscover@{}", email.domain));
        let url = format!("{AUTODISCOVER}?Email={probe}&Protocol=AutodiscoverV1");
        let Ok(resp) = self.http.get(&url).await else {
            return Partial::default();
        };
        let target = match resp.status {
            200 => serde_json::from_str::<serde_json::Value>(&resp.body)
                .ok()
                .and_then(|v| v.get("Url").and_then(|u| u.as_str()).map(str::to_string)),
            301 | 302 | 307 | 308 => resp.location,
            _ => None,
        };
        let is_m365 = target.as_deref().and_then(url_host).is_some_and(|h| xml::host_matches(&h, "office365.com"));
        match (is_m365, providers::PROVIDERS.iter().find(|p| p.id == "microsoft365")) {
            (true, Some(p)) => Partial {
                candidates: p.candidates(email, Source::Autodiscover, 75),
                notes: p.notes(),
                ..Default::default()
            },
            _ => Partial::default(),
        }
    }

    /// Exchange Server local: Autodiscover v2 y POX en `autodiscover.<dominio>`, `<dominio>` y el
    /// destino SRV `_autodiscover._tcp` (solo si queda dentro del dominio).
    async fn exchange(&self, email: &EmailAddress) -> Partial {
        let d = &email.domain;
        let srv = self
            .dns
            .srv(&format!("_autodiscover._tcp.{d}"))
            .await
            .ok()
            .and_then(|v| srv::pick(&v))
            .filter(|(h, _)| exchange::within(h, d))
            .map(|(h, p)| exchange::origin(&h, p));
        let (o1, o2) = (exchange::origin(&format!("autodiscover.{d}"), 443), exchange::origin(d, 443));
        let o3 = srv.filter(|o| *o != o1 && *o != o2);
        let (h1, h2, h3) = tokio::join!(self.exchange_at(&o1, email), self.exchange_at(&o2, email), async {
            match &o3 {
                Some(o) => self.exchange_at(o, email).await,
                None => ExchangeHint::default(),
            }
        });
        let mut hint = h1;
        hint.merge(h2);
        hint.merge(h3);
        // Buzón en Microsoft 365 (Exchange híbrido): se entra como Microsoft 365.
        if hint.ews_url.as_deref().and_then(url_host).is_some_and(|h| exchange::is_microsoft_cloud(&h))
            && let Some(p) = providers::PROVIDERS.iter().find(|p| p.id == "microsoft365")
        {
            return Partial {
                candidates: p.candidates(email, Source::ExchangeAutodiscover, 80),
                notes: p.notes(),
                ..Default::default()
            };
        }
        if hint.is_empty() {
            return Partial::default();
        }
        let candidates = hint
            .ews_url
            .iter()
            .map(|url| {
                let server = ServerConfig {
                    protocol: Protocol::Ews,
                    host: url.clone(),
                    port: 443,
                    security: Security::Tls,
                    auth: AuthMethod::Password,
                    username: email.address.clone(),
                };
                Candidate {
                    kind: AccountKind::Exchange,
                    provider: None,
                    incoming: server.clone(),
                    outgoing: Some(server),
                    source: Source::ExchangeAutodiscover,
                    score: 88,
                    caldav_url: None,
                    carddav_url: None,
                }
            })
            .collect();
        Partial {
            candidates,
            notes: vec![Note::new(
                "Your organization uses Exchange: sign in with your work email and password.",
                "Tu organización usa Exchange: entra con el correo y la contraseña del trabajo.",
            )],
            exchange: Some(hint),
            ..Default::default()
        }
    }

    /// Autodiscover v2 y POX en un servidor.
    async fn exchange_at(&self, origin: &str, email: &EmailAddress) -> ExchangeHint {
        let pox = exchange::pox_url(origin);
        let v2_url = exchange::v2_url(origin, &email.address);
        let (v2, probe) = tokio::join!(self.fetch(&v2_url), self.http.get(&pox));
        ExchangeHint {
            ews_url: v2.as_deref().and_then(exchange::parse_v2),
            autodiscover: match probe {
                Ok(r) if exchange::looks_like_exchange(r.status, &r.auth_schemes) => vec![pox],
                _ => Vec::new(),
            },
        }
    }

    async fn srv_mail(&self, email: &EmailAddress) -> Partial {
        let d = &email.domain;
        let q = |svc: &str| format!("_{svc}._tcp.{d}");
        let names = [q("imaps"), q("imap"), q("pop3s"), q("pop3"), q("submissions"), q("submission")];
        let [n1, n2, n3, n4, n5, n6] = &names;
        let (imaps, imap, pop3s, pop3, subs, sub) = tokio::join!(
            self.dns.srv(n1),
            self.dns.srv(n2),
            self.dns.srv(n3),
            self.dns.srv(n4),
            self.dns.srv(n5),
            self.dns.srv(n6),
        );
        let found = |r: Result<Vec<net::SrvRecord>, NetError>| r.ok().and_then(|v| srv::pick(&v));
        let server = |protocol, (host, port): (String, u16), security| ServerConfig {
            protocol,
            host,
            port,
            security,
            auth: AuthMethod::Password,
            username: email.address.clone(),
        };
        let outgoing = found(subs)
            .map(|hp| server(Protocol::Smtp, hp, Security::Tls))
            .or_else(|| found(sub).map(|hp| server(Protocol::Smtp, hp, Security::StartTls)));
        let mut candidates = Vec::new();
        for (r, protocol, security, score) in [
            (imaps, Protocol::Imap, Security::Tls, 65u8),
            (imap, Protocol::Imap, Security::StartTls, 63),
            (pop3s, Protocol::Pop3, Security::Tls, 55),
            (pop3, Protocol::Pop3, Security::StartTls, 53),
        ] {
            if let Some(hp) = found(r) {
                candidates.push(Candidate {
                    kind: AccountKind::Generic,
                    provider: None,
                    incoming: server(protocol, hp, security),
                    outgoing: outgoing.clone(),
                    source: Source::Srv,
                    score,
                    caldav_url: None,
                    carddav_url: None,
                });
            }
        }
        Partial { candidates, ..Default::default() }
    }

    async fn srv_dav(&self, email: &EmailAddress) -> Partial {
        let d = &email.domain;
        let (cal_name, card_name) = (format!("_caldavs._tcp.{d}"), format!("_carddavs._tcp.{d}"));
        let (cal, cal_txt, card, card_txt) = tokio::join!(
            self.dns.srv(&cal_name),
            self.dns.txt(&cal_name),
            self.dns.srv(&card_name),
            self.dns.txt(&card_name),
        );
        let hint = |srv: Result<Vec<net::SrvRecord>, NetError>, txt: Result<Vec<String>, NetError>, wk: &str| {
            let (host, port) = srv.ok().and_then(|v| srv::pick(&v))?;
            let path = txt.ok().and_then(|t| srv::txt_path(&t)).unwrap_or_else(|| wk.to_string());
            Some(DavHint { url: srv::dav_url(&host, port, Some(&path)), source: Source::Srv })
        };
        Partial {
            caldav: hint(cal, cal_txt, "/.well-known/caldav"),
            carddav: hint(card, card_txt, "/.well-known/carddav"),
            ..Default::default()
        }
    }

    async fn guess(&self, email: &EmailAddress) -> Partial {
        let d = &email.domain;
        let (imap_h, mail_h, smtp_h) = (format!("imap.{d}"), format!("mail.{d}"), format!("smtp.{d}"));
        let (imap_ok, mail_ok, smtp_ok) =
            tokio::join!(self.dns.resolves(&imap_h), self.dns.resolves(&mail_h), self.dns.resolves(&smtp_h));
        let incoming = if imap_ok {
            imap_h
        } else if mail_ok {
            mail_h.clone()
        } else {
            return Partial::default();
        };
        let outgoing = if smtp_ok {
            Some(smtp_h)
        } else if mail_ok {
            Some(mail_h)
        } else {
            None
        };
        let server = |protocol, host: String, security| ServerConfig {
            protocol,
            port: Protocol::default_port(protocol, security),
            host,
            security,
            auth: AuthMethod::Password,
            username: email.address.clone(),
        };
        Partial {
            candidates: vec![Candidate {
                kind: AccountKind::Generic,
                provider: None,
                incoming: server(Protocol::Imap, incoming, Security::Tls),
                outgoing: outgoing.map(|h| server(Protocol::Smtp, h, Security::StartTls)),
                source: Source::Guess,
                score: 25,
                caldav_url: None,
                carddav_url: None,
            }],
            notes: vec![Note::new(
                "These servers are a guess: test the connection before saving.",
                "Estos servidores son una suposición: prueba la conexión antes de guardar.",
            )],
            ..Default::default()
        }
    }
}

/// Familia según los servidores de un config-v1.1.xml.
fn kind_for(server: &ServerConfig) -> AccountKind {
    match server.auth {
        AuthMethod::OAuth2 { provider: OAuthProvider::Google } => AccountKind::Gmail,
        AuthMethod::OAuth2 { provider: OAuthProvider::Microsoft } => AccountKind::Microsoft365,
        _ if xml::host_matches(&server.host, "me.com") || xml::host_matches(&server.host, "icloud.com") => {
            AccountKind::ICloud
        }
        _ => AccountKind::Generic,
    }
}

/// Candidatos de un config-v1.1.xml: en el orden del archivo, con castigos por POP3 y texto plano.
fn from_config(cfg: &xml::ParsedConfig, source: Source, score: u8) -> Partial {
    let outgoing =
        cfg.outgoing.iter().find(|s| s.security != Security::Plain).or_else(|| cfg.outgoing.first()).cloned();
    let candidates = cfg
        .incoming
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let mut penalty = u8::try_from(i.saturating_mul(2)).unwrap_or(u8::MAX);
            if s.protocol == Protocol::Pop3 {
                penalty = penalty.saturating_add(10);
            }
            if s.security == Security::Plain {
                penalty = penalty.saturating_add(40);
            }
            Candidate {
                kind: kind_for(s),
                provider: cfg.display_name.clone(),
                incoming: s.clone(),
                outgoing: outgoing.clone(),
                source,
                score: score.saturating_sub(penalty),
                caldav_url: None,
                carddav_url: None,
            }
        })
        .collect();
    let mut notes = Vec::new();
    if let Some(doc) = &cfg.documentation {
        notes.push(Note {
            en: format!("Provider instructions: {doc}"),
            es: format!("Instrucciones del proveedor: {doc}"),
        });
    }
    Partial { candidates, notes, ..Default::default() }
}

/// Dominio base de un nombre de host (heurística sin lista de sufijos públicos: `mx1.mail.example.com`
/// → `example.com`; `mx.example.com.mx` → `example.com.mx`).
pub fn base_domain(host: &str) -> Option<String> {
    let labels: Vec<&str> = host.trim_end_matches('.').split('.').filter(|l| !l.is_empty()).collect();
    let n = labels.len();
    if n < 2 {
        return None;
    }
    let last = labels.get(n - 1)?;
    let second = labels.get(n - 2)?;
    let take = if n >= 3
        && last.len() == 2
        && ["com", "net", "org", "co", "gob", "gov", "edu", "ac", "or", "ne"].contains(second)
    {
        3
    } else {
        2
    };
    Some(labels.get(n - take..)?.join(".").to_ascii_lowercase())
}

/// Host de una URL `https://host[:puerto]/…`.
fn url_host(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://")?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = rest.get(..end)?;
    let host = authority.rsplit('@').next()?.split(':').next()?;
    xml::valid_host(host).then(|| host.to_ascii_lowercase())
}

/// Junta, ordena y depura lo que aportaron las fuentes.
fn finish(email: &EmailAddress, parts: Vec<Partial>, timed_out: bool) -> Discovery {
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut notes: Vec<Note> = Vec::new();
    let mut caldav: Option<DavHint> = None;
    let mut carddav: Option<DavHint> = None;
    let mut exchange: Option<ExchangeHint> = None;
    for p in parts {
        if let Some(h) = p.exchange {
            exchange.get_or_insert_with(ExchangeHint::default).merge(h);
        }
        candidates.extend(p.candidates);
        for n in p.notes {
            if !notes.contains(&n) {
                notes.push(n);
            }
        }
        caldav = caldav.or(p.caldav);
        carddav = carddav.or(p.carddav);
    }
    candidates.sort_by(|a, b| b.score.cmp(&a.score).then(a.source.cmp(&b.source)));
    let mut seen = std::collections::HashSet::new();
    candidates
        .retain(|c| seen.insert((c.incoming.protocol, c.incoming.host.clone(), c.incoming.port, c.incoming.security)));
    candidates.truncate(MAX_CANDIDATES);

    // Exchange encontrado por Autodiscover manda salvo que haya algo mejor de verdad (Microsoft
    // 365 por el MX, el autoconfig del dominio): sin URL de EWS todavía, la pide el asistente con
    // la contraseña.
    let mut kind = candidates.first().map(|c| c.kind).unwrap_or_default();
    if exchange.is_some() && candidates.first().is_none_or(|c| c.score < 80) {
        kind = AccountKind::Exchange;
    }
    let exchange = exchange.filter(|_| kind == AccountKind::Exchange);
    // Notas de los candidatos de baja confianza solo si el mejor es de baja confianza.
    let best_low = exchange.is_none() && candidates.first().is_none_or(|c| c.confidence() == Confidence::Low);
    if !best_low {
        notes.retain(|n| !n.es.starts_with("Estos servidores son una suposición"));
    }
    if let Some(best) = candidates.first() {
        let local = matches!(best.incoming.host.as_str(), "127.0.0.1" | "localhost" | "::1");
        if best.incoming.security == Security::Plain && !local {
            notes.push(Note::new(
                "The proposed server does not encrypt the connection; your password would travel in clear text.",
                "El servidor propuesto no cifra la conexión; tu contraseña viajaría sin cifrar.",
            ));
        }
        if caldav.is_none() {
            caldav = best.caldav_url.clone().map(|url| DavHint { url, source: best.source });
        }
        if carddav.is_none() {
            carddav = best.carddav_url.clone().map(|url| DavHint { url, source: best.source });
        }
    } else if exchange.is_none() {
        notes.push(Note::new(
            "We couldn't find the settings automatically; enter the servers by hand (your provider has them).",
            "No encontramos la configuración automáticamente; escribe los servidores a mano (tu proveedor te los da).",
        ));
    }
    if timed_out {
        notes.push(Note::new(
            "Some sources did not answer in time; the results may be incomplete.",
            "Algunas fuentes no respondieron a tiempo; el resultado puede estar incompleto.",
        ));
    }
    // Para un dominio propio sin DAV conocido, el «well-known» de RFC 6764 es la mejor suposición.
    if kind == AccountKind::Generic && !candidates.is_empty() {
        let wk =
            |svc: &str| DavHint { url: format!("https://{}/.well-known/{svc}", email.domain), source: Source::Guess };
        caldav = caldav.or_else(|| Some(wk("caldav")));
        carddav = carddav.or_else(|| Some(wk("carddav")));
    }
    Discovery {
        email: email.address.clone(),
        domain: email.domain.clone(),
        kind,
        candidates,
        notes,
        caldav,
        carddav,
        timed_out,
        exchange,
    }
}

#[cfg(test)]
mod tests;
