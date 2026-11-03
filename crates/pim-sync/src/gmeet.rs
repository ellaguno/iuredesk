//! UX-11: reunión de Google Meet para un evento de Google (CalDAV no sabe crearlas).
//!
//! El evento se sube por CalDAV como siempre; si pide una reunión (`online_meeting` de Meet sin
//! enlace), se busca en la API de Calendar (`GET /calendars/{id}/events?iCalUID=`) y se le pide
//! la sala (`PATCH …?conferenceDataVersion=1&sendUpdates=all` con `conferenceData.createRequest`
//! `hangoutsMeet`): los invitados reciben el enlace. El `requestId` sale del `UID`, así que un
//! reintento no crea otra sala. El enlace se guarda en el evento local; la próxima pasada de
//! CalDAV lo trae también (`X-GOOGLE-CONFERENCE`).
//!
//! Alcance `https://www.googleapis.com/auth/calendar` (el mismo de CalDAV). Si Google lo rechaza
//! (sin permiso, API apagada en el proyecto…), se quita la petición y se avisa en palabras; un
//! fallo de red se reintenta con la operación.

use iuredesk_graph::{Body, GraphClient, enc};
use iuredesk_model::{Calendar, Event, EventId, MeetingProvider};
use iuredesk_store::CollectionRef;
use reqwest::Method;
use serde_json::{Value, json};

use crate::error::{PimError, Result};
use crate::provider::Ctx;
use crate::{FriendlyError, PimEvent};

/// Raíz de la API de Google Calendar.
pub const GOOGLE_CALENDAR_BASE: &str = "https://www.googleapis.com/calendar/v3";

pub(crate) struct GoogleMeet {
    client: GraphClient,
}

/// Decodifica `%XX` de un segmento de ruta (lo demás tal cual; un `%` suelto se queda).
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while let Some(&c) = b.get(i) {
        let hex = |k: usize| b.get(k).and_then(|h| (*h as char).to_digit(16));
        match (c, hex(i + 1), hex(i + 2)) {
            (b'%', Some(h), Some(l)) => {
                out.push(u8::try_from(h * 16 + l).unwrap_or(b'?'));
                i += 3;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Id de calendario de Google en la ruta de su colección CalDAV
/// (`…/caldav/v2/{id}/events/`). `None` si la ruta no tiene esa forma.
pub(crate) fn calendar_id(remote_path: &str) -> Option<String> {
    let url = reqwest::Url::parse(remote_path.trim()).ok()?;
    let segs: Vec<&str> = url.path_segments()?.collect();
    let i = segs.windows(2).position(|w| w.first() == Some(&"caldav") && w.get(1) == Some(&"v2"))?;
    let id = segs.get(i + 2).filter(|s| !s.is_empty())?;
    (segs.get(i + 3) == Some(&"events")).then(|| percent_decode(id))
}

/// Enlace de la sala en la respuesta de la API (`hangoutLink` o el punto de entrada de vídeo).
pub(crate) fn join_link(v: &Value) -> Option<String> {
    let https = |u: &str| u.get(..8).is_some_and(|p| p.eq_ignore_ascii_case("https://")) && u.len() <= 2_048;
    v.get("hangoutLink").and_then(Value::as_str).filter(|u| https(u)).map(str::to_string).or_else(|| {
        v.get("conferenceData")?
            .get("entryPoints")?
            .as_array()?
            .iter()
            .filter(|e| e.get("entryPointType").and_then(Value::as_str) == Some("video"))
            .find_map(|e| e.get("uri").and_then(Value::as_str).filter(|u| https(u)).map(str::to_string))
    })
}

/// `requestId` estable para el evento (solo letras y números, como pide Google; ≤ 64).
fn request_id(uid: &str) -> String {
    let clean: String = uid.chars().filter(char::is_ascii_alphanumeric).take(48).collect();
    format!("iuredesk{clean}")
}

impl GoogleMeet {
    pub fn new(ctx: &Ctx, base: &str) -> Result<Self> {
        let client = GraphClient::new(ctx.account.account.clone(), ctx.creds.tokens()?, base, ctx.opts.timeout)?;
        Ok(Self { client })
    }

    /// Si el evento pide una sala de Meet y aún no la tiene, la crea.
    pub async fn ensure(&self, ctx: &Ctx, id: EventId) -> Result<()> {
        let store = &ctx.store;
        let Some(ev) = store.get_event(id)? else { return Ok(()) };
        if !wants_meet(&ev) {
            return Ok(());
        }
        let Some(cal) = store.get_calendar(ev.calendar)? else { return Ok(()) };
        let Some(cal_id) = calendar_id(&cal.remote_path) else {
            return self.give_up(ctx, &ev, &cal, "el calendario no es de Google");
        };
        let path = format!("/calendars/{}/events", enc(&cal_id));
        let list = self.client.url(&format!("{path}?iCalUID={}&maxResults=1", enc(&ev.uid)));
        let found = self.client.request(Method::GET, &list, None, &[]).await?;
        if !found.is_success() {
            return self.fail(ctx, &ev, &cal, found.status, &found.error().to_string());
        }
        let Some(gid) = found
            .body
            .get("items")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(|e| e.get("id"))
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            // Aún no aparece (consistencia eventual): otra vuelta.
            return Err(PimError::Network("Google aún no muestra el evento en su API".into()));
        };
        let body = json!({
            "conferenceData": {
                "createRequest": {
                    "requestId": request_id(&ev.uid),
                    "conferenceSolutionKey": { "type": "hangoutsMeet" }
                }
            }
        });
        let url = self.client.url(&format!("{path}/{}?conferenceDataVersion=1&sendUpdates=all", enc(&gid)));
        let r = self.client.request(Method::PATCH, &url, Some(Body::Json(&body)), &[]).await?;
        if !r.is_success() {
            return self.fail(ctx, &ev, &cal, r.status, &r.error().to_string());
        }
        // Sin enlace todavía (`createRequest.status = pending`): lo trae la próxima pasada.
        if let Some(link) = join_link(&r.body)
            && let Some(mut current) = store.get_event(id)?
            && wants_meet(&current)
            && let Some(m) = current.online_meeting.as_mut()
        {
            m.join_url = link;
            store.put_event(&current)?;
            ctx.changed(CollectionRef::Calendar(current.calendar));
        }
        Ok(())
    }

    /// 4xx definitivo: se quita la petición y se dice por qué. 5xx/429: se reintenta.
    fn fail(&self, ctx: &Ctx, ev: &Event, cal: &Calendar, status: u16, detail: &str) -> Result<()> {
        if status == 429 || status >= 500 {
            return Err(PimError::Network(format!("Google Calendar {status}: {detail}")));
        }
        self.give_up(ctx, ev, cal, &format!("{status}: {detail}"))
    }

    fn give_up(&self, ctx: &Ctx, ev: &Event, cal: &Calendar, detail: &str) -> Result<()> {
        if let Some(mut current) = ctx.store.get_event(ev.id)? {
            current.online_meeting = None;
            ctx.store.put_event(&current)?;
            ctx.changed(CollectionRef::Calendar(current.calendar));
        }
        ctx.emit(PimEvent::Error(FriendlyError {
            account: Some(ctx.acct),
            es: format!(
                "No se pudo crear la reunión de Google Meet para «{}» en «{}». La cita quedó guardada sin videollamada.",
                ev.summary, cal.name
            ),
            en: format!(
                "Couldn't create the Google Meet for «{}» in «{}». The event was saved without a video call.",
                ev.summary, cal.name
            ),
            transient: false,
            needs_reauth: false,
            detail: detail.chars().take(500).collect(),
        }));
        Ok(())
    }
}

/// ¿Pide una sala de Meet que aún no tiene?
pub(crate) fn wants_meet(ev: &Event) -> bool {
    ev.online_meeting.as_ref().is_some_and(|m| m.provider == MeetingProvider::GoogleMeet && m.is_pending())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calendar_id_from_caldav_paths() {
        assert_eq!(
            calendar_id("https://apidata.googleusercontent.com/caldav/v2/ana%40gmail.com/events/").as_deref(),
            Some("ana@gmail.com")
        );
        assert_eq!(
            calendar_id("http://127.0.0.1:9/caldav/v2/abc123%40group.calendar.google.com/events").as_deref(),
            Some("abc123@group.calendar.google.com")
        );
        assert_eq!(calendar_id("https://dav.example.com/cal/ana/"), None);
        assert_eq!(calendar_id("https://x/caldav/v2//events/"), None);
        assert_eq!(calendar_id("no es url"), None);
    }

    #[test]
    fn decoding_is_bounded_and_lossless_for_plain_text() {
        assert_eq!(percent_decode("a%2Fb%zz%"), "a/b%zz%");
        assert_eq!(percent_decode("simple"), "simple");
    }

    #[test]
    fn join_link_prefers_hangout_and_needs_https() {
        let v = json!({ "hangoutLink": "https://meet.google.com/abc-defg-hij" });
        assert_eq!(join_link(&v).as_deref(), Some("https://meet.google.com/abc-defg-hij"));
        let v = json!({ "conferenceData": { "entryPoints": [
            { "entryPointType": "phone", "uri": "tel:+1-555" },
            { "entryPointType": "video", "uri": "https://meet.google.com/xyz" }
        ]}});
        assert_eq!(join_link(&v).as_deref(), Some("https://meet.google.com/xyz"));
        assert_eq!(join_link(&json!({ "hangoutLink": "javascript:alert(1)" })), None);
        assert_eq!(join_link(&json!({})), None);
    }

    #[test]
    fn request_id_is_stable_and_clean() {
        assert_eq!(request_id("abc-123@iuredesk"), request_id("abc-123@iuredesk"));
        assert!(request_id(&"é@-".repeat(100)).chars().all(|c| c.is_ascii_alphanumeric()));
        assert!(request_id(&"a".repeat(500)).len() <= 64);
    }
}
