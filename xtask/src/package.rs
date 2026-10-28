//! `cargo xtask package <linux|windows|macos> [--tag vX.Y.Z] [--target <triple>]`: instaladores con
//! `cargo-packager` (herramienta de compilación, se usa por su CLI: `cargo install cargo-packager
//! --version 0.11.8 --locked`; nada de ella entra en el producto). La configuración se arma aquí
//! (versión del workspace, asociaciones por SO, firma desde variables de entorno) y queda en
//! `target/packager/<triple>/packager.json` para revisarla.
//!
//! - linux: el `.tar.gz` de `cargo xtask dist` + `.deb` + `.AppImage` (con `.desktop`, metainfo
//!   AppStream e iconos hicolor).
//! - windows: instalador NSIS `.exe` por usuario (sin UAC) que registra `mailto:`, `.eml`, `.ics` y
//!   `.vcf` en «Aplicaciones predeterminadas» (`packaging/windows/default-apps.nsh`).
//! - macos: `.app` (Info.plist con el esquema `mailto` y los tipos de documento) y `.dmg`.
//!
//! Todo queda en `dist/` con `SHA256SUMS`. Firma (opcional; sin las variables sale sin firmar):
//! - macOS: `APPLE_SIGNING_IDENTITY` (+ `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD` en CI) y
//!   notarización con `APPLE_ID`/`APPLE_PASSWORD`/`APPLE_TEAM_ID` o `APPLE_API_KEY`/
//!   `APPLE_API_ISSUER`/`APPLE_API_KEY_PATH` (las lee cargo-packager).
//! - Windows: `WINDOWS_SIGN_COMMAND` (p. ej. Azure Trusted Signing; `%1` = archivo) o
//!   `WINDOWS_CERTIFICATE_THUMBPRINT` (+ `WINDOWS_TIMESTAMP_URL`) con signtool.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

use crate::{DIST_BINS, Res, cargo, copy, metadata, notices_file_for, packages, root, s, sha256_hex};

const PRODUCT: &str = "IureDesk";
const IDENTIFIER: &str = "com.iurefficient.IureDesk";
const PUBLISHER: &str = "Eduardo Llaguno Velasco";
const DESCRIPTION: &str = "Mail, calendar, contacts, notes and tasks";
/// Versión de cargo-packager con la que se probó esta configuración.
pub const PACKAGER_VERSION: &str = "0.11.8";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Os {
    Linux,
    Windows,
    Mac,
}

impl Os {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "linux" => Some(Self::Linux),
            "windows" => Some(Self::Windows),
            "macos" => Some(Self::Mac),
            _ => None,
        }
    }

    fn default_target(self) -> &'static str {
        match self {
            Self::Linux => crate::DIST_TARGET,
            Self::Windows => "x86_64-pc-windows-msvc",
            Self::Mac => {
                if cfg!(target_arch = "x86_64") {
                    "x86_64-apple-darwin"
                } else {
                    "aarch64-apple-darwin"
                }
            }
        }
    }

    fn formats(self) -> &'static [&'static str] {
        match self {
            Self::Linux => &["deb", "appimage"],
            Self::Windows => &["nsis"],
            Self::Mac => &["app", "dmg"],
        }
    }
}

struct Args {
    os: Os,
    tag: Option<String>,
    target: Option<String>,
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    const USAGE: &str = "uso: cargo xtask package <linux|windows|macos> [--tag vX.Y.Z] [--target <triple>]";
    let mut it = args.iter();
    let os = it.next().and_then(|a| Os::parse(a)).ok_or(USAGE)?;
    let (mut tag, mut target) = (None, None);
    while let Some(flag) = it.next() {
        let value = it.next().ok_or(USAGE)?.clone();
        match flag.as_str() {
            "--tag" => tag = Some(value),
            "--target" => target = Some(value),
            _ => return Err(USAGE.into()),
        }
    }
    Ok(Args { os, tag, target })
}

pub fn package(raw: Vec<String>) -> Res {
    let args = parse_args(&raw)?;
    let m = metadata(None)?;
    let version = packages(&m).find(|p| s(p, "name") == "iuredesk").map(|p| s(p, "version")).unwrap_or("").to_string();
    if version.is_empty() {
        return Err("no encuentro la versión del paquete iuredesk".into());
    }
    if let Some(tag) = &args.tag
        && tag.strip_prefix('v') != Some(version.as_str())
    {
        return Err(format!("la etiqueta {tag} no coincide con la versión del workspace ({version})"));
    }
    let target = args.target.clone().unwrap_or_else(|| args.os.default_target().to_string());
    let dist = root().join("dist");

    if args.os == Os::Linux && target == crate::DIST_TARGET {
        // El .tar.gz de siempre (compila y vacía dist/); luego se le suman .deb y .AppImage.
        let mut dist_args = Vec::new();
        if let Some(tag) = &args.tag {
            dist_args.extend(["--tag".to_string(), tag.clone()]);
        }
        crate::dist(dist_args)?;
    } else {
        let mut build = vec!["build", "--profile", "dist", "--locked", "--target", &target];
        for b in DIST_BINS {
            build.extend(["-p", b]);
        }
        cargo(&build)?;
        if dist.exists() {
            std::fs::remove_dir_all(&dist).map_err(|e| format!("{}: {e}", dist.display()))?;
        }
        std::fs::create_dir_all(&dist).map_err(|e| format!("{}: {e}", dist.display()))?;
    }

    // Documentos que acompañan a los binarios.
    let stage = root().join("target/packager").join(&target);
    std::fs::create_dir_all(&stage).map_err(|e| format!("{}: {e}", stage.display()))?;
    for f in ["LICENSE.md", "NOTICE"] {
        copy(&root().join(f), &stage.join(f))?;
    }
    copy(&root().join("packaging/README.txt"), &stage.join("README.txt"))?;
    notices_file_for(&stage.join("THIRD-PARTY-NOTICES.txt"), &target)?;

    let config = config(args.os, &version, &target, &stage, &dist)?;
    let config_path = stage.join("packager.json");
    let text = serde_json::to_string_pretty(&config).map_err(|e| e.to_string())?;
    std::fs::write(&config_path, text).map_err(|e| format!("{}: {e}", config_path.display()))?;

    run_packager(&config_path, &target)?;
    write_sums(&dist)
}

/// Configuración de cargo-packager (esquema: `cargo_packager::Config`, camelCase).
fn config(os: Os, version: &str, target: &str, stage: &Path, dist: &Path) -> Result<Value, String> {
    let icons = root().join("assets/icons");
    let packaging = root().join("packaging");
    let p = |path: PathBuf| path.display().to_string();
    let bin_dir = root().join("target").join(target).join("dist");

    let mut cfg = json!({
        // Con `name` puesto, cargo-packager 0.11.8 no busca el Cargo.toml «más cercano» al archivo
        // de configuración (falla con «Not a directory»).
        "name": "iuredesk",
        "productName": PRODUCT,
        "version": version,
        "identifier": IDENTIFIER,
        "description": DESCRIPTION,
        "longDescription": "IureDesk: correo, calendario, contactos, notas y tareas, 100 % en Rust, integrado con Iurefficient.",
        "homepage": "https://iurefficient.com/",
        "authors": [PUBLISHER],
        "publisher": PUBLISHER,
        "copyright": "© 2026 Eduardo Llaguno Velasco. Apache-2.0.",
        "licenseFile": p(root().join("LICENSE.md")),
        "category": "Productivity",
        "outDir": p(dist.to_path_buf()),
        "binariesDir": p(bin_dir),
        "targetTriple": target,
        "binaries": [{ "path": "iuredesk", "main": true }],
        "formats": os.formats(),
        "logLevel": "info",
    });
    let obj = cfg.as_object_mut().ok_or("configuración inválida")?;

    match os {
        Os::Linux => {
            // Iconos hicolor: los PNG cuadrados (cargo-packager toma el tamaño del archivo).
            let pngs: Vec<String> = crate::icons::SIZES
                .iter()
                .filter(|&&n| n <= 512)
                .map(|n| p(icons.join(format!("iuredesk-{n}.png"))))
                .collect();
            obj.insert("icons".into(), json!(pngs));
            let mut files = serde_json::Map::new();
            files.insert(
                p(packaging.join("linux/com.iurefficient.IureDesk.metainfo.xml")),
                json!("usr/share/metainfo/com.iurefficient.IureDesk.metainfo.xml"),
            );
            files.insert(p(icons.join("iuredesk.svg")), json!("usr/share/icons/hicolor/scalable/apps/iuredesk.svg"));
            for f in ["LICENSE.md", "NOTICE", "README.txt", "THIRD-PARTY-NOTICES.txt"] {
                files.insert(p(stage.join(f)), json!(format!("usr/share/doc/iuredesk/{f}")));
            }
            // El .desktop es fijo (MimeType, categorías, acciones): se pasa como plantilla.
            obj.insert(
                "deb".into(),
                json!({
                    "desktopTemplate": p(packaging.join("linux/iuredesk.desktop")),
                    "section": "mail",
                    "priority": "optional",
                    "packageName": "iuredesk",
                    "files": files,
                }),
            );
            obj.insert("appimage".into(), json!({ "files": files }));
        }
        Os::Windows => {
            obj.insert("icons".into(), json!([p(icons.join("iuredesk.ico"))]));
            obj.insert("fileAssociations".into(), file_associations(true));
            obj.insert("resources".into(), resources(stage));
            let section = std::fs::read_to_string(packaging.join("windows/default-apps.nsh"))
                .map_err(|e| format!("packaging/windows/default-apps.nsh: {e}"))?;
            obj.insert(
                "nsis".into(),
                json!({
                    "installMode": "currentUser",
                    "installerIcon": p(icons.join("iuredesk.ico")),
                    "languages": ["English", "Spanish"],
                    "displayLanguageSelector": false,
                    "preinstallSection": section,
                }),
            );
            obj.insert("windows".into(), windows_signing());
        }
        Os::Mac => {
            obj.insert("icons".into(), json!([p(icons.join("iuredesk.icns"))]));
            obj.insert("fileAssociations".into(), file_associations(false));
            obj.insert(
                "deepLinkProtocols".into(),
                json!([{ "schemes": ["mailto"], "name": "E-mail address", "role": "viewer" }]),
            );
            obj.insert("resources".into(), resources(stage));
            let mut macos = json!({ "minimumSystemVersion": "11.0" });
            if let Some(identity) = env("APPLE_SIGNING_IDENTITY") {
                macos["signingIdentity"] = json!(identity);
            } else {
                println!("package: sin APPLE_SIGNING_IDENTITY; el .app y el .dmg salen sin firmar");
            }
            obj.insert("macos".into(), macos);
        }
    }
    Ok(cfg)
}

/// Archivos que abre IureDesk. En Windows el nombre es el ProgID que usa `default-apps.nsh`.
fn file_associations(windows: bool) -> Value {
    let item = |ext: &str, mime: &str, desc: &str| {
        let name = if windows { format!("IureDesk.{ext}") } else { desc.to_string() };
        json!({ "extensions": [ext], "mimeType": mime, "description": desc, "name": name, "role": "viewer" })
    };
    json!([
        item("eml", "message/rfc822", "E-mail message"),
        item("ics", "text/calendar", "iCalendar file"),
        item("vcf", "text/vcard", "vCard contact"),
    ])
}

fn resources(stage: &Path) -> Value {
    let files: Vec<String> = ["LICENSE.md", "NOTICE", "README.txt", "THIRD-PARTY-NOTICES.txt"]
        .iter()
        .map(|f| stage.join(f).display().to_string())
        .collect();
    json!(files)
}

/// Firma en Windows: comando propio (Azure Trusted Signing, etc.) o certificado del almacén.
fn windows_signing() -> Value {
    if let Some(cmd) = env("WINDOWS_SIGN_COMMAND") {
        return json!({ "signCommand": cmd });
    }
    if let Some(thumb) = env("WINDOWS_CERTIFICATE_THUMBPRINT") {
        let ts = env("WINDOWS_TIMESTAMP_URL").unwrap_or_else(|| "http://timestamp.digicert.com".into());
        return json!({ "certificateThumbprint": thumb, "digestAlgorithm": "sha256", "timestampUrl": ts });
    }
    println!("package: sin WINDOWS_SIGN_COMMAND ni WINDOWS_CERTIFICATE_THUMBPRINT; el instalador sale sin firmar");
    json!({})
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// Ejecuta `cargo packager` (o el binario de `CARGO_PACKAGER`).
fn run_packager(config: &Path, target: &str) -> Res {
    // `cargo packager …` o, directamente, `cargo-packager packager …` (así lo llama cargo).
    let program = env("CARGO_PACKAGER").unwrap_or_else(|| env!("CARGO").to_string());
    let pre: Vec<String> =
        vec!["packager".into(), "--config".into(), config.display().to_string(), "--target".into(), target.into()];
    println!("$ {program} {}", pre.join(" "));
    let status = Command::new(&program)
        .args(&pre)
        .current_dir(root())
        .status()
        .map_err(|e| format!("no pude ejecutar cargo-packager ({e}); instálalo con `cargo install cargo-packager --version {PACKAGER_VERSION} --locked`"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "cargo-packager falló; ¿está instalado? `cargo install cargo-packager --version {PACKAGER_VERSION} --locked`"
        ))
    }
}

/// Extensiones de los archivos publicables de `dist/`.
const ARTIFACTS: &[&str] = &[".tar.gz", ".deb", ".AppImage", ".exe", ".msi", ".dmg"];

/// `SHA256SUMS` con todos los paquetes de `dist/` (formato de `sha256sum -c`).
fn write_sums(dist: &Path) -> Res {
    let mut names: Vec<String> = std::fs::read_dir(dist)
        .map_err(|e| format!("{}: {e}", dist.display()))?
        .flatten()
        .filter(|e| e.path().is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| ARTIFACTS.iter().any(|ext| n.ends_with(ext)))
        .collect();
    names.sort();
    if names.is_empty() {
        return Err(format!("cargo-packager no dejó paquetes en {}", dist.display()));
    }
    let mut sums = String::new();
    for n in &names {
        let bytes = std::fs::read(dist.join(n)).map_err(|e| format!("{n}: {e}"))?;
        sums.push_str(&format!("{}  {n}\n", sha256_hex(&bytes)));
        println!("package: {n} ({} KB)", bytes.len() / 1024);
    }
    std::fs::write(dist.join("SHA256SUMS"), &sums).map_err(|e| e.to_string())?;
    print!("{sums}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn args() {
        let a = parse_args(&strings(&["macos", "--tag", "v1.0.0", "--target", "x86_64-apple-darwin"]));
        assert!(a.as_ref().is_ok_and(|a| a.os == Os::Mac && a.tag.as_deref() == Some("v1.0.0")));
        assert!(parse_args(&strings(&["linux"])).is_ok_and(|a| a.target.is_none()));
        assert!(parse_args(&strings(&["haiku"])).is_err());
        assert!(parse_args(&strings(&["linux", "--tag"])).is_err());
        assert!(parse_args(&strings(&["linux", "--foo", "x"])).is_err());
    }

    #[test]
    fn config_per_os() {
        let stage = Path::new("/tmp/stage");
        let dist = Path::new("/tmp/dist");
        let linux = config(Os::Linux, "1.2.3", crate::DIST_TARGET, stage, dist).unwrap_or_default();
        assert_eq!(linux["formats"], json!(["deb", "appimage"]));
        assert!(linux["deb"]["desktopTemplate"].as_str().is_some_and(|p| p.ends_with("iuredesk.desktop")));
        let mac = config(Os::Mac, "1.2.3", "aarch64-apple-darwin", stage, dist).unwrap_or_default();
        assert_eq!(mac["deepLinkProtocols"][0]["schemes"], json!(["mailto"]));
        let win = config(Os::Windows, "1.2.3", "x86_64-pc-windows-msvc", stage, dist).unwrap_or_default();
        assert_eq!(win["fileAssociations"][0]["name"], json!("IureDesk.eml"));
        assert!(win["nsis"]["preinstallSection"].as_str().is_some_and(|s| s.contains("RegisteredApplications")));
    }

    /// Cada archivo que la configuración referencia existe en el repositorio.
    #[test]
    fn packaging_files_exist() {
        for f in [
            "packaging/README.txt",
            "packaging/linux/iuredesk.desktop",
            "packaging/linux/com.iurefficient.IureDesk.metainfo.xml",
            "packaging/windows/default-apps.nsh",
            "assets/icons/iuredesk.ico",
            "assets/icons/iuredesk.icns",
            "assets/icons/iuredesk.svg",
        ] {
            assert!(root().join(f).is_file(), "falta {f}");
        }
        for n in crate::icons::SIZES {
            assert!(root().join(format!("assets/icons/iuredesk-{n}.png")).is_file(), "falta el PNG de {n}");
        }
    }

    /// El .desktop declara lo que pide la integración con el escritorio.
    #[test]
    fn desktop_entry() {
        let text = std::fs::read_to_string(root().join("packaging/linux/iuredesk.desktop")).unwrap_or_default();
        for want in [
            "MimeType=x-scheme-handler/mailto;text/calendar;text/vcard;",
            "message/rfc822;",
            "Categories=Office;Email;Calendar;ContactManagement;",
            "Exec=iuredesk %U",
            "Icon=iuredesk",
        ] {
            assert!(text.contains(want), "falta «{want}» en iuredesk.desktop");
        }
        // Sin variables de plantilla: cargo-packager lo usa tal cual.
        assert!(!text.contains("{{"));
    }
}
