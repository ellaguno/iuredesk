//! Operaciones sobre recursos: `GET`, `PUT`, `DELETE`, `calendar-query`, `multiget` y free/busy.

use chrono::{DateTime, Utc};
use reqwest::Url;

use super::CollectionKind;
use super::client::{DavClient, Depth, XML, status_error};
use super::xml::{self, CALDAV, CARDDAV, DAV, Multistatus, escape};
use crate::calendar::{ICalendar, strip_mailto};
use crate::value::{self, DateTimeValue, format_utc};
use crate::{Error, Result};

/// Un objeto del servidor (`.ics` o `.vcf`) con su ETag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resource {
    /// URL absoluta.
    pub url: Url,
    /// ETag (con comillas, tal cual).
    pub etag: Option<String>,
    /// Contenido, si se pidió.
    pub data: Option<String>,
}

/// Condición para `PUT`/`DELETE`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Precondition {
    /// Sin condición (sobrescribe).
    None,
    /// Solo si no existe (`If-None-Match: *`): creación.
    Create,
    /// Solo si sigue en esta versión (`If-Match: "etag"`): edición sin pisar cambios ajenos.
    IfMatch(String),
}

/// Resultado de un `PUT`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PutResult {
    /// URL final del recurso.
    pub url: Url,
    /// ETag nuevo, si el servidor lo devolvió (si no, hay que volver a leerlo).
    pub etag: Option<String>,
}

/// Intervalo ocupado.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BusyPeriod {
    /// Inicio.
    pub start: DateTime<Utc>,
    /// Fin.
    pub end: DateTime<Utc>,
    /// `FBTYPE` (`BUSY`, `BUSY-TENTATIVE`, `BUSY-UNAVAILABLE`, `FREE`).
    pub kind: String,
}

/// Respuesta de disponibilidad de un destinatario (RFC 6638 §6.1.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FreeBusyReply {
    /// Correo del destinatario.
    pub recipient: String,
    /// `request-status` (`2.0;Success`, `3.7;Invalid calendar user`…).
    pub request_status: String,
    /// Intervalos ocupados.
    pub busy: Vec<BusyPeriod>,
}

/// Máximo de `href` por `multiget` y destinatarios por consulta de disponibilidad.
const MAX_FREEBUSY_ATTENDEES: usize = 200;
const MAX_BUSY_PERIODS: usize = 10_000;

impl DavClient {
    /// `GET` de un objeto.
    pub async fn get(&self, url: &Url) -> Result<Resource> {
        let resp = self.request("GET", url, None, &[], None).await?;
        if !resp.is_success() {
            return Err(status_error(&resp, "GET"));
        }
        Ok(Resource {
            url: resp.url.clone(),
            etag: resp.etag.clone(),
            data: Some(String::from_utf8_lossy(&resp.body).into_owned()),
        })
    }

    /// `PUT` de un `.ics` (`text/calendar`) o `.vcf` (`text/vcard`). Con
    /// [`Precondition::IfMatch`] un 412 se devuelve como [`Error::PreconditionFailed`]: alguien
    /// cambió el objeto; hay que releerlo y fusionar.
    pub async fn put(&self, url: &Url, body: &str, kind: CollectionKind, cond: Precondition) -> Result<PutResult> {
        let ct = match kind {
            CollectionKind::Calendar => "text/calendar; charset=utf-8",
            CollectionKind::AddressBook => "text/vcard; charset=utf-8",
        };
        let headers = precondition_headers(&cond);
        let resp = self.request("PUT", url, None, &headers, Some((ct, body.as_bytes().to_vec()))).await?;
        if !resp.is_success() {
            return Err(status_error(&resp, "PUT"));
        }
        let final_url = resp.location.as_deref().and_then(|l| resp.url.join(l).ok()).unwrap_or(resp.url.clone());
        Ok(PutResult { url: final_url, etag: resp.etag })
    }

    /// `DELETE`. Si ya no existía (404/410) se toma como hecho.
    pub async fn delete(&self, url: &Url, cond: Precondition) -> Result<()> {
        let headers = precondition_headers(&cond);
        let resp = self.request("DELETE", url, None, &headers, None).await?;
        if resp.is_success() || resp.status == 404 || resp.status == 410 {
            Ok(())
        } else {
            Err(status_error(&resp, "DELETE"))
        }
    }

    /// `calendar-query` (RFC 4791 §7.8) de `component` (`VEVENT`, `VTODO`) que toca el rango.
    /// El servidor expande nada: devuelve los objetos completos (serie + excepciones).
    pub async fn calendar_query(
        &self,
        calendar: &Url,
        component: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<Resource>> {
        let body = format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<c:calendar-query xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
<d:prop><d:getetag/><c:calendar-data/></d:prop>
<c:filter><c:comp-filter name="VCALENDAR"><c:comp-filter name="{}">
<c:time-range start="{}" end="{}"/></c:comp-filter></c:comp-filter></c:filter>
</c:calendar-query>"#,
            escape(&component.to_ascii_uppercase()),
            format_utc(start),
            format_utc(end)
        );
        let (base, ms) = self.report(calendar, Depth::One, &body).await?;
        Ok(resources(&base, &ms, CollectionKind::Calendar))
    }

    /// `calendar-multiget` / `addressbook-multiget` en lotes de [`super::DavOptions::multiget_batch`].
    /// Los `href` que el servidor no encuentra se omiten.
    pub async fn multiget(&self, collection: &Url, kind: CollectionKind, hrefs: &[Url]) -> Result<Vec<Resource>> {
        let mut out = Vec::new();
        let batch = self.opts.multiget_batch.max(1);
        for chunk in hrefs.chunks(batch) {
            let (root, data_el) = match kind {
                CollectionKind::Calendar => {
                    (r#"c:calendar-multiget xmlns:c="urn:ietf:params:xml:ns:caldav""#, "c:calendar-data")
                }
                CollectionKind::AddressBook => {
                    (r#"c:addressbook-multiget xmlns:c="urn:ietf:params:xml:ns:carddav""#, "c:address-data")
                }
            };
            let mut body = format!(
                r#"<?xml version="1.0" encoding="utf-8"?><{root} xmlns:d="DAV:"><d:prop><d:getetag/><{data_el}/></d:prop>"#
            );
            for h in chunk {
                body.push_str(&format!("<d:href>{}</d:href>", escape(h.path())));
            }
            let close = root.split(' ').next().unwrap_or_default();
            body.push_str(&format!("</{close}>"));
            let (base, ms) = self.report(collection, Depth::One, &body).await?;
            out.extend(resources(&base, &ms, kind));
        }
        Ok(out)
    }

    /// Disponibilidad por el buzón de salida de agenda (RFC 6638 §3.2.1): un `POST` con un
    /// `VFREEBUSY` `METHOD:REQUEST`. Es la vía de iCloud, Nextcloud, Fastmail y Stalwart.
    pub async fn free_busy(
        &self,
        outbox: &Url,
        organizer: &str,
        attendees: &[&str],
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        uid: &str,
    ) -> Result<Vec<FreeBusyReply>> {
        if attendees.is_empty() || attendees.len() > MAX_FREEBUSY_ATTENDEES {
            return Err(Error::Limit(format!("entre 1 y {MAX_FREEBUSY_ATTENDEES} destinatarios")));
        }
        let mut cal = ICalendar::new();
        cal.root.push(crate::ical::Property::new("METHOD", "REQUEST"));
        let mut fb = crate::ical::Component::new("VFREEBUSY");
        fb.push(crate::ical::Property::text("UID", uid));
        fb.push(crate::ical::Property::new("DTSTAMP", &format_utc(Utc::now())));
        fb.push(crate::ical::Property::new("DTSTART", &format_utc(start)));
        fb.push(crate::ical::Property::new("DTEND", &format_utc(end)));
        fb.push(crate::ical::Property::new("ORGANIZER", &format!("mailto:{}", strip_mailto(organizer))));
        for a in attendees {
            fb.push(crate::ical::Property::new("ATTENDEE", &format!("mailto:{}", strip_mailto(a))));
        }
        cal.root.components.push(fb);
        let headers = [
            ("Originator", format!("mailto:{}", strip_mailto(organizer))),
            (
                "Recipient",
                attendees.iter().map(|a| format!("mailto:{}", strip_mailto(a))).collect::<Vec<_>>().join(", "),
            ),
        ];
        let resp = self
            .request("POST", outbox, None, &headers, Some(("text/calendar; charset=utf-8", cal.to_ics().into_bytes())))
            .await?;
        if !resp.is_success() {
            return Err(status_error(&resp, "POST free/busy"));
        }
        let root = xml::parse(&resp.body)?;
        if !root.is(CALDAV, "schedule-response") {
            return Err(Error::Xml("se esperaba schedule-response".into()));
        }
        let mut out = Vec::new();
        for r in root.children_named(CALDAV, "response") {
            let recipient = r
                .child(CALDAV, "recipient")
                .and_then(|x| x.href().or_else(|| Some(x.text_trim().to_string())))
                .map(|h| strip_mailto(&h))
                .unwrap_or_default();
            let request_status =
                r.child(CALDAV, "request-status").map(|s| s.text_trim().to_string()).unwrap_or_default();
            let busy = match r.child(CALDAV, "calendar-data").map(|d| d.text.as_str()) {
                Some(data) if !data.trim().is_empty() => busy_periods(&ICalendar::parse(data)?),
                _ => Vec::new(),
            };
            out.push(FreeBusyReply { recipient, request_status, busy });
        }
        Ok(out)
    }

    /// Disponibilidad propia de un calendario con `free-busy-query` (RFC 4791 §7.10), para
    /// servidores sin buzón de salida (Radicale, Google).
    pub async fn free_busy_query(
        &self,
        calendar: &Url,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<BusyPeriod>> {
        let body = format!(
            r#"<?xml version="1.0" encoding="utf-8"?><c:free-busy-query xmlns:c="urn:ietf:params:xml:ns:caldav"><c:time-range start="{}" end="{}"/></c:free-busy-query>"#,
            format_utc(start),
            format_utc(end)
        );
        let resp = self.request("REPORT", calendar, Some(Depth::One), &[], Some((XML, body.into_bytes()))).await?;
        if !resp.is_success() {
            return Err(match resp.status {
                403 | 405 | 501 => Error::Unsupported("free-busy-query"),
                _ => status_error(&resp, "REPORT free-busy-query"),
            });
        }
        Ok(busy_periods(&ICalendar::parse_bytes(&resp.body)?))
    }
}

fn precondition_headers(cond: &Precondition) -> Vec<(&'static str, String)> {
    match cond {
        Precondition::None => Vec::new(),
        Precondition::Create => vec![("If-None-Match", "*".to_string())],
        Precondition::IfMatch(etag) => vec![("If-Match", etag.clone())],
    }
}

/// Recursos de un `multistatus` de `REPORT` (se omiten los 404 y el propio contenedor).
pub(crate) fn resources(base: &Url, ms: &Multistatus, kind: CollectionKind) -> Vec<Resource> {
    let (ns, data) = match kind {
        CollectionKind::Calendar => (CALDAV, "calendar-data"),
        CollectionKind::AddressBook => (CARDDAV, "address-data"),
    };
    ms.responses
        .iter()
        .filter(|r| r.ok())
        .filter_map(|r| {
            let url = base.join(&r.href).ok()?;
            let etag = r.prop_text(DAV, "getetag");
            let data = r.prop(ns, data).map(|d| d.text.clone()).filter(|d| !d.trim().is_empty());
            (etag.is_some() || data.is_some()).then_some(Resource { url, etag, data })
        })
        .collect()
}

/// Intervalos `FREEBUSY` de los `VFREEBUSY` de un calendario.
pub fn busy_periods(cal: &ICalendar) -> Vec<BusyPeriod> {
    let mut out = Vec::new();
    for fb in cal.root.children("VFREEBUSY") {
        for p in fb.props("FREEBUSY") {
            let kind = p.param("FBTYPE").unwrap_or("BUSY").to_ascii_uppercase();
            for period in p.value.split(',') {
                if out.len() >= MAX_BUSY_PERIODS {
                    return out;
                }
                let Some((a, b)) = period.trim().split_once('/') else { continue };
                let Ok(DateTimeValue::Utc(start)) = value::parse_date_time(a, None, None) else { continue };
                let end = match value::parse_date_time(b, None, None) {
                    Ok(DateTimeValue::Utc(e)) => e,
                    _ => match value::parse_duration(b) {
                        Ok(d) => start.checked_add_signed(d).unwrap_or(start),
                        Err(_) => continue,
                    },
                };
                out.push(BusyPeriod { start, end, kind: kind.clone() });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_parsing() {
        let cal = ICalendar::parse(
            "BEGIN:VCALENDAR\r\nBEGIN:VFREEBUSY\r\nFREEBUSY;FBTYPE=BUSY-TENTATIVE:20261110T150000Z/20261110T160000Z,\
20261111T150000Z/PT30M\r\nFREEBUSY:basura\r\nEND:VFREEBUSY\r\nEND:VCALENDAR\r\n",
        )
        .unwrap();
        let b = busy_periods(&cal);
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].kind, "BUSY-TENTATIVE");
        assert_eq!((b[1].end - b[1].start).num_minutes(), 30);
    }
}
