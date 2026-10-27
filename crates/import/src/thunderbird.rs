//! Thunderbird: perfiles (`profiles.ini`), cuentas (`prefs.js`), carpetas mbox o Maildir
//! (`Carpeta`, `Carpeta.msf` ignorado, `Carpeta.sbd/` con las subcarpetas) y libretas
//! (`abook.sqlite`, `history.sqlite`, leídas con el lector propio de [`crate::sqlite`]).
//!
//! El calendario local de Thunderbird (`calendar-data/local.sqlite`) guarda los eventos en tablas
//! propias, no en iCalendar: se pide exportarlo a `.ics` (aviso en el informe).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use iuredesk_model::{Contact, Labeled, PostalAddress};

use crate::checkpoint::unit_key;
use crate::error::Result;
use crate::mail::{Target, import_maildir, import_mbox, is_maildir};
use crate::report::{Notice, SkipReason};
use crate::sink::Sink;
use crate::sqlite::SqliteDb;
use crate::util::{file_name, files_in, head, subdirs};

/// Archivos de un directorio de correo de Thunderbird que no son carpetas.
const IGNORED_FILES: &[&str] = &[
    "msgfilterrules.dat",
    "popstate.dat",
    "filterlog.html",
    "junklog.html",
    "virtualfolders.dat",
    "foldertree.json",
    "feeds.json",
    "feeditems.json",
    "feeds.rdf",
    "feeditems.rdf",
];
/// Extensiones que nunca son carpetas mbox.
const IGNORED_EXT: &[&str] =
    &["msf", "dat", "html", "json", "rdf", "sqlite", "sqlite-wal", "sqlite-shm", "mab", "js", "log", "bak", "tmp"];
/// Tamaño máximo de `prefs.js` / `profiles.ini`.
const MAX_CONFIG: u64 = 16 * 1024 * 1024;
/// Profundidad máxima del árbol de carpetas.
const MAX_TREE_DEPTH: usize = 32;

/// Rutas de perfiles de un `profiles.ini`.
pub fn profiles_from_ini(root: &Path) -> Vec<PathBuf> {
    let Ok(text) = crate::util::read_bounded(&root.join("profiles.ini"), MAX_CONFIG) else { return Vec::new() };
    let text = String::from_utf8_lossy(&text);
    let mut out: Vec<PathBuf> = Vec::new();
    let mut path: Option<String> = None;
    let mut relative = true;
    let push = |path: &mut Option<String>, relative: bool, out: &mut Vec<PathBuf>| {
        if let Some(p) = path.take() {
            let p = p.replace('\\', "/");
            let full = if relative { root.join(&p) } else { PathBuf::from(&p) };
            if !out.contains(&full) {
                out.push(full);
            }
        }
    };
    for line in text.lines().take(100_000) {
        let line = line.trim();
        if line.starts_with('[') {
            push(&mut path, relative, &mut out);
            relative = true;
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            match k.trim() {
                "Path" => path = Some(v.trim().to_string()),
                "IsRelative" => relative = v.trim() != "0",
                _ => {}
            }
        }
    }
    push(&mut path, relative, &mut out);
    out.retain(|p| p.is_dir());
    out
}

/// `user_pref("clave", valor);` de un `prefs.js`.
pub fn parse_prefs(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in text.lines().take(1_000_000) {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("user_pref(").or_else(|| line.strip_prefix("pref(")) else { continue };
        let Some((key, rest)) = parse_js_string(rest) else { continue };
        let rest = rest.trim_start().strip_prefix(',').unwrap_or("").trim_start();
        let value = if rest.starts_with('"') {
            parse_js_string(rest).map(|(v, _)| v)
        } else {
            rest.split(')').next().map(|v| v.trim().to_string())
        };
        if let Some(v) = value {
            out.insert(key, v);
        }
    }
    out
}

/// Cadena JS entre comillas al inicio de `s`: (valor, resto).
fn parse_js_string(s: &str) -> Option<(String, &str)> {
    let s = s.trim_start().strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = s.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some((_, 'n')) => out.push('\n'),
                Some((_, 'u')) => {
                    let hex: String = (0..4).filter_map(|_| chars.next().map(|(_, c)| c)).collect();
                    if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                        out.push(ch);
                    }
                }
                Some((_, c)) => out.push(c),
                None => return None,
            },
            '"' => return Some((out, s.get(i.saturating_add(1)..).unwrap_or(""))),
            c => out.push(c),
        }
        if out.len() > 64 * 1024 {
            return None;
        }
    }
    None
}

/// Cuenta de correo de un perfil.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TbServer {
    /// Directorio de sus carpetas.
    pub dir: PathBuf,
    /// Nombre visible.
    pub name: String,
    /// Correo de la identidad principal.
    pub email: String,
    /// Tipo (`none`, `pop3`, `imap`, `rss`, `nntp`).
    pub kind: String,
}

/// Cuentas de un perfil según `prefs.js` (o, si no hay, los directorios `Mail/*` e `ImapMail/*`).
pub fn servers(profile: &Path) -> Vec<TbServer> {
    let prefs = crate::util::read_bounded(&profile.join("prefs.js"), MAX_CONFIG)
        .map(|b| parse_prefs(&String::from_utf8_lossy(&b)))
        .unwrap_or_default();
    let mut by_server: BTreeMap<String, HashMap<String, String>> = BTreeMap::new();
    for (k, v) in &prefs {
        if let Some(rest) = k.strip_prefix("mail.server.")
            && let Some((id, field)) = rest.split_once('.')
        {
            by_server.entry(id.to_string()).or_default().insert(field.to_string(), v.clone());
        }
    }
    // servidor → correo de la identidad de su cuenta.
    let mut emails: HashMap<String, String> = HashMap::new();
    for (k, v) in &prefs {
        if let Some(acct) = k.strip_prefix("mail.account.").and_then(|r| r.strip_suffix(".server"))
            && let Some(ids) = prefs.get(&format!("mail.account.{acct}.identities"))
            && let Some(first) = ids.split(',').next()
            && let Some(email) = prefs.get(&format!("mail.identity.{}.useremail", first.trim()))
        {
            emails.insert(v.clone(), email.clone());
        }
    }
    let mut out = Vec::new();
    for (id, f) in by_server {
        let dir = f
            .get("directory-rel")
            .and_then(|r| r.strip_prefix("[ProfD]"))
            .map(|r| profile.join(r.replace('\\', "/")))
            .or_else(|| f.get("directory").map(PathBuf::from))
            .filter(|d| d.is_dir());
        let Some(dir) = dir else { continue };
        let kind = f.get("type").cloned().unwrap_or_default();
        let email = emails.get(&id).cloned().unwrap_or_default();
        let name = f
            .get("name")
            .cloned()
            .filter(|n| !n.trim().is_empty())
            .or_else(|| f.get("hostname").cloned())
            .unwrap_or_else(|| file_name(&dir));
        if !out.iter().any(|s: &TbServer| s.dir == dir) {
            out.push(TbServer { dir, name, email, kind });
        }
    }
    if out.is_empty() {
        for (sub, kind) in [("Mail", "none"), ("ImapMail", "imap")] {
            for d in subdirs(&profile.join(sub)) {
                let name = file_name(&d);
                out.push(TbServer { dir: d, name, email: String::new(), kind: kind.into() });
            }
        }
    }
    out
}

/// ¿Es un perfil de Thunderbird?
pub fn is_profile(dir: &Path) -> bool {
    dir.join("prefs.js").is_file() && (dir.join("Mail").is_dir() || dir.join("ImapMail").is_dir())
        || dir.join("abook.sqlite").is_file() && dir.join("prefs.js").is_file()
}

/// ¿Es el directorio raíz de Thunderbird (con `profiles.ini`)?
pub fn is_root(dir: &Path) -> bool {
    dir.join("profiles.ini").is_file()
}

/// Importa todos los perfiles de un directorio raíz.
pub(crate) fn import_root(sink: &mut Sink<'_>, root: &Path) -> Result<()> {
    let profiles = profiles_from_ini(root);
    for p in profiles {
        import_profile(sink, &p)?;
    }
    Ok(())
}

/// Importa un perfil: correo de cada cuenta, libretas y avisos del calendario.
pub(crate) fn import_profile(sink: &mut Sink<'_>, profile: &Path) -> Result<()> {
    let multi = profile.parent().is_some_and(|p| profiles_from_ini(p).len() > 1);
    let suffix = if multi { format!(" ({})", file_name(profile)) } else { String::new() };
    for s in servers(profile) {
        match s.kind.as_str() {
            "rss" | "nntp" => {
                let what = if s.kind == "rss" { "RSS" } else { "News (NNTP)" };
                sink.notice(Notice::Ignored { path: s.dir.clone(), what: what.into() });
                continue;
            }
            "imap" => sink.notice(Notice::ImapOfflineCache { path: s.dir.clone() }),
            _ => {}
        }
        let label = format!("Thunderbird · {}{suffix}", s.name);
        sink.set_current(label.clone());
        import_mail_tree(sink, &s.dir, &(label, s.email.clone()), Vec::new(), 0)?;
    }
    import_address_books(sink, profile, &suffix)?;
    let cal = profile.join("calendar-data").join("local.sqlite");
    if cal.is_file() {
        sink.notice(Notice::ThunderbirdCalendarNeedsExport { path: cal });
    }
    Ok(())
}

/// ¿Parece una carpeta mbox de Thunderbird?
fn is_tb_mbox(f: &Path) -> bool {
    let name = file_name(f);
    let lower = name.to_ascii_lowercase();
    if name.starts_with('.') || IGNORED_FILES.contains(&lower.as_str()) {
        return false;
    }
    if let Some((_, ext)) = lower.rsplit_once('.')
        && IGNORED_EXT.contains(&ext)
    {
        return false;
    }
    let len = std::fs::metadata(f).map(|m| m.len()).unwrap_or(0);
    let has_msf = f.with_file_name(format!("{name}.msf")).is_file();
    if len == 0 {
        return has_msf;
    }
    head(f, 5) == b"From " || has_msf
}

/// Recorre un directorio de correo de Thunderbird.
fn import_mail_tree(
    sink: &mut Sink<'_>,
    dir: &Path,
    account: &(String, String),
    prefix: Vec<String>,
    depth: usize,
) -> Result<()> {
    if depth > MAX_TREE_DEPTH {
        sink.skip(dir.display().to_string(), SkipReason::LimitReached { what: "profundidad de carpetas".into() });
        return Ok(());
    }
    for f in files_in(dir) {
        sink.check_cancel()?;
        if !is_tb_mbox(&f) {
            continue;
        }
        let mut path = prefix.clone();
        path.push(file_name(&f));
        import_mbox(sink, &f, &Target::new(account.clone(), path))?;
    }
    for d in subdirs(dir) {
        sink.check_cancel()?;
        let name = file_name(&d);
        if let Some(parent) = name.strip_suffix(".sbd") {
            let mut path = prefix.clone();
            path.push(parent.to_string());
            import_mail_tree(sink, &d, account, path, depth.saturating_add(1))?;
        } else if is_maildir(&d) {
            // Almacenamiento Maildir de Thunderbird: cada carpeta es un directorio con cur/new.
            let mut path = prefix.clone();
            path.push(name);
            import_maildir(sink, &d, &Target::new(account.clone(), path))?;
        }
    }
    Ok(())
}

/// Nombres de las libretas según `prefs.js` (`ldap_2.servers.X.filename` → `description`).
fn book_names(profile: &Path) -> HashMap<String, String> {
    let prefs = crate::util::read_bounded(&profile.join("prefs.js"), MAX_CONFIG)
        .map(|b| parse_prefs(&String::from_utf8_lossy(&b)))
        .unwrap_or_default();
    let mut out = HashMap::new();
    for (k, v) in &prefs {
        if let Some(id) = k.strip_prefix("ldap_2.servers.").and_then(|r| r.strip_suffix(".filename"))
            && let Some(desc) = prefs.get(&format!("ldap_2.servers.{id}.description"))
        {
            out.insert(v.clone(), desc.clone());
        }
    }
    out
}

fn import_address_books(sink: &mut Sink<'_>, profile: &Path, suffix: &str) -> Result<()> {
    let names = book_names(profile);
    for f in files_in(profile) {
        let name = file_name(&f);
        if name.ends_with(".mab") {
            sink.notice(Notice::Ignored {
                path: f.clone(),
                what: sink
                    .opts
                    .text(
                        "old address book format: open it once in a recent Thunderbird, or export it as vCard",
                        "formato antiguo de libreta: ábrela una vez en un Thunderbird reciente o expórtala como vCard",
                    )
                    .into(),
            });
            continue;
        }
        if !(name.ends_with(".sqlite") && (name.starts_with("abook") || name == "history.sqlite")) {
            continue;
        }
        let book = match names.get(&name) {
            Some(d) if !d.starts_with("chrome://") => d.clone(),
            _ if name == "history.sqlite" => sink.opts.text("Collected Addresses", "Direcciones recopiladas").into(),
            _ if name == "abook.sqlite" => sink.opts.text("Personal Address Book", "Libreta personal").into(),
            _ => name.trim_end_matches(".sqlite").to_string(),
        };
        let account = (format!("Thunderbird{suffix}"), String::new());
        import_abook(sink, &f, &account, &format!("{book}{suffix}"))?;
    }
    Ok(())
}

/// Importa una libreta `abook.sqlite`.
pub(crate) fn import_abook(sink: &mut Sink<'_>, path: &Path, account: &(String, String), book: &str) -> Result<()> {
    let key = unit_key("abook", path);
    if sink.checkpoint.is_done(&key) {
        sink.report.resumed = true;
        return Ok(());
    }
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    sink.set_current(book.to_string());
    let db = match SqliteDb::open(path, sink.opts.max_message_bytes) {
        Ok(db) => db,
        Err(e) => {
            sink.skip(path.display().to_string(), SkipReason::Malformed { detail: e.to_string() });
            return Ok(());
        }
    };
    let rows = match db.rows("properties", 5_000_000) {
        Ok((_, rows)) => rows,
        Err(e) => {
            sink.skip(path.display().to_string(), SkipReason::Malformed { detail: e.to_string() });
            return Ok(());
        }
    };
    // card → propiedades (en orden de aparición de la tarjeta).
    let mut order: Vec<String> = Vec::new();
    let mut cards: HashMap<String, HashMap<String, String>> = HashMap::new();
    for r in rows {
        let (Some(card), Some(name)) = (r.first().and_then(|v| v.as_text()), r.get(1).and_then(|v| v.as_text())) else {
            continue;
        };
        let value = r.get(2).and_then(|v| v.as_text()).unwrap_or_default();
        let entry = cards.entry(card.clone()).or_insert_with(|| {
            order.push(card.clone());
            HashMap::new()
        });
        if entry.len() < 500 && value.len() <= sink.opts.max_object_bytes {
            entry.insert(name, value);
        }
    }
    let acct = sink.account(&account.0, &account.1)?;
    for card in order {
        let Some(props) = cards.remove(&card) else { continue };
        let (contact, photo) = contact_from_tb(&card, &props);
        sink.contact(acct, book, contact, photo)?;
    }
    sink.advance(size);
    sink.checkpoint.stage_done(key);
    sink.flush()
}

/// Contacto a partir de las propiedades de Thunderbird: la vCard completa (`_vCard`, versión 102+)
/// si está; si no, los campos clásicos (`DisplayName`, `PrimaryEmail`…).
pub fn contact_from_tb(card: &str, p: &HashMap<String, String>) -> (Contact, Option<Vec<u8>>) {
    if let Some(v) = p.get("_vCard")
        && let Ok(vc) = iuredesk_dav::vcard::VCard::parse(v)
    {
        let (mut c, photo) = iuredesk_pim_sync::map::vcard::contact_from_vcard(&vc);
        if c.uid.is_empty() {
            c.uid = card.to_string();
        }
        return (c, photo);
    }
    let get = |k: &str| p.get(k).map(|s| s.trim().to_string()).unwrap_or_default();
    let mut c = Contact {
        uid: card.to_string(),
        full_name: get("DisplayName"),
        given_name: get("FirstName"),
        family_name: get("LastName"),
        nickname: get("NickName"),
        org: get("Company"),
        title: get("JobTitle"),
        notes: get("Notes"),
        ..Default::default()
    };
    for (k, label) in [("PrimaryEmail", "home"), ("SecondEmail", "other")] {
        let v = get(k);
        if !v.is_empty() {
            c.emails.push(Labeled::new(label, v));
        }
    }
    for (k, label) in [
        ("WorkPhone", "work"),
        ("HomePhone", "home"),
        ("CellularNumber", "cell"),
        ("FaxNumber", "fax"),
        ("PagerNumber", "pager"),
    ] {
        let v = get(k);
        if !v.is_empty() {
            c.phones.push(Labeled::new(label, v));
        }
    }
    for (prefix, label) in [("Home", "home"), ("Work", "work")] {
        let a = PostalAddress {
            label: label.into(),
            street: [get(&format!("{prefix}Address")), get(&format!("{prefix}Address2"))]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("\n"),
            city: get(&format!("{prefix}City")),
            region: get(&format!("{prefix}State")),
            postal_code: get(&format!("{prefix}ZipCode")),
            country: get(&format!("{prefix}Country")),
        };
        if !(a.street.is_empty() && a.city.is_empty() && a.postal_code.is_empty() && a.country.is_empty()) {
            c.addresses.push(a);
        }
    }
    let num = |k: &str| get(k).parse::<u32>().ok();
    if let (Some(y), Some(m), Some(d)) = (num("BirthYear"), num("BirthMonth"), num("BirthDay")) {
        c.birthday = i32::try_from(y).ok().and_then(|y| NaiveDate::from_ymd_opt(y, m, d));
    }
    (c, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefs_and_servers() {
        let dir = tempfile::tempdir().unwrap();
        let prof = dir.path().join("abc.default-release");
        std::fs::create_dir_all(prof.join("Mail/Local Folders")).unwrap();
        std::fs::create_dir_all(prof.join("ImapMail/imap.example.com")).unwrap();
        std::fs::write(
            prof.join("prefs.js"),
            "// Mozilla\nuser_pref(\"mail.server.server1.directory-rel\", \"[ProfD]Mail/Local Folders\");\nuser_pref(\"mail.server.server1.name\", \"Carpetas locales\");\nuser_pref(\"mail.server.server1.type\", \"none\");\nuser_pref(\"mail.server.server2.directory-rel\", \"[ProfD]ImapMail/imap.example.com\");\nuser_pref(\"mail.server.server2.name\", \"ana@example.com\");\nuser_pref(\"mail.server.server2.type\", \"imap\");\nuser_pref(\"mail.account.account2.server\", \"server2\");\nuser_pref(\"mail.account.account2.identities\", \"id1\");\nuser_pref(\"mail.identity.id1.useremail\", \"ana@example.com\");\nuser_pref(\"ldap_2.servers.pab.filename\", \"abook.sqlite\");\nuser_pref(\"ldap_2.servers.pab.description\", \"Personal \\\"Libreta\\\"\");\nuser_pref(\"mail.server.server3.directory\", \"/no/existe\");\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("profiles.ini"), "[General]\nStartWithLastProfile=1\n\n[Profile0]\nName=default-release\nIsRelative=1\nPath=abc.default-release\nDefault=1\n\n[Profile1]\nName=ghost\nIsRelative=1\nPath=missing\n").unwrap();
        assert!(is_root(dir.path()));
        assert!(is_profile(&prof));
        assert_eq!(profiles_from_ini(dir.path()), vec![prof.clone()]);
        let s = servers(&prof);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].name, "Carpetas locales");
        assert_eq!(s[1].email, "ana@example.com");
        assert_eq!(s[1].kind, "imap");
        assert_eq!(book_names(&prof).get("abook.sqlite").map(String::as_str), Some("Personal \"Libreta\""));
    }

    #[test]
    fn js_strings() {
        assert_eq!(parse_js_string(r#""a\"bé" , 1"#), Some(("a\"bé".to_string(), " , 1")));
        assert_eq!(parse_js_string("\"sin cerrar"), None);
        let p = parse_prefs("user_pref(\"n\", 42);\nuser_pref(\"b\", true);\n");
        assert_eq!(p.get("n").map(String::as_str), Some("42"));
        assert_eq!(p.get("b").map(String::as_str), Some("true"));
    }

    #[test]
    fn legacy_card_fields() {
        let mut p = HashMap::new();
        for (k, v) in [
            ("DisplayName", "Luis Ejemplo"),
            ("PrimaryEmail", "luis@example.com"),
            ("CellularNumber", "+52 1"),
            ("WorkCity", "Monterrey"),
            ("BirthYear", "1980"),
            ("BirthMonth", "2"),
            ("BirthDay", "29"),
        ] {
            p.insert(k.to_string(), v.to_string());
        }
        let (c, _) = contact_from_tb("id-1", &p);
        assert_eq!(c.uid, "id-1");
        assert_eq!(c.primary_email(), Some("luis@example.com"));
        assert_eq!(c.phones[0].label, "cell");
        assert_eq!(c.addresses[0].city, "Monterrey");
        assert_eq!(c.birthday, NaiveDate::from_ymd_opt(1980, 2, 29));
    }
}
