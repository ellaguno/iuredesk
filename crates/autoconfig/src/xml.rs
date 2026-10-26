//! Lector del formato `config-v1.1.xml` de Thunderbird (autoconfig del dominio e ISPDB).
//!
//! <https://wiki.mozilla.org/Thunderbird:Autoconfiguration:ConfigFileFormat>
//!
//! El XML viene de la red: se ignoran DTD y entidades externas (quick-xml no las expande), se
//! acotan servidores, textos, anidamiento y eventos, y cada host y puerto se valida antes de usarlo.

use iuredesk_model::account::{AuthMethod, OAuthProvider, Protocol, Security, ServerConfig};
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

use crate::email::EmailAddress;

/// Tope de eventos XML que se leen (un archivo real tiene unos cientos).
const MAX_EVENTS: usize = 20_000;
/// Tope de servidores por archivo.
const MAX_SERVERS: usize = 16;
/// Tope de un texto (hostname, usuario, nombre…), en bytes.
const MAX_TEXT: usize = 512;
/// Tope de anidamiento.
const MAX_DEPTH: usize = 32;

/// Lo que se saca de un `config-v1.1.xml`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParsedConfig {
    pub id: Option<String>,
    pub display_name: Option<String>,
    pub domains: Vec<String>,
    /// Entrantes en el orden del archivo (el primero es el preferido del proveedor).
    pub incoming: Vec<ServerConfig>,
    pub outgoing: Vec<ServerConfig>,
    /// Página de instrucciones del proveedor, si la da (solo `https`).
    pub documentation: Option<String>,
}

/// Error de formato.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum XmlError {
    #[error("XML mal formado: {0}")]
    Malformed(String),
    #[error("no es un config-v1.1.xml")]
    NotAConfig,
    #[error("el archivo no trae servidores utilizables")]
    NoServers,
    #[error("archivo demasiado grande")]
    TooLarge,
}

#[derive(Default)]
struct RawServer {
    kind: String,
    hostname: String,
    port: String,
    socket: String,
    username: String,
    auth: Vec<String>,
}

/// ¿Es un nombre de host razonable? Letras, dígitos, guiones y puntos; sin espacios ni rutas.
pub(crate) fn valid_host(h: &str) -> bool {
    !h.is_empty()
        && h.len() <= 253
        && !h.starts_with(['-', '.'])
        && !h.ends_with('-')
        && !h.contains("..")
        && h.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
}

/// ¿`host` es `suffix` o un subdominio suyo?
pub(crate) fn host_matches(host: &str, suffix: &str) -> bool {
    let h = host.trim_end_matches('.');
    h.eq_ignore_ascii_case(suffix)
        || (h.len() > suffix.len()
            && h.get(h.len() - suffix.len()..).is_some_and(|t| t.eq_ignore_ascii_case(suffix))
            && h.as_bytes().get(h.len() - suffix.len() - 1) == Some(&b'.'))
}

/// Proveedor OAuth que corresponde a un servidor, por su nombre.
pub(crate) fn oauth_provider_for_host(host: &str) -> Option<OAuthProvider> {
    if ["gmail.com", "googlemail.com", "google.com"].iter().any(|s| host_matches(host, s)) {
        Some(OAuthProvider::Google)
    } else if ["office365.com", "outlook.com", "hotmail.com", "live.com"].iter().any(|s| host_matches(host, s)) {
        Some(OAuthProvider::Microsoft)
    } else {
        None
    }
}

fn map_auth(methods: &[String], host: &str) -> AuthMethod {
    let has = |m: &str| methods.iter().any(|x| x.eq_ignore_ascii_case(m));
    if has("OAuth2")
        && let Some(provider) = oauth_provider_for_host(host)
    {
        return AuthMethod::OAuth2 { provider };
    }
    AuthMethod::Password
}

fn map_server(raw: &RawServer, email: &EmailAddress) -> Option<ServerConfig> {
    let protocol = match raw.kind.to_ascii_lowercase().as_str() {
        "imap" => Protocol::Imap,
        "pop3" => Protocol::Pop3,
        "smtp" => Protocol::Smtp,
        _ => return None, // exchange, ews, jmap…: no aplican aquí
    };
    let host = email.substitute(raw.hostname.trim()).to_ascii_lowercase();
    if !valid_host(&host) {
        return None;
    }
    let security = match raw.socket.trim().to_ascii_uppercase().as_str() {
        "SSL" | "TLS" => Security::Tls,
        "STARTTLS" => Security::StartTls,
        "PLAIN" => Security::Plain,
        _ => return None,
    };
    let port = match raw.port.trim() {
        "" => protocol.default_port(security),
        p => p.parse::<u16>().ok().filter(|p| *p != 0)?,
    };
    let username = match raw.username.trim() {
        "" => email.address.clone(),
        u => email.substitute(u),
    };
    let auth = map_auth(&raw.auth, &host);
    Some(ServerConfig { protocol, host, port, security, auth, username })
}

/// Interpreta un `config-v1.1.xml`, sustituyendo `%EMAILADDRESS%`, `%EMAILLOCALPART%` y
/// `%EMAILDOMAIN%` con los datos de `email`.
pub fn parse_config(xml: &str, email: &EmailAddress) -> Result<ParsedConfig, XmlError> {
    if xml.len() > crate::net::MAX_BODY {
        return Err(XmlError::TooLarge);
    }
    let mut reader = Reader::from_str(xml);
    // Sin recortar cada trozo: `Example &amp; Co` llega en tres eventos y se recorta al cerrar.
    reader.config_mut().trim_text(false);
    let mut out = ParsedConfig::default();
    let mut stack: Vec<String> = Vec::new();
    let mut text = String::new();
    let mut current: Option<RawServer> = None;
    let mut saw_root = false;
    let mut raw_servers: Vec<RawServer> = Vec::new();
    let mut finished = false;

    for _ in 0..MAX_EVENTS {
        let ev = reader.read_event().map_err(|e| XmlError::Malformed(e.to_string()))?;
        match ev {
            Event::Start(e) => {
                if stack.len() >= MAX_DEPTH {
                    return Err(XmlError::Malformed("anidamiento excesivo".into()));
                }
                let name = start_element(&e, &mut out, &mut current);
                if stack.is_empty() && name == "clientConfig" {
                    saw_root = true;
                }
                text.clear();
                stack.push(name);
            }
            Event::Empty(e) => {
                // Sin cierre: no se apila. Un servidor vacío no sirve.
                let name = start_element(&e, &mut out, &mut current);
                if matches!(name.as_str(), "incomingServer" | "outgoingServer") {
                    current = None;
                }
            }
            Event::Text(t) => {
                if let Ok(s) = t.decode() {
                    push_bounded(&mut text, &s);
                }
            }
            Event::CData(t) => {
                if let Ok(s) = t.decode() {
                    push_bounded(&mut text, &s);
                }
            }
            Event::GeneralRef(r) => {
                if let Ok(name) = r.decode()
                    && name.len() <= 16
                    && let Ok(s) = quick_xml::escape::unescape(&format!("&{name};"))
                {
                    push_bounded(&mut text, &s);
                }
            }
            Event::End(_) => {
                let name = stack.pop().unwrap_or_default();
                let value = text.trim().to_string();
                text.clear();
                let parent = stack.last().map(String::as_str).unwrap_or_default();
                end_element(parent, &name, value, &mut out, &mut current, &mut raw_servers);
            }
            Event::Eof => {
                finished = true;
                break;
            }
            _ => {}
        }
    }
    if !finished {
        return Err(XmlError::TooLarge);
    }
    if !saw_root {
        return Err(XmlError::NotAConfig);
    }
    for raw in &raw_servers {
        let Some(server) = map_server(raw, email) else { continue };
        if server.protocol.is_incoming() {
            out.incoming.push(server);
        } else {
            out.outgoing.push(server);
        }
    }
    if out.incoming.is_empty() {
        return Err(XmlError::NoServers);
    }
    Ok(out)
}

/// Atiende la apertura de un elemento y devuelve su nombre local.
fn start_element(e: &BytesStart<'_>, out: &mut ParsedConfig, current: &mut Option<RawServer>) -> String {
    let name = String::from_utf8_lossy(e.local_name().as_ref()).chars().take(64).collect::<String>();
    let attr = |key: &str| -> Option<String> {
        e.try_get_attribute(key).ok().flatten().and_then(|a| {
            a.normalized_value(quick_xml::XmlVersion::Implicit1_0).ok().map(|v| v.chars().take(MAX_TEXT).collect())
        })
    };
    match name.as_str() {
        "emailProvider" => {
            if out.id.is_none() {
                out.id = attr("id");
            }
        }
        "incomingServer" | "outgoingServer" => {
            *current = Some(RawServer { kind: attr("type").unwrap_or_default(), ..Default::default() });
        }
        "documentation" => {
            if out.documentation.is_none()
                && let Some(url) = attr("url").filter(|u| u.starts_with("https://"))
            {
                out.documentation = Some(url);
            }
        }
        _ => {}
    }
    name
}

/// Atiende el cierre de un elemento con su texto.
fn end_element(
    parent: &str,
    name: &str,
    value: String,
    out: &mut ParsedConfig,
    current: &mut Option<RawServer>,
    raw_servers: &mut Vec<RawServer>,
) {
    match (parent, name) {
        ("emailProvider", "displayName") if out.display_name.is_none() && !value.is_empty() => {
            out.display_name = Some(value);
        }
        ("emailProvider", "domain") if out.domains.len() < 64 => out.domains.push(value.to_ascii_lowercase()),
        (_, "incomingServer" | "outgoingServer") => {
            if let Some(s) = current.take()
                && raw_servers.len() < MAX_SERVERS
            {
                raw_servers.push(s);
            }
        }
        ("incomingServer" | "outgoingServer", field) => {
            if let Some(s) = current.as_mut() {
                match field {
                    "hostname" => s.hostname = value,
                    "port" => s.port = value,
                    "socketType" => s.socket = value,
                    "username" => s.username = value,
                    "authentication" if s.auth.len() < 8 => s.auth.push(value),
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

fn push_bounded(buf: &mut String, s: &str) {
    for c in s.chars() {
        if buf.len().saturating_add(c.len_utf8()) > MAX_TEXT {
            break;
        }
        buf.push(c);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ana() -> EmailAddress {
        EmailAddress::parse("ana.perez@example.com").unwrap()
    }

    #[test]
    fn parses_ispdb_gmail_fixture() {
        let email = EmailAddress::parse("ana@gmail.com").unwrap();
        let c = parse_config(include_str!("../tests/fixtures/ispdb-gmail.xml"), &email).unwrap();
        assert_eq!(c.id.as_deref(), Some("googlemail.com"));
        assert_eq!(c.display_name.as_deref(), Some("Google Mail"));
        assert!(c.domains.contains(&"gmail.com".to_string()));
        let imap = c.incoming.first().unwrap();
        assert_eq!(imap.protocol, Protocol::Imap);
        assert_eq!(imap.host, "imap.gmail.com");
        assert_eq!(imap.port, 993);
        assert_eq!(imap.security, Security::Tls);
        assert_eq!(imap.username, "ana@gmail.com");
        assert_eq!(imap.auth, AuthMethod::OAuth2 { provider: OAuthProvider::Google });
        assert!(c.incoming.iter().any(|s| s.protocol == Protocol::Pop3 && s.port == 995));
        let smtp = c.outgoing.first().unwrap();
        assert_eq!((smtp.host.as_str(), smtp.port, smtp.security), ("smtp.gmail.com", 465, Security::Tls));
        assert!(c.documentation.unwrap().starts_with("https://"));
    }

    #[test]
    fn parses_domain_autoconfig_with_placeholders_and_entities() {
        let c = parse_config(include_str!("../tests/fixtures/autoconfig-example.xml"), &ana()).unwrap();
        assert_eq!(c.display_name.as_deref(), Some("Example & Co"));
        let imap = c.incoming.first().unwrap();
        assert_eq!(imap.host, "mail.example.com");
        assert_eq!(imap.username, "ana.perez");
        assert_eq!(imap.auth, AuthMethod::Password);
        let smtp = c.outgoing.first().unwrap();
        assert_eq!((smtp.port, smtp.security), (587, Security::StartTls));
        assert_eq!(smtp.username, "ana.perez@example.com");
        // El servidor «exchange» se ignora; el de host inválido también.
        assert_eq!(c.incoming.len(), 2);
    }

    #[test]
    fn parses_ispdb_with_mx_domain_fixture() {
        let c = parse_config(include_str!("../tests/fixtures/ispdb-fastmail.xml"), &ana()).unwrap();
        assert_eq!(c.incoming.first().unwrap().host, "imap.fastmail.com");
        assert_eq!(c.outgoing.first().unwrap().port, 465);
    }

    #[test]
    fn rejects_garbage() {
        assert!(matches!(parse_config("hola", &ana()), Err(XmlError::NotAConfig)));
        assert!(parse_config("<clientConfig><emailProvider>", &ana()).is_err());
        assert!(matches!(parse_config("<html><body>404</body></html>", &ana()), Err(XmlError::NotAConfig)));
        assert!(matches!(
            parse_config("<clientConfig version=\"1.1\"><emailProvider/></clientConfig>", &ana()),
            Err(XmlError::NoServers)
        ));
        let deep = "<a>".repeat(100);
        assert!(parse_config(&deep, &ana()).is_err());
        let huge = "x".repeat(crate::net::MAX_BODY + 1);
        assert_eq!(parse_config(&huge, &ana()), Err(XmlError::TooLarge));
    }

    #[test]
    fn rejects_bad_hosts_and_ports() {
        let xml = r#"<clientConfig><emailProvider id="x">
            <incomingServer type="imap"><hostname>evil host/../</hostname><port>993</port><socketType>SSL</socketType></incomingServer>
            <incomingServer type="imap"><hostname>imap.x.com</hostname><port>99999</port><socketType>SSL</socketType></incomingServer>
            <incomingServer type="imap"><hostname>imap.x.com</hostname><port>0</port><socketType>SSL</socketType></incomingServer>
            <incomingServer type="imap"><hostname>imap.x.com</hostname><port>993</port><socketType>weird</socketType></incomingServer>
        </emailProvider></clientConfig>"#;
        assert_eq!(parse_config(xml, &ana()), Err(XmlError::NoServers));
    }

    #[test]
    fn host_validation() {
        assert!(valid_host("imap.gmail.com"));
        assert!(!valid_host(""));
        assert!(!valid_host("-a.com"));
        assert!(!valid_host("a..com"));
        assert!(!valid_host("a.com/x"));
        assert!(host_matches("aspmx.l.google.com", "google.com"));
        assert!(!host_matches("evilgoogle.com", "google.com"));
        assert!(host_matches("Google.com.", "google.com"));
    }
}
