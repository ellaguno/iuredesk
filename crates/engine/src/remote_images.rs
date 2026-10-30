//! UX-5: imágenes remotas del correo.
//!
//! - **Cuándo se cargan solas** ([`RemoteImages`], Ajustes → Privacidad): «Nunca (preguntar)»
//!   (por omisión), «Solo de contactos y remitentes de confianza» o «Siempre». Además, la lista
//!   de remitentes y dominios permitidos («Mostrar siempre de este remitente / dominio») vale en
//!   cualquier modo ([`should_load`]).
//! - **Píxeles de rastreo**: aun en «Siempre» se bloquean (no se piden): las URL de una lista
//!   corta de rastreadores conocidos ([`is_tracker`]), las `<img>` declaradas de 1×1 o 0×0 en el
//!   HTML ([`declared_pixels`]) y, ya descargadas, las imágenes de 2×2 px o menos
//!   ([`is_pixel_image`]). La lista es deliberadamente pequeña y de servicios cuyo fin es saber
//!   si se abrió un correo; un boletín sigue viéndose (sus imágenes de contenido sí se cargan).
//! - **Caché** ([`ImageCache`]): en memoria (acotada en bytes) y en disco
//!   (`<caché>/iuredesk/remote-images/<sha256 de la URL>`, carpeta 0700 y archivos 0600, acotada
//!   en tamaño y edad). Las descargas las hace [`crate::Session::fetch_remote`] en el runtime de
//!   fondo (reqwest + rustls, tiempo de espera y tope por imagen): la interfaz nunca espera.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Tope por imagen (bytes).
pub const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;
/// Imágenes que se piden por mensaje.
pub const MAX_IMAGES_PER_MESSAGE: usize = 60;
/// Tiempo de espera por imagen.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
/// Remitentes o dominios permitidos que se guardan.
pub const MAX_ALLOWLIST: usize = 2_000;
/// Memoria de la caché.
const MEMORY_BYTES: usize = 32 * 1024 * 1024;
/// Disco de la caché.
const DISK_BYTES: u64 = 192 * 1024 * 1024;
/// Edad máxima en disco.
const DISK_AGE: Duration = Duration::from_secs(30 * 24 * 3600);
/// HTML que se revisa en busca de píxeles declarados.
const MAX_SCAN_HTML: usize = 2 * 1024 * 1024;

/// Cuándo se cargan las imágenes remotas sin preguntar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteImages {
    /// Nunca: se pregunta en cada correo (salvo remitentes y dominios permitidos).
    #[default]
    Ask,
    /// De contactos y remitentes de confianza.
    Trusted,
    /// Siempre (los píxeles de rastreo conocidos siguen bloqueados).
    Always,
}

impl RemoteImages {
    pub const ALL: [RemoteImages; 3] = [RemoteImages::Ask, RemoteImages::Trusted, RemoteImages::Always];

    pub fn label(self) -> &'static str {
        match self {
            RemoteImages::Ask => crate::lang::pick("Never (ask)", "Nunca (preguntar)"),
            RemoteImages::Trusted => crate::lang::pick(
                "Only from contacts and trusted senders",
                "Solo de contactos y remitentes de confianza",
            ),
            RemoteImages::Always => crate::lang::pick("Always", "Siempre"),
        }
    }
}

/// Dirección normalizada (minúsculas, sin `<>` ni espacios); `None` si no parece una dirección.
pub fn normalize_sender(addr: &str) -> Option<String> {
    let a = addr.trim().trim_start_matches('<').trim_end_matches('>').trim().to_lowercase();
    let (l, d) = a.split_once('@')?;
    (!l.is_empty() && d.contains('.') && a.len() <= 254 && !a.contains(char::is_whitespace)).then_some(a)
}

/// Dominio normalizado (de una dirección o escrito tal cual); `None` si no parece un dominio.
pub fn normalize_domain(s: &str) -> Option<String> {
    let s = s.trim().trim_start_matches('<').trim_end_matches('>');
    let d = s.rsplit_once('@').map_or(s, |(_, d)| d).trim().trim_start_matches("*.").trim_matches('.').to_lowercase();
    (d.contains('.') && d.len() <= 253 && d.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '.')).then_some(d)
}

/// Dominios de correo público: «Mostrar siempre de este dominio» no se ofrece (sería de todo el
/// mundo).
const PUBLIC_DOMAINS: [&str; 22] = [
    "gmail.com",
    "googlemail.com",
    "outlook.com",
    "hotmail.com",
    "live.com",
    "msn.com",
    "yahoo.com",
    "yahoo.com.mx",
    "ymail.com",
    "icloud.com",
    "me.com",
    "mac.com",
    "aol.com",
    "proton.me",
    "protonmail.com",
    "gmx.com",
    "gmx.net",
    "mail.com",
    "zoho.com",
    "yandex.com",
    "prodigy.net.mx",
    "hotmail.es",
];

/// ¿Se ofrece «Mostrar siempre de este dominio» para este remitente?
pub fn domain_offered(addr: &str) -> Option<String> {
    let d = normalize_domain(addr)?;
    (!PUBLIC_DOMAINS.contains(&d.as_str())).then_some(d)
}

/// ¿Está permitido por la lista? El remitente exacto, o su dominio (o un dominio padre) en la
/// lista de dominios.
pub fn allowed_by_list(senders: &[String], domains: &[String], addr: &str) -> bool {
    let Some(a) = normalize_sender(addr) else { return false };
    if senders.iter().any(|s| s.eq_ignore_ascii_case(&a)) {
        return true;
    }
    let Some(d) = normalize_domain(&a) else { return false };
    domains.iter().any(|x| {
        let x = x.to_lowercase();
        d == x || d.strip_suffix(&x).is_some_and(|p| p.ends_with('.'))
    })
}

/// ¿Se cargan solas las imágenes de este correo? `trusted` = el remitente es un contacto, una
/// dirección propia o alguien con relación en Iurefficient (lo decide quien llama).
pub fn should_load(mode: RemoteImages, senders: &[String], domains: &[String], addr: &str, trusted: bool) -> bool {
    if allowed_by_list(senders, domains, addr) {
        return true;
    }
    match mode {
        RemoteImages::Ask => false,
        RemoteImages::Trusted => trusted,
        RemoteImages::Always => true,
    }
}

/// Agrega a una lista (sin repetir, acotada). Devuelve `true` si cambió.
pub fn add_to(list: &mut Vec<String>, value: String) -> bool {
    if list.iter().any(|x| x.eq_ignore_ascii_case(&value)) {
        return false;
    }
    if list.len() >= MAX_ALLOWLIST {
        list.remove(0);
    }
    list.push(value);
    true
}

/// Rastreadores conocidos: (dominio, fragmento de la ruta o "" para todo el dominio). Solo
/// servicios de «correo abierto»; no se bloquea a un remitente entero.
const TRACKERS: [(&str, &str); 26] = [
    ("google-analytics.com", ""),
    ("doubleclick.net", ""),
    ("bat.bing.com", ""),
    ("facebook.com", "/tr"),
    ("mailtrack.io", ""),
    ("mailtrack.com", ""),
    ("getnotify.com", ""),
    ("yesware.com", ""),
    ("bananatag.com", ""),
    ("mailfoogae.appspot.com", ""),
    ("sidekickopen", ""),
    ("emltrk.com", ""),
    ("pixel.mailerlite.com", ""),
    ("mailstat.us", ""),
    ("trk.klclick.com", "/open"),
    ("list-manage.com", "/track/open"),
    ("sendgrid.net", "/wf/open"),
    ("mandrillapp.com", "/track/open"),
    ("mailgun.org", "/o/"),
    ("hubspotemail.net", "/e2t/to"),
    ("track.hubspot.com", ""),
    ("t.co", "/i/adsct"),
    ("analytics.twitter.com", ""),
    ("pixel.wp.com", ""),
    ("ct.pinterest.com", ""),
    ("open.convertkit-mail.com", ""),
];

/// Anfitrión y ruta de una URL http(s) (en minúsculas).
fn host_path(url: &str) -> Option<(String, String)> {
    let u = url.trim();
    let rest = u.strip_prefix("https://").or_else(|| u.strip_prefix("http://")).or_else(|| {
        let lower = u.get(..8).unwrap_or_default().to_ascii_lowercase();
        if lower.starts_with("https://") {
            u.get(8..)
        } else if lower.starts_with("http://") {
            u.get(7..)
        } else {
            None
        }
    })?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let auth = rest.get(..end).unwrap_or_default();
    let host = auth.rsplit_once('@').map_or(auth, |(_, h)| h);
    let host = host.split(':').next().unwrap_or_default().trim_end_matches('.').to_lowercase();
    let path = rest.get(end..).unwrap_or_default().to_lowercase();
    (!host.is_empty()).then_some((host, path))
}

/// ¿La URL es de un rastreador conocido?
pub fn is_tracker(url: &str) -> bool {
    let Some((host, path)) = host_path(url) else { return false };
    let path_only = path.split(['?', '#']).next().unwrap_or_default();
    if path_only.contains("/open.gif") || path_only.contains("/pixel.gif") || path_only.ends_with("/track/open") {
        return true;
    }
    TRACKERS.iter().any(|(d, p)| {
        let host_hit = if d.contains('.') {
            host == *d || host.strip_suffix(d).is_some_and(|x| x.ends_with('.'))
        } else {
            // Sin punto: prefijo de una etiqueta (`t.sidekickopen06.com`).
            host.split('.').any(|label| label.starts_with(d))
        };
        host_hit && (p.is_empty() || path_only.starts_with(p) || path_only.contains(p))
    })
}

/// Valor numérico de un atributo o propiedad CSS de tamaño («1», «1px», «0»).
fn tiny_value(v: &str) -> bool {
    let v = v.trim().trim_matches(['"', '\'']).trim();
    let n = v.strip_suffix("px").unwrap_or(v).trim();
    n.parse::<f32>().is_ok_and(|x| x <= 1.0)
}

/// Valor de un atributo dentro de una etiqueta (ya en minúsculas).
fn attr<'a>(tag: &'a str, original: &'a str, name: &str) -> Option<&'a str> {
    let mut from = 0;
    while let Some(pos) = tag.get(from..).and_then(|t| t.find(name)) {
        let at = from.saturating_add(pos);
        let before_ok = at == 0 || tag.as_bytes().get(at.saturating_sub(1)).is_some_and(|b| b.is_ascii_whitespace());
        let after = tag.get(at.saturating_add(name.len())..).unwrap_or_default().trim_start();
        from = at.saturating_add(name.len());
        if !before_ok || !after.starts_with('=') {
            continue;
        }
        let val = after.get(1..).unwrap_or_default().trim_start();
        let start = tag.len().saturating_sub(val.len());
        let (body, len) = match val.chars().next() {
            Some(q @ ('"' | '\'')) => {
                let inner = val.get(1..).unwrap_or_default();
                let l = inner.find(q).unwrap_or(inner.len());
                (start.saturating_add(1), l)
            }
            _ => (start, val.find(|c: char| c.is_ascii_whitespace() || c == '>').unwrap_or(val.len())),
        };
        return original.get(body..body.saturating_add(len));
    }
    None
}

/// URL de las `<img>` declaradas de 1×1 (o 0×0, o `display:none`) en el HTML: píxeles de rastreo
/// que no se piden nunca. Revisión acotada, sin un parser completo.
pub fn declared_pixels(html: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    let html = html.get(..html.len().min(MAX_SCAN_HTML)).unwrap_or(html);
    // Minúsculas ASCII: mismas posiciones de byte que el original.
    let lower = html.to_ascii_lowercase();
    let mut from = 0;
    for _ in 0..5_000 {
        let Some(pos) = lower.get(from..).and_then(|t| t.find("<img")) else { break };
        let start = from.saturating_add(pos);
        let end = lower.get(start..).and_then(|t| t.find('>')).map_or(lower.len(), |e| start.saturating_add(e));
        let (Some(tag), Some(orig)) = (lower.get(start..end), html.get(start..end)) else { break };
        from = end.max(start.saturating_add(4));
        let Some(src) = attr(tag, orig, "src") else { continue };
        let w = attr(tag, tag, "width").is_some_and(tiny_value);
        let h = attr(tag, tag, "height").is_some_and(tiny_value);
        let style = attr(tag, tag, "style").unwrap_or_default().replace(' ', "");
        let style_tiny = |prop: &str| {
            style.split(';').any(|d| d.strip_prefix(prop).and_then(|v| v.strip_prefix(':')).is_some_and(tiny_value))
        };
        let hidden = style.contains("display:none") || style.contains("visibility:hidden");
        if (w && h) || (style_tiny("width") && style_tiny("height")) || hidden {
            out.insert(src.trim().to_string());
        }
    }
    out
}

/// Dimensiones de una imagen por su cabecera (GIF, PNG, JPEG, WebP, BMP); `None` si no se sabe.
pub fn image_size(b: &[u8]) -> Option<(u32, u32)> {
    let arr2 = |i: usize| b.get(i..i.saturating_add(2)).and_then(|x| <[u8; 2]>::try_from(x).ok());
    let arr4 = |i: usize| b.get(i..i.saturating_add(4)).and_then(|x| <[u8; 4]>::try_from(x).ok());
    let arr3 = |i: usize| b.get(i..i.saturating_add(3)).and_then(|x| <[u8; 3]>::try_from(x).ok());
    let u16le = |i: usize| arr2(i).map(|x| u32::from(u16::from_le_bytes(x)));
    let u16be = |i: usize| arr2(i).map(|x| u32::from(u16::from_be_bytes(x)));
    let u32be = |i: usize| arr4(i).map(u32::from_be_bytes);
    let i32le = |i: usize| arr4(i).map(|x| i32::from_le_bytes(x).unsigned_abs());
    if b.starts_with(b"GIF8") {
        return Some((u16le(6)?, u16le(8)?));
    }
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some((u32be(16)?, u32be(20)?));
    }
    if b.starts_with(b"BM") {
        return Some((i32le(18)?, i32le(22)?));
    }
    if b.starts_with(b"RIFF") && b.get(8..12) == Some(b"WEBP") {
        return match b.get(12..16)? {
            b"VP8X" => {
                let n = |x: [u8; 3]| u32::from_le_bytes([x[0], x[1], x[2], 0]).saturating_add(1);
                Some((n(arr3(24)?), n(arr3(27)?)))
            }
            b"VP8L" => {
                let v = u32::from_le_bytes(arr4(21)?);
                Some(((v & 0x3FFF).saturating_add(1), ((v >> 14) & 0x3FFF).saturating_add(1)))
            }
            b"VP8 " => Some((u16le(26)? & 0x3FFF, u16le(28)? & 0x3FFF)),
            _ => None,
        };
    }
    if b.starts_with(&[0xFF, 0xD8]) {
        // Segmentos hasta el SOF (acotado).
        let mut i = 2usize;
        for _ in 0..512 {
            if *b.get(i)? != 0xFF {
                return None;
            }
            let marker = *b.get(i.saturating_add(1))?;
            if marker == 0xD8 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
                i = i.saturating_add(2);
                continue;
            }
            let len = usize::try_from(u16be(i.saturating_add(2))?).ok()?;
            if matches!(marker, 0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF) {
                return Some((u16be(i.saturating_add(7))?, u16be(i.saturating_add(5))?));
            }
            i = i.saturating_add(2).saturating_add(len);
        }
    }
    None
}

/// ¿Es un píxel de rastreo ya descargado? (2×2 px o menos, o un archivo diminuto sin forma de
/// imagen conocida).
pub fn is_pixel_image(b: &[u8]) -> bool {
    match image_size(b) {
        Some((w, h)) => w <= 2 && h <= 2,
        None => b.len() <= 64,
    }
}

/// Separa las URL que se piden de las bloqueadas (rastreadores y píxeles declarados). Devuelve
/// (a pedir, bloqueadas).
pub fn filter_urls(urls: Vec<String>, html: Option<&str>) -> (Vec<String>, usize) {
    let declared = html.map(declared_pixels).unwrap_or_default();
    let mut seen = HashSet::new();
    let mut keep = Vec::new();
    let mut blocked = 0usize;
    for u in urls {
        let lower = u.trim().to_ascii_lowercase();
        if !(lower.starts_with("https://") || lower.starts_with("http://")) || !seen.insert(u.clone()) {
            continue;
        }
        if is_tracker(&u) || declared.contains(u.trim()) {
            blocked = blocked.saturating_add(1);
            continue;
        }
        if keep.len() < MAX_IMAGES_PER_MESSAGE {
            keep.push(u);
        }
    }
    (keep, blocked)
}

// --------------------------------------------------------------------------------------- caché

/// Carpeta de la caché en disco (con `IUREDESK_DATA_DIR`, dentro de esa carpeta).
pub fn cache_root() -> PathBuf {
    if let Some(d) = std::env::var_os("IUREDESK_DATA_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(d).join("cache").join("remote-images");
    }
    match dirs::cache_dir() {
        Some(c) => c.join("iuredesk").join("remote-images"),
        None => std::env::temp_dir().join("iuredesk-remote-images"),
    }
}

fn key_of(url: &str) -> String {
    let d = Sha256::digest(url.trim().as_bytes());
    d.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Default)]
struct Memory {
    map: HashMap<String, std::sync::Arc<Vec<u8>>>,
    order: VecDeque<String>,
    bytes: usize,
}

/// Caché de imágenes remotas: memoria acotada + disco (opcional).
pub struct ImageCache {
    dir: Option<PathBuf>,
    mem: Mutex<Memory>,
    writes: std::sync::atomic::AtomicUsize,
}

impl ImageCache {
    /// Caché con disco en `dir` (`None` = solo memoria).
    pub fn new(dir: Option<PathBuf>) -> Self {
        ImageCache { dir, mem: Mutex::new(Memory::default()), writes: std::sync::atomic::AtomicUsize::new(0) }
    }

    /// Lo guardado para `url` (memoria y luego disco).
    pub fn get(&self, url: &str) -> Option<Vec<u8>> {
        let key = key_of(url);
        if let Ok(m) = self.mem.lock()
            && let Some(v) = m.map.get(&key)
        {
            return Some(v.as_ref().clone());
        }
        let path = self.dir.as_ref()?.join(&key);
        let meta = std::fs::symlink_metadata(&path).ok()?;
        if !meta.is_file() || meta.len() > MAX_IMAGE_BYTES as u64 {
            return None;
        }
        if meta.modified().ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > DISK_AGE) {
            let _ = std::fs::remove_file(&path);
            return None;
        }
        let bytes = std::fs::read(&path).ok()?;
        self.remember(key, &bytes);
        Some(bytes)
    }

    /// Guarda una imagen descargada.
    pub fn put(&self, url: &str, bytes: &[u8]) {
        if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES {
            return;
        }
        let key = key_of(url);
        if let Some(dir) = &self.dir
            && private_dir(dir).is_ok()
        {
            let _ = write_private(&dir.join(&key), bytes);
            // De vez en cuando se recorta el disco.
            if self.writes.fetch_add(1, std::sync::atomic::Ordering::Relaxed).is_multiple_of(50) {
                prune(dir, DISK_BYTES, DISK_AGE);
            }
        }
        self.remember(key, bytes);
    }

    fn remember(&self, key: String, bytes: &[u8]) {
        let Ok(mut m) = self.mem.lock() else { return };
        if m.map.contains_key(&key) || bytes.len() > MEMORY_BYTES / 4 {
            return;
        }
        m.bytes = m.bytes.saturating_add(bytes.len());
        m.map.insert(key.clone(), std::sync::Arc::new(bytes.to_vec()));
        m.order.push_back(key);
        while m.bytes > MEMORY_BYTES {
            let Some(old) = m.order.pop_front() else { break };
            if let Some(v) = m.map.remove(&old) {
                m.bytes = m.bytes.saturating_sub(v.len());
            }
        }
    }

    /// Bytes en memoria (diagnóstico y pruebas).
    pub fn memory_bytes(&self) -> usize {
        self.mem.lock().map(|m| m.bytes).unwrap_or(0)
    }
}

/// Carpeta solo para el usuario (Unix: 0700); no sigue un enlace.
fn private_dir(p: &Path) -> std::io::Result<()> {
    if let Ok(m) = std::fs::symlink_metadata(p)
        && (m.file_type().is_symlink() || !m.is_dir())
    {
        return Err(std::io::Error::other("no es una carpeta"));
    }
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(p)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Archivo privado (Unix: 0600), sin seguir un enlace plantado.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    if std::fs::symlink_metadata(path).is_ok() {
        std::fs::remove_file(path)?;
    }
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o.open(path)?;
    f.write_all(bytes)
}

/// Quita lo viejo y, si pasa del tope, lo más antiguo primero.
fn prune(dir: &Path, max_bytes: u64, max_age: Duration) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<(SystemTime, u64, PathBuf)> = Vec::new();
    for e in rd.flatten().take(100_000) {
        let Ok(m) = e.metadata() else { continue };
        if !m.is_file() {
            continue;
        }
        let t = m.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        if t.elapsed().is_ok_and(|age| age > max_age) {
            let _ = std::fs::remove_file(e.path());
            continue;
        }
        files.push((t, m.len(), e.path()));
    }
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    files.sort_by_key(|f| f.0);
    for (_, len, p) in files {
        if total <= max_bytes {
            break;
        }
        if std::fs::remove_file(&p).is_ok() {
            total = total.saturating_sub(len);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn v(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn allowlist_by_sender_and_domain() {
        let senders = v(&["ana@acme.com"]);
        let domains = v(&["proveedor.mx"]);
        assert!(allowed_by_list(&senders, &domains, "Ana@ACME.com"));
        assert!(!allowed_by_list(&senders, &domains, "bob@acme.com"));
        assert!(allowed_by_list(&senders, &domains, "x@proveedor.mx"));
        assert!(allowed_by_list(&senders, &domains, "x@news.proveedor.mx"));
        assert!(!allowed_by_list(&senders, &domains, "x@otroproveedor.mx"));
        assert!(!allowed_by_list(&senders, &domains, "no es correo"));
    }

    #[test]
    fn modes_decide_with_the_allowlist_first() {
        let s = v(&["ana@acme.com"]);
        let none: Vec<String> = Vec::new();
        assert!(!should_load(RemoteImages::Ask, &none, &none, "bob@x.com", true));
        assert!(should_load(RemoteImages::Ask, &s, &none, "ana@acme.com", false));
        assert!(should_load(RemoteImages::Trusted, &none, &none, "bob@x.com", true));
        assert!(!should_load(RemoteImages::Trusted, &none, &none, "bob@x.com", false));
        assert!(should_load(RemoteImages::Always, &none, &none, "bob@x.com", false));
    }

    #[test]
    fn domain_option_is_not_offered_for_webmail() {
        assert_eq!(domain_offered("ana@acme.com").as_deref(), Some("acme.com"));
        assert_eq!(domain_offered("ana@gmail.com"), None);
        assert_eq!(normalize_domain("*.Acme.com").as_deref(), Some("acme.com"));
        assert_eq!(normalize_domain("acme"), None);
        let mut l = Vec::new();
        assert!(add_to(&mut l, "a.com".into()));
        assert!(!add_to(&mut l, "A.com".into()));
    }

    #[test]
    fn known_trackers_are_blocked() {
        assert!(is_tracker("https://mailtrack.io/trace/mail/abc.png"));
        assert!(is_tracker("https://acme.us1.list-manage.com/track/open.php?u=1"));
        assert!(is_tracker("http://u123.ct.sendgrid.net/wf/open?upn=x"));
        assert!(is_tracker("https://www.google-analytics.com/collect?v=1"));
        assert!(is_tracker("https://news.acme.com/e/open.gif?id=1"));
        assert!(is_tracker("https://t.sidekickopen06.com/x"));
        assert!(!is_tracker("https://cdn.acme.com/logo.png"));
        assert!(!is_tracker("https://mcusercontent.com/abc/images/banner.jpg"));
        assert!(!is_tracker("https://sendgrid.net/images/logo.png"));
        assert!(!is_tracker("mailto:x@y.z"));
    }

    #[test]
    fn declared_pixels_are_found() {
        let html = r#"<p><IMG SRC="https://x.com/p.gif" width="1" height="1">
            <img src='https://x.com/logo.png' width=120 height=40>
            <img style="width:1px; height:1px" src="https://y.com/o">
            <img src="https://z.com/h" style="display: none">
            <img alt="srcset" src="https://ok.com/a.png" width="1"></p>"#;
        let d = declared_pixels(html);
        assert!(d.contains("https://x.com/p.gif"));
        assert!(d.contains("https://y.com/o"));
        assert!(d.contains("https://z.com/h"));
        assert!(!d.contains("https://x.com/logo.png"));
        assert!(!d.contains("https://ok.com/a.png"));
        let (keep, blocked) = filter_urls(
            v(&[
                "https://x.com/p.gif",
                "https://x.com/logo.png",
                "https://mailtrack.io/t",
                "data:x",
                "https://x.com/logo.png",
            ]),
            Some(html),
        );
        assert_eq!(keep, v(&["https://x.com/logo.png"]));
        assert_eq!(blocked, 2);
    }

    #[test]
    fn pixel_images_by_size() {
        // GIF 1×1 típico de rastreo.
        let gif = b"GIF89a\x01\x00\x01\x00\x80\x00\x00\xff\xff\xff\x00\x00\x00!\xf9\x04\x01\x00\x00\x00\x00,\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02D\x01\x00;";
        assert_eq!(image_size(gif), Some((1, 1)));
        assert!(is_pixel_image(gif));
        let mut png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR".to_vec();
        png.extend_from_slice(&600u32.to_be_bytes());
        png.extend_from_slice(&200u32.to_be_bytes());
        png.extend_from_slice(&[0; 64]);
        assert_eq!(image_size(&png), Some((600, 200)));
        assert!(!is_pixel_image(&png));
        // JPEG con SOF0 de 1×1.
        let jpg = [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0, 0, 0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x00, 0x01, 0x00, 0x01];
        assert_eq!(image_size(&jpg), Some((1, 1)));
        assert!(is_pixel_image(b"tiny"));
        assert!(!is_pixel_image(&[7u8; 500]));
    }

    #[test]
    fn cache_survives_in_memory_and_disk() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ri");
        let c = ImageCache::new(Some(root.clone()));
        assert!(c.get("https://a.com/x.png").is_none());
        c.put("https://a.com/x.png", b"hello image");
        assert_eq!(c.get("https://a.com/x.png").as_deref(), Some(&b"hello image"[..]));
        // Otra caché (otro arranque) lo lee del disco.
        let c2 = ImageCache::new(Some(root.clone()));
        assert_eq!(c2.get("https://a.com/x.png").as_deref(), Some(&b"hello image"[..]));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&root).unwrap().permissions().mode() & 0o777, 0o700);
        }
        // Demasiado grande: no se guarda.
        c.put("https://a.com/big", &vec![0u8; MAX_IMAGE_BYTES + 1]);
        assert!(c.get("https://a.com/big").is_none());
        prune(&root, 0, DISK_AGE);
        assert!(ImageCache::new(Some(root)).get("https://a.com/x.png").is_none());
    }

    proptest! {
        #[test]
        fn scanners_never_panic(html in ".{0,400}", url in ".{0,120}", bytes in proptest::collection::vec(any::<u8>(), 0..64)) {
            let _ = declared_pixels(&html);
            let _ = is_tracker(&url);
            let _ = image_size(&bytes);
            let _ = filter_urls(vec![url.clone()], Some(&html));
            let _ = allowed_by_list(std::slice::from_ref(&url), std::slice::from_ref(&html), &url);
        }
    }
}
