//! `cargo xtask site <carpeta> --tag vX.Y.Z [--repo dueño/repo] [--out ruta]` (R4.3): página de
//! descargas estática (HTML y CSS en línea, sin JavaScript) con el instalador de cada sistema del
//! release, su SHA-256 y su firma minisign. La genera `release.yml` con los paquetes ya firmados y
//! la publica en GitHub Pages. Los nombres siguen el esquema de iureapps (`publicacion.md`).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

type Res = Result<(), String>;

/// (sufijo del archivo, sistema, descripción es, descripción en)
const KINDS: &[(&str, &str, &str, &str)] = &[
    (
        "windows-x64.exe",
        "Windows",
        "Instalador (por usuario, sin permisos de administrador)",
        "Installer (per user, no admin rights)",
    ),
    ("windows-x64.msi", "Windows", "Instalador MSI (para todo el equipo; TI)", "MSI installer (whole machine; IT)"),
    ("macos-arm64.dmg", "macOS", "Apple Silicon (M1 y posteriores)", "Apple Silicon (M1 and later)"),
    ("macos-x64.dmg", "macOS", "Intel", "Intel"),
    ("linux-x64.AppImage", "Linux", "AppImage (cualquier distribución)", "AppImage (any distribution)"),
    ("linux-x64.deb", "Linux", "Debian, Ubuntu y derivadas (.deb)", "Debian, Ubuntu and derivatives (.deb)"),
];

/// Un instalador encontrado en la carpeta.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub file: String,
    pub os: &'static str,
    pub es: &'static str,
    pub en: &'static str,
    pub sha256: Option<String>,
    pub signed: bool,
    pub size: u64,
}

pub fn site(args: Vec<String>) -> Res {
    let mut dir: Option<PathBuf> = None;
    let mut tag: Option<String> = None;
    let mut repo = "ellaguno/iuredesk".to_string();
    let mut out: Option<PathBuf> = None;
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--tag" => tag = it.next(),
            "--repo" => repo = it.next().ok_or("falta el valor de --repo")?,
            "--out" => out = it.next().map(PathBuf::from),
            other if !other.starts_with('-') && dir.is_none() => dir = Some(PathBuf::from(other)),
            other => return Err(format!("opción desconocida: {other}")),
        }
    }
    let dir = dir.ok_or("uso: cargo xtask site <carpeta-con-paquetes> --tag vX.Y.Z")?;
    let tag = tag.ok_or("falta --tag vX.Y.Z")?;
    let out = out.unwrap_or_else(|| dir.join("site"));
    let assets = scan(&dir, tag.trim_start_matches('v'))?;
    if assets.is_empty() {
        return Err(format!("no hay instaladores de {tag} en {}", dir.display()));
    }
    std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    let html = render(&tag, &repo, &assets);
    let index = out.join("index.html");
    std::fs::write(&index, html).map_err(|e| format!("{}: {e}", index.display()))?;
    println!("{} ({} instaladores)", index.display(), assets.len());
    Ok(())
}

/// Instaladores de la versión `ver` en `dir`, con su suma (de `SHA256SUMS`) y si llevan `.sig`.
pub fn scan(dir: &Path, ver: &str) -> Result<Vec<Asset>, String> {
    let sums = std::fs::read_to_string(dir.join("SHA256SUMS")).unwrap_or_default();
    let sum_of = |file: &str| {
        sums.lines().find_map(|l| {
            let (h, f) = l.split_once(char::is_whitespace)?;
            (f.trim().trim_start_matches('*') == file && h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
                .then(|| h.to_ascii_lowercase())
        })
    };
    let mut out = Vec::new();
    for (suffix, os, es, en) in KINDS {
        let file = format!("IureDesk_{ver}-{suffix}");
        let path = dir.join(&file);
        let Ok(meta) = std::fs::metadata(&path) else { continue };
        out.push(Asset {
            sha256: sum_of(&file),
            signed: dir.join(format!("{file}.sig")).is_file(),
            size: meta.len(),
            file,
            os,
            es,
            en,
        });
    }
    Ok(out)
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn mb(n: u64) -> String {
    format!("{:.1} MB", n as f64 / 1_000_000.0)
}

/// La página.
pub fn render(tag: &str, repo: &str, assets: &[Asset]) -> String {
    let ver = esc(tag.trim_start_matches('v'));
    let tag = esc(tag);
    let repo = esc(repo);
    let base = format!("https://github.com/{repo}/releases/download/{tag}");
    let mut rows = String::new();
    for os in ["Windows", "macOS", "Linux"] {
        let list: Vec<&Asset> = assets.iter().filter(|a| a.os == os).collect();
        if list.is_empty() {
            continue;
        }
        let _ = write!(rows, "<section><h2>{os}</h2><ul>");
        for a in list {
            let file = esc(&a.file);
            let sig =
                if a.signed { format!(" · <a href=\"{base}/{file}.sig\">{}</a>", "minisign") } else { String::new() };
            let sum = a.sha256.as_deref().map(|h| format!("<code>{h}</code>")).unwrap_or_default();
            let _ = write!(
                rows,
                "<li><a class=\"dl\" href=\"{base}/{file}\"><span lang=\"es\">{}</span><span lang=\"en\">{}</span></a> \
                 <small>{file} · {}{sig}</small><br>{sum}</li>",
                esc(a.es),
                esc(a.en),
                mb(a.size)
            );
        }
        rows.push_str("</ul></section>");
    }
    format!(
        r##"<!doctype html>
<html lang="es">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>IureDesk {ver} — descargas</title>
<meta name="description" content="IureDesk: correo, calendario, contactos, tareas y notas en Rust para Linux, Windows y macOS.">
<style>
:root {{ color-scheme: light dark; --bg:#f3f4f6; --fg:#1c1e22; --muted:#5c626e; --card:#fff; --accent:#1d58d0; --line:#dee1e6; }}
@media (prefers-color-scheme: dark) {{ :root {{ --bg:#16171a; --fg:#e6e8eb; --muted:#a8aeb8; --card:#1e1f23; --accent:#6ea0fa; --line:#34363c; }} }}
* {{ box-sizing: border-box; }}
body {{ margin:0; background:var(--bg); color:var(--fg); font:16px/1.5 system-ui, sans-serif; }}
main {{ max-width:880px; margin:0 auto; padding:32px 16px 64px; }}
h1 {{ margin:0 0 4px; font-size:2rem; }} h2 {{ margin:0 0 8px; font-size:1.2rem; }}
p.lead {{ color:var(--muted); margin:0 0 24px; }}
section {{ background:var(--card); border:1px solid var(--line); border-radius:10px; padding:16px; margin:0 0 16px; }}
ul {{ list-style:none; margin:0; padding:0; }} li {{ padding:8px 0; border-top:1px solid var(--line); }} li:first-child {{ border-top:0; }}
a {{ color:var(--accent); }} a.dl {{ font-weight:600; }}
small {{ color:var(--muted); }} code {{ font-size:.75rem; color:var(--muted); word-break:break-all; }}
.lang {{ float:right; }} :root:has(#en:target) [lang=es], :root:not(:has(#en:target)) [lang=en] {{ display:none; }}
</style>
</head>
<body>
<main>
<span id="en"></span>
<nav class="lang"><a href="#en" lang="es">English</a><a href="#" lang="en">Español</a></nav>
<h1>IureDesk {ver}</h1>
<p class="lead"><span lang="es">Correo, calendario, contactos, tareas y notas en una sola ventana. Versión de prueba (beta).</span><span lang="en">Mail, calendar, contacts, tasks and notes in one window. Test version (beta).</span></p>
<section>
<p lang="es">La forma más fácil de instalarlo y mantenerlo al día es <a href="https://github.com/ellaguno/iureapps">iureapps</a>. Si prefieres el instalador, elige el de tu sistema:</p>
<p lang="en">The easiest way to install and keep it up to date is <a href="https://github.com/ellaguno/iureapps">iureapps</a>. If you prefer the installer, pick yours:</p>
</section>
{rows}
<section>
<p lang="es"><a href="https://github.com/{repo}/releases/tag/{tag}">Notas de la versión</a> · <a href="https://github.com/{repo}/blob/main/docs/guia/es/README.md">Guía de uso</a> · <a href="{base}/SHA256SUMS">SHA256SUMS</a> · <a href="https://github.com/{repo}/blob/main/docs/minisign.pub">clave minisign</a> · <a href="https://github.com/{repo}">código (Apache-2.0)</a></p>
<p lang="en"><a href="https://github.com/{repo}/releases/tag/{tag}">Release notes</a> · <a href="https://github.com/{repo}/blob/main/docs/guia/en/README.md">User guide</a> · <a href="{base}/SHA256SUMS">SHA256SUMS</a> · <a href="https://github.com/{repo}/blob/main/docs/minisign.pub">minisign key</a> · <a href="https://github.com/{repo}">source (Apache-2.0)</a></p>
<p lang="es"><small>Comprueba una descarga con <code>sha256sum -c SHA256SUMS</code>. En Windows, mientras el instalador no esté firmado, SmartScreen pide «Más información → Ejecutar de todas formas».</small></p>
<p lang="en"><small>Check a download with <code>sha256sum -c SHA256SUMS</code>. On Windows, until the installer is signed, SmartScreen asks for "More info → Run anyway".</small></p>
</section>
</main>
</body>
</html>
"##
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_and_renders_the_release() {
        let d = std::env::temp_dir().join(format!("iuredesk-site-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        for f in [
            "IureDesk_0.1.0-beta.1-windows-x64.exe",
            "IureDesk_0.1.0-beta.1-linux-x64.AppImage",
            "IureDesk_0.1.0-beta.1-macos-arm64.dmg",
            "otro.txt",
        ] {
            std::fs::write(d.join(f), b"x").unwrap();
        }
        std::fs::write(d.join("IureDesk_0.1.0-beta.1-windows-x64.exe.sig"), b"s").unwrap();
        let h = "a".repeat(64);
        std::fs::write(
            d.join("SHA256SUMS"),
            format!("{h}  IureDesk_0.1.0-beta.1-windows-x64.exe\nzz  IureDesk_0.1.0-beta.1-linux-x64.AppImage\n"),
        )
        .unwrap();
        let a = scan(&d, "0.1.0-beta.1").unwrap();
        assert_eq!(a.len(), 3);
        let win = a.iter().find(|x| x.os == "Windows").unwrap();
        assert!(win.signed && win.sha256.as_deref() == Some(h.as_str()));
        let lin = a.iter().find(|x| x.os == "Linux").unwrap();
        assert!(!lin.signed && lin.sha256.is_none(), "suma mal formada");
        let html = render("v0.1.0-beta.1", "ellaguno/iuredesk", &a);
        assert!(html.contains("https://github.com/ellaguno/iuredesk/releases/download/v0.1.0-beta.1/IureDesk_0.1.0-beta.1-windows-x64.exe\""));
        assert!(html.contains(".exe.sig"));
        assert!(!html.contains("<script"));
        assert!(html.find("<h2>Windows</h2>") < html.find("<h2>macOS</h2>"));
        assert!(!html.contains("<h2>Linux</h2>") || html.contains("AppImage"));
        // Nada sin escapar.
        let evil = render("v1<script>", "a/b\"", &a);
        assert!(!evil.contains("<script>") && !evil.contains("a/b\""));
        let _ = std::fs::remove_dir_all(&d);
    }
}
