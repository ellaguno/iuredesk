//! `cargo xtask sweep [--dry-run] [--days N]`: limpia lo viejo de `target/` sin perder la
//! compilación actual.
//!
//! Cargo deja una copia nueva de cada binario de prueba o ejemplo cada vez que cambian sus
//! dependencias y nunca borra las anteriores (en noviembre de 2026, 12–22 copias de ~700 MB por
//! prueba: más de 270 GB). Aquí:
//! - en `target/<perfil>/deps` y `target/<perfil>/examples`, de cada binario (archivo sin
//!   extensión con `-<hash>`) se queda solo la copia más reciente, agrupando por nombre **y** por
//!   archivo fuente (leído de su `.d`: las `props` de un crate no borran las de otro); se borran
//!   las demás con sus `.d`/`.dwp`/`.pdb`;
//! - en `target/<perfil>/incremental`, los directorios que no se usan desde hace `N` días
//!   (7 por omisión).
//!
//! No toca bibliotecas (`.rlib`, `.rmeta`, `.so`), no sigue enlaces simbólicos ni sale de
//! `target/`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::Res;

/// Días sin uso tras los que se borra un directorio de compilación incremental.
const DEFAULT_DAYS: u64 = 7;

/// Lo que se haría o se hizo.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// Archivos y directorios a borrar.
    pub remove: Vec<PathBuf>,
    /// Bytes que se liberan.
    pub bytes: u64,
}

pub fn sweep(args: Vec<String>) -> Res {
    let mut dry = false;
    let mut days = DEFAULT_DAYS;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--dry-run" => dry = true,
            "--days" => {
                days = it.next().and_then(|d| d.parse().ok()).ok_or("uso: cargo xtask sweep [--dry-run] [--days N]")?;
            }
            _ => return Err("uso: cargo xtask sweep [--dry-run] [--days N]".into()),
        }
    }
    run(&crate::root().join("target"), days, dry)
}

/// Barre `target` (lo usa también `ci` al terminar).
pub fn run(target: &Path, days: u64, dry: bool) -> Res {
    let plan = plan(target, SystemTime::now(), Duration::from_secs(days.saturating_mul(86_400)))?;
    if !dry {
        for p in &plan.remove {
            let r = if p.is_dir() { std::fs::remove_dir_all(p) } else { std::fs::remove_file(p) };
            if let Err(e) = r {
                // Un archivo en uso (otra compilación) no detiene el barrido.
                eprintln!("sweep: no se pudo borrar {}: {e}", p.display());
            }
        }
    }
    println!(
        "sweep: {} {} elementos, {:.1} GB",
        if dry { "se borrarían" } else { "borrados" },
        plan.remove.len(),
        plan.bytes as f64 / 1e9
    );
    Ok(())
}

/// Calcula qué borrar en `target` (sin borrar nada).
pub fn plan(target: &Path, now: SystemTime, max_idle: Duration) -> Result<Plan, String> {
    let mut out = Plan::default();
    let Ok(profiles) = std::fs::read_dir(target) else { return Ok(out) };
    for p in profiles.flatten() {
        let dir = p.path();
        if !is_real_dir(&dir) {
            continue;
        }
        for sub in ["deps", "examples"] {
            stale_binaries(&dir.join(sub), &mut out);
        }
        idle_dirs(&dir.join("incremental"), now, max_idle, &mut out);
    }
    Ok(out)
}

fn is_real_dir(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir())
}

/// `nombre-0123456789abcdef` → (`nombre`, hash). Solo si el hash son 16 dígitos hexadecimales.
fn split_hash(stem: &str) -> Option<(&str, &str)> {
    let (name, hash) = stem.rsplit_once('-')?;
    (hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit()) && !name.is_empty()).then_some((name, hash))
}

/// Primer archivo fuente de un `.d` de cargo (`destino: fuente1 fuente2 …`).
fn first_source(d: &Path) -> String {
    let Ok(text) = std::fs::read_to_string(d) else { return String::new() };
    text.lines()
        .next()
        .and_then(|l| l.split_once(": "))
        .and_then(|(_, srcs)| srcs.split_whitespace().next())
        .unwrap_or_default()
        .to_string()
}

fn size_of(p: &Path) -> u64 {
    match std::fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => std::fs::read_dir(p).map(|r| r.flatten().map(|e| size_of(&e.path())).sum()).unwrap_or(0),
        Ok(m) => m.len(),
        Err(_) => 0,
    }
}

fn modified(p: &Path) -> SystemTime {
    std::fs::symlink_metadata(p).and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH)
}

/// Binarios viejos de `dir`: de cada (nombre, fuente) se queda el más reciente.
fn stale_binaries(dir: &Path, out: &mut Plan) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    // (nombre, fuente) → [(fecha, ruta sin extensión)]
    let mut groups: BTreeMap<(String, String), Vec<(SystemTime, PathBuf)>> = BTreeMap::new();
    for e in entries.flatten() {
        let path = e.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
        // Un binario: archivo normal, sin extensión, con `-<hash>`.
        if !meta.is_file() || path.extension().is_some() {
            continue;
        }
        let Some(stem) = path.file_name().and_then(|s| s.to_str()) else { continue };
        let Some((name, _)) = split_hash(stem) else { continue };
        let source = first_source(&path.with_extension("d"));
        groups.entry((name.to_string(), source)).or_default().push((modified(&path), path));
    }
    for (_, mut copies) in groups {
        copies.sort_by_key(|c| std::cmp::Reverse(c.0));
        for (_, bin) in copies.into_iter().skip(1) {
            for ext in ["", "d", "dwp", "pdb"] {
                let p = if ext.is_empty() { bin.clone() } else { bin.with_extension(ext) };
                if std::fs::symlink_metadata(&p).is_ok_and(|m| m.is_file()) {
                    out.bytes = out.bytes.saturating_add(size_of(&p));
                    out.remove.push(p);
                }
            }
        }
    }
}

/// Directorios de compilación incremental sin usar desde hace más de `max_idle`.
fn idle_dirs(dir: &Path, now: SystemTime, max_idle: Duration, out: &mut Plan) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let path = e.path();
        if !is_real_dir(&path) {
            continue;
        }
        // El uso más reciente: el del directorio o el de su sesión más nueva.
        let newest = std::fs::read_dir(&path)
            .map(|r| r.flatten().map(|s| modified(&s.path())).max().unwrap_or(SystemTime::UNIX_EPOCH))
            .unwrap_or(SystemTime::UNIX_EPOCH)
            .max(modified(&path));
        if now.duration_since(newest).is_ok_and(|idle| idle > max_idle) {
            out.bytes = out.bytes.saturating_add(size_of(&path));
            out.remove.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(p: &Path, bytes: usize, age_secs: u64) {
        std::fs::write(p, vec![0u8; bytes]).unwrap();
        let t = SystemTime::now() - Duration::from_secs(age_secs);
        std::fs::File::options().write(true).open(p).unwrap().set_modified(t).unwrap();
    }

    /// Abre una carpeta para cambiarle la fecha. En Windows `File::open` no abre carpetas: hace
    /// falta `FILE_FLAG_BACKUP_SEMANTICS` y el permiso de escribir atributos (falló la CI de Windows).
    fn open_dir(p: &Path) -> std::fs::File {
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
            const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
            std::fs::OpenOptions::new()
                .access_mode(FILE_WRITE_ATTRIBUTES)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
                .open(p)
                .unwrap()
        }
        #[cfg(not(windows))]
        {
            std::fs::File::open(p).unwrap()
        }
    }

    #[test]
    fn keeps_the_newest_copy_per_name_and_source() {
        let root = std::env::temp_dir().join(format!("xtask-sweep-{}", std::process::id()));
        let deps = root.join("debug/deps");
        std::fs::create_dir_all(&deps).unwrap();
        // Dos copias de la prueba `today` (misma fuente): se borra la vieja con su `.d`.
        for (hash, age) in [("0000000000000001", 100), ("0000000000000002", 10)] {
            touch(&deps.join(format!("today-{hash}")), 10, age);
            std::fs::write(deps.join(format!("today-{hash}.d")), "x: /w/crates/ui/tests/today.rs\n").unwrap();
        }
        // `props` de dos crates distintos: se quedan las dos.
        for (hash, src) in
            [("00000000000000a1", "/w/crates/ews/tests/props.rs"), ("00000000000000b2", "/w/crates/dav/tests/props.rs")]
        {
            touch(&deps.join(format!("props-{hash}")), 10, 50);
            std::fs::write(deps.join(format!("props-{hash}.d")), format!("x: {src}\n")).unwrap();
        }
        // Bibliotecas y archivos sin hash: nunca.
        touch(&deps.join("libfoo-0000000000000003.rlib"), 10, 1_000);
        touch(&deps.join("libfoo-0000000000000004.rlib"), 10, 1);
        touch(&deps.join("build-script"), 10, 1_000);
        // Incremental: uno viejo y uno reciente.
        let inc = root.join("debug/incremental");
        for (name, age) in [("old-abc", 30 * 86_400), ("new-def", 60)] {
            std::fs::create_dir_all(inc.join(name)).unwrap();
            touch(&inc.join(name).join("s-1"), 5, age);
            open_dir(&inc.join(name)).set_modified(SystemTime::now() - Duration::from_secs(age)).unwrap();
        }

        let plan = plan(&root, SystemTime::now(), Duration::from_secs(7 * 86_400)).unwrap();
        // Con «/» en todos los sistemas (Windows separa con «\»).
        let mut names: Vec<String> = plan
            .remove
            .iter()
            .map(|p| {
                let rel = p.strip_prefix(&root).unwrap();
                rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/")
            })
            .collect();
        names.sort();
        assert_eq!(
            names,
            ["debug/deps/today-0000000000000001", "debug/deps/today-0000000000000001.d", "debug/incremental/old-abc"]
        );
        assert_eq!(plan.bytes, 10 + 31 + 5);

        run(&root, 7, false).unwrap();
        assert!(!deps.join("today-0000000000000001").exists());
        assert!(deps.join("today-0000000000000002").exists());
        assert!(deps.join("props-00000000000000a1").exists() && deps.join("props-00000000000000b2").exists());
        assert!(deps.join("libfoo-0000000000000003.rlib").exists());
        assert!(!inc.join("old-abc").exists() && inc.join("new-def").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn hash_suffix_must_be_16_hex() {
        assert_eq!(split_hash("today-0123456789abcdef"), Some(("today", "0123456789abcdef")));
        assert_eq!(split_hash("iuredesk_ews-0123456789abcdef"), Some(("iuredesk_ews", "0123456789abcdef")));
        assert_eq!(split_hash("today-xyz"), None);
        assert_eq!(split_hash("-0123456789abcdef"), None);
    }
}
