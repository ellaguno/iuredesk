//! Descubrimiento (RFC 6764, RFC 5397, RFC 4791 §6.2.1, RFC 6352 §7.1.1) y lista de colecciones.

use reqwest::Url;

use super::client::{DavClient, Depth};
use super::quirks::{self, Provider, Service};
use super::xml::{APPLE, CALDAV, CARDDAV, CS, DAV, Element, Response};
use super::{Collection, CollectionKind};
use crate::{Error, Result};

/// Lo que se encontró a partir de la URL del usuario.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovery {
    /// Proveedor reconocido.
    pub provider: Provider,
    /// URL del principal del usuario.
    pub principal: Option<Url>,
    /// Hogares de calendarios.
    pub calendar_homes: Vec<Url>,
    /// Hogares de libretas de direcciones.
    pub addressbook_homes: Vec<Url>,
    /// Buzón de salida de agenda (RFC 6638), para free/busy.
    pub schedule_outbox: Option<Url>,
    /// Direcciones del usuario para agenda (`mailto:…`).
    pub user_addresses: Vec<String>,
    /// Nombre del usuario según el servidor.
    pub display_name: Option<String>,
}

const PRINCIPAL_PROPS: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav" xmlns:card="urn:ietf:params:xml:ns:carddav">
<d:prop><d:current-user-principal/><d:principal-URL/><d:resourcetype/><d:displayname/>
<c:calendar-home-set/><card:addressbook-home-set/><c:schedule-outbox-URL/><c:calendar-user-address-set/></d:prop>
</d:propfind>"#;

const COLLECTION_PROPS: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav" xmlns:card="urn:ietf:params:xml:ns:carddav" xmlns:cs="http://calendarserver.org/ns/" xmlns:a="http://apple.com/ns/ical/">
<d:prop><d:resourcetype/><d:displayname/><cs:getctag/><d:sync-token/><a:calendar-color/>
<c:supported-calendar-component-set/><c:calendar-description/><card:addressbook-description/>
<d:current-user-privilege-set/></d:prop>
</d:propfind>"#;

impl DavClient {
    /// Descubre principal y hogares a partir de lo que escribió el usuario (`example.com`,
    /// `https://cloud.example.com/remote.php/dav`, la URL del principal…). Prueba en orden las
    /// candidatas de [`quirks::candidates`]; un 401 corta enseguida.
    pub async fn discover(&self, start: &str, service: Service) -> Result<Discovery> {
        let start = parse_start(start)?;
        let provider = quirks::detect(&start);
        let mut last_err = Error::Unsupported("CalDAV/CardDAV en esta dirección");
        for candidate in quirks::candidates(&start, service) {
            let (final_url, ms) = match self.propfind(&candidate, Depth::Zero, PRINCIPAL_PROPS).await {
                Ok(r) => r,
                Err(Error::Unauthorized) => return Err(Error::Unauthorized),
                Err(e) => {
                    last_err = e;
                    continue;
                }
            };
            let Some(resp) = ms.responses.first() else { continue };
            let principal = first_href(resp, DAV, "current-user-principal")
                .or_else(|| first_href(resp, DAV, "principal-URL"))
                .and_then(|h| final_url.join(&h).ok());
            let mut found = Discovery {
                provider,
                principal: principal.clone(),
                calendar_homes: Vec::new(),
                addressbook_homes: Vec::new(),
                schedule_outbox: None,
                user_addresses: Vec::new(),
                display_name: None,
            };
            // Las propiedades de hogar están en el principal; si la candidata ya es el
            // principal (o el servidor las da aquí), se aprovechan.
            fill_principal(&mut found, &final_url, resp);
            if let Some(p) = principal.filter(|p| *p != final_url) {
                match self.propfind(&p, Depth::Zero, PRINCIPAL_PROPS).await {
                    Ok((purl, pms)) => {
                        if let Some(presp) = pms.responses.first() {
                            fill_principal(&mut found, &purl, presp);
                        }
                    }
                    Err(Error::Unauthorized) => return Err(Error::Unauthorized),
                    Err(e) => last_err = e,
                }
            }
            let has_home = match service {
                Service::CalDav => !found.calendar_homes.is_empty(),
                Service::CardDav => !found.addressbook_homes.is_empty(),
            };
            if has_home || found.principal.is_some() {
                return Ok(found);
            }
            // Servidor sin principal (Radicale viejo): la colección misma sirve de hogar.
            if is_collection(resp) {
                match service {
                    Service::CalDav => found.calendar_homes.push(final_url),
                    Service::CardDav => found.addressbook_homes.push(final_url),
                }
                return Ok(found);
            }
        }
        Err(last_err)
    }

    /// Lista los calendarios o libretas que cuelgan de un hogar (`Depth: 1`). Se omiten el propio
    /// hogar, los buzones de agenda y lo que no sea calendario ni libreta.
    pub async fn list_collections(&self, home: &Url) -> Result<Vec<Collection>> {
        let (base, ms) = self.propfind(home, Depth::One, COLLECTION_PROPS).await?;
        let mut out = Vec::new();
        for r in &ms.responses {
            let Ok(url) = base.join(&r.href) else { continue };
            let Some(rt) = r.prop(DAV, "resourcetype") else { continue };
            let kind = if rt.child(CALDAV, "calendar").is_some() {
                CollectionKind::Calendar
            } else if rt.child(CARDDAV, "addressbook").is_some() {
                CollectionKind::AddressBook
            } else {
                continue;
            };
            let components = r.prop(CALDAV, "supported-calendar-component-set").map(comp_names).unwrap_or_default();
            out.push(Collection {
                url,
                kind,
                display_name: r.prop_text(DAV, "displayname"),
                description: r
                    .prop_text(CALDAV, "calendar-description")
                    .or_else(|| r.prop_text(CARDDAV, "addressbook-description")),
                color: r.prop_text(APPLE, "calendar-color"),
                ctag: r.prop_text(CS, "getctag"),
                sync_token: r.prop_text(DAV, "sync-token"),
                components,
                read_only: r.prop(DAV, "current-user-privilege-set").is_some_and(|p| !can_write(p)),
            });
        }
        Ok(out)
    }

    /// Descubre y lista en un paso.
    pub async fn discover_collections(&self, start: &str, service: Service) -> Result<(Discovery, Vec<Collection>)> {
        let d = self.discover(start, service).await?;
        let homes = match service {
            Service::CalDav => &d.calendar_homes,
            Service::CardDav => &d.addressbook_homes,
        };
        let mut all = Vec::new();
        for h in homes {
            all.extend(self.list_collections(h).await?);
        }
        Ok((d, all))
    }

    /// Propiedades de una sola colección (para `getctag`/`sync-token` antes de sincronizar).
    pub async fn collection_props(&self, url: &Url) -> Result<Option<Collection>> {
        let (base, ms) = self.propfind(url, Depth::Zero, COLLECTION_PROPS).await?;
        let Some(r) = ms.responses.first() else { return Ok(None) };
        let kind = match r.prop(DAV, "resourcetype") {
            Some(rt) if rt.child(CARDDAV, "addressbook").is_some() => CollectionKind::AddressBook,
            _ => CollectionKind::Calendar,
        };
        Ok(Some(Collection {
            url: base.join(&r.href).unwrap_or(base),
            kind,
            display_name: r.prop_text(DAV, "displayname"),
            description: None,
            color: r.prop_text(APPLE, "calendar-color"),
            ctag: r.prop_text(CS, "getctag"),
            sync_token: r.prop_text(DAV, "sync-token"),
            components: r.prop(CALDAV, "supported-calendar-component-set").map(comp_names).unwrap_or_default(),
            read_only: r.prop(DAV, "current-user-privilege-set").is_some_and(|p| !can_write(p)),
        }))
    }
}

/// `<c:comp name="VEVENT"/>` → `VEVENT`.
fn comp_names(set: &Element) -> Vec<String> {
    set.children_named(CALDAV, "comp")
        .filter_map(|c| c.attr("name"))
        .map(|n| n.trim().to_ascii_uppercase())
        .filter(|n| !n.is_empty())
        .collect()
}

fn parse_start(s: &str) -> Result<Url> {
    let t = s.trim();
    if t.is_empty() {
        return Err(Error::Url("vacía".into()));
    }
    let with_scheme = if t.contains("://") { t.to_string() } else { format!("https://{t}") };
    Url::parse(&with_scheme).map_err(|e| Error::Url(format!("{t}: {e}")))
}

fn first_href(r: &Response, ns: &str, name: &str) -> Option<String> {
    r.prop_hrefs(ns, name).into_iter().next()
}

fn fill_principal(d: &mut Discovery, base: &Url, r: &Response) {
    let join = |h: String| base.join(&h).ok();
    for h in r.prop_hrefs(CALDAV, "calendar-home-set").into_iter().filter_map(join) {
        if !d.calendar_homes.contains(&h) {
            d.calendar_homes.push(h);
        }
    }
    for h in r.prop_hrefs(CARDDAV, "addressbook-home-set").into_iter().filter_map(join) {
        if !d.addressbook_homes.contains(&h) {
            d.addressbook_homes.push(h);
        }
    }
    if d.schedule_outbox.is_none() {
        d.schedule_outbox = r.prop_hrefs(CALDAV, "schedule-outbox-URL").into_iter().find_map(join);
    }
    for a in r.prop_hrefs(CALDAV, "calendar-user-address-set") {
        if !d.user_addresses.contains(&a) {
            d.user_addresses.push(a);
        }
    }
    if d.display_name.is_none() {
        d.display_name = r.prop_text(DAV, "displayname");
    }
}

fn is_collection(r: &Response) -> bool {
    r.prop(DAV, "resourcetype").is_some_and(|rt| rt.child(DAV, "collection").is_some())
}

/// ¿El conjunto de privilegios permite escribir?
fn can_write(set: &Element) -> bool {
    set.children_named(DAV, "privilege").any(|p| {
        p.children.iter().any(|c| c.ns == DAV && matches!(c.name.as_str(), "all" | "write" | "write-content" | "bind"))
    })
}
