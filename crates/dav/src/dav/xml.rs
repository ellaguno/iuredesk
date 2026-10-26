//! XML de WebDAV: un DOM mínimo con espacios de nombres resueltos y el `multistatus` (RFC 4918).
//!
//! Acotado contra respuestas hostiles: profundidad y número de elementos limitados; las
//! entidades propias de un DTD no se expanden (solo las cinco predefinidas y `&#…;`).

use quick_xml::NsReader;
use quick_xml::events::Event;
use quick_xml::name::ResolveResult;

use crate::{Error, Result};

/// `DAV:`
pub const DAV: &str = "DAV:";
/// CalDAV (RFC 4791).
pub const CALDAV: &str = "urn:ietf:params:xml:ns:caldav";
/// CardDAV (RFC 6352).
pub const CARDDAV: &str = "urn:ietf:params:xml:ns:carddav";
/// CalendarServer (`getctag`).
pub const CS: &str = "http://calendarserver.org/ns/";
/// Apple iCal (`calendar-color`, `calendar-order`).
pub const APPLE: &str = "http://apple.com/ns/ical/";

const MAX_DEPTH: usize = 64;
const MAX_ELEMENTS: usize = 500_000;
const MAX_ATTRS: usize = 32;

/// Elemento XML con su espacio de nombres resuelto.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Element {
    /// Espacio de nombres (vacío si no tiene).
    pub ns: String,
    /// Nombre local.
    pub name: String,
    /// Hijos en orden.
    pub children: Vec<Element>,
    /// Texto concatenado (sin recortar).
    pub text: String,
    /// Atributos (nombre local, valor sin escapes).
    pub attrs: Vec<(String, String)>,
}

impl Element {
    /// Valor del atributo con ese nombre local.
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    /// ¿Es `{ns}name`?
    pub fn is(&self, ns: &str, name: &str) -> bool {
        self.ns == ns && self.name == name
    }

    /// Primer hijo `{ns}name`.
    pub fn child(&self, ns: &str, name: &str) -> Option<&Element> {
        self.children.iter().find(|c| c.is(ns, name))
    }

    /// Hijos `{ns}name`.
    pub fn children_named<'a>(&'a self, ns: &'a str, name: &'a str) -> impl Iterator<Item = &'a Element> + 'a {
        self.children.iter().filter(move |c| c.is(ns, name))
    }

    /// Texto recortado.
    pub fn text_trim(&self) -> &str {
        self.text.trim()
    }

    /// Texto del primer `DAV:href` hijo.
    pub fn href(&self) -> Option<String> {
        self.child(DAV, "href").map(|h| h.text_trim().to_string()).filter(|h| !h.is_empty())
    }

    /// Textos de todos los `DAV:href` hijos.
    pub fn hrefs(&self) -> Vec<String> {
        self.children_named(DAV, "href").map(|h| h.text_trim().to_string()).filter(|h| !h.is_empty()).collect()
    }
}

/// Lee un documento XML y devuelve el elemento raíz.
pub fn parse(bytes: &[u8]) -> Result<Element> {
    let mut reader = NsReader::from_reader(bytes);
    reader.config_mut().trim_text(false);
    let mut stack: Vec<Element> = Vec::new();
    let mut count = 0usize;
    loop {
        let (ns, event) = reader.read_resolved_event().map_err(|e| Error::Xml(e.to_string()))?;
        match event {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let empty = matches!(event, Event::Empty(_));
                count = count.saturating_add(1);
                if count > MAX_ELEMENTS {
                    return Err(Error::Limit("respuesta XML con demasiados elementos".into()));
                }
                if stack.len() >= MAX_DEPTH {
                    return Err(Error::Limit("respuesta XML demasiado anidada".into()));
                }
                let mut attrs = Vec::new();
                for a in e.attributes().with_checks(false).flatten().take(MAX_ATTRS) {
                    let key = String::from_utf8_lossy(a.key.local_name().as_ref()).into_owned();
                    if let Ok(v) = a.decoded_and_normalized_value(quick_xml::XmlVersion::Implicit1_0, e.decoder()) {
                        attrs.push((key, v.into_owned()));
                    }
                }
                let el = Element {
                    ns: ns_string(&ns),
                    name: String::from_utf8_lossy(e.local_name().as_ref()).into_owned(),
                    attrs,
                    ..Element::default()
                };
                if !empty {
                    stack.push(el);
                    continue;
                }
                match stack.last_mut() {
                    Some(parent) => parent.children.push(el),
                    None => return Ok(el),
                }
            }
            Event::End(_) => {
                let Some(done) = stack.pop() else { return Err(Error::Xml("cierre sin apertura".into())) };
                match stack.last_mut() {
                    Some(parent) => parent.children.push(done),
                    None => return Ok(done),
                }
            }
            Event::Text(t) => {
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&t.decode().map_err(|e| Error::Xml(e.to_string()))?);
                }
            }
            Event::CData(t) => {
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&t.decode().map_err(|e| Error::Xml(e.to_string()))?);
                }
            }
            Event::GeneralRef(r) => {
                if let Some(top) = stack.last_mut() {
                    if let Ok(Some(c)) = r.resolve_char_ref() {
                        top.text.push(c);
                    } else if let Ok(name) = r.decode()
                        && let Some(s) = quick_xml::escape::resolve_predefined_entity(&name)
                    {
                        top.text.push_str(s);
                    }
                }
            }
            Event::Eof => return Err(Error::Xml("documento XML incompleto".into())),
            _ => {}
        }
    }
}

fn ns_string(ns: &ResolveResult<'_>) -> String {
    match ns {
        ResolveResult::Bound(n) => String::from_utf8_lossy(n.as_ref()).into_owned(),
        _ => String::new(),
    }
}

/// Escapa texto para meterlo en XML.
pub fn escape(s: &str) -> String {
    quick_xml::escape::escape(s).into_owned()
}

/// Un `DAV:response` de un `multistatus`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Response {
    /// `href` tal como lo mandó el servidor.
    pub href: String,
    /// Estado de la respuesta completa (`DAV:status` directo), si lo hay.
    pub status: Option<u16>,
    /// Bloques `propstat`.
    pub propstats: Vec<PropStat>,
}

/// Un `propstat`: propiedades con su estado.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PropStat {
    /// Código HTTP.
    pub status: u16,
    /// Elementos dentro de `DAV:prop`.
    pub props: Vec<Element>,
}

impl Response {
    /// Propiedad `{ns}name` en un `propstat` 2xx.
    pub fn prop(&self, ns: &str, name: &str) -> Option<&Element> {
        self.propstats
            .iter()
            .filter(|p| (200..300).contains(&p.status))
            .flat_map(|p| p.props.iter())
            .find(|e| e.is(ns, name))
    }

    /// Texto recortado de una propiedad (vacío → `None`).
    pub fn prop_text(&self, ns: &str, name: &str) -> Option<String> {
        self.prop(ns, name).map(|e| e.text_trim().to_string()).filter(|t| !t.is_empty())
    }

    /// `href` dentro de una propiedad (`current-user-principal`, `calendar-home-set`…).
    pub fn prop_hrefs(&self, ns: &str, name: &str) -> Vec<String> {
        self.prop(ns, name).map(Element::hrefs).unwrap_or_default()
    }

    /// ¿La respuesta es correcta (sin estado, o 2xx)?
    pub fn ok(&self) -> bool {
        self.status.is_none_or(|s| (200..300).contains(&s))
    }
}

/// Un `DAV:multistatus` leído.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Multistatus {
    /// Respuestas.
    pub responses: Vec<Response>,
    /// `DAV:sync-token` (RFC 6578).
    pub sync_token: Option<String>,
}

/// Lee un `multistatus`.
pub fn parse_multistatus(bytes: &[u8]) -> Result<Multistatus> {
    let root = parse(bytes)?;
    if !root.is(DAV, "multistatus") {
        return Err(Error::Xml(format!("se esperaba multistatus y llegó {}", root.name)));
    }
    let mut out = Multistatus {
        sync_token: root.child(DAV, "sync-token").map(|t| t.text_trim().to_string()).filter(|t| !t.is_empty()),
        ..Multistatus::default()
    };
    for r in root.children_named(DAV, "response") {
        let Some(href) = r.href() else { continue };
        let status = r.child(DAV, "status").and_then(|s| parse_status(s.text_trim()));
        let propstats = r
            .children_named(DAV, "propstat")
            .map(|ps| PropStat {
                status: ps.child(DAV, "status").and_then(|s| parse_status(s.text_trim())).unwrap_or(200),
                props: ps.child(DAV, "prop").map(|p| p.children.clone()).unwrap_or_default(),
            })
            .collect();
        out.responses.push(Response { href, status, propstats });
    }
    Ok(out)
}

/// `HTTP/1.1 200 OK` → 200.
pub fn parse_status(s: &str) -> Option<u16> {
    s.split_whitespace().nth(1)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespaces_and_entities() {
        let xml = br#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:" xmlns:x="http://apple.com/ns/ical/">
<d:response><d:href>/cal/a%20b/</d:href><d:propstat><d:prop><d:displayname>A &amp; B &#233;</d:displayname>
<x:calendar-color><![CDATA[#FF0000]]></x:calendar-color></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>
<d:propstat><d:prop><d:getctag/></d:prop><d:status>HTTP/1.1 404 Not Found</d:status></d:propstat></d:response>
</d:multistatus>"#;
        let ms = parse_multistatus(xml).unwrap();
        let r = &ms.responses[0];
        assert_eq!(r.href, "/cal/a%20b/");
        assert_eq!(r.prop_text(DAV, "displayname").as_deref(), Some("A & B é"));
        assert_eq!(r.prop_text(APPLE, "calendar-color").as_deref(), Some("#FF0000"));
        assert!(r.prop(DAV, "getctag").is_none());
    }

    #[test]
    fn hostile_xml() {
        assert!(parse(b"<a>").is_err());
        assert!(parse(b"").is_err());
        let deep = "<a>".repeat(200) + &"</a>".repeat(200);
        assert!(matches!(parse(deep.as_bytes()), Err(Error::Limit(_))));
        // Entidades propias de un DTD no se expanden.
        let lol = br#"<!DOCTYPE x [<!ENTITY lol "lololol">]><x>&lol;</x>"#;
        assert_eq!(parse(lol).unwrap().text, "");
        assert!(parse_multistatus(b"<html/>").is_err());
    }

    proptest::proptest! {
        #[test]
        fn never_panics(s in "\\PC{0,300}") {
            let _ = parse_multistatus(s.as_bytes());
        }
    }
}
