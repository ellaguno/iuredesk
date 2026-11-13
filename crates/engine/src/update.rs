//! Aviso de versión nueva (R3.2). Consulta el último release publicado de IureDesk en GitHub (el
//! mismo que instala iureapps) y lo compara con la versión en ejecución, prelanzamientos incluidos:
//! `0.1.0-alpha.12` < `0.1.0-alpha.13` < `0.1.0-beta.1` < `0.1.0`. Solo avisa: la instalación la
//! hace iureapps o el usuario con el instalador. La petición no lleva datos del usuario (solo el
//! `User-Agent` con la versión).

use std::cmp::Ordering;
use std::time::Duration;

use serde_json::Value;

use crate::lang::Text;

/// Último release publicado (GitHub no cuenta los marcados como prelanzamiento ni los borradores).
pub const RELEASES_API: &str = "https://api.github.com/repos/ellaguno/iuredesk/releases/latest";
/// Solo se ofrecen enlaces a las páginas de releases del propio repositorio.
pub const RELEASES_PAGE: &str = "https://github.com/ellaguno/iuredesk/releases/";
/// Versión en ejecución.
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
/// Cada cuánto se vuelve a consultar con la app abierta.
pub const CHECK_EVERY: Duration = Duration::from_secs(24 * 3600);
/// Tras un error (sin conexión), cuándo se reintenta.
pub const RETRY: Duration = Duration::from_secs(6 * 3600);
const TIMEOUT: Duration = Duration::from_secs(15);
const MAX_JSON: usize = 512 * 1024;
const USER_AGENT: &str = concat!("IureDesk/", env!("CARGO_PKG_VERSION"), " (update check)");

/// R4.2: guía de uso (en el repositorio público; se lee en el navegador).
pub fn guide_url(spanish: bool) -> String {
    let lang = if spanish { "es" } else { "en" };
    format!("https://github.com/ellaguno/iuredesk/blob/main/docs/guia/{lang}/README.md")
}

/// Una sección de la guía (`anchor_es`/`anchor_en`: el ancla de su título en cada idioma).
pub fn guide_section_url(spanish: bool, anchor_es: &str, anchor_en: &str) -> String {
    format!("{}#{}", guide_url(spanish), if spanish { anchor_es } else { anchor_en })
}

/// Una versión publicada más nueva que la actual.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    /// Sin la `v` (`0.1.0-beta.1`).
    pub version: String,
    /// Página del release (notas e instaladores).
    pub url: String,
}

/// Respuesta de [`crate::Session::request_update_check`] (llega por `Event::Custom`).
#[derive(Debug)]
pub struct UpdateResult {
    /// `Ok(None)`: estás al día.
    pub result: Result<Option<Release>, Text>,
    /// La pidió el usuario (avisar también si está al día o si falló).
    pub manual: bool,
}

/// Versión semántica con identificadores de prelanzamiento.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    core: [u64; 3],
    pre: Vec<Ident>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Ident {
    Num(u64),
    Alpha(String),
}

impl Ord for Ident {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Ident::Num(a), Ident::Num(b)) => a.cmp(b),
            (Ident::Alpha(a), Ident::Alpha(b)) => a.cmp(b),
            // SemVer 2.0 §11: los numéricos van antes que los alfanuméricos.
            (Ident::Num(_), Ident::Alpha(_)) => Ordering::Less,
            (Ident::Alpha(_), Ident::Num(_)) => Ordering::Greater,
        }
    }
}

impl PartialOrd for Ident {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.core.cmp(&other.core).then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
            (true, true) => Ordering::Equal,
            // Una versión final es mayor que cualquiera de sus prelanzamientos.
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            (false, false) => self.pre.cmp(&other.pre),
        })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Lee `v1.2.3`, `1.2.3-beta.1` o `1.2.3-alpha.13+build`; `None` si no es una versión.
pub fn parse_version(s: &str) -> Option<Version> {
    let s = s.trim();
    let s = s.strip_prefix('v').unwrap_or(s);
    if s.is_empty() || s.len() > 64 {
        return None;
    }
    let s = s.split_once('+').map_or(s, |(v, _)| v);
    let (core, pre) = match s.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (s, None),
    };
    let mut nums = core.split('.');
    let mut out = [0u64; 3];
    for slot in &mut out {
        let part = nums.next()?;
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *slot = part.parse().ok()?;
    }
    if nums.next().is_some() {
        return None;
    }
    let mut ids = Vec::new();
    if let Some(p) = pre {
        for id in p.split('.').take(8) {
            if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
                return None;
            }
            ids.push(if id.bytes().all(|b| b.is_ascii_digit()) {
                Ident::Num(id.parse().ok()?)
            } else {
                Ident::Alpha(id.to_ascii_lowercase())
            });
        }
    }
    Some(Version { core: out, pre: ids })
}

/// `true` si `latest` es posterior a `current` (si alguna no se entiende, no se avisa).
pub fn is_newer(current: &str, latest: &str) -> bool {
    match (parse_version(current), parse_version(latest)) {
        (Some(c), Some(l)) => l > c,
        _ => false,
    }
}

/// Saca el release de la respuesta de GitHub (o `None` si no es aprovechable: borrador,
/// prelanzamiento, sin versión o con un enlace fuera del repositorio).
pub fn parse_release(json: &Value) -> Option<Release> {
    if json.get("draft").and_then(Value::as_bool) == Some(true)
        || json.get("prerelease").and_then(Value::as_bool) == Some(true)
    {
        return None;
    }
    let tag = json.get("tag_name").and_then(Value::as_str)?;
    parse_version(tag)?;
    let url = json.get("html_url").and_then(Value::as_str)?;
    if !url.starts_with(RELEASES_PAGE) || url.len() > 300 || url.chars().any(char::is_whitespace) {
        return None;
    }
    Some(Release { version: tag.trim().trim_start_matches('v').to_string(), url: url.to_string() })
}

/// Consulta `api` y devuelve el release si es más nuevo que `current`.
pub async fn check(api: &str, current: &str) -> Result<Option<Release>, Text> {
    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(3))
        .user_agent(USER_AGENT)
        .build()
        .map_err(|_| unavailable())?;
    let resp =
        client.get(api).header("Accept", "application/vnd.github+json").send().await.map_err(|_| unavailable())?;
    // Aún no hay ningún release publicado.
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !resp.status().is_success() {
        return Err(unavailable());
    }
    let body = crate::today_bg::body_limited(resp, MAX_JSON).await.ok_or_else(unavailable)?;
    let json: Value = serde_json::from_slice(&body).map_err(|_| unavailable())?;
    Ok(parse_release(&json).filter(|r| is_newer(current, &r.version)))
}

fn unavailable() -> Text {
    Text::new("Couldn't check for updates (no connection?).", "No se pudo buscar actualizaciones (¿sin conexión?).")
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn orders_prereleases() {
        let chain = [
            "0.1.0-alpha.2",
            "0.1.0-alpha.12",
            "0.1.0-alpha.13",
            "0.1.0-beta",
            "0.1.0-beta.1",
            "0.1.0-beta.2",
            "0.1.0-rc.1",
            "0.1.0",
            "0.1.1",
            "0.2.0",
            "1.0.0",
        ];
        for w in chain.windows(2) {
            if let [a, b] = w {
                assert!(is_newer(a, b), "{a} < {b}");
                assert!(!is_newer(b, a), "{b} > {a}");
            }
        }
        assert!(!is_newer("0.1.0-alpha.13", "v0.1.0-alpha.13"));
        assert!(!is_newer("0.1.0", "0.1.0+build.5"));
        // Lo que no se entiende no se avisa.
        for bad in ["", "latest", "1.2", "1.2.3.4", "1.2.x", "1.2.3-", "1.2.3-al pha", "01a.2.3"] {
            assert!(!is_newer("0.1.0", bad), "{bad}");
        }
    }

    #[test]
    fn reads_github_release() {
        let ok = serde_json::json!({
            "tag_name": "v0.1.0-beta.1",
            "html_url": "https://github.com/ellaguno/iuredesk/releases/tag/v0.1.0-beta.1",
            "draft": false, "prerelease": false
        });
        let r = parse_release(&ok).unwrap();
        assert_eq!(r.version, "0.1.0-beta.1");
        assert!(r.url.ends_with("v0.1.0-beta.1"));
        let mut bad = ok.clone();
        bad["html_url"] = "https://evil.example/iuredesk".into();
        assert!(parse_release(&bad).is_none());
        let mut pre = ok.clone();
        pre["prerelease"] = true.into();
        assert!(parse_release(&pre).is_none());
        let mut tag = ok;
        tag["tag_name"] = "nightly".into();
        assert!(parse_release(&tag).is_none());
        assert!(parse_release(&serde_json::json!([1, 2])).is_none());
    }

    #[test]
    fn current_version_parses() {
        assert!(parse_version(CURRENT).is_some(), "{CURRENT}");
    }

    proptest! {
        #[test]
        fn parse_never_panics(s in ".{0,80}") {
            let _ = parse_version(&s);
            let _ = is_newer(&s, "0.1.0");
        }

        #[test]
        fn order_is_consistent(a in 0u64..5, b in 0u64..5, c in 0u64..5, n in 0u64..20, m in 0u64..20) {
            let x = format!("{a}.{b}.{c}-alpha.{n}");
            let y = format!("{a}.{b}.{c}-alpha.{m}");
            prop_assert_eq!(is_newer(&x, &y), m > n);
            let fin = format!("{a}.{b}.{c}");
            prop_assert!(is_newer(&x, &fin));
        }
    }
}
