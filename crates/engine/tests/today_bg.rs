//! UX-13 de extremo a extremo contra un Commons falso: la imagen del día con su crédito, la
//! caché (sin volver a pedir), «ayer» si hoy aún no hay, respuestas hostiles y sin red la última
//! guardada.

#[path = "../../dav/tests/common/mod.rs"]
mod common;

use std::sync::{Arc, Mutex};

use chrono::NaiveDate;
use common::{Req, Resp, serve};
use iuredesk_engine::today_bg::fetch_potd_with;

/// Cabecera GIF (ASCII: el servidor de pruebas manda texto).
const IMG: &[u8] = b"GIF89a-falso";

fn day(d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, d).unwrap()
}

#[derive(Default)]
struct S {
    requests: Vec<String>,
    /// Días que tienen imagen.
    days: Vec<String>,
    base: String,
    /// La imagen devuelve esto (para probar lo hostil).
    image: Option<Vec<u8>>,
}

fn route(st: &Mutex<S>, req: Req) -> Resp {
    let mut s = st.lock().unwrap();
    s.requests.push(req.path.clone());
    if req.path.starts_with("/w/api.php") {
        let has = s.days.iter().any(|d| req.path.contains(&format!("Template:Potd/{d}")));
        if !has {
            return Resp::new(200, r#"{"batchcomplete":true}"#).with("Content-Type", "application/json");
        }
        let body = serde_json::json!({ "query": { "pages": [{
            "title": "File:Ballesta.jpg",
            "imageinfo": [{
                "mime": "image/jpeg",
                "thumburl": format!("{}/thumb/Ballesta.jpg", s.base),
                "descriptionurl": "https://commons.wikimedia.org/wiki/File:Ballesta.jpg",
                "extmetadata": { "Artist": { "value": "<bdi><a href=\"x\">Diego Delso</a></bdi>" },
                                 "LicenseShortName": { "value": "CC BY-SA 4.0" } }
            }]
        }]}});
        return Resp::new(200, body.to_string()).with("Content-Type", "application/json");
    }
    if req.path == "/thumb/Ballesta.jpg" {
        let b = s.image.clone().unwrap_or_else(|| IMG.to_vec());
        return Resp::new(200, String::from_utf8_lossy(&b).into_owned()).with("Content-Type", "image/jpeg");
    }
    Resp::new(404, "")
}

#[test]
fn picture_of_the_day_with_credit_cache_and_fallbacks() {
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    rt.block_on(async {
        let st = Arc::new(Mutex::new(S { days: vec!["2026-11-03".into()], ..Default::default() }));
        let h = Arc::clone(&st);
        let base = serve(move |r| route(&h, r)).await;
        st.lock().unwrap().base = base.clone();
        let host = format!("{base}/");
        let api = format!("{base}/w/api.php");
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("potd");

        // Hoy: imagen y crédito; queda en la caché.
        let (bytes, p) = fetch_potd_with(&api, &[&host], day(3), &cache).await.unwrap();
        assert_eq!(bytes, IMG);
        assert_eq!(
            (p.artist.as_str(), p.license.as_str(), p.title.as_str()),
            ("Diego Delso", "CC BY-SA 4.0", "Ballesta.jpg")
        );
        assert!(cache.join("2026-11-03.img").is_file());
        // Otra vez: de la caché, sin red.
        let n = st.lock().unwrap().requests.len();
        fetch_potd_with(&api, &[&host], day(3), &cache).await.unwrap();
        assert_eq!(st.lock().unwrap().requests.len(), n, "no se vuelve a pedir");

        // Mañana aún no hay: se usa la de ayer (la de hoy, ya guardada).
        let (_, p) = fetch_potd_with(&api, &[&host], day(4), &cache).await.unwrap();
        assert_eq!(p.date, day(3));

        // Una «imagen» que no lo es no se acepta y, sin otra, se usa la última guardada.
        st.lock().unwrap().days = vec!["2026-11-10".into()];
        st.lock().unwrap().image = Some(b"<html>no</html>".to_vec());
        let (_, p) = fetch_potd_with(&api, &[&host], day(10), &cache).await.unwrap();
        assert_eq!(p.date, day(3), "la última buena");
        assert!(!cache.join("2026-11-10.img").exists());

        // Una miniatura de otro anfitrión no se pide.
        let empty = dir.path().join("vacia");
        let r = fetch_potd_with(&api, &["https://upload.wikimedia.org/"], day(10), &empty).await;
        assert!(r.is_err(), "sin imagen ni caché: error con texto");
        assert!(r.unwrap_err().es.contains("Wikimedia Commons"));
    });
}
