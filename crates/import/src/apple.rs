//! Apple Mail: `~/Library/Mail/V<n>/<cuenta>/<Buzón>.mbox/…/Messages/<id>.emlx`.
//!
//! - Solo se lee la carpeta `V<n>` más reciente (las anteriores son copias que Apple Mail dejó al
//!   migrar). `MailData` (índices y ajustes) se ignora.
//! - Cada buzón es un paquete `Nombre.mbox/`; los buzones hijos son paquetes dentro del padre. Los
//!   mensajes están en `Messages/` (directamente o bajo `<UUID>/Data/<n>/<n>/Messages/`).
//! - «Exportar buzón» de Apple Mail produce `Nombre.mbox/mbox` (un mbox normal): también vale.
//! - Contactos y Calendario: se importan los `.vcf` / `.ics` que exportan esas apps.

use std::path::{Path, PathBuf};

use crate::checkpoint::unit_key;
use crate::error::Result;
use crate::headers;
use crate::mail::{Target, emlx_in, import_emlx_files, import_mbox};
use crate::report::SkipReason;
use crate::sink::Sink;
use crate::util::{file_name, subdirs};

/// Profundidad máxima de paquetes anidados y de búsqueda de `Messages/`.
const MAX_DEPTH: usize = 12;

/// Número de versión de una carpeta `V10`.
fn version_of(dir: &Path) -> Option<u32> {
    file_name(dir).strip_prefix('V').and_then(|n| n.parse().ok())
}

/// ¿Es `~/Library/Mail` o una carpeta `V<n>`?
pub fn is_apple_mail(dir: &Path) -> bool {
    version_of(dir).is_some() && dir.join("MailData").is_dir()
        || subdirs(dir).iter().any(|d| version_of(d).is_some() && d.join("MailData").is_dir())
}

/// ¿Es un paquete de buzón (`Nombre.mbox/` con `.emlx` o con un `mbox` exportado)?
pub fn is_bundle(dir: &Path) -> bool {
    file_name(dir).ends_with(".mbox") && dir.is_dir()
}

/// Cuentas (directorios) de la versión más reciente.
fn account_dirs(root: &Path) -> Vec<PathBuf> {
    let version = if version_of(root).is_some() {
        Some(root.to_path_buf())
    } else {
        let mut v: Vec<(u32, PathBuf)> =
            subdirs(root).into_iter().filter_map(|d| version_of(&d).map(|n| (n, d))).collect();
        v.sort();
        v.pop().map(|(_, d)| d)
    };
    let Some(version) = version else { return Vec::new() };
    subdirs(&version).into_iter().filter(|d| file_name(d) != "MailData").collect()
}

/// Nombre para la cuenta: `IMAP-ana@imap.example.com` → `ana@imap.example.com`; un UUID → la
/// dirección del primer mensaje de la bandeja de entrada (`Delivered-To` o `To`).
fn account_label(dir: &Path) -> (String, String) {
    let name = file_name(dir);
    for p in ["IMAP-", "POP-", "EWS-", "Exchange-"] {
        if let Some(rest) = name.strip_prefix(p) {
            let email = rest.split('@').take(2).collect::<Vec<_>>().join("@");
            return (format!("Apple Mail · {rest}"), if email.contains('@') { email } else { String::new() });
        }
    }
    if let Some(addr) = guess_address(dir) {
        return (format!("Apple Mail · {addr}"), addr);
    }
    if name == "Mailboxes" {
        return ("Apple Mail".into(), String::new());
    }
    let short: String = name.chars().take(8).collect();
    (format!("Apple Mail · {short}"), String::new())
}

fn guess_address(dir: &Path) -> Option<String> {
    let inbox = dir.join("INBOX.mbox");
    let messages = find_messages_dirs(&inbox, 0).into_iter().next()?;
    let first = emlx_in(&messages).into_iter().next()?;
    let data = crate::util::head(&first, 64 * 1024);
    let (h, _) = headers::split_header_body(data.get(data.iter().position(|&b| b == b'\n')?.saturating_add(1)..)?);
    let fields = headers::header_fields(h);
    let v = headers::first(&fields, "delivered-to").or_else(|| headers::first(&fields, "to"))?;
    let addr = v.rsplit('<').next()?.trim_end_matches('>').trim().to_string();
    addr.contains('@').then_some(addr)
}

/// Directorios `Messages/` de un paquete, sin entrar en paquetes hijos.
fn find_messages_dirs(bundle: &Path, depth: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if depth > MAX_DEPTH {
        return out;
    }
    for d in subdirs(bundle) {
        let name = file_name(&d);
        if name.ends_with(".mbox") || name == "Attachments" {
            continue;
        }
        if name == "Messages" {
            out.push(d);
        } else {
            out.extend(find_messages_dirs(&d, depth.saturating_add(1)));
        }
    }
    out
}

/// Importa Apple Mail entero (`~/Library/Mail` o `V<n>`).
pub(crate) fn import_root(sink: &mut Sink<'_>, root: &Path) -> Result<()> {
    for acct in account_dirs(root) {
        import_account(sink, &acct)?;
    }
    Ok(())
}

/// Importa una cuenta (directorio con paquetes `.mbox`).
pub(crate) fn import_account(sink: &mut Sink<'_>, dir: &Path) -> Result<()> {
    let account = account_label(dir);
    sink.set_current(account.0.clone());
    for b in subdirs(dir) {
        if is_bundle(&b) {
            import_bundle(sink, &b, &account, Vec::new(), 0)?;
        }
    }
    Ok(())
}

/// Importa un paquete `Nombre.mbox/` y sus hijos.
pub(crate) fn import_bundle(
    sink: &mut Sink<'_>,
    bundle: &Path,
    account: &(String, String),
    prefix: Vec<String>,
    depth: usize,
) -> Result<()> {
    if depth > MAX_DEPTH {
        sink.skip(bundle.display().to_string(), SkipReason::LimitReached { what: "profundidad de buzones".into() });
        return Ok(());
    }
    sink.check_cancel()?;
    let stem = file_name(bundle).trim_end_matches(".mbox").to_string();
    let mut path = prefix;
    path.push(stem);
    let target = Target::new(account.clone(), path.clone());
    let exported = bundle.join("mbox");
    if exported.is_file() {
        import_mbox(sink, &exported, &target)?;
    }
    for m in find_messages_dirs(bundle, 0) {
        let key = unit_key("emlx", &m);
        if sink.checkpoint.is_done(&key) {
            sink.report.resumed = true;
            continue;
        }
        import_emlx_files(sink, &emlx_in(&m), &target)?;
        sink.checkpoint.stage_done(key);
        sink.flush()?;
    }
    for child in subdirs(bundle) {
        if is_bundle(&child) {
            import_bundle(sink, &child, account, path.clone(), depth.saturating_add(1))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_and_detection() {
        let dir = tempfile::tempdir().unwrap();
        let v = dir.path().join("V10");
        std::fs::create_dir_all(v.join("MailData")).unwrap();
        std::fs::create_dir_all(dir.path().join("V9/MailData")).unwrap();
        std::fs::create_dir_all(v.join("IMAP-ana@imap.example.com/INBOX.mbox")).unwrap();
        assert!(is_apple_mail(dir.path()));
        assert!(is_apple_mail(&v));
        assert!(!is_apple_mail(&v.join("MailData")));
        let accts = account_dirs(dir.path());
        assert_eq!(accts.len(), 1);
        assert_eq!(account_label(&accts[0]).0, "Apple Mail · ana@imap.example.com");
    }
}
