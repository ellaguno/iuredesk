//! Tareas de mantenimiento. `cargo xtask ci` es la puerta de calidad antes de cada commit.
//!
//! - `ci`: fmt, clippy -D warnings, pruebas, layers, purity, licenses, assets.
//! - `layers`: ninguna capa depende de una superior; nada bajo la UI depende de egui/eframe/winit/rfd.
//! - `purity`: política "solo Rust" (PLAN.md D1): ningún crate enlaza bibliotecas en C/C++ salvo
//!   las excepciones de `docs/excepciones.md`.
//! - `licenses`: solo licencias permisivas (el proyecto es FSL-1.1-ALv2 y pasa a Apache-2.0).
//! - `notices`: genera `target/THIRD-PARTY-NOTICES.txt` con la licencia de cada crate de terceros
//!   que va dentro de los binarios distribuidos; falla si alguno no tiene texto de licencia.
//! - `assets`: todo archivo de `assets/` tiene su fila en `ATTRIBUTION.md`.
//! - `dist [--tag vX.Y.Z]`: compila con el perfil `dist` (release sin símbolos) y deja en `dist/` el paquete
//!   `iuredesk-<versión>-x86_64-linux.tar.gz` (binarios, licencia, avisos de terceros) y
//!   `SHA256SUMS`. Con `--tag` falla si la etiqueta no coincide con la versión del workspace.
//! - `icons`: rasteriza `assets/icons/*.svg` a PNG 16–1024, `.ico` y `.icns` (ver `icons.rs`).
//! - `package <linux|windows|macos> [--tag vX.Y.Z] [--target <triple>]`: instaladores con
//!   cargo-packager (`.deb` + AppImage, NSIS, `.app` + `.dmg`) en `dist/` (ver `package.rs` y
//!   `docs/publicacion.md`).
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

mod icons;
mod package;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use serde_json::Value;

type Res = Result<(), String>;

fn main() -> ExitCode {
    let task = std::env::args().nth(1).unwrap_or_else(|| "ci".into());
    let r = match task.as_str() {
        "ci" => ci(),
        "layers" => metadata(None).and_then(|m| layers(&m)),
        "purity" => purity_all(),
        "licenses" => metadata(None).and_then(|m| licenses(&m)),
        "notices" => notices_file(&root().join("target/THIRD-PARTY-NOTICES.txt")),
        "assets" => assets(),
        "dist" => dist(std::env::args().skip(2).collect()),
        "icons" => icons::icons(),
        "package" => package::package(std::env::args().skip(2).collect()),
        other => Err(format!("tarea desconocida: {other}")),
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xtask {task}: {e}");
            ExitCode::FAILURE
        }
    }
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

fn cargo(args: &[&str]) -> Res {
    println!("$ cargo {}", args.join(" "));
    let ok = Command::new(env!("CARGO")).args(args).current_dir(root()).status().map_err(|e| e.to_string())?.success();
    if ok { Ok(()) } else { Err(format!("falló cargo {}", args.join(" "))) }
}

fn ci() -> Res {
    cargo(&["fmt", "--all", "--check"])?;
    cargo(&["clippy", "--workspace", "--all-targets", "--", "-D", "warnings"])?;
    cargo(&["test", "--workspace", "-q"])?;
    let m = metadata(None)?;
    layers(&m)?;
    purity_all()?;
    licenses(&m)?;
    notices_file(&root().join("target/THIRD-PARTY-NOTICES.txt"))?;
    assets()?;
    println!("xtask ci: todo en verde");
    Ok(())
}

fn metadata(platform: Option<&str>) -> Result<Value, String> {
    let mut args = vec!["metadata", "--format-version", "1", "--locked"];
    if let Some(p) = platform {
        args.extend(["--filter-platform", p]);
    }
    let out = Command::new(env!("CARGO")).args(&args).current_dir(root()).output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned());
    }
    serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())
}

fn packages(m: &Value) -> impl Iterator<Item = &Value> {
    m.get("packages").and_then(Value::as_array).into_iter().flatten()
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

/// Capa de cada crate del workspace (PLAN.md §7). Un crate nuevo debe registrarse aquí.
const LAYERS: &[(&str, u8)] = &[
    ("iuredesk-model", 1),
    ("iuredesk-store", 2),
    ("iuredesk-autoconfig", 2),
    ("iuredesk-net", 2),
    ("iuredesk-accounts", 3),
    ("iuredesk-imap", 3),
    ("iuredesk-pop3", 3),
    ("iuredesk-sync", 3),
    ("iuredesk-oauth", 3),
    ("iuredesk-dav", 3),
    ("iuredesk-pim-sync", 3),
    ("iuredesk-compose", 3),
    ("iuredesk-import", 3),
    ("iuredesk-engine", 4),
    ("iuredesk-integrations", 4),
    ("iuredesk-mcp", 4),
    ("iuredesk-assistant", 4),
    ("iuredesk-html-view", 5),
    ("iuredesk-ui", 6),
    ("iuredesk", 7),
    ("xtask", 7),
];
const UI_CRATES: &[&str] = &["egui", "eframe", "winit", "rfd", "egui-winit", "egui-wgpu"];

fn layers(m: &Value) -> Res {
    let layer: BTreeMap<&str, u8> = LAYERS.iter().copied().collect();
    let members: BTreeSet<&str> =
        m.get("workspace_members").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).collect();
    let mut errors = Vec::new();
    for p in packages(m).filter(|p| members.contains(s(p, "id"))) {
        let name = s(p, "name");
        let Some(&mine) = layer.get(name) else {
            errors.push(format!("{name} no tiene capa asignada en xtask/src/main.rs (LAYERS)"));
            continue;
        };
        for d in p.get("dependencies").and_then(Value::as_array).into_iter().flatten() {
            if s(d, "kind") == "dev" {
                continue;
            }
            let dep = s(d, "name");
            if let Some(&theirs) = layer.get(dep)
                && theirs > mine
            {
                errors.push(format!("{name} (L{mine}) depende de {dep} (L{theirs})"));
            }
            if mine < 6 && UI_CRATES.contains(&dep) {
                errors.push(format!("{name} (L{mine}) depende de la UI ({dep})"));
            }
        }
    }
    report("layers", errors)
}

/// Excepciones documentadas a la política "solo Rust": `nombre  # motivo` por línea.
fn allowlist() -> Result<BTreeSet<String>, String> {
    let text = std::fs::read_to_string(root().join("xtask/purity-allow.txt"))
        .map_err(|e| format!("xtask/purity-allow.txt: {e}"))?;
    Ok(text
        .lines()
        .filter_map(|l| l.split('#').next())
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect())
}

/// Crates de compilación que delatan código C/C++ (o enlace a bibliotecas del sistema).
const NATIVE_BUILD: &[&str] = &["cc", "cmake", "pkg-config", "bindgen", "vcpkg", "system-deps", "cxx-build"];

/// Plataformas en las que se exige la política. Linux primero; Windows y macOS en la Fase 7.
const PLATFORMS: &[&str] = &["x86_64-unknown-linux-gnu"];

fn purity_all() -> Res {
    for p in PLATFORMS {
        println!("purity [{p}]");
        purity(&metadata(Some(p))?, Some(p))?;
    }
    Ok(())
}

fn purity(m: &Value, platform: Option<&str>) -> Res {
    let allow = allowlist()?;
    let graph = resolved(platform)?;
    let mut errors = Vec::new();
    for p in packages(m).filter(|p| in_graph(&graph, p)) {
        let name = s(p, "name");
        if allow.contains(name) {
            continue;
        }
        if let Some(links) = p.get("links").and_then(Value::as_str) {
            errors.push(format!("{name} enlaza la biblioteca nativa «{links}»"));
        }
        let native: Vec<&str> = p
            .get("dependencies")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|d| s(d, "kind") == "build" && !d.get("optional").and_then(Value::as_bool).unwrap_or(false))
            .map(|d| s(d, "name"))
            .filter(|d| NATIVE_BUILD.contains(d))
            .collect();
        if !native.is_empty() {
            errors.push(format!("{name} compila código nativo ({})", native.join(", ")));
        }
    }
    report("purity", errors)
}

/// Paquetes («nombre vX.Y.Z») que de verdad entran en la compilación para `platform` (todas si es
/// `None`). Se pregunta a `cargo tree` y no al `resolve` de `cargo metadata`: este sigue el grafo
/// de Cargo.lock, que trae dependencias opcionales que ninguna feature activa (p. ej. la variante
/// de `keyring` con libdbus que `iurefficient-connect` declara y nosotros no pedimos).
fn resolved(platform: Option<&str>) -> Result<BTreeSet<String>, String> {
    tree(&["--workspace", "-e", "normal,build,dev"], platform)
}

/// Salida de `cargo tree` como conjunto de «nombre vX.Y.Z».
fn tree(selection: &[&str], platform: Option<&str>) -> Result<BTreeSet<String>, String> {
    let target = platform.unwrap_or("all");
    let out = Command::new(env!("CARGO"))
        .args(["tree", "--locked", "--prefix", "none", "-f", "{p}"])
        .args(selection)
        .args(["--target", target])
        .current_dir(root())
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned());
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            Some(format!("{} {}", w.next()?, w.next()?))
        })
        .collect())
}

fn in_graph(graph: &BTreeSet<String>, p: &Value) -> bool {
    graph.contains(&format!("{} v{}", s(p, "name"), s(p, "version")))
}

const LICENSES: &[&str] = &[
    "MIT",
    "Apache-2.0",
    "Apache-2.0 WITH LLVM-exception",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "ISC",
    "Zlib",
    "Unicode-3.0",
    "Unicode-DFS-2016",
    "BSL-1.0",
    "CC0-1.0",
    "0BSD",
    "MIT-0",
    "OFL-1.1",
    "Ubuntu-font-1.0",
    "LicenseRef-UFL-1.0",
    "MPL-2.0",
    "Unlicense",
    "CDLA-Permissive-2.0",
];

/// Expresión SPDX aceptable: algún término de cada `OR` permitido; todos los de un `AND`.
fn license_ok(expr: &str) -> bool {
    let e = expr.replace(['(', ')'], " ").replace('/', " OR ");
    e.split(" OR ").any(|alt| alt.split(" AND ").all(|t| LICENSES.contains(&t.trim())))
}

fn licenses(m: &Value) -> Res {
    let graph = resolved(None)?;
    let mut errors = Vec::new();
    for p in packages(m).filter(|p| in_graph(&graph, p)) {
        let lic = s(p, "license");
        if p.get("source").is_some_and(Value::is_null) {
            continue; // crates propios
        }
        // Crates de la suite Iurefficient (repos hermanos del mismo titular, misma licencia FSL).
        let suite = s(p, "source").contains("github.com/ellaguno/");
        if suite && lic == "FSL-1.1-ALv2" {
            continue;
        }
        if !license_ok(lic) {
            errors.push(format!("{} {}: licencia «{lic}» no aprobada", s(p, "name"), s(p, "version")));
        }
    }
    report("licenses", errors)
}

/// Binarios que se distribuyen y plataforma de los paquetes de `dist` (Linux primero).
const DIST_BINS: &[&str] = &["iuredesk"];
const DIST_TARGET: &str = "x86_64-unknown-linux-gnu";
const DIST_SUFFIX: &str = "x86_64-linux";

/// Límites para leer los archivos de licencia de los crates descargados.
const MAX_LICENSE_BYTES: u64 = 256 * 1024;
const MAX_LICENSE_FILES: usize = 16;

/// Crates cuyas licencias no están en la raíz: archivos adicionales y, para su código, la
/// alternativa `OR` elegida con texto estándar. `epaint_default_fonts` es «(MIT OR Apache-2.0) AND
/// OFL-1.1 AND Ubuntu-font-1.0»: el código se toma como Apache-2.0 y cada fuente trae su licencia.
const EXTRA_LICENSES: &[(&str, &[&str], &str)] = &[(
    "epaint_default_fonts",
    &["fonts/OFL.txt", "fonts/UFL.txt", "fonts/Hack-Regular.txt", "fonts/emoji-icon-font-mit-license.txt"],
    "Apache-2.0",
)];

fn notices_file(path: &Path) -> Res {
    notices_file_for(path, DIST_TARGET)
}

/// Avisos de terceros para los binarios compilados para `target` (cambia un poco por SO).
fn notices_file_for(path: &Path, target: &str) -> Res {
    let (text, count) = notices(target)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    println!("notices: {count} crates de terceros → {}", path.display());
    Ok(())
}

/// Avisos de terceros: nombre, versión, licencia y textos de licencia de cada crate que entra en
/// los binarios de `DIST_BINS`. MIT, Apache-2.0 y BSD exigen acompañar los binarios con ellos.
/// Un texto idéntico a uno anterior se cita en vez de repetirse.
fn notices(target: &str) -> Result<(String, usize), String> {
    let m = metadata(Some(target))?;
    let mut selection: Vec<&str> = vec!["-e", "normal"];
    for b in DIST_BINS {
        selection.extend(["-p", b]);
    }
    let graph = tree(&selection, Some(target))?;
    let mut third: Vec<&Value> =
        packages(&m).filter(|p| in_graph(&graph, p) && !p.get("source").is_some_and(Value::is_null)).collect();
    third.sort_by_key(|p| (s(p, "name").to_string(), s(p, "version").to_string()));

    let mut out = String::from(
        "IureDesk: avisos de terceros / third-party notices\n\n\
         IureDesk incluye los siguientes componentes de código abierto, cada uno bajo su propia\n\
         licencia. IureDesk includes the following open-source components, each under its own\n\
         license. Generado por `cargo xtask notices`.\n",
    );
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    let mut errors = Vec::new();
    for p in &third {
        let id = format!("{} {}", s(p, "name"), s(p, "version"));
        let lic = s(p, "license");
        out.push_str(&format!("\n{}\n{id}\nLicense: {lic}\n", "=".repeat(78)));
        let repo = s(p, "repository");
        if !repo.is_empty() {
            out.push_str(&format!("Source: {repo}\n"));
        }
        let mut texts = license_files(p);
        if let Some((_, files, code)) = EXTRA_LICENSES.iter().find(|(n, ..)| *n == s(p, "name")) {
            let found = extra_files(p, files);
            if found.len() < files.len() {
                errors.push(format!("{id}: faltan archivos de licencia de EXTRA_LICENSES"));
            }
            if texts.is_empty() {
                texts.extend(fallback_texts(code).unwrap_or_default());
            }
            texts.extend(found);
        }
        if texts.is_empty() {
            match fallback_texts(lic) {
                Some(t) => texts = t,
                None => {
                    errors.push(format!("{id}: sin archivo de licencia ni texto estándar para «{lic}»"));
                    continue;
                }
            }
        }
        for (name, text) in texts {
            let body = text.trim().to_string();
            out.push_str(&format!("\n--- {name} ---\n"));
            match seen.get(&body) {
                Some(first) => out.push_str(&format!("(Mismo texto que {first} / same text as {first})\n")),
                None => {
                    out.push_str(&body);
                    out.push('\n');
                    seen.insert(body, format!("{id} ({name})"));
                }
            }
        }
    }
    report("notices", errors)?;
    Ok((out, third.len()))
}

/// Archivos LICENSE*, LICENCE*, COPYING*, NOTICE*, COPYRIGHT* y UNLICENSE* del crate, más los de
/// un directorio LICENSES/ (REUSE). Tamaño y número acotados.
fn license_files(p: &Value) -> Vec<(String, String)> {
    let Some(dir) = Path::new(s(p, "manifest_path")).parent() else { return Vec::new() };
    let wanted = |n: &str| {
        let u = n.to_uppercase();
        ["LICENSE", "LICENCE", "COPYING", "NOTICE", "COPYRIGHT", "UNLICENSE"].iter().any(|k| u.starts_with(k))
    };
    let mut paths: Vec<(String, PathBuf)> = Vec::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let path = e.path();
        if path.is_dir() && name.eq_ignore_ascii_case("LICENSES") {
            for f in std::fs::read_dir(&path).into_iter().flatten().flatten() {
                if f.path().is_file() {
                    paths.push((format!("{name}/{}", f.file_name().to_string_lossy()), f.path()));
                }
            }
        } else if path.is_file() && wanted(&name) {
            paths.push((name, path));
        }
    }
    paths.sort();
    paths
        .into_iter()
        .take(MAX_LICENSE_FILES)
        .filter_map(|(name, path)| {
            let len = std::fs::metadata(&path).ok()?.len();
            if len == 0 || len > MAX_LICENSE_BYTES {
                return None;
            }
            Some((name, std::fs::read_to_string(&path).ok()?))
        })
        .collect()
}

/// Archivos de licencia en rutas fijas dentro del crate (ver `EXTRA_LICENSES`).
fn extra_files(p: &Value, files: &[&str]) -> Vec<(String, String)> {
    let Some(dir) = Path::new(s(p, "manifest_path")).parent() else { return Vec::new() };
    files
        .iter()
        .filter_map(|f| {
            let path = dir.join(f);
            let len = std::fs::metadata(&path).ok()?.len();
            if len == 0 || len > MAX_LICENSE_BYTES {
                return None;
            }
            Some(((*f).to_string(), std::fs::read_to_string(&path).ok()?))
        })
        .collect()
}

/// Texto estándar (`xtask/license-texts/<SPDX>.txt`) para crates que no traen su archivo: la
/// primera alternativa `OR` cuyos términos tienen todos texto.
fn fallback_texts(expr: &str) -> Option<Vec<(String, String)>> {
    let e = expr.replace(['(', ')'], " ").replace('/', " OR ");
    e.split(" OR ").find_map(|alt| {
        alt.split(" AND ")
            .map(str::trim)
            .map(|id| {
                let path = root().join("xtask/license-texts").join(format!("{id}.txt"));
                std::fs::read_to_string(path).ok().map(|t| (format!("{id} (texto estándar / standard text)"), t))
            })
            .collect::<Option<Vec<_>>>()
    })
}

fn dist(args: Vec<String>) -> Res {
    let m = metadata(None)?;
    let version = packages(&m).find(|p| s(p, "name") == "iuredesk").map(|p| s(p, "version")).unwrap_or("");
    if version.is_empty() {
        return Err("no encuentro la versión del paquete iuredesk".into());
    }
    match args.as_slice() {
        [] => {}
        [flag, tag] if flag == "--tag" => {
            if tag.strip_prefix('v') != Some(version) {
                return Err(format!("la etiqueta {tag} no coincide con la versión del workspace ({version})"));
            }
        }
        _ => return Err("uso: cargo xtask dist [--tag vX.Y.Z]".into()),
    }
    let mut build = vec!["build", "--profile", "dist", "--locked", "--target", DIST_TARGET];
    for b in DIST_BINS {
        build.extend(["-p", b]);
    }
    cargo(&build)?;

    let dist = root().join("dist");
    if dist.exists() {
        std::fs::remove_dir_all(&dist).map_err(|e| format!("{}: {e}", dist.display()))?;
    }
    let name = format!("iuredesk-{version}-{DIST_SUFFIX}");
    let stage = dist.join(&name);
    std::fs::create_dir_all(&stage).map_err(|e| e.to_string())?;
    let bin_dir = root().join("target").join(DIST_TARGET).join("dist");
    for b in DIST_BINS {
        copy(&bin_dir.join(b), &stage.join(b))?;
    }
    for f in ["LICENSE.md", "NOTICE"] {
        copy(&root().join(f), &stage.join(f))?;
    }
    copy(&root().join("packaging/README.txt"), &stage.join("README.txt"))?;
    notices_file(&stage.join("THIRD-PARTY-NOTICES.txt"))?;

    let archive = format!("{name}.tar.gz");
    let file = std::fs::File::create(dist.join(&archive)).map_err(|e| e.to_string())?;
    let gz = flate2::write::GzEncoder::new(file, flate2::Compression::best());
    let mut tar = tar::Builder::new(gz);
    tar.append_dir_all(&name, &stage).map_err(|e| format!("tar: {e}"))?;
    tar.into_inner().and_then(flate2::write::GzEncoder::finish).map_err(|e| format!("tar.gz: {e}"))?;

    let bytes = std::fs::read(dist.join(&archive)).map_err(|e| e.to_string())?;
    let sums = format!("{}  {archive}\n", sha256_hex(&bytes));
    std::fs::write(dist.join("SHA256SUMS"), &sums).map_err(|e| e.to_string())?;
    println!("dist: {} ({} KB)\n{sums}", dist.join(&archive).display(), bytes.len() / 1024);
    Ok(())
}

fn copy(from: &Path, to: &Path) -> Res {
    std::fs::copy(from, to).map(|_| ()).map_err(|e| format!("{} → {}: {e}", from.display(), to.display()))
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn assets() -> Res {
    let attribution =
        std::fs::read_to_string(root().join("ATTRIBUTION.md")).map_err(|e| format!("ATTRIBUTION.md: {e}"))?;
    let mut errors = Vec::new();
    let mut stack = vec![root().join("assets")];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let path = e.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().is_some_and(|n| n != ".gitkeep") {
                let rel =
                    path.strip_prefix(root()).map_or_else(|_| path.display().to_string(), |r| r.display().to_string());
                if !attribution.contains(&rel) {
                    errors.push(format!("{rel} no tiene fila en ATTRIBUTION.md"));
                }
            }
        }
    }
    report("assets", errors)
}

fn report(task: &str, errors: Vec<String>) -> Res {
    if errors.is_empty() {
        println!("{task}: ok");
        Ok(())
    } else {
        for e in &errors {
            eprintln!("  {e}");
        }
        Err(format!("{} problema(s)", errors.len()))
    }
}

#[cfg(test)]
mod tests {
    use super::license_ok;

    #[test]
    fn spdx() {
        assert!(license_ok("MIT OR Apache-2.0"));
        assert!(license_ok("MIT/Apache-2.0"));
        assert!(license_ok("(MIT OR Apache-2.0) AND Unicode-3.0"));
        assert!(!license_ok("GPL-3.0"));
        assert!(!license_ok("LGPL-2.1-or-later"));
        assert!(!license_ok("MIT AND GPL-2.0"));
        assert!(!license_ok(""));
    }
}
