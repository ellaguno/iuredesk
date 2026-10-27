//! Servidor CalDAV + CardDAV falso con estado, sobre el servidor HTTP mínimo de las pruebas de
//! `iuredesk-dav` (reutilizado tal cual con `#[path]`).
#![allow(dead_code)]

#[path = "../../../dav/tests/common/mod.rs"]
pub mod common;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use common::{Req, Resp, between, multistatus, serve};

pub const CAL: &str = "/dav/cal/ana/work/";
pub const BOOK: &str = "/dav/card/ana/contacts/";

#[derive(Default)]
pub struct State {
    /// ruta → (etag, datos)
    pub items: BTreeMap<String, (u64, String)>,
    /// (secuencia, ruta, borrado)
    pub log: Vec<(u64, String, bool)>,
    pub seq: u64,
    /// Peticiones recibidas («MÉTODO ruta»).
    pub requests: Vec<String>,
    /// Cuerpos de los PUT recibidos (ruta, cuerpo).
    pub puts: Vec<(String, String)>,
    /// Anunciar buzón de salida de agenda (programación en el servidor).
    pub outbox: bool,
}

impl State {
    /// Cambio hecho por «otro cliente» (o por un PUT).
    pub fn change(&mut self, path: &str, data: Option<String>) -> u64 {
        self.seq += 1;
        match data {
            Some(d) => {
                self.items.insert(path.to_string(), (self.seq, d));
            }
            None => {
                self.items.remove(path);
            }
        }
        self.log.push((self.seq, path.to_string(), !self.items.contains_key(path)));
        self.seq
    }

    pub fn data(&self, path: &str) -> Option<String> {
        self.items.get(path).map(|(_, d)| d.clone())
    }
}

pub fn etag(n: u64) -> String {
    format!("\"e{n}\"")
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn ok_prop(inner: &str) -> String {
    format!("<d:propstat><d:prop>{inner}</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>")
}

fn handle(state: &Mutex<State>, req: Req) -> Resp {
    let mut st = state.lock().unwrap();
    st.requests.push(format!("{} {}", req.method, req.path));
    let expected = format!("Basic {}", base64::engine::general_purpose::STANDARD.encode("ana:secreto"));
    if req.header("Authorization") != Some(expected.as_str()) {
        return Resp::new(401, "").with("WWW-Authenticate", "Basic realm=\"x\"");
    }
    let path = req.path.split('?').next().unwrap_or_default().to_string();
    let coll_of = |p: &str| {
        if p.starts_with(CAL) {
            Some(CAL)
        } else if p.starts_with(BOOK) {
            Some(BOOK)
        } else {
            None
        }
    };
    match (req.method.as_str(), path.as_str()) {
        ("PROPFIND", "/dav/") => Resp::xml(
            207,
            multistatus(&format!(
                "<d:response><d:href>/dav/</d:href>{}</d:response>",
                ok_prop(
                    "<d:current-user-principal><d:href>/dav/principals/ana/</d:href></d:current-user-principal><d:resourcetype><d:collection/></d:resourcetype>"
                )
            )),
        ),
        ("PROPFIND", "/dav/principals/ana/") => {
            let outbox = if st.outbox {
                "<c:schedule-outbox-URL><d:href>/dav/cal/ana/outbox/</d:href></c:schedule-outbox-URL>"
            } else {
                ""
            };
            Resp::xml(
                207,
                multistatus(&format!(
                    "<d:response><d:href>/dav/principals/ana/</d:href>{}</d:response>",
                    ok_prop(&format!(
                        "<d:displayname>Ana</d:displayname><c:calendar-home-set><d:href>/dav/cal/ana/</d:href></c:calendar-home-set>\
<card:addressbook-home-set><d:href>/dav/card/ana/</d:href></card:addressbook-home-set>{outbox}\
<c:calendar-user-address-set><d:href>mailto:ana@example.com</d:href></c:calendar-user-address-set>"
                    ))
                )),
            )
        }
        ("PROPFIND", "/dav/cal/ana/") => Resp::xml(
            207,
            multistatus(&format!(
                "<d:response><d:href>/dav/cal/ana/</d:href>{}</d:response><d:response><d:href>{CAL}</d:href>{}</d:response>",
                ok_prop("<d:resourcetype><d:collection/></d:resourcetype>"),
                ok_prop(&format!(
                    "<d:resourcetype><d:collection/><c:calendar/></d:resourcetype><d:displayname>Trabajo</d:displayname>\
<a:calendar-color>#0B5FFFFF</a:calendar-color><d:sync-token>tok-{}</d:sync-token>\
<c:supported-calendar-component-set><c:comp name=\"VEVENT\"/><c:comp name=\"VTODO\"/></c:supported-calendar-component-set>\
<d:current-user-privilege-set><d:privilege><d:read/></d:privilege><d:privilege><d:write/></d:privilege></d:current-user-privilege-set>",
                    st.seq
                ))
            )),
        ),
        ("PROPFIND", "/dav/card/ana/") => Resp::xml(
            207,
            multistatus(&format!(
                "<d:response><d:href>/dav/card/ana/</d:href>{}</d:response><d:response><d:href>{BOOK}</d:href>{}</d:response>",
                ok_prop("<d:resourcetype><d:collection/></d:resourcetype>"),
                ok_prop(&format!(
                    "<d:resourcetype><d:collection/><card:addressbook/></d:resourcetype><d:displayname>Contactos</d:displayname>\
<d:sync-token>tok-{}</d:sync-token>",
                    st.seq
                ))
            )),
        ),
        ("REPORT", p) if (p == CAL || p == BOOK) && req.body.contains("sync-collection") => {
            let token = between(&req.body, "<d:sync-token>", "</d:sync-token>").unwrap_or_default();
            let mut inner = String::new();
            let row = |href: &str, e: u64| {
                format!(
                    "<d:response><d:href>{href}</d:href>{}</d:response>",
                    ok_prop(&format!("<d:getetag>{}</d:getetag>", etag(e)))
                )
            };
            if token.is_empty() {
                for (href, (e, _)) in st.items.iter().filter(|(k, _)| k.starts_with(p)) {
                    inner.push_str(&row(href, *e));
                }
            } else {
                let Some(since) =
                    token.strip_prefix("tok-").and_then(|n| n.parse::<u64>().ok()).filter(|n| *n <= st.seq)
                else {
                    return Resp::xml(
                        403,
                        r#"<?xml version="1.0"?><d:error xmlns:d="DAV:"><d:valid-sync-token/></d:error>"#,
                    );
                };
                let mut last: BTreeMap<String, bool> = BTreeMap::new();
                for (s, href, deleted) in st.log.iter().filter(|(s, h, _)| *s > since && h.starts_with(p)) {
                    let _ = s;
                    last.insert(href.clone(), *deleted);
                }
                for (href, deleted) in last {
                    if deleted {
                        inner.push_str(&format!("<d:response><d:href>{href}</d:href><d:status>HTTP/1.1 404 Not Found</d:status></d:response>"));
                    } else if let Some((e, _)) = st.items.get(&href) {
                        inner.push_str(&row(&href, *e));
                    }
                }
            }
            inner.push_str(&format!("<d:sync-token>tok-{}</d:sync-token>", st.seq));
            Resp::xml(207, multistatus(&inner))
        }
        ("REPORT", p) if (p == CAL || p == BOOK) && req.body.contains("multiget") => {
            let tag = if p == CAL { "c:calendar-data" } else { "card:address-data" };
            let wanted: Vec<String> = req
                .body
                .split("<d:href>")
                .skip(1)
                .filter_map(|s| s.split("</d:href>").next())
                .map(str::to_string)
                .collect();
            let mut inner = String::new();
            for href in wanted {
                if let Some((e, data)) = st.items.get(&href) {
                    inner.push_str(&format!(
                        "<d:response><d:href>{href}</d:href>{}</d:response>",
                        ok_prop(&format!("<d:getetag>{}</d:getetag><{tag}>{}</{tag}>", etag(*e), xml_escape(data)))
                    ));
                }
            }
            Resp::xml(207, multistatus(&inner))
        }
        ("GET", p) => match st.items.get(p) {
            Some((e, data)) => {
                Resp::new(200, data.clone()).with("ETag", &etag(*e)).with("Content-Type", "text/calendar")
            }
            None => Resp::new(404, ""),
        },
        ("PUT", p) if coll_of(p).is_some() => {
            let current = st.items.get(p).map(|(e, _)| etag(*e));
            if let Some(m) = req.header("If-Match")
                && current.as_deref() != Some(m)
            {
                return Resp::new(412, "");
            }
            if req.header("If-None-Match") == Some("*") && current.is_some() {
                return Resp::new(412, "");
            }
            st.puts.push((p.to_string(), req.body.clone()));
            let n = st.change(p, Some(req.body.clone()));
            Resp::new(if current.is_some() { 204 } else { 201 }, "").with("ETag", &etag(n))
        }
        ("DELETE", p) => {
            let current = st.items.get(p).map(|(e, _)| etag(*e));
            if current.is_none() {
                return Resp::new(404, "");
            }
            if let Some(m) = req.header("If-Match")
                && current.as_deref() != Some(m)
            {
                return Resp::new(412, "");
            }
            st.change(p, None);
            Resp::new(204, "")
        }
        _ => Resp::new(405, ""),
    }
}

/// Arranca el servidor y devuelve (URL base, estado).
pub async fn start(state: State) -> (String, Arc<Mutex<State>>) {
    let st = Arc::new(Mutex::new(state));
    let h = Arc::clone(&st);
    let base = serve(move |req| handle(&h, req)).await;
    (base, st)
}
