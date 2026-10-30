//! UX-3: adjuntos fuera del mensaje. Se extraen del original (ya descifrado), se escriben en una
//! caché privada del usuario (carpeta `0700`, archivos `0600` en Unix) con un nombre saneado y la
//! UI los abre con la aplicación del sistema. El nombre y el tipo los declara el remitente: son
//! hostiles. Nunca se abre nada solo; los tipos ejecutables piden confirmación ([`is_dangerous`]).

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use iuredesk_model::MessageId;
use iuredesk_store::Store;
use mail_parser::{MessageParser, MimeHeaders};

use crate::lang::Text;

/// Tamaño máximo de un adjunto que se extrae a disco.
pub const MAX_ATTACHMENT_BYTES: usize = 256 * 1024 * 1024;
/// Largo máximo (bytes UTF-8) del nombre saneado.
const MAX_NAME_BYTES: usize = 120;
/// Lo que se guarda en la caché antes de borrarse en la limpieza.
pub const CACHE_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 3600);
/// Entradas máximas que revisa la limpieza (carpeta hostil o enorme: no se recorre entera).
const MAX_PRUNE_ENTRIES: usize = 10_000;

/// Error al sacar un adjunto.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AttachError {
    #[error("el mensaje aún no está descargado")]
    NotDownloaded,
    #[error("el adjunto ya no está en el mensaje")]
    NotFound,
    #[error("el adjunto es demasiado grande")]
    TooLarge,
    #[error("archivo: {0}")]
    Io(String),
}

impl AttachError {
    /// Mensaje para el usuario.
    pub fn text(&self) -> Text {
        match self {
            AttachError::NotDownloaded => Text::new(
                "The message is still downloading. Try again in a moment.",
                "El mensaje aún se está descargando. Inténtalo en un momento.",
            ),
            AttachError::NotFound => {
                Text::new("That attachment is no longer in the message.", "Ese adjunto ya no está en el mensaje.")
            }
            AttachError::TooLarge => {
                Text::new("The attachment is too large to open.", "El adjunto es demasiado grande para abrirlo.")
            }
            AttachError::Io(e) => Text::new(
                format!("Could not write the attachment to disk ({e})."),
                format!("No se pudo escribir el adjunto en el disco ({e})."),
            ),
        }
    }
}

fn io(e: std::io::Error) -> AttachError {
    AttachError::Io(e.to_string())
}

/// Un adjunto sacado del mensaje.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Extracted {
    /// Nombre declarado (sin sanear).
    pub name: String,
    pub mime: String,
    pub bytes: Vec<u8>,
}

/// Saca el adjunto número `index` (en el orden de [`crate::body::MessageBody::attachments`]) de un
/// mensaje RFC 822. Descifra antes si el mensaje viene cifrado.
pub fn extract(raw: &[u8], index: usize) -> Result<Extracted, AttachError> {
    let opened = crate::security::open(raw);
    let msg = MessageParser::default().parse(&opened.display).ok_or(AttachError::NotFound)?;
    let (part, _) = crate::body::classify(&msg)
        .into_iter()
        .filter(|(_, cid)| cid.is_none())
        .nth(index)
        .ok_or(AttachError::NotFound)?;
    if part.len() > MAX_ATTACHMENT_BYTES {
        return Err(AttachError::TooLarge);
    }
    Ok(Extracted {
        name: part.attachment_name().unwrap_or("adjunto").to_string(),
        mime: crate::body::part_mime(part),
        bytes: part.contents().to_vec(),
    })
}

/// Todos los adjuntos listados de un mensaje («Guardar todos»), como mucho `max`.
pub fn extract_all(raw: &[u8], max: usize) -> Vec<Extracted> {
    let opened = crate::security::open(raw);
    let Some(msg) = MessageParser::default().parse(&opened.display) else { return Vec::new() };
    crate::body::classify(&msg)
        .into_iter()
        .filter(|(p, cid)| cid.is_none() && p.len() <= MAX_ATTACHMENT_BYTES)
        .take(max)
        .map(|(p, _)| Extracted {
            name: p.attachment_name().unwrap_or("adjunto").to_string(),
            mime: crate::body::part_mime(p),
            bytes: p.contents().to_vec(),
        })
        .collect()
}

/// El adjunto `index` de un mensaje del almacén. Si el cuerpo aún no está en el equipo,
/// [`AttachError::NotDownloaded`] (la UI pide el cuerpo y reintenta).
pub fn extract_from_store(store: &Store, msg: MessageId, index: usize) -> Result<Extracted, AttachError> {
    let env = store.get_envelope(msg).ok().flatten().ok_or(AttachError::NotFound)?;
    if env.blob.is_none() {
        return Err(AttachError::NotDownloaded);
    }
    let raw = store.get_raw(msg).map_err(|_| AttachError::NotDownloaded)?;
    extract(&raw, index)
}

/// Caracteres invisibles que cambian el orden de lectura («factura<RLO>fdp.exe» se ve como
/// «factura.exe.pdf») o esconden algo: se quitan del nombre.
fn is_invisible(c: char) -> bool {
    matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2069}' | '\u{FEFF}')
}

/// Nombres reservados de Windows (con o sin extensión).
fn is_reserved_windows(stem: &str) -> bool {
    let s = stem.to_ascii_uppercase();
    let s = s.split('.').next().unwrap_or("");
    matches!(s, "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
        || ((s.starts_with("COM") || s.starts_with("LPT"))
            && s.len() == 4
            && s.chars().nth(3).is_some_and(|d| d.is_ascii_digit() && d != '0'))
}

/// Nombre de archivo seguro para escribir en disco en los tres sistemas: sin carpetas (`../`,
/// `C:\`), sin caracteres de control ni invisibles, sin reservados de Windows, sin punto inicial
/// (archivo oculto) ni final, y acotado conservando la extensión. Nunca vacío.
pub fn sanitize_filename(name: &str) -> String {
    // Solo el último componente: el remitente no elige la carpeta.
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let mut s: String = base
        .chars()
        .filter(|c| !is_invisible(*c))
        .map(|c| if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*') { '_' } else { c })
        .collect();
    // Sin espacios ni puntos en los extremos (Windows los quita; un punto inicial lo oculta).
    s = s.trim_matches(|c: char| c == '.' || c.is_whitespace()).to_string();
    if s.is_empty() {
        s = "adjunto".into();
    }
    if is_reserved_windows(&s) {
        s.insert(0, '_');
    }
    if s.len() > MAX_NAME_BYTES {
        let (stem, ext) = match s.rfind('.') {
            Some(i) if s.len() - i <= 16 && i > 0 => (s.get(..i).unwrap_or(""), s.get(i..).unwrap_or("")),
            _ => (s.as_str(), ""),
        };
        let mut cut = MAX_NAME_BYTES.saturating_sub(ext.len());
        while cut > 0 && !stem.is_char_boundary(cut) {
            cut -= 1;
        }
        let stem = stem
            .get(..cut.min(stem.len()))
            .unwrap_or("")
            .trim_end_matches(|c: char| c == '.' || c.is_whitespace())
            .to_string();
        let name = if stem.is_empty() { format!("adjunto{ext}") } else { format!("{stem}{ext}") };
        s = name;
    }
    s
}

/// Extensión final en minúsculas (sin punto).
fn extension(name: &str) -> String {
    let n = sanitize_filename(name);
    match n.rsplit_once('.') {
        Some((_, e)) => e.to_ascii_lowercase(),
        None => String::new(),
    }
}

/// Extensiones que ejecutan código al abrirse (o lo traen en macros).
const DANGEROUS_EXT: &[&str] = &[
    // Windows
    "exe",
    "com",
    "bat",
    "cmd",
    "scr",
    "pif",
    "cpl",
    "msi",
    "msp",
    "mst",
    "msc",
    "dll",
    "sys",
    "drv",
    "ocx",
    "lnk",
    "url",
    "scf",
    "reg",
    "inf",
    "hta",
    "chm",
    "gadget",
    "appx",
    "msix",
    "appref-ms",
    "application",
    // Scripts
    "js",
    "jse",
    "vbs",
    "vbe",
    "wsf",
    "wsh",
    "wsc",
    "ps1",
    "psm1",
    "psd1",
    "ps1xml",
    "sh",
    "bash",
    "zsh",
    "csh",
    "ksh",
    "fish",
    "command",
    "tool",
    "py",
    "pyw",
    "pl",
    "rb",
    "php",
    "lua",
    "jar",
    "jnlp",
    "class",
    // Linux y macOS
    "desktop",
    "appimage",
    "run",
    "bin",
    "elf",
    "deb",
    "rpm",
    "snap",
    "flatpakref",
    "app",
    "pkg",
    "mpkg",
    "dmg",
    "workflow",
    "scpt",
    "applescript",
    "terminal",
    "webloc",
    "kext",
    // Imágenes de disco que montan y ejecutan
    "iso",
    "img",
    "vhd",
    "vhdx",
    // Office con macros
    "docm",
    "dotm",
    "xlsm",
    "xltm",
    "xlam",
    "xlsb",
    "pptm",
    "potm",
    "ppam",
    "ppsm",
    "sldm",
    "accde",
    "mde",
];

/// ¿Abrir este adjunto puede ejecutar código? (pedir confirmación clara antes de abrirlo).
pub fn is_dangerous(name: &str, mime: &str) -> bool {
    let ext = extension(name);
    if DANGEROUS_EXT.contains(&ext.as_str()) {
        return true;
    }
    let m = mime.trim().to_ascii_lowercase();
    m.contains("x-msdownload")
        || m.contains("x-msdos-program")
        || m.contains("x-executable")
        || m.contains("x-sharedlib")
        || m.contains("x-sh")
        || m.contains("x-shellscript")
        || m.contains("java-archive")
        || m.contains("x-msi")
        || m.contains("x-ms-installer")
        || m.contains("macroenabled")
        || m.contains("x-desktop")
        || m.contains("x-apple-diskimage")
        || m.contains("vbscript")
        || m == "application/javascript"
        || m == "text/javascript"
}

/// ¿Imagen que se puede mostrar en miniatura?
pub fn is_image(name: &str, mime: &str) -> bool {
    let ext = extension(name);
    matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp")
        || matches!(
            mime.trim().to_ascii_lowercase().as_str(),
            "image/png" | "image/jpeg" | "image/jpg" | "image/gif" | "image/webp" | "image/bmp"
        )
}

/// Carpeta de la caché de adjuntos: `<caché del usuario>/iuredesk/attachments` (con
/// `IUREDESK_DATA_DIR`, dentro de esa carpeta, para no tocar la instalación normal).
pub fn cache_root() -> PathBuf {
    if let Some(d) = std::env::var_os("IUREDESK_DATA_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(d).join("cache").join("attachments");
    }
    match dirs::cache_dir() {
        Some(c) => c.join("iuredesk").join("attachments"),
        // Sin carpeta de caché: la temporal, con la carpeta privada y su comprobación de dueño.
        None => std::env::temp_dir().join("iuredesk-attachments"),
    }
}

/// Crea (si falta) una carpeta solo para el usuario. Si ya existe y es de otro (o un enlace), falla.
fn private_dir(p: &Path) -> Result<(), AttachError> {
    if let Ok(m) = std::fs::symlink_metadata(p)
        && (m.file_type().is_symlink() || !m.is_dir())
    {
        return Err(AttachError::Io(format!("{} no es una carpeta", p.display())));
    }
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(p).map_err(io)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Si la carpeta ya estaba (otro dueño, otros permisos), esto la cierra o falla.
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o700)).map_err(io)?;
    }
    Ok(())
}

/// Escribe `bytes` como archivo privado (Unix: `0600`) sin seguir un enlace que hubiera en su lugar.
fn write_private_file(path: &Path, bytes: &[u8]) -> Result<(), AttachError> {
    if let Ok(m) = std::fs::symlink_metadata(path) {
        if m.is_dir() {
            return Err(AttachError::Io(format!("{} es una carpeta", path.display())));
        }
        std::fs::remove_file(path).map_err(io)?;
    }
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o.open(path).map_err(io)?;
    std::io::Write::write_all(&mut f, bytes).map_err(io)?;
    f.sync_all().map_err(io)?;
    Ok(())
}

/// Escribe el adjunto en `<root>/<id del mensaje>/<nombre saneado>` y devuelve la ruta. La
/// carpeta y el archivo son privados del usuario.
pub fn write_private(root: &Path, msg: MessageId, name: &str, bytes: &[u8]) -> Result<PathBuf, AttachError> {
    if bytes.len() > MAX_ATTACHMENT_BYTES {
        return Err(AttachError::TooLarge);
    }
    private_dir(root)?;
    let dir = root.join(msg.get().to_string());
    private_dir(&dir)?;
    let path = dir.join(sanitize_filename(name));
    write_private_file(&path, bytes)?;
    Ok(path)
}

/// Saca el adjunto del almacén y lo deja en la caché privada (para abrirlo).
pub fn export_to_cache(store: &Store, root: &Path, msg: MessageId, index: usize) -> Result<PathBuf, AttachError> {
    let a = extract_from_store(store, msg, index)?;
    write_private(root, msg, &a.name, &a.bytes)
}

/// Ruta libre en `dir` para `name` saneado: `nombre.pdf`, `nombre (2).pdf`… («Guardar todos»).
pub fn unique_in(dir: &Path, name: &str) -> PathBuf {
    let clean = sanitize_filename(name);
    let first = dir.join(&clean);
    if std::fs::symlink_metadata(&first).is_err() {
        return first;
    }
    let (stem, ext) = match clean.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (clean.clone(), String::new()),
    };
    for n in 2..1000 {
        let p = dir.join(format!("{stem} ({n}){ext}"));
        if std::fs::symlink_metadata(&p).is_err() {
            return p;
        }
    }
    dir.join(format!("{stem} ({}){ext}", std::process::id()))
}

/// Borra de la caché lo que tenga más de `max_age` (y las carpetas que queden vacías). Devuelve
/// cuántos archivos borró. Solo toca `<root>/<id>/<archivo>`.
pub fn prune(root: &Path, max_age: Duration, now: SystemTime) -> usize {
    let Ok(dirs) = std::fs::read_dir(root) else { return 0 };
    let mut removed = 0usize;
    let mut seen = 0usize;
    for d in dirs.flatten() {
        let Ok(t) = d.file_type() else { continue };
        if !t.is_dir() {
            continue;
        }
        let Ok(files) = std::fs::read_dir(d.path()) else { continue };
        let mut left = 0usize;
        for f in files.flatten() {
            seen += 1;
            if seen > MAX_PRUNE_ENTRIES {
                return removed;
            }
            let old = f
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|m| now.duration_since(m).ok())
                .is_some_and(|age| age > max_age);
            let is_file = f.file_type().is_ok_and(|t| t.is_file() || t.is_symlink());
            if old && is_file && std::fs::remove_file(f.path()).is_ok() {
                removed += 1;
            } else {
                left += 1;
            }
        }
        if left == 0 {
            let _ = std::fs::remove_dir(d.path());
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Partes ya en base64: (nombre, tipo, contenido base64).
    fn message_with(parts: &[(&str, &str, &str)]) -> Vec<u8> {
        let mut s = String::from(
            "From: a@example.com\r\nTo: b@example.com\r\nSubject: Adjuntos\r\nMIME-Version: 1.0\r\n\
             Content-Type: multipart/mixed; boundary=\"XX\"\r\n\r\n--XX\r\nContent-Type: text/plain\r\n\r\nHola\r\n",
        );
        for (name, mime, b64) in parts {
            s.push_str(&format!(
                "--XX\r\nContent-Type: {mime}; name=\"{name}\"\r\nContent-Disposition: attachment; \
                 filename=\"{name}\"\r\nContent-Transfer-Encoding: base64\r\n\r\n{b64}\r\n"
            ));
        }
        s.push_str("--XX--\r\n");
        s.into_bytes()
    }

    #[test]
    fn extracts_by_index_and_decodes() {
        let raw = message_with(&[
            ("factura.pdf", "application/pdf", "JVBERi0xLjQgaG9sYQ=="),
            ("foto.png", "image/png", "iVBORw=="),
        ]);
        let a = extract(&raw, 0).unwrap();
        assert_eq!(
            (a.name.as_str(), a.mime.as_str(), a.bytes.as_slice()),
            ("factura.pdf", "application/pdf", &b"%PDF-1.4 hola"[..])
        );
        let b = extract(&raw, 1).unwrap();
        assert_eq!((b.name.as_str(), b.bytes.as_slice()), ("foto.png", &[0x89, b'P', b'N', b'G'][..]));
        assert_eq!(extract(&raw, 2), Err(AttachError::NotFound));
        assert_eq!(extract(b"basura sin mime", 0), Err(AttachError::NotFound));
        assert_eq!(extract_all(&raw, 10).len(), 2);
        assert_eq!(extract_all(&raw, 1).len(), 1);
        // El orden coincide con el que muestra el panel de lectura.
        let body = crate::body::parse(&raw);
        let names: Vec<_> = body.attachments.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["factura.pdf", "foto.png"]);
    }

    #[test]
    fn sanitizes_hostile_names() {
        assert_eq!(sanitize_filename("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_filename("C:\\Windows\\system32\\evil.dll"), "evil.dll");
        assert_eq!(sanitize_filename("..."), "adjunto");
        assert_eq!(sanitize_filename(""), "adjunto");
        assert_eq!(sanitize_filename(".bashrc"), "bashrc");
        assert_eq!(sanitize_filename("a<b>c:d\"e|f?g*h.txt"), "a_b_c_d_e_f_g_h.txt");
        assert_eq!(sanitize_filename("linea\nnueva\u{0}.txt"), "linea_nueva_.txt");
        // RLO: «factura<RLO>fdp.exe» se vería «factura.exe.pdf»; sin él queda la extensión real.
        let spoof = sanitize_filename("factura\u{202E}fdp.exe");
        assert_eq!(spoof, "facturafdp.exe");
        assert!(is_dangerous("factura\u{202E}fdp.exe", "application/pdf"));
        assert_eq!(sanitize_filename("CON.txt"), "_CON.txt");
        assert_eq!(sanitize_filename("com1"), "_com1");
        assert_eq!(sanitize_filename("COM0.txt"), "COM0.txt");
        assert_eq!(sanitize_filename("  informe final.pdf. "), "informe final.pdf");
        let long = format!("{}.pdf", "ñ".repeat(200));
        let s = sanitize_filename(&long);
        assert!(s.len() <= MAX_NAME_BYTES && s.ends_with(".pdf"), "{s}");
    }

    #[test]
    fn dangerous_types_need_confirmation() {
        for n in [
            "setup.exe",
            "a.BAT",
            "x.cmd",
            "run.js",
            "m.vbs",
            "i.sh",
            "Thing.app",
            "lib.jar",
            "p.msi",
            "s.scr",
            "p.ps1",
            "launcher.desktop",
            "budget.xlsm",
            "letter.docm",
            "deck.pptm",
            "disk.dmg",
        ] {
            assert!(is_dangerous(n, "application/octet-stream"), "{n}");
        }
        for n in ["factura.pdf", "foto.JPG", "notas.txt", "contrato.docx", "tabla.xlsx", "datos.csv", "a.zip"] {
            assert!(!is_dangerous(n, "application/octet-stream"), "{n}");
        }
        assert!(is_dangerous("documento", "application/x-msdownload"));
        assert!(is_dangerous("x", "application/vnd.ms-excel.sheet.macroEnabled.12"));
        assert!(is_dangerous("x", "application/x-sh"));
        assert!(is_image("foto.jpeg", "") && is_image("x", "image/png") && !is_image("x.svg", "image/svg+xml"));
    }

    #[test]
    fn writes_private_files_in_a_private_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("cache").join("attachments");
        let p = write_private(&root, MessageId(42), "../../informe.pdf", b"hola").unwrap();
        assert_eq!(p, root.join("42").join("informe.pdf"));
        assert_eq!(std::fs::read(&p).unwrap(), b"hola");
        // Se puede volver a abrir (sobrescribe).
        let p2 = write_private(&root, MessageId(42), "informe.pdf", b"otra").unwrap();
        assert_eq!(std::fs::read(&p2).unwrap(), b"otra");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&root), 0o700);
            assert_eq!(mode(&root.join("42")), 0o700);
            assert_eq!(mode(&p2), 0o600);
            // Un enlace plantado en lugar del archivo no se sigue: se reemplaza.
            let target = tmp.path().join("victima.txt");
            std::fs::write(&target, b"intacto").unwrap();
            let link = root.join("42").join("x.txt");
            std::os::unix::fs::symlink(&target, &link).unwrap();
            write_private(&root, MessageId(42), "x.txt", b"nuevo").unwrap();
            assert_eq!(std::fs::read(&target).unwrap(), b"intacto");
            assert!(!std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        }
    }

    #[test]
    fn unique_names_and_prune() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        assert_eq!(unique_in(d, "a.pdf"), d.join("a.pdf"));
        std::fs::write(d.join("a.pdf"), b"1").unwrap();
        assert_eq!(unique_in(d, "a.pdf"), d.join("a (2).pdf"));
        std::fs::write(d.join("a (2).pdf"), b"1").unwrap();
        assert_eq!(unique_in(d, "a.pdf"), d.join("a (3).pdf"));

        let root = d.join("cache");
        write_private(&root, MessageId(1), "viejo.txt", b"x").unwrap();
        let later = SystemTime::now() + Duration::from_secs(8 * 24 * 3600);
        assert_eq!(prune(&root, CACHE_MAX_AGE, SystemTime::now()), 0, "reciente: se queda");
        assert_eq!(prune(&root, CACHE_MAX_AGE, later), 1);
        assert!(!root.join("1").exists(), "carpeta vacía borrada");
        assert_eq!(prune(&d.join("no-existe"), CACHE_MAX_AGE, later), 0);
    }

    #[test]
    fn not_downloaded_is_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path()).unwrap();
        assert_eq!(extract_from_store(&store, MessageId(999), 0), Err(AttachError::NotFound));
    }

    proptest::proptest! {
        #[test]
        fn sanitized_names_are_always_safe(name in ".{0,300}") {
            let s = sanitize_filename(&name);
            proptest::prop_assert!(!s.is_empty());
            proptest::prop_assert!(s.len() <= MAX_NAME_BYTES + "adjunto".len());
            proptest::prop_assert!(!s.contains('/') && !s.contains('\\') && !s.contains('\0'));
            proptest::prop_assert!(!s.starts_with('.') && !s.chars().any(|c| c.is_control() || is_invisible(c)));
            proptest::prop_assert!(s != ".." && s != ".");
        }
    }
}
