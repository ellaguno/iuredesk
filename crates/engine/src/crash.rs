//! Informe de fallos local (R3.3). Si algo entra en pánico (en el código propio no debería, pero
//! puede venir de una dependencia), el gancho deja un archivo de texto en `crash/` de la carpeta de
//! datos con la versión, el sistema, el hilo, el mensaje y el backtrace. En el siguiente arranque la
//! interfaz lo ofrece: copiar, abrir la carpeta o reportarlo en GitHub. **Nada se envía solo** y el
//! informe no lleva contenido de correos (solo lo que el pánico dice de sí mismo).
//!
//! Lo ya visto pasa a `crash/seen/` y se guardan como mucho [`MAX_KEPT`] informes.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// Repositorio donde se reportan los fallos.
pub const ISSUES_URL: &str = "https://github.com/ellaguno/iuredesk/issues/new";
/// Tope del tamaño de un informe (lo que pase se corta).
pub const MAX_REPORT_BYTES: usize = 64 * 1024;
/// Informes que se guardan (pendientes y vistos, cada grupo por separado).
pub const MAX_KEPT: usize = 10;
/// Tope de informes que se leen al arrancar.
const MAX_PENDING: usize = 5;
/// Tope del cuerpo del issue que va en la URL (el informe completo se pega a mano).
const MAX_ISSUE_BODY: usize = 1500;

/// Un informe que el usuario aún no ha visto.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CrashReport {
    pub path: PathBuf,
    pub text: String,
}

/// Lo que cuenta un pánico, ya separado del gancho (para probarlo sin entrar en pánico).
#[derive(Clone, Debug, Default)]
pub struct PanicFacts {
    pub message: String,
    pub location: Option<String>,
    pub thread: Option<String>,
    pub backtrace: String,
}

/// Instala el gancho de pánico: escribe el informe en `dir` y luego llama al gancho anterior
/// (que imprime en la terminal como siempre). Si no se puede escribir, el pánico sigue igual.
pub fn install(dir: PathBuf) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let message = if let Some(s) = info.payload().downcast_ref::<&str>() {
            (*s).to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "(sin mensaje)".to_string()
        };
        let facts = PanicFacts {
            message,
            location: info.location().map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column())),
            thread: std::thread::current().name().map(str::to_string),
            backtrace: std::backtrace::Backtrace::force_capture().to_string(),
        };
        let _ = write_report(&dir, &render(&facts, chrono::Utc::now()));
        previous(info);
    }));
}

/// Texto del informe.
pub fn render(f: &PanicFacts, at: chrono::DateTime<chrono::Utc>) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "IureDesk {}", env!("CARGO_PKG_VERSION"));
    let _ = writeln!(s, "Sistema: {} {} ({})", std::env::consts::OS, os_version(), std::env::consts::ARCH);
    let _ = writeln!(s, "Fecha: {}", at.format("%Y-%m-%d %H:%M:%S UTC"));
    let _ = writeln!(s, "Hilo: {}", f.thread.as_deref().unwrap_or("(sin nombre)"));
    let _ = writeln!(s, "Lugar: {}", f.location.as_deref().unwrap_or("(desconocido)"));
    let _ = writeln!(s, "Mensaje: {}", f.message);
    let _ = writeln!(s, "\nBacktrace:\n{}", f.backtrace);
    truncate_at_char(&mut s, MAX_REPORT_BYTES);
    s
}

/// Versión del sistema si se sabe sin bloquear (Linux: `/etc/os-release`).
fn os_version() -> String {
    #[cfg(target_os = "linux")]
    {
        if let Ok(t) = std::fs::read_to_string("/etc/os-release") {
            for l in t.lines().take(64) {
                if let Some(v) = l.strip_prefix("PRETTY_NAME=") {
                    return v.trim_matches('"').chars().take(80).collect();
                }
            }
        }
    }
    String::new()
}

/// Escribe un informe nuevo en `dir` (y recorta los viejos).
pub fn write_report(dir: &Path, text: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S%.3f");
    let mut path = dir.join(format!("crash-{stamp}.txt"));
    // Dos pánicos en el mismo milisegundo (dos hilos): no pisar el primero.
    for n in 1..100u32 {
        if !path.exists() {
            break;
        }
        path = dir.join(format!("crash-{stamp}-{n}.txt"));
    }
    let mut t = text.to_string();
    truncate_at_char(&mut t, MAX_REPORT_BYTES);
    std::fs::write(&path, t)?;
    prune(dir, MAX_KEPT);
    Ok(path)
}

/// Informes sin ver, del más nuevo al más viejo (como mucho unos pocos).
pub fn pending(dir: &Path) -> Vec<CrashReport> {
    let mut v: Vec<CrashReport> = reports_in(dir)
        .into_iter()
        .rev()
        .take(MAX_PENDING)
        .filter_map(|path| {
            let bytes = std::fs::read(&path).ok()?;
            let mut text = String::from_utf8_lossy(bytes.get(..MAX_REPORT_BYTES).unwrap_or(&bytes)).into_owned();
            truncate_at_char(&mut text, MAX_REPORT_BYTES);
            Some(CrashReport { path, text })
        })
        .collect();
    v.dedup_by(|a, b| a.path == b.path);
    v
}

/// Marca como vistos todos los informes pendientes (pasan a `seen/`).
pub fn acknowledge(dir: &Path) {
    let seen = dir.join("seen");
    if std::fs::create_dir_all(&seen).is_err() {
        return;
    }
    for p in reports_in(dir) {
        if let Some(name) = p.file_name() {
            let _ = std::fs::rename(&p, seen.join(name));
        }
    }
    prune(&seen, MAX_KEPT);
}

/// `crash-*.txt` de `dir`, ordenados por nombre (que lleva la fecha).
fn reports_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut v: Vec<PathBuf> = rd
        .filter_map(Result::ok)
        .take(1000)
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("crash-") && n.ends_with(".txt"))
        })
        .collect();
    v.sort();
    v
}

/// Deja solo los `keep` informes más nuevos de `dir`.
fn prune(dir: &Path, keep: usize) {
    let v = reports_in(dir);
    let extra = v.len().saturating_sub(keep);
    for p in v.into_iter().take(extra) {
        let _ = std::fs::remove_file(p);
    }
}

/// Enlace para abrir un issue con un resumen (versión, sistema, lugar y mensaje). El informe
/// completo no cabe en una URL: la interfaz lo copia al portapapeles para pegarlo. El usuario ve
/// el texto en GitHub antes de enviarlo.
pub fn issue_url(report: &CrashReport) -> String {
    let field = |key: &str| {
        report.text.lines().find_map(|l| l.strip_prefix(key)).map(str::trim).unwrap_or_default().to_string()
    };
    let version = report.text.lines().next().unwrap_or_default().to_string();
    let message = field("Mensaje:");
    let mut title = format!("Fallo: {message}");
    truncate_at_char(&mut title, 120);
    let mut body = format!(
        "{version}\nSistema: {}\nLugar: {}\nMensaje: {message}\n\n\
         ¿Qué estabas haciendo? / What were you doing?\n\n\n\
         Informe completo (ya está en tu portapapeles; pégalo aquí) / Full report (paste it here):\n",
        field("Sistema:"),
        field("Lugar:"),
    );
    truncate_at_char(&mut body, MAX_ISSUE_BODY);
    format!("{ISSUES_URL}?title={}&body={}", percent(&title), percent(&body))
}

/// Codificación de URL de un valor de consulta.
fn percent(s: &str) -> String {
    let mut out = String::with_capacity(s.len().saturating_mul(3));
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// Corta `s` a `max` bytes como mucho, sin partir un carácter.
fn truncate_at_char(s: &mut String, max: usize) {
    if s.len() <= max {
        return;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("iuredesk-crash-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn facts() -> PanicFacts {
        PanicFacts {
            message: "index out of bounds: ñandú".into(),
            location: Some("crates/x/src/lib.rs:10:5".into()),
            thread: Some("sync".into()),
            backtrace: "0: main".into(),
        }
    }

    #[test]
    fn render_has_the_facts() {
        let t = render(&facts(), chrono::DateTime::from_timestamp(0, 0).unwrap_or_default());
        assert!(t.starts_with(&format!("IureDesk {}", env!("CARGO_PKG_VERSION"))));
        assert!(t.contains("Hilo: sync"));
        assert!(t.contains("Lugar: crates/x/src/lib.rs:10:5"));
        assert!(t.contains("Mensaje: index out of bounds: ñandú"));
        assert!(t.contains("1970-01-01 00:00:00 UTC"));
        let big = PanicFacts { backtrace: "é".repeat(MAX_REPORT_BYTES), ..facts() };
        assert!(render(&big, chrono::Utc::now()).len() <= MAX_REPORT_BYTES);
    }

    #[test]
    fn write_pending_acknowledge_and_prune() {
        let d = tmp("cycle");
        assert!(pending(&d).is_empty());
        let p = write_report(&d, "IureDesk 1\nMensaje: a").unwrap();
        // Otro en el mismo instante no pisa el primero.
        let q = write_report(&d, "IureDesk 1\nMensaje: b").unwrap();
        assert_ne!(p, q);
        let v = pending(&d);
        assert_eq!(v.len(), 2);
        acknowledge(&d);
        assert!(pending(&d).is_empty());
        assert!(d.join("seen").join(p.file_name().unwrap()).exists());
        // Archivos ajenos no se tocan ni se leen.
        std::fs::write(d.join("notas.txt"), "x").unwrap();
        for i in 0..(MAX_KEPT + 5) {
            std::fs::write(d.join(format!("crash-2026{i:04}.txt")), "x").unwrap();
        }
        prune(&d, MAX_KEPT);
        assert_eq!(reports_in(&d).len(), MAX_KEPT);
        assert!(d.join("notas.txt").exists());
        assert!(pending(&d).len() <= MAX_PENDING);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn issue_url_is_encoded_and_bounded() {
        let text = render(&PanicFacts { message: "x&y=z #1 ".repeat(400), ..facts() }, chrono::Utc::now());
        let r = CrashReport { path: PathBuf::new(), text };
        let url = issue_url(&r);
        assert!(url.starts_with(ISSUES_URL));
        let query = url.split_once('?').map(|(_, q)| q).unwrap_or_default();
        assert!(!query.contains(' ') && !query.contains('#'));
        assert_eq!(query.matches('&').count(), 1);
        // Título ≤ 120 y cuerpo ≤ 1500 bytes antes de codificar: la URL queda bajo ~8 KB.
        assert!(url.len() < 8 * 1024, "{}", url.len());
        // El backtrace no va en la URL.
        assert!(!url.contains("Backtrace"));
    }

    #[test]
    fn hostile_files_are_bounded() {
        let d = tmp("hostile");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("crash-1.txt"), vec![0xFFu8; MAX_REPORT_BYTES * 3]).unwrap();
        let v = pending(&d);
        assert_eq!(v.len(), 1);
        assert!(v.first().is_some_and(|r| r.text.len() <= MAX_REPORT_BYTES));
        let _ = std::fs::remove_dir_all(&d);
    }
}
