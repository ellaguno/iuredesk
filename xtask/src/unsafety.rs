//! `unsafe` acotado (R1.4, decisión del dueño 2026-10-29; ver `docs/excepciones.md`).
//!
//! Todo crate del workspace hereda `[workspace.lints]` (`unsafe_code = "forbid"`) y no escribe la
//! palabra clave `unsafe`, salvo [`ALLOWED_CRATE`], que solo puede usarla en [`ALLOWED_FILE`]
//! (los Apple Events de macOS). Lo corre `cargo xtask layers` (y por tanto `ci`).

use std::path::Path;

use serde_json::Value;

use crate::{Res, packages, report, s};

/// El único crate con `unsafe`.
pub(crate) const ALLOWED_CRATE: &str = "iuredesk-macos-events";
/// El único archivo de ese crate donde puede aparecer (relativo a su carpeta).
pub(crate) const ALLOWED_FILE: &str = "src/macos.rs";

pub(crate) fn check(m: &Value) -> Res {
    let members: std::collections::BTreeSet<&str> =
        m.get("workspace_members").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).collect();
    let mut errors = Vec::new();
    for p in packages(m).filter(|p| members.contains(s(p, "id"))) {
        let name = s(p, "name");
        let manifest = Path::new(s(p, "manifest_path"));
        let Some(dir) = manifest.parent() else { continue };
        let allowed = name == ALLOWED_CRATE;
        if !allowed {
            match std::fs::read_to_string(manifest) {
                Ok(text) if inherits_workspace_lints(&text) => {}
                Ok(_) => errors.push(format!(
                    "{name}: su Cargo.toml no hereda [workspace.lints] (unsafe_code = \"forbid\"); \
                     solo {ALLOWED_CRATE} puede (docs/excepciones.md)"
                )),
                Err(e) => errors.push(format!("{}: {e}", manifest.display())),
            }
        }
        let mut files = Vec::new();
        rust_files(dir, &mut files, 0);
        for f in files {
            let rel = f.strip_prefix(dir).unwrap_or(&f);
            if allowed && rel == Path::new(ALLOWED_FILE) {
                continue;
            }
            match std::fs::read_to_string(&f) {
                Ok(src) => {
                    if let Some(line) = unsafe_line(&src) {
                        errors.push(format!(
                            "{}:{line}: `unsafe` fuera de {ALLOWED_CRATE}/{ALLOWED_FILE} (docs/excepciones.md)",
                            f.display()
                        ));
                    }
                }
                Err(e) => errors.push(format!("{}: {e}", f.display())),
            }
        }
    }
    report("unsafe", errors)
}

/// ¿La sección `[lints]` del manifiesto dice `workspace = true`?
pub(crate) fn inherits_workspace_lints(manifest: &str) -> bool {
    let mut in_lints = false;
    for line in manifest.lines() {
        let t = line.split('#').next().unwrap_or("").trim();
        if t.starts_with('[') {
            in_lints = t == "[lints]";
            continue;
        }
        if in_lints && t.replace(' ', "") == "workspace=true" {
            return true;
        }
    }
    false
}

/// `.rs` bajo `dir` (sin `target/` ni carpetas ocultas), con profundidad acotada.
fn rust_files(dir: &Path, out: &mut Vec<std::path::PathBuf>, depth: u8) {
    if depth > 12 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name();
        let name = name.to_string_lossy();
        if p.is_dir() {
            if name != "target" && !name.starts_with('.') {
                rust_files(&p, out, depth.saturating_add(1));
            }
        } else if name.ends_with(".rs") {
            out.push(p);
        }
    }
}

/// Primera línea (desde 1) con la palabra clave `unsafe` en código: se saltan comentarios,
/// cadenas (también crudas) y literales de carácter; `unsafe_code` o `es_unsafe` no cuentan.
pub(crate) fn unsafe_line(src: &str) -> Option<usize> {
    let b = src.as_bytes();
    let mut i = 0usize;
    let mut line = 1usize;
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    while let Some(&c) = b.get(i) {
        match c {
            b'\n' => {
                line += 1;
                i += 1;
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while b.get(i).is_some_and(|&c| c != b'\n') {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                let mut depth = 0usize;
                while let Some(&c) = b.get(i) {
                    if c == b'/' && b.get(i + 1) == Some(&b'*') {
                        depth += 1;
                        i += 2;
                    } else if c == b'*' && b.get(i + 1) == Some(&b'/') {
                        depth = depth.saturating_sub(1);
                        i += 2;
                        if depth == 0 {
                            break;
                        }
                    } else {
                        line += usize::from(c == b'\n');
                        i += 1;
                    }
                }
            }
            b'r' if (i == 0 || !b.get(i - 1).is_some_and(|&p| ident(p)))
                && matches!(b.get(i + 1), Some(b'"' | b'#')) =>
            {
                // Cadena cruda r"…" / r#"…"#.
                let mut j = i + 1;
                let mut hashes = 0usize;
                while b.get(j) == Some(&b'#') {
                    hashes += 1;
                    j += 1;
                }
                if b.get(j) != Some(&b'"') {
                    i += 1; // `r#ident`
                    continue;
                }
                j += 1;
                loop {
                    match b.get(j) {
                        None => return None,
                        Some(b'"') if (1..=hashes).all(|k| b.get(j + k) == Some(&b'#')) => {
                            j += 1 + hashes;
                            break;
                        }
                        Some(&c) => {
                            line += usize::from(c == b'\n');
                            j += 1;
                        }
                    }
                }
                i = j;
            }
            b'"' => {
                i += 1;
                while let Some(&c) = b.get(i) {
                    match c {
                        b'\\' => {
                            line += usize::from(b.get(i + 1) == Some(&b'\n'));
                            i += 2;
                        }
                        b'"' => {
                            i += 1;
                            break;
                        }
                        _ => {
                            line += usize::from(c == b'\n');
                            i += 1;
                        }
                    }
                }
            }
            b'\'' => {
                // Literal de carácter ('a', '\n', '\u{..}', '"'); si no, es una etiqueta de vida.
                if b.get(i + 1) == Some(&b'\\') {
                    let mut j = i + 3; // tras la barra y el carácter escapado
                    while b.get(j).is_some_and(|&c| c != b'\'') && j < i + 12 {
                        j += 1;
                    }
                    i = j + 1;
                } else {
                    let ch_len = src.get(i + 1..).and_then(|r| r.chars().next()).map_or(1, char::len_utf8);
                    if b.get(i + 1 + ch_len) == Some(&b'\'') { i += 2 + ch_len } else { i += 1 }
                }
            }
            c if ident(c) => {
                let start = i;
                while b.get(i).is_some_and(|&c| ident(c)) {
                    i += 1;
                }
                if src.get(start..i) == Some("unsafe") {
                    return Some(line);
                }
            }
            _ => i += 1,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encuentra_la_palabra_clave() {
        assert_eq!(unsafe_line("fn a() {}\nfn b() { unsafe { x() } }"), Some(2));
        assert_eq!(unsafe_line("#[unsafe(no_mangle)]\nfn f() {}"), Some(1));
        assert_eq!(unsafe_line("unsafe impl Send for X {}"), Some(1));
    }

    #[test]
    fn ignora_comentarios_cadenas_e_identificadores() {
        let src = r##"
//! El único `unsafe` está en otro sitio.
/* unsafe /* anidado unsafe */ unsafe */
#![forbid(unsafe_code)]
fn unsafe_images_become_alt() {}
const A: &str = "unsafe \" unsafe";
const B: &str = r#"unsafe "# ;
const C: char = '"';
const D: char = '\'';
fn f<'a>(x: &'a str) -> &'a str { x }
let r#unsafe_x = 1;
"##;
        assert_eq!(unsafe_line(src), None);
        assert_eq!(unsafe_line(&format!("{src}unsafe fn g() {{}}")), Some(src.lines().count() + 1));
    }

    #[test]
    fn lints_del_workspace() {
        assert!(inherits_workspace_lints("[package]\nname = \"a\"\n\n[lints]\nworkspace = true\n"));
        assert!(inherits_workspace_lints("[lints]\nworkspace=true # sí\n[dev-dependencies]\n"));
        assert!(!inherits_workspace_lints("[package]\nname = \"a\"\n"));
        assert!(!inherits_workspace_lints("[lints.rust]\nunsafe_code = \"deny\"\n"));
        assert!(!inherits_workspace_lints("[lints]\n[dependencies]\nworkspace = true\n"));
    }

    #[test]
    fn el_workspace_cumple() {
        let m = crate::metadata(None);
        assert!(m.is_ok());
        if let Ok(m) = m {
            assert_eq!(check(&m), Ok(()));
        }
    }
}
