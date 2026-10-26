//! Pruebas del descubrimiento completo con red falsa (sin conexión).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use super::*;
use crate::net::{HttpResponse, MxRecord, SrvRecord};

#[derive(Default)]
struct FakeHttp {
    pages: HashMap<String, HttpResponse>,
    slow: Vec<String>,
    calls: Mutex<Vec<String>>,
}

impl FakeHttp {
    fn page(mut self, url: &str, body: &str) -> Self {
        self.pages.insert(url.into(), HttpResponse { status: 200, location: None, body: body.into() });
        self
    }
    fn redirect(mut self, url: &str, to: &str) -> Self {
        self.pages.insert(url.into(), HttpResponse { status: 302, location: Some(to.into()), body: String::new() });
        self
    }
    fn slow(mut self, url: &str) -> Self {
        self.slow.push(url.into());
        self
    }
    fn calls(&self) -> usize {
        self.calls.lock().map(|c| c.len()).unwrap_or(0)
    }
}

impl Http for FakeHttp {
    async fn get(&self, url: &str) -> Result<HttpResponse, NetError> {
        if let Ok(mut c) = self.calls.lock() {
            c.push(url.to_string());
        }
        if self.slow.iter().any(|s| s == url) {
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
        self.pages.get(url).cloned().ok_or(NetError::NotFound)
    }
}

#[derive(Default)]
struct FakeDns {
    mx: HashMap<String, Vec<MxRecord>>,
    srv: HashMap<String, Vec<SrvRecord>>,
    txt: HashMap<String, Vec<String>>,
    hosts: Vec<String>,
}

impl FakeDns {
    fn mx(mut self, domain: &str, hosts: &[(u16, &str)]) -> Self {
        self.mx.insert(
            domain.into(),
            hosts.iter().map(|(p, h)| MxRecord { preference: *p, exchange: (*h).into() }).collect(),
        );
        self
    }
    fn srv(mut self, name: &str, target: &str, port: u16) -> Self {
        self.srv.insert(name.into(), vec![SrvRecord { priority: 0, weight: 1, port, target: target.into() }]);
        self
    }
    fn txt(mut self, name: &str, value: &str) -> Self {
        self.txt.insert(name.into(), vec![value.into()]);
        self
    }
    fn host(mut self, h: &str) -> Self {
        self.hosts.push(h.into());
        self
    }
}

impl Dns for FakeDns {
    async fn mx(&self, domain: &str) -> Result<Vec<MxRecord>, NetError> {
        self.mx.get(domain).cloned().ok_or(NetError::NotFound)
    }
    async fn srv(&self, name: &str) -> Result<Vec<SrvRecord>, NetError> {
        self.srv.get(name).cloned().ok_or(NetError::NotFound)
    }
    async fn txt(&self, name: &str) -> Result<Vec<String>, NetError> {
        self.txt.get(name).cloned().ok_or(NetError::NotFound)
    }
    async fn resolves(&self, host: &str) -> bool {
        self.hosts.iter().any(|h| h == host)
    }
}

const EXAMPLE_XML: &str = include_str!("../tests/fixtures/autoconfig-example.xml");
const FASTMAIL_XML: &str = include_str!("../tests/fixtures/ispdb-fastmail.xml");

fn run<H: Http + Sync, D: Dns + Sync>(d: &Discoverer<H, D>, email: &str) -> Result<Discovery, Error> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    rt.block_on(d.discover(email))
}

#[test]
fn known_provider_needs_no_network() {
    let d = Discoverer::new(FakeHttp::default(), FakeDns::default());
    let r = run(&d, "ana@gmail.com").unwrap();
    assert_eq!(d.http.calls(), 0);
    assert_eq!(r.kind, AccountKind::Gmail);
    let best = r.best().unwrap();
    assert_eq!(best.source, Source::KnownProvider);
    assert_eq!(best.confidence(), Confidence::High);
    assert_eq!(best.incoming.host, "imap.gmail.com");
    assert_eq!(best.incoming.auth, AuthMethod::OAuth2 { provider: OAuthProvider::Google });
    assert!(r.caldav.is_some());

    let r = run(&d, "ana@icloud.com").unwrap();
    assert_eq!(r.kind, AccountKind::ICloud);
    assert!(r.notes.iter().any(|n| n.es.contains("iCloud requiere una contraseña específica de app")));

    let r = run(&d, "ana@hotmail.es").unwrap();
    assert_eq!(r.kind, AccountKind::Microsoft365);
}

#[test]
fn invalid_email_is_an_error() {
    let d = Discoverer::new(FakeHttp::default(), FakeDns::default());
    assert!(matches!(run(&d, "no-es-correo"), Err(Error::InvalidEmail(_))));
}

#[test]
fn domain_autoconfig_wins_and_ranks_above_guess() {
    let http = FakeHttp::default()
        .page("https://autoconfig.example.com/mail/config-v1.1.xml?emailaddress=ana.perez%40example.com", EXAMPLE_XML);
    let dns = FakeDns::default().host("imap.example.com").host("smtp.example.com");
    let d = Discoverer::new(http, dns);
    let r = run(&d, "ana.perez@example.com").unwrap();
    let best = r.best().unwrap();
    assert_eq!(best.source, Source::DomainAutoconfig);
    assert_eq!(best.score, 90);
    assert_eq!(best.incoming.host, "mail.example.com");
    assert_eq!(best.incoming.username, "ana.perez");
    assert_eq!(best.provider.as_deref(), Some("Example & Co"));
    // POP3 del mismo archivo, debajo; la adivinanza al final.
    let sources: Vec<Source> = r.candidates.iter().map(|c| c.source).collect();
    assert_eq!(sources.last(), Some(&Source::Guess));
    assert!(r.candidates.windows(2).all(|w| w.first().map(|c| c.score) >= w.get(1).map(|c| c.score)));
    // Con un candidato bueno, la nota de «suposición» se quita.
    assert!(!r.notes.iter().any(|n| n.es.contains("suposición")));
    // Dominio propio sin DAV conocido: well-known como pista de baja confianza.
    assert_eq!(r.caldav.as_ref().map(|h| h.source), Some(Source::Guess));
}

#[test]
fn well_known_autoconfig_follows_https_redirects_only() {
    let http = FakeHttp::default()
        .redirect(
            "https://example.com/.well-known/autoconfig/mail/config-v1.1.xml",
            "https://cdn.example.net/config.xml",
        )
        .page("https://cdn.example.net/config.xml", EXAMPLE_XML)
        .redirect("https://autoconfig.thunderbird.net/v1.1/example.com", "http://insecure.example.com/x.xml")
        .page("http://insecure.example.com/x.xml", FASTMAIL_XML);
    let d = Discoverer::new(http, FakeDns::default());
    let r = run(&d, "ana@example.com").unwrap();
    assert_eq!(r.best().map(|c| c.source), Some(Source::DomainAutoconfig));
    assert!(!r.candidates.iter().any(|c| c.incoming.host == "imap.fastmail.com"));
}

#[test]
fn mx_google_means_workspace() {
    let dns = FakeDns::default().mx("acme.mx", &[(5, "alt1.aspmx.l.google.com"), (1, "aspmx.l.google.com")]);
    let d = Discoverer::new(FakeHttp::default(), dns);
    let r = run(&d, "ana@acme.mx").unwrap();
    assert_eq!(r.kind, AccountKind::Gmail);
    let best = r.best().unwrap();
    assert_eq!(best.source, Source::Mx);
    assert_eq!(best.incoming.username, "ana@acme.mx");
    assert!(r.notes.iter().any(|n| n.es.contains("Google Workspace")));
    assert_eq!(best.provider.as_deref(), Some("Google Workspace"));
}

#[test]
fn mx_microsoft_and_autodiscover_agree_on_m365() {
    let dns = FakeDns::default().mx("contoso.com", &[(0, "contoso-com.mail.protection.outlook.com")]);
    let http = FakeHttp::default().redirect(
        "https://autodiscover-s.outlook.com/autodiscover/autodiscover.json?Email=autodiscover%40contoso.com&Protocol=AutodiscoverV1",
        "https://outlook.office365.com/autodiscover/autodiscover.json/v1.0/autodiscover@contoso.com?Protocol=AutodiscoverV1",
    );
    let d = Discoverer::new(http, dns);
    let r = run(&d, "ana@contoso.com").unwrap();
    assert_eq!(r.kind, AccountKind::Microsoft365);
    let best = r.best().unwrap();
    assert_eq!(best.source, Source::Mx);
    assert_eq!(best.incoming.host, "outlook.office365.com");
    assert_eq!(best.outgoing.as_ref().map(|s| s.host.as_str()), Some("smtp.office365.com"));
    // Deduplicado: el de Autodiscover (mismo servidor) no se repite.
    assert_eq!(r.candidates.iter().filter(|c| c.incoming.protocol == Protocol::Imap).count(), 1);
}

#[test]
fn autodiscover_alone_detects_m365_behind_a_filter() {
    let dns = FakeDns::default().mx("contoso.com", &[(0, "mx.spamfilter.example")]);
    let http = FakeHttp::default().page(
        "https://autodiscover-s.outlook.com/autodiscover/autodiscover.json?Email=autodiscover%40contoso.com&Protocol=AutodiscoverV1",
        r#"{"Protocol":"AutodiscoverV1","Url":"https://outlook.office365.com/autodiscover/autodiscover.xml"}"#,
    );
    let d = Discoverer::new(http, dns);
    let r = run(&d, "ana@contoso.com").unwrap();
    assert_eq!(r.best().map(|c| c.source), Some(Source::Autodiscover));
    assert_eq!(r.kind, AccountKind::Microsoft365);
}

#[test]
fn autodiscover_redirect_to_own_domain_is_not_m365() {
    let http = FakeHttp::default().redirect(
        "https://autodiscover-s.outlook.com/autodiscover/autodiscover.json?Email=autodiscover%40llaguno.com&Protocol=AutodiscoverV1",
        "https://autodiscover.llaguno.com/autodiscover/autodiscover.json/v1.0/x?Protocol=AutodiscoverV1",
    );
    let d = Discoverer::new(http, FakeDns::default());
    let r = run(&d, "ana@llaguno.com").unwrap();
    assert!(r.candidates.is_empty());
    assert!(r.notes.iter().any(|n| n.es.contains("escribe los servidores a mano")));
}

#[test]
fn mx_base_domain_queries_ispdb() {
    let dns = FakeDns::default().mx("smallbiz.org", &[(10, "mx1.mail.hostingco.net")]);
    let http = FakeHttp::default().page("https://autoconfig.thunderbird.net/v1.1/hostingco.net", FASTMAIL_XML);
    let d = Discoverer::new(http, dns);
    let r = run(&d, "ana@smallbiz.org").unwrap();
    let best = r.best().unwrap();
    assert_eq!(best.source, Source::MxIspdb);
    assert_eq!(best.score, 70);
    assert_eq!(best.incoming.username, "ana@smallbiz.org");
}

#[test]
fn srv_records_rfc6186_and_rfc6764() {
    let dns = FakeDns::default()
        .srv("_imaps._tcp.example.org", "mail.example.org", 993)
        .srv("_submission._tcp.example.org", "smtp.example.org", 587)
        .srv("_pop3s._tcp.example.org", ".", 0)
        .srv("_caldavs._tcp.example.org", "dav.example.org", 443)
        .txt("_caldavs._tcp.example.org", "path=/cal/")
        .srv("_carddavs._tcp.example.org", "dav.example.org", 8443);
    let d = Discoverer::new(FakeHttp::default(), dns);
    let r = run(&d, "ana@example.org").unwrap();
    assert_eq!(r.candidates.len(), 1);
    let best = r.best().unwrap();
    assert_eq!(best.source, Source::Srv);
    assert_eq!(best.confidence(), Confidence::Medium);
    assert_eq!((best.incoming.host.as_str(), best.incoming.security), ("mail.example.org", Security::Tls));
    let out = best.outgoing.as_ref().unwrap();
    assert_eq!((out.host.as_str(), out.port, out.security), ("smtp.example.org", 587, Security::StartTls));
    assert_eq!(r.caldav.as_ref().map(|h| h.url.as_str()), Some("https://dav.example.org/cal/"));
    assert_eq!(r.carddav.as_ref().map(|h| h.url.as_str()), Some("https://dav.example.org:8443/.well-known/carddav"));
}

#[test]
fn guess_is_low_confidence_only() {
    let dns = FakeDns::default().host("mail.tiny.net");
    let d = Discoverer::new(FakeHttp::default(), dns);
    let r = run(&d, "ana@tiny.net").unwrap();
    let best = r.best().unwrap();
    assert_eq!(best.source, Source::Guess);
    assert_eq!(best.confidence(), Confidence::Low);
    assert_eq!(best.incoming.host, "mail.tiny.net");
    assert_eq!(best.outgoing.as_ref().map(|s| s.host.as_str()), Some("mail.tiny.net"));
    assert!(r.notes.iter().any(|n| n.es.contains("suposición")));
}

#[test]
fn overall_timeout_keeps_partial_results() {
    let http = FakeHttp::default().slow("https://autoconfig.thunderbird.net/v1.1/slow.org");
    let dns = FakeDns::default().srv("_imaps._tcp.slow.org", "imap.slow.org", 993);
    let d = Discoverer::new(http, dns).with_timeout(Duration::from_millis(300));
    let started = std::time::Instant::now();
    let r = run(&d, "ana@slow.org").unwrap();
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(r.timed_out);
    assert_eq!(r.best().map(|c| c.source), Some(Source::Srv));
    assert!(r.notes.iter().any(|n| n.es.contains("no respondieron a tiempo")));
}

#[test]
fn plain_text_server_gets_a_warning() {
    let xml = r#"<clientConfig version="1.1"><emailProvider id="old.net">
        <incomingServer type="imap"><hostname>mail.old.net</hostname><port>143</port><socketType>plain</socketType></incomingServer>
        <outgoingServer type="smtp"><hostname>mail.old.net</hostname><port>25</port><socketType>plain</socketType></outgoingServer>
    </emailProvider></clientConfig>"#;
    let http = FakeHttp::default().page("https://old.net/.well-known/autoconfig/mail/config-v1.1.xml", xml);
    let d = Discoverer::new(http, FakeDns::default());
    let r = run(&d, "ana@old.net").unwrap();
    let best = r.best().unwrap();
    assert_eq!(best.score, 50);
    assert!(r.notes.iter().any(|n| n.es.contains("no cifra")));
}

#[test]
fn base_domain_and_url_host() {
    assert_eq!(base_domain("mx1.mail.hostingco.net").as_deref(), Some("hostingco.net"));
    assert_eq!(base_domain("mx.example.com.mx").as_deref(), Some("example.com.mx"));
    assert_eq!(base_domain("mx.example.co.uk").as_deref(), Some("example.co.uk"));
    assert_eq!(base_domain("example.de").as_deref(), Some("example.de"));
    assert_eq!(base_domain("localhost"), None);
    assert_eq!(url_host("https://outlook.office365.com/x").as_deref(), Some("outlook.office365.com"));
    assert_eq!(url_host("https://user@Host.Example.com:443?x").as_deref(), Some("host.example.com"));
    assert_eq!(url_host("http://outlook.office365.com/"), None);
    assert_eq!(url_host("https://bad host/"), None);
}
