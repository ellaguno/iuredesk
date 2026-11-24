//! S1.36: noticias en el encabezado de «Mi día» (lo pidió el dueño 2026-11-23): de cada fuente,
//! sus últimos 1 a 3 artículos (lo elige el usuario); el que tiene audio lleva ▶, los demás solo
//! el título con su liga.
//!
//! - **Fuentes por omisión**, en este orden (lo pidió el dueño 2026-11-24): los artículos de
//!   Iurefficient (feed RSS de `iurefficient.com/articulos`), SesoLibre (noticias generales) y el
//!   blog de Iurefficient (noticias jurídicas). Estas dos son WordPress; la lista sale de su API
//!   REST (`wp/v2/posts`), porque SesoLibre tiene el RSS apagado. La categoría «Noticias» de cada
//!   una (14 y 46) es la del resumen con audio.
//! - **El audio** no está en la API ni en el feed: lo pone en la página el reproductor del plugin
//!   de TTS (`<div class="tts-enhanced-sesolibre-player" data-main-audio="….mp3">`). Se lee al
//!   pulsar ▶, se acepta solo un MP3 del mismo sitio y se guarda en caché.
//! - **Otras fuentes**: un sitio WordPress (solo títulos) o un feed RSS/Atom cualquiera, cuyo
//!   `<enclosure>` de audio también se reproduce (pódcasts).
//!
//! Todo es dato hostil: tiempos de espera, tamaños acotados, textos recortados, URL comprobadas.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, NaiveDateTime, Utc};
use quick_xml::events::Event as XmlEvent;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::lang::{Text, pick};

/// Lista de artículos (JSON o XML) como mucho.
pub const MAX_LIST: usize = 1024 * 1024;
/// Página de un artículo como mucho.
pub const MAX_PAGE: usize = 4 * 1024 * 1024;
/// Audio como mucho (un resumen de 10 min pesa ~10 MB).
pub const MAX_AUDIO: usize = 120 * 1024 * 1024;
/// Fuentes como mucho.
pub const MAX_SOURCES: usize = 8;
/// Artículos que se leen por fuente.
const MAX_ITEMS: usize = 20;
/// Cada cuánto se vuelve a pedir la lista.
pub const REFRESH: Duration = Duration::from_secs(60 * 60);
/// Tras un error, cuándo se reintenta.
pub const RETRY: Duration = Duration::from_secs(10 * 60);
/// Artículos por fuente que se pueden mostrar (1 a 3).
pub const MAX_PER_SOURCE: u8 = 3;
/// Audios guardados en caché.
const KEEP_AUDIO: usize = 6;
const TIMEOUT: Duration = Duration::from_secs(15);
const AUDIO_TIMEOUT: Duration = Duration::from_secs(300);
// Un agente honesto: el cortafuegos de SesoLibre rechaza (406) a quien se hace pasar por navegador.
const USER_AGENT: &str = concat!("IureDesk/", env!("CARGO_PKG_VERSION"), " (https://iurefficient.com; desktop PIM)");

/// De qué trata una fuente (su etiqueta en la pantalla).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Topic {
    General,
    Legal,
    #[default]
    #[serde(other)]
    Other,
}

impl Topic {
    pub fn label(self) -> &'static str {
        match self {
            Topic::General => pick("General news", "Noticias generales"),
            Topic::Legal => pick("Legal news", "Noticias jurídicas"),
            Topic::Other => pick("News", "Noticias"),
        }
    }
}

/// Una fuente de noticias.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewsSource {
    pub name: String,
    /// Sitio WordPress (`https://sesolibre.com`) o URL de un feed RSS/Atom.
    pub url: String,
    #[serde(default)]
    pub topic: Topic,
    /// WordPress: categoría del resumen con audio (su reproductor está en la página).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_category: Option<u64>,
}

/// Preferencias de las noticias.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewsPref {
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default = "default_sources", deserialize_with = "sources_or_new_defaults")]
    pub sources: Vec<NewsSource>,
    /// Artículos por fuente (1 a [`MAX_PER_SOURCE`]).
    #[serde(default = "one")]
    pub per_source: u8,
}

fn yes() -> bool {
    true
}

fn one() -> u8 {
    1
}

impl Default for NewsPref {
    fn default() -> Self {
        NewsPref { enabled: true, sources: default_sources(), per_source: 1 }
    }
}

impl NewsPref {
    /// Artículos por fuente, ya acotado.
    pub fn count(&self) -> usize {
        usize::from(self.per_source.clamp(1, MAX_PER_SOURCE))
    }
}

/// Los artículos de Iurefficient, SesoLibre y el blog de Iurefficient.
pub fn default_sources() -> Vec<NewsSource> {
    let mut v = vec![NewsSource {
        name: "Iurefficient".into(),
        url: "https://iurefficient.com/articulos/feed.xml".into(),
        topic: Topic::Legal,
        audio_category: None,
    }];
    v.extend(first_defaults().into_iter().map(|s| match s.name.as_str() {
        "Iurefficient" => NewsSource { name: "Blog Iurefficient".into(), ..s },
        _ => s,
    }));
    v
}

/// Las fuentes por omisión de la 0.1.0-beta.6 a la beta.8 (SesoLibre y el blog).
fn first_defaults() -> Vec<NewsSource> {
    vec![
        NewsSource {
            name: "SesoLibre".into(),
            url: "https://sesolibre.com".into(),
            topic: Topic::General,
            audio_category: Some(14),
        },
        NewsSource {
            name: "Iurefficient".into(),
            url: "https://blog.iurefficient.com".into(),
            topic: Topic::Legal,
            audio_category: Some(46),
        },
    ]
}

/// Quien nunca tocó sus fuentes (tiene las de omisión de antes) recibe las de ahora; una lista
/// cambiada por el usuario se respeta.
fn sources_or_new_defaults<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<NewsSource>, D::Error> {
    let v = Vec::<NewsSource>::deserialize(d)?;
    Ok(if v == first_defaults() { default_sources() } else { v })
}

/// Dónde está el audio de un artículo.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AudioRef {
    None,
    /// En el reproductor de la página del artículo.
    Page,
    /// Directo (enclosure de un feed).
    Url(String),
}

/// Un artículo.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewsItem {
    pub title: String,
    pub link: String,
    pub date: Option<DateTime<Utc>>,
    pub audio: AudioRef,
}

impl NewsItem {
    pub fn has_audio(&self) -> bool {
        self.audio != AudioRef::None
    }
}

/// Lo de una fuente.
#[derive(Clone, Debug)]
pub struct SourceNews {
    pub source: NewsSource,
    pub result: Result<Vec<NewsItem>, Text>,
}

/// Los últimos `n` artículos de una fuente (ya vienen del más nuevo al más viejo).
pub fn latest(items: &[NewsItem], n: usize) -> &[NewsItem] {
    items.get(..n.min(items.len())).unwrap_or(&[])
}

/// Respuesta de [`crate::Session::request_news`].
pub struct NewsResult {
    pub for_sources: Vec<NewsSource>,
    pub news: Vec<SourceNews>,
}

/// Respuesta de [`crate::Session::request_news_audio`].
pub struct NewsAudio {
    pub link: String,
    pub result: Result<PathBuf, Text>,
}

/// Carpeta de los audios (junto a la caché de imágenes remotas).
pub fn cache_dir() -> PathBuf {
    crate::remote_images::cache_root().with_file_name("news-audio")
}

fn unavailable(name: &str) -> Text {
    Text::new(format!("Couldn't read {name} (no connection?)."), format!("No se pudo leer {name} (¿sin conexión?)."))
}

fn no_audio() -> Text {
    Text::new("This article's audio couldn't be found.", "No se encontró el audio de este artículo.")
}

/// `https://` (o `http://` a la propia máquina, para pruebas y desarrollo local).
pub fn parse_url(s: &str) -> Option<Url> {
    let u = Url::parse(s.trim()).ok()?;
    let local = matches!(u.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
    let ok = u.scheme() == "https" || (u.scheme() == "http" && local);
    (ok && u.host_str().is_some() && s.len() <= 2_048).then_some(u)
}

fn client(timeout: Duration) -> Option<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::limited(3))
        .user_agent(USER_AGENT)
        .build()
        .ok()
}

async fn get(client: &reqwest::Client, url: &str, max: usize) -> Option<Vec<u8>> {
    let resp = client.get(url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    crate::today_bg::body_limited(resp, max).await
}

/// Títulos de WordPress y feeds: entidades (`&#8211;`, `&amp;`), sin etiquetas, una línea.
pub fn clean_title(raw: &str) -> String {
    let mut no_tags = String::with_capacity(raw.len());
    let mut in_tag = false;
    for ch in raw.chars().take(4_000) {
        match ch {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            c if !in_tag => no_tags.push(c),
            _ => {}
        }
    }
    let text = unescape(&no_tags);
    text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(300).collect()
}

/// Entidades HTML: numéricas (`&#8211;`, `&#x2013;`) y las con nombre comunes en español. Lo
/// desconocido queda tal cual.
pub fn unescape(s: &str) -> String {
    const NAMED: &[(&str, &str)] = &[
        ("amp", "&"),
        ("lt", "<"),
        ("gt", ">"),
        ("quot", "\""),
        ("apos", "'"),
        ("nbsp", " "),
        ("hellip", "…"),
        ("ndash", "–"),
        ("mdash", "—"),
        ("laquo", "«"),
        ("raquo", "»"),
        ("ldquo", "“"),
        ("rdquo", "”"),
        ("lsquo", "‘"),
        ("rsquo", "’"),
        ("iexcl", "¡"),
        ("iquest", "¿"),
        ("aacute", "á"),
        ("eacute", "é"),
        ("iacute", "í"),
        ("oacute", "ó"),
        ("uacute", "ú"),
        ("ntilde", "ñ"),
        ("uuml", "ü"),
        ("Aacute", "Á"),
        ("Eacute", "É"),
        ("Iacute", "Í"),
        ("Oacute", "Ó"),
        ("Uacute", "Ú"),
        ("Ntilde", "Ñ"),
        ("Uuml", "Ü"),
    ];
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('&') {
        out.push_str(rest.get(..at).unwrap_or(""));
        let tail = rest.get(at..).unwrap_or("");
        let decoded = tail.get(1..).and_then(|t| {
            let end = t.find(';').filter(|e| *e > 0 && *e <= 10)?;
            let name = t.get(..end)?;
            let ch = if let Some(num) = name.strip_prefix('#') {
                let n = match num.strip_prefix(['x', 'X']) {
                    Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                    None => num.parse::<u32>().ok()?,
                };
                char::from_u32(n).filter(|c| !c.is_control() || c.is_whitespace())?.to_string()
            } else {
                NAMED.iter().find(|(k, _)| *k == name)?.1.to_string()
            };
            Some((ch, end + 2))
        });
        match decoded {
            Some((ch, used)) => {
                out.push_str(&ch);
                rest = tail.get(used..).unwrap_or("");
            }
            None => {
                out.push('&');
                rest = tail.get(1..).unwrap_or("");
            }
        }
    }
    out.push_str(rest);
    out
}

fn link_ok(s: &str) -> Option<String> {
    parse_url(s).map(|u| u.to_string())
}

/// Artículos de `wp/v2/posts` (con `_fields=date_gmt,link,title,categories`). Los de
/// `audio_category` llevan su audio en la página.
pub fn parse_wp(json: &Value, audio_category: Option<u64>) -> Vec<NewsItem> {
    let Some(arr) = json.as_array() else { return Vec::new() };
    arr.iter()
        .take(MAX_ITEMS)
        .filter_map(|p| {
            let title = clean_title(p.get("title")?.get("rendered")?.as_str()?);
            let link = link_ok(p.get("link")?.as_str()?)?;
            let date = p
                .get("date_gmt")
                .and_then(Value::as_str)
                .and_then(|d| NaiveDateTime::parse_from_str(d, "%Y-%m-%dT%H:%M:%S").ok())
                .map(|d| d.and_utc());
            let audio = audio_category.is_some_and(|cat| {
                p.get("categories").and_then(Value::as_array).is_some_and(|c| c.iter().any(|x| x.as_u64() == Some(cat)))
            });
            (!title.is_empty()).then_some(NewsItem {
                title,
                link,
                date,
                audio: if audio { AudioRef::Page } else { AudioRef::None },
            })
        })
        .collect()
}

fn feed_date(s: &str) -> Option<DateTime<Utc>> {
    let s = s.trim();
    DateTime::parse_from_rfc2822(s).or_else(|_| DateTime::parse_from_rfc3339(s)).ok().map(|d| d.with_timezone(&Utc))
}

/// Artículos de un feed RSS 2.0 o Atom. Un `<enclosure>` de audio (o `link rel="enclosure"`)
/// se reproduce.
pub fn parse_feed(xml: &[u8]) -> Vec<NewsItem> {
    #[derive(Default)]
    struct Cur {
        title: String,
        link: String,
        date: Option<DateTime<Utc>>,
        audio: Option<String>,
    }
    let mut reader = quick_xml::Reader::from_reader(xml);
    let mut buf = Vec::new();
    let mut out = Vec::new();
    let mut cur: Option<Cur> = None;
    let mut field: Option<&'static str> = None;
    let attr = |e: &quick_xml::events::BytesStart<'_>, name: &[u8]| -> Option<String> {
        e.attributes()
            .flatten()
            .find(|a| a.key.local_name().as_ref() == name)
            .map(|a| unescape(&String::from_utf8_lossy(&a.value)))
    };
    // Cada vuelta consume al menos un byte: el tamaño de la entrada acota el bucle.
    for _ in 0..xml.len().saturating_add(1) {
        let ev = match reader.read_event_into(&mut buf) {
            Ok(XmlEvent::Eof) | Err(_) => break,
            Ok(ev) => ev,
        };
        match &ev {
            XmlEvent::Start(e) | XmlEvent::Empty(e) => {
                let name = e.local_name();
                let empty = matches!(ev, XmlEvent::Empty(_));
                match name.as_ref() {
                    b"item" | b"entry" if !empty => cur = Some(Cur::default()),
                    b"title" if cur.is_some() && !empty => field = Some("title"),
                    b"pubDate" | b"published" | b"updated" if cur.is_some() && !empty => field = Some("date"),
                    b"link" => {
                        if let Some(c) = cur.as_mut() {
                            let rel = attr(e, b"rel");
                            let typ = attr(e, b"type").unwrap_or_default();
                            match (attr(e, b"href"), rel.as_deref()) {
                                (Some(h), Some("enclosure")) if typ.starts_with("audio/") => c.audio = Some(h),
                                (Some(h), None | Some("alternate")) if c.link.is_empty() => c.link = h,
                                (None, _) if !empty => field = Some("link"),
                                _ => {}
                            }
                        }
                    }
                    b"enclosure" => {
                        if let Some(c) = cur.as_mut()
                            && attr(e, b"type").unwrap_or_default().starts_with("audio/")
                        {
                            c.audio = attr(e, b"url");
                        }
                    }
                    _ => {}
                }
            }
            XmlEvent::Text(t) => {
                if let (Some(c), Some(f)) = (cur.as_mut(), field)
                    && let Ok(s) = t.decode()
                {
                    push(c, f, &s);
                }
            }
            XmlEvent::CData(t) => {
                if let (Some(c), Some(f)) = (cur.as_mut(), field) {
                    push(c, f, &String::from_utf8_lossy(t));
                }
            }
            XmlEvent::GeneralRef(r) => {
                if let (Some(c), Some(f)) = (cur.as_mut(), field) {
                    let s = format!("&{};", String::from_utf8_lossy(r));
                    push(c, f, &s);
                }
            }
            XmlEvent::End(e) => match e.local_name().as_ref() {
                b"item" | b"entry" => {
                    if let Some(c) = cur.take()
                        && let Some(link) = link_ok(c.link.trim())
                    {
                        let title = clean_title(&c.title);
                        if !title.is_empty() && out.len() < MAX_ITEMS {
                            let audio = c.audio.and_then(|a| link_ok(&a)).map_or(AudioRef::None, AudioRef::Url);
                            out.push(NewsItem { title, link, date: c.date, audio });
                        }
                    }
                    field = None;
                }
                b"title" | b"pubDate" | b"published" | b"updated" | b"link" => field = None,
                _ => {}
            },
            _ => {}
        }
        buf.clear();
    }
    fn push(c: &mut Cur, f: &str, s: &str) {
        match f {
            "title" if c.title.len() < 4_000 => c.title.push_str(s),
            "link" if c.link.len() < 2_048 => c.link.push_str(s),
            "date" if c.date.is_none() => c.date = feed_date(s),
            _ => {}
        }
    }
    out.sort_by_key(|i| std::cmp::Reverse(i.date));
    out
}

/// URL del MP3 en la página de un artículo: `data-main-audio` del reproductor del plugin de TTS
/// (o el `src` de un `<audio>`/`<source>`). Solo del mismo sitio que la página.
pub fn page_audio(html: &str, page: &Url) -> Option<String> {
    let host = page.host_str()?;
    let attr_values = |key: &str| -> Vec<String> {
        let mut found = Vec::new();
        let mut rest = html;
        for _ in 0..64 {
            let Some(at) = rest.find(key) else { break };
            rest = rest.get(at + key.len()..).unwrap_or("");
            let Some(q) = rest.chars().next().filter(|c| *c == '"' || *c == '\'') else { continue };
            let body = rest.get(1..).unwrap_or("");
            if let Some(end) = body.find(q).filter(|e| *e <= 2_048) {
                found.push(body.get(..end).unwrap_or("").replace("&amp;", "&"));
            }
        }
        found
    };
    let audio_like = |u: &str| {
        let p = u.split(['?', '#']).next().unwrap_or("").to_ascii_lowercase();
        [".mp3", ".m4a", ".ogg", ".oga", ".opus", ".wav", ".aac"].iter().any(|e| p.ends_with(e))
    };
    let main = attr_values("data-main-audio=");
    let srcs = attr_values("src=");
    main.into_iter()
        .chain(srcs.into_iter().filter(|s| audio_like(s)))
        .filter(|v| !v.trim().is_empty())
        .filter_map(|v| page.join(v.trim()).ok())
        .filter(|u| u.path() != page.path())
        .find(|u| u.host_str() == Some(host) && (u.scheme() == "https" || parse_url(u.as_str()).is_some()))
        .map(|u| u.to_string())
}

/// ¿La URL parece un feed (y no la portada de un sitio)?
fn looks_like_feed(u: &Url) -> bool {
    let p = u.path().to_ascii_lowercase();
    p.contains("feed") || p.contains("rss") || p.contains("atom") || p.ends_with(".xml")
}

/// Los últimos artículos de un WordPress (los que se pueden mostrar como mucho).
async fn wordpress(client: &reqwest::Client, origin: &str, audio_category: Option<u64>) -> Option<Vec<NewsItem>> {
    let url = format!("{origin}/wp-json/wp/v2/posts?per_page={MAX_PER_SOURCE}&_fields=date_gmt,link,title,categories");
    let body = get(client, &url, MAX_LIST).await?;
    let json: Value = serde_json::from_slice(&body).ok()?;
    let mut items = json.is_array().then(|| parse_wp(&json, audio_category))?;
    items.sort_by_key(|i| std::cmp::Reverse(i.date));
    Some(items)
}

/// Los artículos fuera de la categoría con audio también pueden traer el reproductor (SesoLibre
/// lo pone en otros artículos, lo notó el dueño 2026-11-24). No está en la API: se mira la página
/// de los que se pueden mostrar y, si lo tiene, su MP3 (del mismo sitio) queda como audio.
async fn probe_pages(client: &reqwest::Client, items: &mut [NewsItem]) {
    for item in items.iter_mut().take(usize::from(MAX_PER_SOURCE)) {
        if item.has_audio() {
            continue;
        }
        let Some(page) = parse_url(&item.link) else { continue };
        let Some(html) = get(client, page.as_str(), MAX_PAGE).await else { continue };
        if let Some(mp3) = page_audio(&String::from_utf8_lossy(&html), &page) {
            item.audio = AudioRef::Url(mp3);
        }
    }
}

async fn feed(client: &reqwest::Client, url: &str) -> Option<Vec<NewsItem>> {
    let body = get(client, url, MAX_LIST).await?;
    let items = parse_feed(&body);
    (!items.is_empty()).then_some(items)
}

/// Los artículos de una fuente.
pub async fn fetch_source(src: &NewsSource) -> Result<Vec<NewsItem>, Text> {
    let fail = || unavailable(&src.name);
    let url = parse_url(&src.url).ok_or_else(fail)?;
    let client = client(TIMEOUT).ok_or_else(fail)?;
    if looks_like_feed(&url) {
        return feed(&client, url.as_str()).await.ok_or_else(fail);
    }
    let origin = url.origin().ascii_serialization();
    if let Some(mut items) = wordpress(&client, &origin, src.audio_category).await {
        probe_pages(&client, &mut items).await;
        return Ok(items);
    }
    if let Some(items) = feed(&client, url.as_str()).await {
        return Ok(items);
    }
    let with_feed = format!("{}/feed/", url.as_str().trim_end_matches('/'));
    feed(&client, &with_feed).await.ok_or_else(fail)
}

/// Todas las fuentes, en orden.
pub async fn fetch_all(sources: Vec<NewsSource>) -> NewsResult {
    let mut news = Vec::new();
    for source in sources.iter().take(MAX_SOURCES) {
        let result = fetch_source(source).await;
        news.push(SourceNews { source: source.clone(), result });
    }
    NewsResult { for_sources: sources, news }
}

fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}

/// Deja los `KEEP_AUDIO` audios más recientes.
fn prune(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> =
        rd.flatten().take(1_000).filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path()))).collect();
    files.sort_by_key(|f| std::cmp::Reverse(f.0));
    for (_, p) in files.into_iter().skip(KEEP_AUDIO) {
        let _ = std::fs::remove_file(p);
    }
}

/// El audio de un artículo, descargado en `dir` (o de la caché).
pub async fn fetch_audio(link: &str, audio: &AudioRef, dir: &Path) -> Result<PathBuf, Text> {
    let page = parse_url(link).ok_or_else(no_audio)?;
    let url = match audio {
        AudioRef::None => return Err(no_audio()),
        AudioRef::Url(u) => parse_url(u).ok_or_else(no_audio)?.to_string(),
        AudioRef::Page => {
            let c = client(TIMEOUT).ok_or_else(no_audio)?;
            let html = get(&c, page.as_str(), MAX_PAGE).await.ok_or_else(|| unavailable(&page_name(&page)))?;
            page_audio(&String::from_utf8_lossy(&html), &page).ok_or_else(no_audio)?
        }
    };
    let ext = Path::new(url.split(['?', '#']).next().unwrap_or(""))
        .extension()
        .and_then(|e| e.to_str())
        .filter(|e| e.len() <= 4 && e.chars().all(|c| c.is_ascii_alphanumeric()))
        .unwrap_or("mp3")
        .to_ascii_lowercase();
    let file = dir.join(format!("{:016x}.{ext}", fnv(&url)));
    if std::fs::metadata(&file).is_ok_and(|m| m.len() > 0) {
        return Ok(file);
    }
    let fail = || unavailable(&page_name(&page));
    let c = client(AUDIO_TIMEOUT).ok_or_else(fail)?;
    let resp = c.get(&url).send().await.map_err(|_| fail())?;
    let typ = resp.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("");
    let audio_type = typ.starts_with("audio/") || typ.starts_with("application/octet-stream");
    if !resp.status().is_success() || !audio_type {
        return Err(no_audio());
    }
    let body = crate::today_bg::body_limited(resp, MAX_AUDIO).await.ok_or_else(fail)?;
    std::fs::create_dir_all(dir).map_err(|_| fail())?;
    let tmp = file.with_extension("part");
    std::fs::write(&tmp, &body).and_then(|_| std::fs::rename(&tmp, &file)).map_err(|_| fail())?;
    prune(dir);
    Ok(file)
}

fn page_name(u: &Url) -> String {
    u.host_str().unwrap_or("").trim_start_matches("www.").to_string()
}

/// El buzón de ejemplo: noticias fijas, sin red.
pub fn sample(now: DateTime<Utc>) -> NewsResult {
    let day = |d: i64| Some(now - chrono::Duration::days(d));
    let item = |title: &str, link: &str, date, audio| NewsItem { title: title.into(), link: link.into(), date, audio };
    let sources = default_sources();
    let src = |i: usize| sources.get(i).cloned().unwrap_or_else(|| default_sources().remove(0));
    let news = vec![
        SourceNews {
            source: src(0),
            result: Ok(vec![item(
                "Formatos y plantillas del despacho: escribir igual de bien siempre",
                "https://iurefficient.com/articulos/mejores-practicas-derecho-formatos-y-plantillas",
                day(0),
                AudioRef::None,
            )]),
        },
        SourceNews {
            source: src(1),
            result: Ok(vec![
                item(
                    "Resumen de noticias – miércoles, 21 de octubre",
                    "https://sesolibre.com/resumen-de-noticias-ejemplo/",
                    day(0),
                    AudioRef::Page,
                ),
                item(
                    "Manejar la casa desde una sola pantalla",
                    "https://sesolibre.com/manejo-del-hogar-iuredesk/",
                    day(1),
                    AudioRef::None,
                ),
            ]),
        },
        SourceNews {
            source: src(2),
            result: Ok(vec![item(
                "Resumen de noticias – miércoles, 21 de octubre",
                "https://blog.iurefficient.com/resumen-de-noticias-ejemplo/",
                day(0),
                AudioRef::Page,
            )]),
        },
    ];
    NewsResult { for_sources: sources, news }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use proptest::prelude::*;

    #[test]
    fn defaults_are_articles_sesolibre_and_blog() {
        let p = NewsPref::default();
        assert!(p.enabled);
        let urls: Vec<_> = p.sources.iter().map(|s| s.url.as_str()).collect();
        assert_eq!(
            urls,
            ["https://iurefficient.com/articulos/feed.xml", "https://sesolibre.com", "https://blog.iurefficient.com"]
        );
        assert!(urls.iter().all(|u| parse_url(u).is_some()));
        assert_eq!(p.sources[1].topic, Topic::General);
        assert_eq!(p.sources[2].audio_category, Some(46));
        // Quien tenía las dos de antes, sin tocar, recibe las tres; una lista propia se respeta.
        let old = serde_json::json!({ "sources": first_defaults() });
        assert_eq!(serde_json::from_value::<NewsPref>(old).unwrap().sources, default_sources());
        let mine = serde_json::json!({ "sources": [first_defaults()[1].clone()] });
        assert_eq!(serde_json::from_value::<NewsPref>(mine).unwrap().sources, [first_defaults()[1].clone()]);
        // Una preferencia vieja (sin `news`) o con un tema desconocido se lee.
        let n: NewsPref = serde_json::from_str("{}").unwrap();
        assert_eq!(n, NewsPref::default());
        let n: NewsPref = serde_json::from_str(
            r#"{"enabled":false,"sources":[{"name":"X","url":"https://x.org","topic":"sports"}]}"#,
        )
        .unwrap();
        assert!(!n.enabled);
        assert_eq!(n.sources[0].topic, Topic::Other);
    }

    #[test]
    fn wordpress_posts_with_entities() {
        let json: Value = serde_json::from_str(
            r#"[{"date_gmt":"2026-11-23T06:00:52","link":"https://sesolibre.com/2026/11/23/resumen/",
                 "title":{"rendered":"Resumen de noticias &#8211; lunes, 23 de noviembre"},"categories":[3,14]},
                {"date_gmt":"bad","link":"javascript:alert(1)","title":{"rendered":"malo"}},
                {"link":"https://sesolibre.com/x/","title":{"rendered":"<b>Caf&eacute;</b> &amp; m&aacute;s"}}]"#,
        )
        .unwrap();
        let v = parse_wp(&json, Some(14));
        assert_eq!(v.len(), 2, "sin enlace https no entra");
        assert_eq!(v[0].title, "Resumen de noticias – lunes, 23 de noviembre");
        assert_eq!(v[0].date, Some(Utc.with_ymd_and_hms(2026, 11, 23, 6, 0, 52).unwrap()));
        assert_eq!(v[0].audio, AudioRef::Page);
        assert_eq!(v[1].title, "Café & más");
        assert_eq!(v[1].audio, AudioRef::None, "no es de la categoría con audio");
        assert!(parse_wp(&json, None).iter().all(|i| !i.has_audio()), "sin categoría, solo títulos");
        assert_eq!(v[1].date, None);
    }

    #[test]
    fn rss_and_atom_with_audio_enclosures() {
        let rss = br#"<?xml version="1.0"?><rss version="2.0"><channel><title>Canal</title>
            <item><title><![CDATA[Episodio 2 &#8211; ley]]></title><link>https://pod.example/2</link>
              <pubDate>Mon, 23 Nov 2026 06:00:52 +0000</pubDate>
              <enclosure url="https://pod.example/2.mp3" length="1" type="audio/mpeg"/></item>
            <item><title>Texto</title><link>https://pod.example/1</link>
              <pubDate>Sun, 22 Nov 2026 06:00:00 +0000</pubDate></item>
            </channel></rss>"#;
        let v = parse_feed(rss);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].title, "Episodio 2 – ley");
        assert_eq!(v[0].audio, AudioRef::Url("https://pod.example/2.mp3".into()));
        assert_eq!(v[1].audio, AudioRef::None);
        let atom = br#"<feed xmlns="http://www.w3.org/2005/Atom"><title>Blog</title>
            <entry><title type="html">Resumen &amp; m&#225;s</title>
              <link rel="alternate" href="https://blog.example/a"/>
              <link rel="enclosure" type="audio/mpeg" href="https://blog.example/a.mp3"/>
              <published>2026-11-23T06:00:52Z</published></entry></feed>"#;
        let v = parse_feed(atom);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].title, "Resumen & más");
        assert_eq!(v[0].link, "https://blog.example/a");
        assert_eq!(v[0].audio, AudioRef::Url("https://blog.example/a.mp3".into()));
        assert!(v[0].date.is_some());
    }

    #[test]
    fn audio_from_the_tts_player_on_the_same_site_only() {
        let page = Url::parse("https://sesolibre.com/2026/11/23/resumen/").unwrap();
        let html = r#"<audio src="https://evil.example/x.mp3"></audio>
            <div class="tts-enhanced-sesolibre-player" id="tts-enhanced-sesolibre-player-15412"
              data-main-audio="https://sesolibre.com/wp-content/uploads/tts-audio/2026/11/azure_tts-es-MX-DaliaNeural-dc0b.mp3"
              data-intro-audio="https://sesolibre.com/wp-content/uploads/2025/01/Your-Intro-Audionautix.mp3"></div>"#;
        assert_eq!(
            page_audio(html, &page).as_deref(),
            Some("https://sesolibre.com/wp-content/uploads/tts-audio/2026/11/azure_tts-es-MX-DaliaNeural-dc0b.mp3")
        );
        // De otro sitio no; un <source> relativo del mismo sitio sí.
        assert_eq!(page_audio(r#"<div data-main-audio="https://evil.example/a.mp3">"#, &page), None);
        assert_eq!(
            page_audio(r#"<source type="audio/mpeg" src="/media/voz.mp3?v=2&amp;x=1">"#, &page).as_deref(),
            Some("https://sesolibre.com/media/voz.mp3?v=2&x=1")
        );
        assert_eq!(page_audio(r#"<img src="/foto.png"><div data-main-audio="">"#, &page), None);
    }

    #[test]
    fn latest_n_per_source() {
        let it = |t: &str, audio: bool| NewsItem {
            title: t.into(),
            link: format!("https://x.org/{t}"),
            date: None,
            audio: if audio { AudioRef::Page } else { AudioRef::None },
        };
        let items = vec![it("nuevo", false), it("resumen", true), it("ayer", false)];
        let t = |n| latest(&items, n).iter().map(|i| i.title.as_str()).collect::<Vec<_>>();
        assert_eq!(t(1), ["nuevo"], "el último, aunque no tenga audio");
        assert_eq!(t(2), ["nuevo", "resumen"]);
        assert_eq!(t(9), ["nuevo", "resumen", "ayer"]);
        let mut p = NewsPref::default();
        assert_eq!(p.count(), 1);
        p.per_source = 0;
        assert_eq!(p.count(), 1);
        p.per_source = 200;
        assert_eq!(p.count(), 3);
    }

    #[test]
    fn entities() {
        assert_eq!(
            unescape("a &#8211; b &#x2014; c &hellip; &amp;amp; &bogus; & fin"),
            "a – b — c … &amp; &bogus; & fin"
        );
        assert_eq!(unescape("&#0;&#xD800;&#99999999999;"), "&#0;&#xD800;&#99999999999;", "inválidas quedan");
    }

    #[test]
    fn only_https_urls() {
        assert!(parse_url("https://sesolibre.com").is_some());
        assert!(parse_url("http://sesolibre.com").is_none());
        assert!(parse_url("http://127.0.0.1:8080/feed").is_some(), "la propia máquina, para pruebas");
        assert!(parse_url("file:///etc/passwd").is_none());
        assert!(parse_url("sesolibre.com").is_none());
    }

    /// Un WordPress de prueba en la propia máquina: el artículo de la categoría con audio y otro
    /// que no lo es pero trae el reproductor en su página; el tercero, sin reproductor, solo título.
    #[tokio::test]
    async fn player_outside_the_audio_category_is_found_on_its_page() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let posts = format!(
            r#"[{{"date_gmt":"2026-11-24T17:22:50","link":"{base}/tecnologia/","title":{{"rendered":"Tecnología"}},"categories":[3]}},
                {{"date_gmt":"2026-11-24T15:00:00","link":"{base}/sin-audio/","title":{{"rendered":"Sin audio"}},"categories":[2]}},
                {{"date_gmt":"2026-11-24T11:00:33","link":"{base}/resumen/","title":{{"rendered":"Resumen"}},"categories":[14]}}]"#
        );
        let b = base.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { break };
                let mut buf = vec![0u8; 4096];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = req.split_whitespace().nth(1).unwrap_or("").to_string();
                let (typ, body) = if path.starts_with("/wp-json/") {
                    ("application/json", posts.clone())
                } else if path == "/tecnologia/" {
                    ("text/html", format!(r#"<div data-main-audio="{b}/voz.mp3"></div>"#))
                } else {
                    ("text/html", "<p>texto</p>".to_string())
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {typ}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            }
        });
        let src = NewsSource { name: "X".into(), url: base.clone(), topic: Topic::Other, audio_category: Some(14) };
        let items = fetch_source(&src).await.unwrap();
        let audio: Vec<_> = items.iter().map(|i| (i.title.as_str(), i.audio.clone())).collect();
        assert_eq!(
            audio,
            [
                ("Tecnología", AudioRef::Url(format!("{base}/voz.mp3"))),
                ("Sin audio", AudioRef::None),
                ("Resumen", AudioRef::Page),
            ]
        );
    }

    proptest! {
        /// Páginas y feeds cualesquiera: sin pánico, y lo que sale es del mismo sitio o https.
        #[test]
        fn hostile_pages_and_feeds(s in ".{0,2000}", bytes in prop::collection::vec(any::<u8>(), 0..2000)) {
            let page = Url::parse("https://sesolibre.com/a/").unwrap();
            if let Some(u) = page_audio(&s, &page) {
                prop_assert!(u.starts_with("https://sesolibre.com/"));
            }
            let html = format!("<div data-main-audio=\"{s}\"></div>");
            if let Some(u) = page_audio(&html, &page) {
                prop_assert!(u.starts_with("https://sesolibre.com/"));
            }
            for i in parse_feed(&bytes).into_iter().chain(parse_feed(s.as_bytes())) {
                prop_assert!(i.link.starts_with("https://"));
                prop_assert!(i.title.chars().count() <= 300);
            }
            let _ = clean_title(&s);
            let _ = unescape(&s);
        }
    }
}
