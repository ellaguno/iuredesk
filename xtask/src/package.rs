//! `cargo xtask package <linux|windows|macos> [--tag vX.Y.Z] [--target <triple>]`: instaladores con
//! `cargo-packager` (herramienta de compilación, se usa por su CLI: `cargo install cargo-packager
//! --version 0.11.8 --locked`; nada de ella entra en el producto). La configuración se arma aquí
//! (versión del workspace, asociaciones por SO, firma desde variables de entorno) y queda en
//! `target/packager/<triple>/packager.json` para revisarla.
//!
//! - linux: el `.tar.gz` de `cargo xtask dist` + `.deb` + `.AppImage` (con `.desktop`, metainfo
//!   AppStream e iconos hicolor).
//! - windows: instalador NSIS `.exe` por usuario (sin UAC) que registra el protocolo `iuredesk:`, el
//!   AUMID de los avisos y `mailto:`, `.eml`, `.ics` y `.vcf` en «Aplicaciones predeterminadas»
//!   (`packaging/windows/default-apps.nsh`); accesos directos con el AUMID (plantilla de
//!   cargo-packager).
//! - macos: `.app` (claves de `packaging/macos/Info.plist`: esquemas `iuredesk` y `mailto`, tipos de
//!   documento) y `.dmg`.
//!
//! Los instaladores se renombran al esquema de iureapps, el lanzador de la suite:
//! `IureDesk_<versión>-linux-x64.AppImage`, `-linux-x64.deb`, `-windows-x64.exe`,
//! `-macos-arm64.app.tar.gz`/`-macos-x64.app.tar.gz` (el `.app` en tar.gz) y `-macos-<arq>.dmg`.
//! `cargo xtask sign` les agrega la firma minisign `.sig` que verifica iureapps.
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
    rename_for_iureapps(args.os, &version, &target, &dist)?;
    write_sums(&dist)
}

/// Plataforma en el esquema de nombres de iureapps (`select_asset` de su `catalog.rs`).
fn platform(os: Os, target: &str) -> String {
    let arch = if target.starts_with("aarch64") { "arm64" } else { "x64" };
    let os = match os {
        Os::Linux => "linux",
        Os::Windows => "windows",
        Os::Mac => "macos",
    };
    format!("{os}-{arch}")
}

/// Nombre publicado: `IureDesk_<versión>-<plataforma><ext>`. iureapps busca el instalador por el
/// final del nombre (`-linux-x64.AppImage`, `-windows-x64.exe`, `-macos-arm64.app.tar.gz`…).
fn asset_name(version: &str, platform: &str, ext: &str) -> String {
    format!("{PRODUCT}_{version}-{platform}{ext}")
}

/// Renombra lo que deja cargo-packager al esquema de iureapps y, en macOS, empaqueta el `.app`
/// como `.app.tar.gz` (lo que instala iureapps en /Applications).
fn rename_for_iureapps(os: Os, version: &str, target: &str, dist: &Path) -> Res {
    let plat = platform(os, target);
    let exts: &[&str] = match os {
        Os::Linux => &[".deb", ".AppImage"],
        Os::Windows => &[".exe"],
        Os::Mac => &[".dmg"],
    };
    let files: Vec<PathBuf> = std::fs::read_dir(dist)
        .map_err(|e| format!("{}: {e}", dist.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    for ext in exts {
        let found: Vec<&PathBuf> =
            files.iter().filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().ends_with(ext))).collect();
        let [from] = found.as_slice() else {
            return Err(format!(
                "esperaba un solo {ext} de cargo-packager en {} y hay {}",
                dist.display(),
                found.len()
            ));
        };
        let to = dist.join(asset_name(version, &plat, ext));
        std::fs::rename(from, &to).map_err(|e| format!("{} → {}: {e}", from.display(), to.display()))?;
    }
    if os == Os::Mac {
        let app = dist.join(format!("{PRODUCT}.app"));
        if !app.is_dir() {
            return Err(format!("cargo-packager no dejó {}", app.display()));
        }
        tar_app(&app, &dist.join(asset_name(version, &plat, ".app.tar.gz")))?;
    }
    Ok(())
}

/// `IureDesk.app` → `.app.tar.gz` con la carpeta en la raíz, permisos y enlaces simbólicos tal cual.
fn tar_app(app: &Path, out: &Path) -> Res {
    let name = app.file_name().ok_or("ruta del .app sin nombre")?;
    let file = std::fs::File::create(out).map_err(|e| format!("{}: {e}", out.display()))?;
    let gz = flate2::write::GzEncoder::new(file, flate2::Compression::best());
    let mut tar = tar::Builder::new(gz);
    tar.follow_symlinks(false);
    tar.append_dir_all(name, app).map_err(|e| format!("{}: {e}", app.display()))?;
    let gz = tar.into_inner().map_err(|e| e.to_string())?;
    gz.finish().map_err(|e| e.to_string())?;
    Ok(())
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
            // Sin `fileAssociations` ni `deepLinkProtocols`: protocolos, tipos de archivo, AUMID y
            // «Aplicaciones predeterminadas» los registra (y los borra) `default-apps.nsh`.
            // El .ico junto al .exe es el `IconUri` del AUMID (icono de los avisos).
            let mut res = resources(stage);
            if let Some(list) = res.as_array_mut() {
                list.push(json!(p(icons.join("iuredesk.ico"))));
            }
            obj.insert("resources".into(), res);
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
            obj.insert("resources".into(), resources(stage));
            // Esquemas (`iuredesk`, `mailto`) y tipos de documento: `packaging/macos/Info.plist`,
            // que cargo-packager mezcla sobre el que genera.
            let mut macos = json!({
                "minimumSystemVersion": "11.0",
                "infoPlistPath": p(packaging.join("macos/Info.plist")),
            });
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
const ARTIFACTS: &[&str] = &[".tar.gz", ".deb", ".AppImage", ".exe", ".msi", ".dmg", ".sig"];

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

    /// Los nombres terminan como los busca iureapps (`-{plataforma}{ext}`).
    #[test]
    fn iureapps_names() {
        let n = |os, target, ext| asset_name("0.1.0-alpha.1", &platform(os, target), ext);
        assert_eq!(n(Os::Linux, crate::DIST_TARGET, ".AppImage"), "IureDesk_0.1.0-alpha.1-linux-x64.AppImage");
        assert_eq!(n(Os::Linux, crate::DIST_TARGET, ".deb"), "IureDesk_0.1.0-alpha.1-linux-x64.deb");
        assert_eq!(n(Os::Windows, "x86_64-pc-windows-msvc", ".exe"), "IureDesk_0.1.0-alpha.1-windows-x64.exe");
        assert_eq!(n(Os::Mac, "aarch64-apple-darwin", ".app.tar.gz"), "IureDesk_0.1.0-alpha.1-macos-arm64.app.tar.gz");
        assert_eq!(n(Os::Mac, "x86_64-apple-darwin", ".app.tar.gz"), "IureDesk_0.1.0-alpha.1-macos-x64.app.tar.gz");
    }

    #[test]
    fn renames_packager_output() -> Res {
        let dist = std::env::temp_dir().join(format!("iuredesk-rename-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dist);
        std::fs::create_dir_all(&dist).map_err(|e| e.to_string())?;
        for f in ["iuredesk_1.0.0_amd64.deb", "iuredesk_1.0.0_x86_64.AppImage", "iuredesk-1.0.0-x86_64-linux.tar.gz"] {
            std::fs::write(dist.join(f), b"x").map_err(|e| e.to_string())?;
        }
        rename_for_iureapps(Os::Linux, "1.0.0", crate::DIST_TARGET, &dist)?;
        for f in
            ["IureDesk_1.0.0-linux-x64.deb", "IureDesk_1.0.0-linux-x64.AppImage", "iuredesk-1.0.0-x86_64-linux.tar.gz"]
        {
            assert!(dist.join(f).is_file(), "falta {f}");
        }
        // Sin el paquete esperado es un error, no un release a medias.
        assert!(rename_for_iureapps(Os::Windows, "1.0.0", "x86_64-pc-windows-msvc", &dist).is_err());

        // macOS: el .app se empaqueta con la carpeta en la raíz.
        let app = dist.join("IureDesk.app/Contents/MacOS");
        std::fs::create_dir_all(&app).map_err(|e| e.to_string())?;
        std::fs::write(app.join("iuredesk"), b"bin").map_err(|e| e.to_string())?;
        std::fs::write(dist.join("IureDesk_1.0.0_aarch64.dmg"), b"x").map_err(|e| e.to_string())?;
        rename_for_iureapps(Os::Mac, "1.0.0", "aarch64-apple-darwin", &dist)?;
        assert!(dist.join("IureDesk_1.0.0-macos-arm64.dmg").is_file());
        let tgz = std::fs::File::open(dist.join("IureDesk_1.0.0-macos-arm64.app.tar.gz")).map_err(|e| e.to_string())?;
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(tgz));
        let names: Vec<String> = archive
            .entries()
            .map_err(|e| e.to_string())?
            .flatten()
            .filter_map(|e| e.path().ok().map(|p| p.to_string_lossy().into_owned()))
            .collect();
        assert!(names.iter().any(|n| n == "IureDesk.app/Contents/MacOS/iuredesk"), "{names:?}");
        let _ = std::fs::remove_dir_all(&dist);
        Ok(())
    }

    #[test]
    fn config_per_os() {
        let stage = Path::new("/tmp/stage");
        let dist = Path::new("/tmp/dist");
        let linux = config(Os::Linux, "1.2.3", crate::DIST_TARGET, stage, dist).unwrap_or_default();
        assert_eq!(linux["formats"], json!(["deb", "appimage"]));
        assert!(linux["deb"]["desktopTemplate"].as_str().is_some_and(|p| p.ends_with("iuredesk.desktop")));
        let mac = config(Os::Mac, "1.2.3", "aarch64-apple-darwin", stage, dist).unwrap_or_default();
        assert!(mac["macos"]["infoPlistPath"].as_str().is_some_and(|p| Path::new(p).is_file()));
        assert_eq!(mac["category"], json!("Productivity"));
        assert!(mac["icons"][0].as_str().is_some_and(|p| p.ends_with("iuredesk.icns")));
        let win = config(Os::Windows, "1.2.3", "x86_64-pc-windows-msvc", stage, dist).unwrap_or_default();
        assert!(win["nsis"]["preinstallSection"].as_str().is_some_and(|s| s.contains("RegisteredApplications")));
        assert_eq!(win["nsis"]["installMode"], json!("currentUser"));
        // El .ico va junto al .exe (IconUri del AUMID).
        let res = win["resources"].as_array().cloned().unwrap_or_default();
        assert!(res.iter().any(|r| r.as_str().is_some_and(|p| p.ends_with("iuredesk.ico"))), "{res:?}");
        // Las asociaciones las hace default-apps.nsh (la macro de cargo-packager no pone comillas).
        for cfg in [&linux, &mac, &win] {
            assert!(cfg.get("fileAssociations").is_none() && cfg.get("deepLinkProtocols").is_none());
            assert_eq!(cfg["identifier"], json!(IDENTIFIER));
        }
    }

    /// El identificador del paquete es el AUMID y el `CFBundleIdentifier` que usa la app.
    #[test]
    fn identifier_matches_app() {
        let lib = std::fs::read_to_string(root().join("crates/desktop/src/lib.rs")).unwrap_or_default();
        assert!(lib.contains(&format!("APP_ID: &str = \"{IDENTIFIER}\"")), "APP_ID distinto de {IDENTIFIER}");
    }

    /// Divide una línea de NSIS en palabras (comillas dobles; `$\"` es una comilla literal).
    fn nsis_words(line: &str) -> Vec<String> {
        let (mut out, mut cur, mut quoted, mut any) = (Vec::new(), String::new(), false, false);
        let mut it = line.chars().peekable();
        while let Some(c) = it.next() {
            match c {
                '$' if it.peek() == Some(&'\\') => {
                    it.next();
                    if let Some(n) = it.next() {
                        cur.push(n);
                    }
                }
                '"' => {
                    quoted = !quoted;
                    any = true;
                }
                c if c.is_whitespace() && !quoted => {
                    if any || !cur.is_empty() {
                        out.push(std::mem::take(&mut cur));
                    }
                    any = false;
                }
                c => cur.push(c),
            }
        }
        if any || !cur.is_empty() {
            out.push(cur);
        }
        out
    }

    type Macros = std::collections::BTreeMap<String, (Vec<String>, Vec<String>)>;

    fn expand_macros(line: &str, macros: &Macros, out: &mut Vec<String>, depth: u32) {
        let w = nsis_words(line.trim());
        if w.first().map(String::as_str) == Some("!insertmacro")
            && depth < 4
            && let Some((params, body)) = w.get(1).and_then(|n| macros.get(n))
        {
            for b in body {
                let mut l = b.clone();
                for (p, a) in params.iter().zip(w.iter().skip(2)) {
                    l = l.replace(&format!("${{{p}}}"), a);
                }
                expand_macros(&l, macros, out, depth + 1);
            }
        } else {
            out.push(line.to_string());
        }
    }

    /// Expande `!define`/`!insertmacro` de `default-apps.nsh` (y los `!define` de la plantilla de
    /// cargo-packager con los valores de la configuración) y devuelve las órdenes de cada sección.
    fn nsis_sections(text: &str) -> std::collections::BTreeMap<String, Vec<Vec<String>>> {
        use std::collections::BTreeMap;
        let mut defines: BTreeMap<String, String> = BTreeMap::from([
            ("IDENTIFIER".into(), IDENTIFIER.into()),
            ("PRODUCTNAME".into(), PRODUCT.into()),
            ("MAINBINARYNAME".into(), "iuredesk".into()),
        ]);
        let mut macros = Macros::new();
        let mut sections: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let (mut in_macro, mut in_section): (Option<String>, Option<String>) = (None, None);
        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with(';') {
                continue;
            }
            let w = nsis_words(line);
            let head = w.first().map(String::as_str).unwrap_or("");
            if let Some(m) = &in_macro {
                if head == "!macroend" {
                    in_macro = None;
                } else if let Some(entry) = macros.get_mut(m) {
                    entry.1.push(raw.to_string());
                }
                continue;
            }
            if let Some(sec) = &in_section {
                if head == "SectionEnd" {
                    in_section = None;
                } else {
                    sections.entry(sec.clone()).or_default().push(raw.to_string());
                }
                continue;
            }
            match head {
                "!define" => {
                    // El valor tal cual (con sus `$\"`): se expande dentro de la línea que lo usa.
                    if let (Some(k), Some(a), Some(b)) = (w.get(1), line.find('"'), line.rfind('"'))
                        && b > a
                    {
                        defines.insert(k.clone(), line[a + 1..b].to_string());
                    }
                }
                "!macro" => {
                    let name = w.get(1).cloned().unwrap_or_default();
                    macros.insert(name.clone(), (w.iter().skip(2).cloned().collect(), Vec::new()));
                    in_macro = Some(name);
                }
                "Section" => in_section = Some(w.get(1).cloned().unwrap_or_default()),
                _ => {}
            }
        }
        let expand_defines = |mut line: String| {
            for _ in 0..8 {
                let before = line.clone();
                for (k, v) in &defines {
                    line = line.replace(&format!("${{{k}}}"), v);
                }
                if line == before {
                    break;
                }
            }
            line
        };
        sections
            .into_iter()
            .map(|(name, lines)| {
                let mut flat = Vec::new();
                for l in &lines {
                    expand_macros(l, &macros, &mut flat, 0);
                }
                (name, flat.into_iter().map(|l| nsis_words(expand_defines(l).trim())).collect())
            })
            .collect()
    }

    /// R1.3: el instalador de Windows registra lo que pide R1.2 (protocolo `iuredesk`, AUMID,
    /// `mailto:` y archivos en «Aplicaciones predeterminadas») y el desinstalador lo borra todo.
    #[test]
    fn windows_registration() {
        let text = std::fs::read_to_string(root().join("packaging/windows/default-apps.nsh")).unwrap_or_default();
        let sections = nsis_sections(&text);
        let install = sections.get("IureDeskDefaultApps").cloned().unwrap_or_default();
        let uninstall = sections.get("un.IureDeskDefaultApps").cloned().unwrap_or_default();
        assert!(!install.is_empty() && !uninstall.is_empty(), "faltan las secciones: {:?}", sections.keys());

        // (clave, valor, dato) de cada WriteRegStr; todo por usuario (SHCTX = HKCU con currentUser).
        let mut writes = Vec::new();
        for w in &install {
            if w.first().map(String::as_str) == Some("WriteRegStr") {
                assert_eq!(w.get(1).map(String::as_str), Some("SHCTX"), "{w:?}");
                let at = |i: usize| w.get(i).cloned().unwrap_or_default();
                writes.push((at(2), at(3), at(4)));
            }
        }
        let has = |k: &str, v: &str, d: &str| writes.iter().any(|(a, b, c)| a == k && b == v && c == d);
        let open = r#""$INSTDIR\iuredesk.exe" "%1""#;
        let caps = r"Software\Iurefficient\IureDesk\Capabilities";
        let aumid = format!(r"Software\Classes\AppUserModelId\{IDENTIFIER}");
        let want: Vec<(String, &str, &str)> = vec![
            (r"Software\Classes\iuredesk".into(), "URL Protocol", ""),
            (r"Software\Classes\iuredesk\shell\open\command".into(), "", open),
            (r"Software\Classes\IureDesk.mailto".into(), "URL Protocol", ""),
            (r"Software\Classes\IureDesk.mailto\shell\open\command".into(), "", open),
            (format!(r"{caps}\URLAssociations"), "mailto", "IureDesk.mailto"),
            (r"Software\RegisteredApplications".into(), "IureDesk", caps),
            (aumid.clone(), "DisplayName", PRODUCT),
            (aumid.clone(), "IconUri", r"$INSTDIR\iuredesk.ico"),
        ];
        for (k, v, d) in &want {
            assert!(has(k, v, d), "falta {k} [{v}] = {d}\n{writes:#?}");
        }
        for ext in ["eml", "ics", "vcf"] {
            let progid = format!("IureDesk.{ext}");
            assert!(has(&format!(r"Software\Classes\{progid}\shell\open\command"), "", open), "{ext}");
            assert!(has(&format!(r"Software\Classes\.{ext}\OpenWithProgids"), &progid, ""), "{ext}");
            assert!(has(&format!(r"{caps}\FileAssociations"), &format!(".{ext}"), &progid), "{ext}");
        }
        // Cada orden de abrir lleva la ruta entre comillas (rutas con espacios).
        for (k, _, d) in &writes {
            if k.ends_with(r"\command") {
                assert_eq!(d, open, "{k}");
            }
        }
        // No se toca el predeterminado de otras apps ni HKLM/HKCR; nada interactivo (vale /S).
        assert!(!writes.iter().any(|(k, v, _)| k.starts_with(r"Software\Classes\.") && v.is_empty()));
        let code: Vec<&str> = text.lines().filter(|l| !l.trim_start().starts_with(';')).collect();
        for bad in ["HKLM", "HKCR", "MessageBox", "HKEY_"] {
            assert!(!code.iter().any(|l| l.contains(bad)), "default-apps.nsh no debe usar {bad}");
        }

        // El desinstalador borra cada valor escrito (su clave, una clave madre o el valor).
        let mut del_keys = Vec::new();
        let mut del_values = Vec::new();
        for w in &uninstall {
            let rest: Vec<String> = w.iter().skip(1).filter(|a| !a.starts_with('/')).cloned().collect();
            let at = |i: usize| rest.get(i).cloned().unwrap_or_default();
            match w.first().map(String::as_str) {
                Some("DeleteRegKey") => {
                    assert_ne!(at(1), r"Software\Classes", "{w:?}");
                    // .eml/.ics/.vcf son de todos: solo si quedaron vacías.
                    if at(1).starts_with(r"Software\Classes\.") {
                        assert!(w.iter().any(|a| a == "/ifempty"), "{w:?}");
                    } else {
                        del_keys.push(at(1));
                    }
                }
                Some("DeleteRegValue") => del_values.push((at(1), at(2))),
                _ => {}
            }
        }
        for (k, v, _) in &writes {
            let by_key = del_keys.iter().any(|d| k == d || k.starts_with(&format!(r"{d}\")));
            let by_value = del_values.iter().any(|(dk, dv)| dk == k && dv == v);
            assert!(by_key || by_value, "el desinstalador no borra {k} [{v}]");
        }
    }

    /// R1.3: claves de Info.plist que mezcla cargo-packager (esquemas y tipos de documento).
    #[test]
    fn macos_info_plist() {
        let value = plist::Value::from_file(root().join("packaging/macos/Info.plist"));
        let dict = value.ok().and_then(plist::Value::into_dictionary).unwrap_or_default();
        let text = |k: &str| dict.get(k).and_then(plist::Value::as_string).unwrap_or("").to_string();
        assert_eq!(text("CFBundleIdentifier"), IDENTIFIER);
        assert_eq!(text("LSApplicationCategoryType"), "public.app-category.productivity");
        let strings = |v: Option<&plist::Value>| -> Vec<String> {
            v.and_then(plist::Value::as_array)
                .map(|a| a.iter().filter_map(plist::Value::as_string).map(str::to_string).collect())
                .unwrap_or_default()
        };
        let items = |k: &str| -> Vec<plist::Dictionary> {
            dict.get(k)
                .and_then(plist::Value::as_array)
                .map(|a| a.iter().filter_map(|v| v.as_dictionary().cloned()).collect())
                .unwrap_or_default()
        };
        let schemes: Vec<String> =
            items("CFBundleURLTypes").iter().flat_map(|d| strings(d.get("CFBundleURLSchemes"))).collect();
        assert_eq!(schemes, ["iuredesk", "mailto"]);
        let docs = items("CFBundleDocumentTypes");
        for (ext, uti) in [("eml", "com.apple.mail.email"), ("ics", "com.apple.ical.ics"), ("vcf", "public.vcard")] {
            let d =
                docs.iter().find(|d| strings(d.get("CFBundleTypeExtensions")) == [ext]).cloned().unwrap_or_default();
            assert_eq!(strings(d.get("LSItemContentTypes")), [uti], "{ext}");
            assert_eq!(d.get("CFBundleTypeRole").and_then(plist::Value::as_string), Some("Viewer"), "{ext}");
            // No se apropia del tipo (Mail, Calendario y Contactos siguen siendo los dueños).
            assert_eq!(d.get("LSHandlerRank").and_then(plist::Value::as_string), Some("Alternate"), "{ext}");
        }
    }

    /// Cada archivo que la configuración referencia existe en el repositorio.
    #[test]
    fn packaging_files_exist() {
        for f in [
            "packaging/README.txt",
            "packaging/linux/iuredesk.desktop",
            "packaging/linux/com.iurefficient.IureDesk.metainfo.xml",
            "packaging/windows/default-apps.nsh",
            "packaging/macos/Info.plist",
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
            "x-scheme-handler/iuredesk;",
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
