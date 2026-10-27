//! Detección del tipo de origen y recorrido de directorios cualesquiera (p. ej. un Google
//! Takeout descomprimido o una carpeta con `.eml`, `.mbox`, `.ics` y `.vcf`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use iuredesk_model::FolderRole;
use serde::Serialize;

use crate::error::{ImportError, Result};
use crate::mail::{Target, import_eml_files, import_emlx_files, import_maildir, import_mbox, is_maildir};
use crate::report::{Notice, SkipReason, SourceKind};
use crate::sink::Sink;
use crate::util::{ext, file_name, files_in, head, subdirs};
use crate::{apple, pim, pst, thunderbird};

/// Profundidad máxima del recorrido de un directorio cualquiera.
const MAX_WALK_DEPTH: usize = 24;

/// Origen encontrado (para el asistente de migración).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DetectedSource {
    /// Tipo.
    pub kind: SourceKind,
    /// Ruta.
    pub path: PathBuf,
    /// Nombre para mostrar («Thunderbird — default-release», «Apple Mail», «archivo.pst»).
    pub label: String,
    /// Tamaño aproximado en bytes (0 si no se calculó).
    pub bytes: u64,
}

/// ¿Es un archivo mbox? (`.mbox` o sin extensión y empieza por `From `).
fn is_mbox_file(p: &Path) -> bool {
    let e = ext(p);
    if e == "mbox" || e == "mbx" {
        return true;
    }
    (e.is_empty() || e == "txt") && head(p, 5) == b"From "
}

/// Tipo de un archivo o directorio, si se reconoce.
pub fn detect(path: &Path) -> Option<SourceKind> {
    if path.is_dir() {
        if thunderbird::is_root(path) {
            return Some(SourceKind::Thunderbird);
        }
        if thunderbird::is_profile(path) {
            return Some(SourceKind::ThunderbirdProfile);
        }
        if apple::is_apple_mail(path) {
            return Some(SourceKind::AppleMail);
        }
        if apple::is_bundle(path) {
            return Some(SourceKind::AppleMailbox);
        }
        if is_maildir(path) {
            return Some(SourceKind::Maildir);
        }
        return Some(SourceKind::Directory);
    }
    if !path.is_file() {
        return None;
    }
    let name = file_name(path).to_ascii_lowercase();
    match ext(path).as_str() {
        "pst" | "ost" => Some(SourceKind::OutlookPst),
        "eml" => Some(SourceKind::Eml),
        "emlx" => Some(SourceKind::Emlx),
        "ics" | "ical" | "icalendar" | "vcs" => Some(SourceKind::Ics),
        "vcf" | "vcard" => Some(SourceKind::Vcf),
        "sqlite" if name.starts_with("abook") || name == "history.sqlite" => Some(SourceKind::ThunderbirdAddressBook),
        _ if is_mbox_file(path) => Some(SourceKind::Mbox),
        _ => None,
    }
}

/// Ubicaciones habituales de Thunderbird, Apple Mail y Outlook en esta máquina.
pub fn find_installed() -> Vec<DetectedSource> {
    let mut out = Vec::new();
    let home = dirs::home_dir();
    let mut tb_roots: Vec<PathBuf> = Vec::new();
    if let Some(h) = &home {
        tb_roots.push(h.join(".thunderbird"));
        tb_roots.push(h.join("snap/thunderbird/common/.thunderbird"));
        tb_roots.push(h.join(".var/app/org.mozilla.Thunderbird/.thunderbird"));
        tb_roots.push(h.join("Library/Thunderbird"));
    }
    if let Some(c) = dirs::config_dir() {
        // Windows: %APPDATA%\Thunderbird.
        tb_roots.push(c.join("Thunderbird"));
    }
    for r in tb_roots {
        if thunderbird::is_root(&r) {
            for p in thunderbird::profiles_from_ini(&r) {
                let label = format!("Thunderbird — {}", file_name(&p));
                out.push(DetectedSource { kind: SourceKind::ThunderbirdProfile, path: p, label, bytes: 0 });
            }
        }
    }
    if let Some(h) = &home {
        let mail = h.join("Library/Mail");
        if apple::is_apple_mail(&mail) {
            out.push(DetectedSource { kind: SourceKind::AppleMail, path: mail, label: "Apple Mail".into(), bytes: 0 });
        }
        let mut pst_dirs = vec![h.join("Documents/Outlook Files"), h.join("Documents/Archivos de Outlook")];
        if let Some(l) = dirs::data_local_dir() {
            pst_dirs.push(l.join("Microsoft/Outlook"));
        }
        for d in pst_dirs {
            for f in files_in(&d) {
                if matches!(ext(&f).as_str(), "pst" | "ost") {
                    let bytes = std::fs::metadata(&f).map(|m| m.len()).unwrap_or(0);
                    out.push(DetectedSource { kind: SourceKind::OutlookPst, label: file_name(&f), path: f, bytes });
                }
            }
        }
    }
    for s in &mut out {
        if s.bytes == 0 {
            s.bytes = crate::util::walk_files(&s.path, 200_000, 32).files.iter().map(|(_, n)| n).sum();
        }
    }
    out
}

/// Cuenta por omisión para correo suelto.
fn default_account(sink: &Sink<'_>) -> (String, String) {
    let name = sink.opts.account_name.clone().unwrap_or_else(|| sink.opts.text("Imported", "Importado").to_string());
    (name, String::new())
}

/// Importa un origen.
pub(crate) fn import_path(sink: &mut Sink<'_>, path: &Path) -> Result<()> {
    let Some(kind) = detect(path) else {
        if !path.exists() {
            return Err(ImportError::io(path, std::io::Error::from(std::io::ErrorKind::NotFound)));
        }
        sink.skip(path.display().to_string(), SkipReason::Unsupported { what: file_name(path) });
        return Ok(());
    };
    sink.report.sources.push((kind, path.to_path_buf()));
    sink.emit(crate::report::ImportEvent::SourceStarted { kind, path: path.to_path_buf() });
    sink.set_current(path.display().to_string());
    let account = default_account(sink);
    let parent_name = path.parent().map(file_name).filter(|n| !n.is_empty()).unwrap_or_else(|| account.0.clone());
    match kind {
        SourceKind::Thunderbird => thunderbird::import_root(sink, path),
        SourceKind::ThunderbirdProfile => thunderbird::import_profile(sink, path),
        SourceKind::ThunderbirdAddressBook => {
            let book = pim::stem_name(path, "Thunderbird");
            thunderbird::import_abook(sink, path, &("Thunderbird".into(), String::new()), &book)
        }
        SourceKind::AppleMail => apple::import_root(sink, path),
        SourceKind::AppleMailbox => {
            apple::import_bundle(sink, path, &("Apple Mail".into(), String::new()), Vec::new(), 0)
        }
        SourceKind::OutlookPst => pst::import_pst(sink, path),
        SourceKind::Mbox => import_mbox(sink, path, &Target::new(account, vec![pim::stem_name(path, "mbox")])),
        SourceKind::Maildir => {
            let mut t = Target::new(account, vec![file_name(path)]);
            if file_name(path).eq_ignore_ascii_case("maildir") || has_maildirpp_children(path) {
                t.path = vec![sink.opts.text("Inbox", "Bandeja de entrada").into()];
                t.role = Some(FolderRole::Inbox);
            }
            import_maildir(sink, path, &t)
        }
        SourceKind::Eml => import_eml_files(sink, &[path.to_path_buf()], &Target::new(account, vec![parent_name])),
        SourceKind::Emlx => import_emlx_files(sink, &[path.to_path_buf()], &Target::new(account, vec![parent_name])),
        SourceKind::Ics => {
            let acct = sink.account(&account.0, &account.1)?;
            pim::import_ics(sink, path, acct, &pim::stem_name(path, "Calendario"))
        }
        SourceKind::Vcf => {
            let acct = sink.account(&account.0, &account.1)?;
            pim::import_vcf(sink, path, acct, &pim::stem_name(path, "Contactos"))
        }
        SourceKind::Directory => import_dir(sink, path, &account, Vec::new(), 0),
    }
}

fn has_maildirpp_children(dir: &Path) -> bool {
    subdirs(dir).iter().any(|d| file_name(d).starts_with('.') && is_maildir(d))
}

/// Recorre un directorio cualquiera: la ruta relativa da la carpeta.
fn import_dir(
    sink: &mut Sink<'_>,
    dir: &Path,
    account: &(String, String),
    rel: Vec<String>,
    depth: usize,
) -> Result<()> {
    if depth > MAX_WALK_DEPTH {
        sink.skip(dir.display().to_string(), SkipReason::LimitReached { what: "profundidad de directorios".into() });
        return Ok(());
    }
    sink.check_cancel()?;
    let here = if rel.is_empty() { vec![file_name(dir)] } else { rel.clone() };
    let mut eml = Vec::new();
    let mut emlx = Vec::new();
    // Contenedores PIM por directorio (Takeout: `Calendar/Trabajo.ics`, `Contacts/Mis contactos/…vcf`).
    let mut others: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for f in files_in(dir) {
        let name = file_name(&f);
        if name.starts_with('.') {
            continue;
        }
        match detect(&f) {
            Some(SourceKind::Eml) => eml.push(f),
            Some(SourceKind::Emlx) => emlx.push(f),
            Some(SourceKind::Mbox) => {
                let mut path = rel.clone();
                path.push(pim::stem_name(&f, "mbox"));
                import_mbox(sink, &f, &Target::new(account.clone(), path))?;
            }
            Some(SourceKind::OutlookPst) => pst::import_pst(sink, &f)?,
            Some(SourceKind::Ics) => others.entry("ics".into()).or_default().push(f),
            Some(SourceKind::Vcf) => others.entry("vcf".into()).or_default().push(f),
            Some(SourceKind::ThunderbirdAddressBook) => {
                let book = pim::stem_name(&f, "Thunderbird");
                thunderbird::import_abook(sink, &f, account, &book)?;
            }
            _ => {
                if ext(&f) == "msf" {
                    continue;
                }
                // Archivos que no son de correo/PIM (fotos de Takeout, HTML…): se ignoran en silencio.
            }
        }
    }
    import_eml_files(sink, &eml, &Target::new(account.clone(), here.clone()))?;
    emlx.sort_by_key(|f| {
        let n = file_name(f);
        (n.split('.').next().and_then(|s| s.parse::<u64>().ok()).unwrap_or(u64::MAX), n)
    });
    import_emlx_files(sink, &emlx, &Target::new(account.clone(), here.clone()))?;
    for f in others.remove("ics").unwrap_or_default() {
        let acct = sink.account(&account.0, &account.1)?;
        pim::import_ics(sink, &f, acct, &pim::stem_name(&f, "Calendario"))?;
    }
    for f in others.remove("vcf").unwrap_or_default() {
        let acct = sink.account(&account.0, &account.1)?;
        pim::import_vcf(sink, &f, acct, &pim::stem_name(&f, "Contactos"))?;
    }
    for d in subdirs(dir) {
        let name = file_name(&d);
        if name.starts_with('.') && !is_maildir(&d) {
            continue;
        }
        let mut child_rel = rel.clone();
        child_rel.push(name.trim_end_matches(".mbox").trim_end_matches(".sbd").to_string());
        if thunderbird::is_root(&d) {
            thunderbird::import_root(sink, &d)?;
        } else if thunderbird::is_profile(&d) {
            thunderbird::import_profile(sink, &d)?;
        } else if apple::is_apple_mail(&d) {
            apple::import_root(sink, &d)?;
        } else if apple::is_bundle(&d) {
            apple::import_bundle(sink, &d, account, rel.clone(), 0)?;
        } else if is_maildir(&d) {
            import_maildir(sink, &d, &Target::new(account.clone(), child_rel))?;
        } else if name.ends_with(".sbd") {
            // Árbol de carpetas de Thunderbird copiado a mano.
            import_dir(sink, &d, account, child_rel, depth.saturating_add(1))?;
        } else if name == "calendar-data" {
            sink.notice(Notice::ThunderbirdCalendarNeedsExport { path: d.join("local.sqlite") });
        } else {
            import_dir(sink, &d, account, child_rel, depth.saturating_add(1))?;
        }
    }
    Ok(())
}

/// Bytes a procesar (para el progreso): archivos del origen, sin índices.
pub(crate) fn estimate_bytes(paths: &[PathBuf], max_files: usize) -> u64 {
    let mut total = 0u64;
    for p in paths {
        if p.is_file() {
            total = total.saturating_add(std::fs::metadata(p).map(|m| m.len()).unwrap_or(0));
        } else {
            let w = crate::util::walk_files(p, max_files, 40);
            for (f, n) in w.files {
                let e = ext(&f);
                if matches!(e.as_str(), "msf" | "jpg" | "jpeg" | "png" | "html" | "json" | "js" | "dat") {
                    continue;
                }
                total = total.saturating_add(n);
            }
        }
    }
    total
}
