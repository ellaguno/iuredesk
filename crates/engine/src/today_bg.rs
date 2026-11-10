//! UX-13: fondo de «Mi día» (lo pidió el dueño 2026-11-03): ninguno, un color, un degradado,
//! una imagen elegida o la **imagen del día de Wikimedia Commons**, que cambia cada día.
//!
//! La imagen del día se pide a la API de MediaWiki de Commons: el archivo que incluye
//! `Template:Potd/<AAAA-MM-DD>` (`generator=images`) con `imageinfo` (miniatura de
//! [`THUMB_WIDTH`] px, página del archivo y metadatos: autor y licencia, que se muestran siempre
//! porque las licencias libres lo piden). Si hoy aún no hay, la de ayer. Todo acotado: 10 s por
//! petición, [`MAX_JSON`] de respuesta, [`MAX_IMAGE`] de imagen (leída por trozos), solo
//! `https://` de `upload.wikimedia.org` o `thumb.wikimedia.org`. Se guarda en la caché (`<caché>/iuredesk/potd/`,
//! carpeta 0700, archivos 0600) y se borran las de más de [`KEEP_DAYS`] días; sin red se usa la
//! última que haya. Solo se pide si el usuario eligió esta opción.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::lang::Text;

/// API de Commons.
pub const COMMONS_API: &str = "https://commons.wikimedia.org/w/api.php";
/// Ancho de la miniatura que se pide.
pub const THUMB_WIDTH: u32 = 2560;
/// Respuesta JSON como mucho.
pub const MAX_JSON: usize = 512 * 1024;
/// Imagen como mucho.
pub const MAX_IMAGE: usize = 15 * 1024 * 1024;
/// Imagen elegida por el usuario como mucho.
pub const MAX_LOCAL_IMAGE: u64 = 25 * 1024 * 1024;
/// Días que se guardan en la caché.
pub const KEEP_DAYS: i64 = 7;
const TIMEOUT: Duration = Duration::from_secs(10);

/// Fondo elegido (Ajustes → Apariencia).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TodayBackground {
    /// El del tema.
    #[default]
    None,
    /// Un color.
    Color { rgb: [u8; 3] },
    /// Degradado en diagonal (arriba a la izquierda → abajo a la derecha).
    Gradient { from: [u8; 3], to: [u8; 3] },
    /// Una imagen del equipo.
    Image { path: String },
    /// La imagen del día de Wikimedia Commons.
    Commons,
}

impl TodayBackground {
    /// Nombre de la opción (para el selector).
    pub fn label(&self) -> &'static str {
        match self {
            TodayBackground::None => crate::lang::pick("Theme background", "Fondo del tema"),
            TodayBackground::Color { .. } => crate::lang::pick("Color", "Color"),
            TodayBackground::Gradient { .. } => crate::lang::pick("Gradient", "Degradado"),
            TodayBackground::Image { .. } => crate::lang::pick("Image", "Imagen"),
            TodayBackground::Commons => {
                crate::lang::pick("Wikimedia Commons picture of the day", "Imagen del día de Wikimedia Commons")
            }
        }
    }
}

/// Imagen del día ya descargada.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Potd {
    pub date: NaiveDate,
    /// Nombre del archivo en Commons (sin «File:»).
    pub title: String,
    /// Autor (texto, sin HTML).
    pub artist: String,
    /// Licencia corta («CC BY-SA 4.0»).
    pub license: String,
    /// Página del archivo en Commons (para el crédito).
    pub page_url: String,
    /// Dónde quedó la imagen en la caché.
    #[serde(skip)]
    pub file: PathBuf,
}

/// Resultado de [`crate::Session::request_today_image`] (llega por `Event::Custom`).
#[derive(Debug)]
pub struct TodayImage {
    /// Para qué fondo era (si cambió mientras tanto, se descarta).
    pub for_bg: TodayBackground,
    /// Bytes de la imagen y, en Commons, su crédito.
    pub result: Result<(Vec<u8>, Option<Potd>), Text>,
}

/// ¿Empieza como JPEG, PNG, GIF o WebP?
pub fn is_image(b: &[u8]) -> bool {
    b.starts_with(&[0xff, 0xd8, 0xff])
        || b.starts_with(b"\x89PNG\r\n\x1a\n")
        || b.starts_with(b"GIF8")
        || (b.starts_with(b"RIFF") && b.get(8..12) == Some(b"WEBP"))
}

/// URL de la API para el día.
pub fn api_url(base: &str, date: NaiveDate) -> String {
    format!(
        "{base}?action=query&format=json&formatversion=2&generator=images&gimlimit=5\
&titles=Template:Potd/{}&prop=imageinfo&iiprop=url%7Cmime%7Cextmetadata&iiurlwidth={THUMB_WIDTH}\
&iiextmetadatafilter=Artist%7CLicenseShortName",
        date.format("%Y-%m-%d")
    )
}

/// Texto sin etiquetas HTML ni espacios de más (el autor viene en HTML), acotado.
pub fn strip_html(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars().take(4_000) {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    let out = out.replace("&amp;", "&").replace("&quot;", "\"").replace("&#39;", "'").replace("&nbsp;", " ");
    out.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(160).collect()
}

/// Anfitriones de las imágenes de Commons (las miniaturas salen de `thumb.`).
const IMAGE_HOSTS: [&str; 2] = ["https://upload.wikimedia.org/", "https://thumb.wikimedia.org/"];

/// ¿Es un enlace de imagen de uno de `hosts` que se puede pedir?
fn upload_url(u: &str, hosts: &[&str]) -> bool {
    u.len() <= 2_048 && hosts.iter().any(|h| u.starts_with(h)) && !u.chars().any(char::is_whitespace)
}

/// (miniatura, crédito) de la respuesta de la API. `None` si no trae una imagen utilizable.
pub fn parse(json: &Value, date: NaiveDate) -> Option<(String, Potd)> {
    parse_with(json, date, &IMAGE_HOSTS)
}

/// Como [`parse`], con los anfitriones de imagen aceptados (pruebas con un servidor local).
pub fn parse_with(json: &Value, date: NaiveDate, hosts: &[&str]) -> Option<(String, Potd)> {
    let pages = json.get("query")?.get("pages")?.as_array()?;
    pages.iter().take(10).find_map(|p| {
        let info = p.get("imageinfo")?.as_array()?.first()?;
        let mime = info.get("mime").and_then(Value::as_str).unwrap_or("");
        if !matches!(mime, "image/jpeg" | "image/png" | "image/webp" | "image/gif") {
            return None;
        }
        let thumb = info.get("thumburl").and_then(Value::as_str).filter(|u| upload_url(u, hosts))?;
        let meta = info.get("extmetadata");
        let field = |k: &str| {
            meta.and_then(|m| m.get(k))
                .and_then(|f| f.get("value"))
                .and_then(Value::as_str)
                .map(strip_html)
                .unwrap_or_default()
        };
        let title = p.get("title").and_then(Value::as_str).unwrap_or("");
        let title = title.strip_prefix("File:").unwrap_or(title).chars().take(200).collect();
        let page_url = info
            .get("descriptionurl")
            .and_then(Value::as_str)
            .filter(|u| u.starts_with("https://commons.wikimedia.org/") && u.len() <= 2_048)
            .unwrap_or("https://commons.wikimedia.org/wiki/Commons:Picture_of_the_day")
            .to_string();
        Some((
            thumb.to_string(),
            Potd {
                date,
                title,
                artist: field("Artist"),
                license: field("LicenseShortName"),
                page_url,
                file: PathBuf::new(),
            },
        ))
    })
}

/// Carpeta de la caché.
pub fn cache_dir() -> PathBuf {
    crate::remote_images::cache_root().with_file_name("potd")
}

fn cached(dir: &Path, date: NaiveDate) -> Option<(Vec<u8>, Potd)> {
    let stem = date.format("%Y-%m-%d").to_string();
    let meta = std::fs::read(dir.join(format!("{stem}.json"))).ok()?;
    let mut p: Potd = serde_json::from_slice(&meta).ok()?;
    let file = dir.join(format!("{stem}.img"));
    let bytes = std::fs::read(&file).ok().filter(|b| !b.is_empty() && b.len() <= MAX_IMAGE)?;
    p.file = file;
    Some((bytes, p))
}

/// La más reciente de la caché (sin red).
fn latest_cached(dir: &Path) -> Option<(Vec<u8>, Potd)> {
    let mut days: Vec<NaiveDate> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            NaiveDate::parse_from_str(n.strip_suffix(".json")?, "%Y-%m-%d").ok()
        })
        .collect();
    days.sort_unstable();
    days.into_iter().rev().find_map(|d| cached(dir, d))
}

/// Borra lo de más de [`KEEP_DAYS`] días.
fn prune(dir: &Path, today: NaiveDate) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten().take(500) {
        let n = e.file_name().to_string_lossy().into_owned();
        let Some(d) = n.get(..10).and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()) else { continue };
        if (today - d).num_days() > KEEP_DAYS {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Lee un cuerpo por trozos, con tope.
pub(crate) async fn body_limited(resp: reqwest::Response, max: usize) -> Option<Vec<u8>> {
    if resp.content_length().is_some_and(|l| l > max as u64) {
        return None;
    }
    let mut resp = resp;
    let mut out = Vec::new();
    while let Ok(Some(chunk)) = resp.chunk().await {
        if out.len() + chunk.len() > max {
            return None;
        }
        out.extend_from_slice(&chunk);
    }
    Some(out)
}

fn unavailable() -> Text {
    Text::new(
        "Couldn't get today's picture from Wikimedia Commons (no connection?).",
        "No se pudo traer la imagen del día de Wikimedia Commons (¿sin conexión?).",
    )
}

/// La imagen del día de `today` (o de ayer): de la caché o de Commons. Sin red, la última guardada.
pub async fn fetch_potd(base: &str, today: NaiveDate, dir: &Path) -> Result<(Vec<u8>, Potd), Text> {
    fetch_potd_with(base, &IMAGE_HOSTS, today, dir).await
}

/// Como [`fetch_potd`], con los anfitriones de imagen aceptados (pruebas con un servidor local).
pub async fn fetch_potd_with(
    base: &str,
    hosts: &[&str],
    today: NaiveDate,
    dir: &Path,
) -> Result<(Vec<u8>, Potd), Text> {
    for d in [today, today - chrono::Duration::days(1)] {
        if let Some(hit) = cached(dir, d) {
            return Ok(hit);
        }
    }
    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(3))
        // Wikimedia pide un User-Agent que diga quién llama.
        .user_agent(concat!("IureDesk/", env!("CARGO_PKG_VERSION"), " (https://iurefficient.com; desktop PIM)"))
        .build()
        .map_err(|_| unavailable())?;
    for d in [today, today - chrono::Duration::days(1)] {
        let Ok(resp) = client.get(api_url(base, d)).send().await else { continue };
        if !resp.status().is_success() {
            continue;
        }
        let Some(body) = body_limited(resp, MAX_JSON).await else { continue };
        let Ok(json) = serde_json::from_slice::<Value>(&body) else { continue };
        let Some((thumb, mut potd)) = parse_with(&json, d, hosts) else { continue };
        let Ok(img) = client.get(&thumb).send().await else { continue };
        if !img.status().is_success() || !upload_url(img.url().as_str(), hosts) {
            continue;
        }
        let Some(bytes) = body_limited(img, MAX_IMAGE).await.filter(|b| !b.is_empty()) else { continue };
        if !is_image(&bytes) {
            continue;
        }
        if crate::remote_images::private_dir(dir).is_ok() {
            let stem = d.format("%Y-%m-%d").to_string();
            let file = dir.join(format!("{stem}.img"));
            if crate::remote_images::write_private(&file, &bytes).is_ok()
                && let Ok(meta) = serde_json::to_vec(&potd)
            {
                let _ = crate::remote_images::write_private(&dir.join(format!("{stem}.json")), &meta);
                potd.file = file;
            }
            prune(dir, today);
        }
        return Ok((bytes, potd));
    }
    latest_cached(dir).ok_or_else(unavailable)
}

/// Bytes de una imagen elegida (acotada; solo archivos normales).
pub fn read_local_image(path: &str) -> Result<Vec<u8>, Text> {
    let bad = || Text::new("Couldn't open that image.", "No se pudo abrir esa imagen.");
    let meta = std::fs::metadata(path).map_err(|_| bad())?;
    if !meta.is_file() || meta.len() > MAX_LOCAL_IMAGE {
        return Err(Text::new(
            "That image is too big (25 MB at most) or isn't a file.",
            "Esa imagen es muy grande (25 MB como mucho) o no es un archivo.",
        ));
    }
    let bytes = std::fs::read(path).map_err(|_| bad())?;
    if !is_image(&bytes) {
        return Err(bad());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, 3).unwrap()
    }

    fn sample() -> Value {
        serde_json::json!({ "query": { "pages": [{
            "title": "File:Mariposa monarca.jpg",
            "imageinfo": [{
                "mime": "image/jpeg",
                "thumburl": "https://upload.wikimedia.org/wikipedia/commons/thumb/a/ab/M.jpg/2560px-M.jpg",
                "descriptionurl": "https://commons.wikimedia.org/wiki/File:Mariposa_monarca.jpg",
                "extmetadata": {
                    "Artist": { "value": "<a href=\"//x\">Ana&nbsp;Ruiz</a>" },
                    "LicenseShortName": { "value": "CC BY-SA 4.0" }
                }
            }]
        }]}})
    }

    #[test]
    fn parses_the_picture_and_its_credit() {
        let (thumb, p) = parse(&sample(), day()).unwrap();
        assert!(thumb.ends_with("2560px-M.jpg"));
        assert_eq!(
            (p.title.as_str(), p.artist.as_str(), p.license.as_str()),
            ("Mariposa monarca.jpg", "Ana Ruiz", "CC BY-SA 4.0")
        );
        assert!(p.page_url.starts_with("https://commons.wikimedia.org/"));
    }

    #[test]
    fn rejects_hostile_answers() {
        let mut v = sample();
        v["query"]["pages"][0]["imageinfo"][0]["thumburl"] = "http://evil.example/x.jpg".into();
        assert!(parse(&v, day()).is_none(), "solo los anfitriones de Wikimedia");
        let mut v = sample();
        v["query"]["pages"][0]["imageinfo"][0]["thumburl"] =
            "https://thumb.wikimedia.org/wikipedia/commons/thumb/a/x.jpg".into();
        assert!(parse(&v, day()).is_some(), "la API da hoy las miniaturas en thumb.");
        let mut v = sample();
        v["query"]["pages"][0]["imageinfo"][0]["mime"] = "image/svg+xml".into();
        assert!(parse(&v, day()).is_none(), "nada de SVG ni vídeo");
        let mut v = sample();
        v["query"]["pages"][0]["imageinfo"][0]["descriptionurl"] = "javascript:alert(1)".into();
        assert!(parse(&v, day()).unwrap().1.page_url.starts_with("https://commons.wikimedia.org/"));
        assert!(parse(&serde_json::json!({}), day()).is_none());
        assert!(parse(&serde_json::json!({ "query": { "pages": "x" } }), day()).is_none());
    }

    #[test]
    fn api_url_names_the_day() {
        let u = api_url(COMMONS_API, day());
        assert!(u.starts_with("https://commons.wikimedia.org/w/api.php?") && u.contains("Template:Potd/2026-11-03"));
        assert!(u.contains("iiurlwidth=2560"));
    }

    #[test]
    fn strip_html_is_bounded() {
        assert_eq!(strip_html("<b>Hola</b> &amp; <i>adiós</i>"), "Hola & adiós");
        assert_eq!(strip_html(&"x".repeat(10_000)).len(), 160);
    }

    #[test]
    fn cache_roundtrip_and_prune() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().join("potd");
        crate::remote_images::private_dir(&d).unwrap();
        let (_, mut p) = parse(&sample(), day()).unwrap();
        p.file = PathBuf::new();
        let png = b"\x89PNG\r\n\x1a\n0000".to_vec();
        std::fs::write(d.join("2026-11-03.img"), &png).unwrap();
        std::fs::write(d.join("2026-11-03.json"), serde_json::to_vec(&p).unwrap()).unwrap();
        std::fs::write(d.join("2026-10-01.img"), &png).unwrap();
        std::fs::write(d.join("2026-10-01.json"), serde_json::to_vec(&p).unwrap()).unwrap();
        let (bytes, got) = cached(&d, day()).unwrap();
        assert_eq!((bytes, got.artist.as_str()), (png.clone(), "Ana Ruiz"));
        prune(&d, day());
        assert!(!d.join("2026-10-01.img").exists() && d.join("2026-11-03.img").exists());
        assert_eq!(latest_cached(&d).unwrap().1.date, day());
    }

    #[test]
    fn prefs_round_trip() {
        for bg in [
            TodayBackground::None,
            TodayBackground::Color { rgb: [1, 2, 3] },
            TodayBackground::Gradient { from: [0, 0, 0], to: [255, 255, 255] },
            TodayBackground::Image { path: "/x/y.jpg".into() },
            TodayBackground::Commons,
        ] {
            let s = serde_json::to_string(&bg).unwrap();
            assert_eq!(serde_json::from_str::<TodayBackground>(&s).unwrap(), bg);
        }
    }

    #[test]
    fn local_images_are_checked() {
        let dir = tempfile::tempdir().unwrap();
        let ok = dir.path().join("a.png");
        std::fs::write(&ok, b"\x89PNG\r\n\x1a\n0000").unwrap();
        assert!(read_local_image(&ok.to_string_lossy()).is_ok());
        let txt = dir.path().join("a.txt");
        std::fs::write(&txt, b"hola").unwrap();
        assert!(read_local_image(&txt.to_string_lossy()).is_err());
        assert!(read_local_image(&dir.path().to_string_lossy()).is_err());
        assert!(read_local_image("/no/existe.png").is_err());
    }
}
