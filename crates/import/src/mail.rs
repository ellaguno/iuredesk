//! Correo en archivos: mbox (incluido Gmail Takeout), Maildir, `.eml` y `.emlx` sueltos.

use std::fs::File;
use std::io::{BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use iuredesk_model::{Flags, FolderId, FolderRole};

use crate::checkpoint::unit_key;
use crate::emlx::{parse_emlx, reattach};
use crate::error::{ImportError, Result};
use crate::headers::{self, MboxFlags};
use crate::mbox::MboxReader;
use crate::report::{Notice, SkipReason};
use crate::sink::{MsgMeta, Sink};
use crate::util::{ReadError, file_name, files_in, read_bounded, subdirs};

/// Dónde va el correo de un archivo: cuenta y ruta de carpeta por omisión (los mensajes con
/// etiquetas de Gmail van a la cuenta de Gmail, a la carpeta que digan sus etiquetas).
#[derive(Clone, Debug)]
pub(crate) struct Target {
    /// Cuenta por omisión (nombre visible y correo).
    pub account: (String, String),
    /// Carpeta por omisión.
    pub path: Vec<String>,
    /// Función forzada de la carpeta por omisión.
    pub role: Option<FolderRole>,
}

impl Target {
    pub fn new(account: (String, String), path: Vec<String>) -> Self {
        Self { account, path, role: None }
    }

    fn folder(&self, sink: &mut Sink<'_>) -> Result<FolderId> {
        let acct = sink.account(&self.account.0, &self.account.1)?;
        sink.folder(acct, &self.path, self.role)
    }
}

fn mtime(path: &Path) -> Option<DateTime<Utc>> {
    let t = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(DateTime::<Utc>::from(t))
}

// ---------------------------------------------------------------------------- Gmail

/// Carpeta, banderas y palabras clave de un mensaje de Gmail Takeout según `X-Gmail-Labels`.
pub(crate) fn gmail_mapping(labels: &[String], lang_en: bool) -> (Vec<String>, Option<FolderRole>, Flags, Vec<String>) {
    let has = |n: &str| labels.iter().any(|l| l.eq_ignore_ascii_case(n));
    let pick = |en: &str, es: &str| if lang_en { en.to_string() } else { es.to_string() };
    let system = |l: &str| {
        let lower = l.to_ascii_lowercase();
        matches!(
            lower.as_str(),
            "inbox"
                | "sent"
                | "drafts"
                | "spam"
                | "trash"
                | "starred"
                | "important"
                | "unread"
                | "opened"
                | "archived"
                | "chat"
                | "chats"
        ) || lower.starts_with("category ")
            || lower.starts_with("categoría ")
            || lower.starts_with("imap_")
    };
    let user: Vec<&String> = labels.iter().filter(|l| !system(l)).collect();
    let (path, role) = if has("Trash") {
        (vec![pick("Trash", "Papelera")], Some(FolderRole::Trash))
    } else if has("Spam") {
        (vec![pick("Junk", "Correo no deseado")], Some(FolderRole::Junk))
    } else if has("Drafts") {
        (vec![pick("Drafts", "Borradores")], Some(FolderRole::Drafts))
    } else if has("Inbox") {
        (vec![pick("Inbox", "Bandeja de entrada")], Some(FolderRole::Inbox))
    } else if has("Sent") {
        (vec![pick("Sent", "Enviados")], Some(FolderRole::Sent))
    } else if has("Chat") || has("Chats") {
        (vec!["Chats".to_string()], None)
    } else if let Some(first) = user.first() {
        (first.split('/').map(str::to_string).collect(), None)
    } else {
        (vec![pick("Archive", "Archivo")], Some(FolderRole::Archive))
    };
    let folder_label = path.join("/");
    let keywords = user.iter().filter(|l| ***l != folder_label).take(100).map(|l| (*l).clone()).collect();
    let flags = Flags { seen: !has("Unread"), flagged: has("Starred"), draft: has("Drafts"), ..Default::default() };
    (path, role, flags, keywords)
}

// ---------------------------------------------------------------------------- mbox

/// Importa un mbox por flujo, reanudable.
pub(crate) fn import_mbox(sink: &mut Sink<'_>, path: &Path, target: &Target) -> Result<()> {
    let key = unit_key("mbox", path);
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if sink.checkpoint.is_done(&key) {
        sink.report.resumed = true;
        sink.advance(size);
        return Ok(());
    }
    let start = sink.checkpoint.offset(&key).min(size);
    if start > 0 {
        sink.report.resumed = true;
        sink.advance(start);
    }
    let mut f = File::open(path).map_err(|e| ImportError::io(path, e))?;
    f.seek(SeekFrom::Start(start)).map_err(|e| ImportError::io(path, e))?;
    let max = usize::try_from(sink.opts.max_message_bytes).unwrap_or(usize::MAX);
    let mut reader = MboxReader::new(BufReader::with_capacity(1024 * 1024, f), start, max);
    let mut default_folder: Option<FolderId> = None;
    let gmail_account = sink.opts.text("Gmail (Takeout)", "Gmail (Takeout)").to_string();
    let lang_en = sink.opts.is_en();
    loop {
        sink.check_cancel()?;
        let m = match reader.next_message() {
            Ok(Some(m)) => m,
            Ok(None) => break,
            Err(e) => {
                sink.skip(path.display().to_string(), SkipReason::Io { detail: e.to_string() });
                break;
            }
        };
        sink.advance(m.end.saturating_sub(m.offset));
        let index = m.index;
        let loc = move || format!("{}#{}", path.display(), index);
        if let Some(size) = m.oversized {
            sink.skip(loc(), SkipReason::TooLarge { size, max: sink.opts.max_message_bytes });
            sink.checkpoint.stage_offset(key.clone(), m.end);
            continue;
        }
        let (hdr, _) = headers::split_header_body(&m.raw);
        let fields = headers::header_fields(hdr);
        let MboxFlags { flags, keywords, expunged, gmail_labels } = headers::mbox_flags(&fields);
        if expunged {
            sink.skip(loc(), SkipReason::DeletedInSource);
            sink.checkpoint.stage_offset(key.clone(), m.end);
            continue;
        }
        let (folder, meta) = match gmail_labels {
            Some(labels) => {
                let (gpath, role, gflags, gkeywords) = gmail_mapping(&labels, lang_en);
                let acct = sink.account(&gmail_account, "")?;
                let folder = sink.folder(acct, &gpath, role)?;
                (folder, MsgMeta { flags: Some(gflags), keywords: gkeywords, received: m.from_date, dedup: None })
            }
            None => {
                let folder = match default_folder {
                    Some(f) => f,
                    None => {
                        let f = target.folder(sink)?;
                        default_folder = Some(f);
                        f
                    }
                };
                (folder, MsgMeta { flags, keywords, received: m.from_date, dedup: None })
            }
        };
        sink.message(folder, m.raw, meta, &loc)?;
        sink.checkpoint.stage_offset(key.clone(), m.end);
    }
    if reader.leading_garbage > 0 && reader.leading_garbage == size.saturating_sub(start) {
        sink.skip(path.display().to_string(), SkipReason::Malformed { detail: "no es un mbox".into() });
    }
    if default_folder.is_none() && size == 0 {
        // Carpeta vacía en el origen: se crea igual para conservar el árbol.
        target.folder(sink)?;
    }
    sink.checkpoint.stage_done(key);
    sink.flush()
}

// ---------------------------------------------------------------------------- Maildir

/// ¿Es un directorio Maildir (`cur/` y `new/`)?
pub(crate) fn is_maildir(dir: &Path) -> bool {
    dir.join("cur").is_dir() && dir.join("new").is_dir()
}

/// Banderas del nombre de archivo Maildir (`…:2,FRS`; en Windows, `!2,FRS`).
pub(crate) fn maildir_flags(name: &str, in_new: bool) -> Flags {
    let info = name.rsplit_once(":2,").or_else(|| name.rsplit_once("!2,")).map(|(_, f)| f).unwrap_or("");
    Flags {
        seen: !in_new && info.contains('S'),
        answered: info.contains('R'),
        flagged: info.contains('F'),
        draft: info.contains('D'),
        deleted: info.contains('T'),
        forwarded: info.contains('P'),
    }
}

/// Importa un Maildir (y sus subcarpetas Maildir++ `.Carpeta.Sub`).
pub(crate) fn import_maildir(sink: &mut Sink<'_>, dir: &Path, target: &Target) -> Result<()> {
    import_maildir_folder(sink, dir, target)?;
    for sub in subdirs(dir) {
        let name = file_name(&sub);
        if let Some(rest) = name.strip_prefix('.')
            && !rest.is_empty()
            && is_maildir(&sub)
        {
            let mut t = target.clone();
            t.role = None;
            // En Maildir++ la raíz es la bandeja de entrada y las demás cuelgan del mismo nivel.
            let root_is_inbox = target.role == Some(FolderRole::Inbox);
            if root_is_inbox {
                t.path.pop();
            }
            t.path.extend(rest.split('.').filter(|s| !s.is_empty()).map(str::to_string));
            import_maildir_folder(sink, &sub, &t)?;
        }
    }
    Ok(())
}

fn import_maildir_folder(sink: &mut Sink<'_>, dir: &Path, target: &Target) -> Result<()> {
    let key = unit_key("maildir", dir);
    if sink.checkpoint.is_done(&key) {
        sink.report.resumed = true;
        return Ok(());
    }
    let folder = target.folder(sink)?;
    for (sub, in_new) in [("cur", false), ("new", true)] {
        for f in files_in(&dir.join(sub)) {
            let name = file_name(&f);
            if name.starts_with('.') {
                continue;
            }
            import_one_file(sink, &f, folder, |_| MsgMeta {
                flags: Some(maildir_flags(&name, in_new)),
                ..Default::default()
            })?;
        }
    }
    sink.checkpoint.stage_done(key);
    sink.flush()
}

/// Lee un archivo de mensaje (acotado) y lo pasa al destino con los metadatos de `meta`.
fn import_one_file(sink: &mut Sink<'_>, f: &Path, folder: FolderId, meta: impl FnOnce(&[u8]) -> MsgMeta) -> Result<()> {
    let loc = || f.display().to_string();
    match read_bounded(f, sink.opts.max_message_bytes) {
        Ok(raw) => {
            sink.advance(raw.len() as u64);
            let mut m = meta(&raw);
            if m.received.is_none() {
                m.received = mtime(f);
            }
            sink.message(folder, raw, m, &loc)
        }
        Err(ReadError::TooLarge(size)) => {
            sink.advance(size);
            sink.skip(loc(), SkipReason::TooLarge { size, max: sink.opts.max_message_bytes });
            Ok(())
        }
        Err(ReadError::Io(detail)) => {
            sink.skip(loc(), SkipReason::Io { detail });
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------- .eml / .emlx

/// Importa los `.eml` de un directorio (no recursivo) en la carpeta del destino.
pub(crate) fn import_eml_files(sink: &mut Sink<'_>, files: &[PathBuf], target: &Target) -> Result<()> {
    if files.is_empty() {
        return Ok(());
    }
    let folder = target.folder(sink)?;
    for f in files {
        sink.check_cancel()?;
        import_one_file(sink, f, folder, |raw| {
            let (h, _) = headers::split_header_body(raw);
            let fl = headers::mbox_flags(&headers::header_fields(h));
            MsgMeta { flags: fl.flags, keywords: fl.keywords, ..Default::default() }
        })?;
    }
    sink.flush()
}

/// Importa `.emlx`/`.partial.emlx` (archivos de un mismo directorio `Messages`).
pub(crate) fn import_emlx_files(sink: &mut Sink<'_>, files: &[PathBuf], target: &Target) -> Result<()> {
    if files.is_empty() {
        return Ok(());
    }
    let folder = target.folder(sink)?;
    let mut partial_missing = 0u64;
    for f in files {
        sink.check_cancel()?;
        let loc = || f.display().to_string();
        let data = match read_bounded(f, sink.opts.max_message_bytes.saturating_add(64 * 1024)) {
            Ok(d) => d,
            Err(ReadError::TooLarge(size)) => {
                sink.advance(size);
                sink.skip(loc(), SkipReason::TooLarge { size, max: sink.opts.max_message_bytes });
                continue;
            }
            Err(ReadError::Io(detail)) => {
                sink.skip(loc(), SkipReason::Io { detail });
                continue;
            }
        };
        sink.advance(data.len() as u64);
        let e = match parse_emlx(&data) {
            Ok(e) => e,
            Err(detail) => {
                sink.skip(loc(), SkipReason::Malformed { detail });
                continue;
            }
        };
        let name = file_name(f);
        let mut raw = e.raw;
        if let Some(id) = name.strip_suffix(".partial.emlx") {
            let att = f.parent().and_then(Path::parent).map(|p| p.join("Attachments").join(id));
            if let Some(att) = att {
                let max = usize::try_from(sink.opts.max_message_bytes).unwrap_or(usize::MAX);
                let r = reattach(raw, &att, sink.opts.max_message_bytes, max);
                if r.missing > 0 {
                    partial_missing = partial_missing.saturating_add(1);
                }
                raw = r.raw;
            }
        }
        let meta = MsgMeta { flags: e.flags, received: e.received.or_else(|| mtime(f)), ..Default::default() };
        sink.message(folder, raw, meta, &loc)?;
    }
    if partial_missing > 0 {
        sink.notice(Notice::PartialMessages { count: partial_missing });
    }
    sink.flush()
}

/// `.emlx` de un directorio, ordenados de forma natural (`2.emlx` antes que `10.emlx`).
pub(crate) fn emlx_in(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = files_in(dir).into_iter().filter(|f| file_name(f).ends_with(".emlx")).collect();
    v.sort_by_key(|f| {
        let n = file_name(f);
        let num: u64 = n.split('.').next().and_then(|s| s.parse().ok()).unwrap_or(u64::MAX);
        (num, n)
    });
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maildir_info_flags() {
        let f = maildir_flags("1700000000.M1P2.host:2,FRS", false);
        assert!(f.seen && f.answered && f.flagged && !f.draft);
        assert!(!maildir_flags("x:2,S", true).seen, "en new/ nunca está leído");
        assert!(maildir_flags("x!2,ST", false).deleted);
        assert_eq!(maildir_flags("sin-info", false), Flags::default());
    }

    #[test]
    fn gmail_labels_to_folders() {
        let l = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let (p, r, f, k) = gmail_mapping(&l(&["Inbox", "Unread", "Clientes/ACME", "Important"]), false);
        assert_eq!((p, r), (vec!["Bandeja de entrada".to_string()], Some(FolderRole::Inbox)));
        assert!(!f.seen);
        assert_eq!(k, vec!["Clientes/ACME".to_string()]);
        let (p, r, f, k) = gmail_mapping(&l(&["Clientes/ACME", "Starred", "Opened"]), true);
        assert_eq!((p, r), (vec!["Clientes".to_string(), "ACME".to_string()], None));
        assert!(f.seen && f.flagged);
        assert!(k.is_empty());
        let (p, r, ..) = gmail_mapping(&l(&["Archived", "Category Updates"]), true);
        assert_eq!((p, r), (vec!["Archive".to_string()], Some(FolderRole::Archive)));
        let (p, r, ..) = gmail_mapping(&l(&["Sent", "Inbox"]), true);
        assert_eq!((p, r), (vec!["Inbox".to_string()], Some(FolderRole::Inbox)));
        let (_, r, ..) = gmail_mapping(&l(&["Spam", "Inbox"]), true);
        assert_eq!(r, Some(FolderRole::Junk));
    }
}
