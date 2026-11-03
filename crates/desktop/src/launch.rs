//! Abrir enlaces, archivos y programas **sin contagiarles el entorno del AppImage**.
//!
//! El AppImage de IureDesk arranca con `APPIMAGE`, `APPDIR`, `ARGV0`, `OWD`… en el entorno. Un
//! programa abierto desde aquí los hereda y algunos los toman por suyos: IureEditor (Tauri)
//! registra su manejador `iureditor://` con `Exec=$APPIMAGE`, así que el siguiente enlace abría
//! IureDesk en vez de IureEditor. Por eso todo lo que lanzamos pasa por aquí y se queda sin esas
//! variables (y sin las rutas de bibliotecas que el AppImage antepone).
//!
//! En Linux y los BSD se prueban los lanzadores de [`open::commands`] (`xdg-open`, `gio`…) en su
//! propio grupo de procesos; uno que sale con error en el primer segundo cede al siguiente, como
//! hace `open::that_detached`. En macOS y Windows no hay AppImage: se usa `open` tal cual.

use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;

/// Variables que pone el entorno de ejecución del AppImage (y `appimagetool`/linuxdeploy).
pub const APPIMAGE_VARS: &[&str] = &["APPIMAGE", "APPDIR", "ARGV0", "OWD", "APPIMAGE_UUID", "APPIMAGE_SILENT_INSTALL"];

/// Quita del comando las variables del AppImage. Si el AppImage antepuso su carpeta a
/// `LD_LIBRARY_PATH`/`PATH`/`XDG_DATA_DIRS`, se quitan esas entradas y se deja lo demás.
pub fn clean_env(cmd: &mut Command) -> &mut Command {
    for v in APPIMAGE_VARS {
        cmd.env_remove(v);
    }
    if let Some(appdir) = std::env::var_os("APPDIR").filter(|d| !d.is_empty()) {
        for var in ["LD_LIBRARY_PATH", "PATH", "XDG_DATA_DIRS", "GSETTINGS_SCHEMA_DIR"] {
            if let Some(val) = std::env::var_os(var) {
                match strip_dir(&val, &appdir) {
                    Some(kept) if kept.is_empty() => {
                        cmd.env_remove(var);
                    }
                    Some(kept) => {
                        cmd.env(var, kept);
                    }
                    None => {}
                }
            }
        }
    }
    cmd
}

/// Quita de una lista separada por `:` las entradas dentro de `dir`. `None` si no había ninguna.
fn strip_dir(list: &OsStr, dir: &OsStr) -> Option<String> {
    let list = list.to_str()?;
    let dir = dir.to_str()?.trim_end_matches('/');
    if dir.is_empty() {
        return None;
    }
    let inside = |e: &str| e == dir || e.strip_prefix(dir).is_some_and(|rest| rest.starts_with('/'));
    if !list.split(':').any(inside) {
        return None;
    }
    Some(list.split(':').filter(|e| !e.is_empty() && !inside(e)).collect::<Vec<_>>().join(":"))
}

/// Abre un enlace o archivo con la app del sistema, sin esperar a que se cierre.
pub fn open_detached(target: impl AsRef<OsStr>) -> std::io::Result<()> {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let mut last = std::io::Error::new(std::io::ErrorKind::NotFound, "no hay con qué abrirlo");
        for mut cmd in open::commands(target) {
            match spawn_checked(clean_env(&mut cmd)) {
                Ok(()) => return Ok(()),
                Err(e) => last = e,
            }
        }
        Err(last)
    }
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    {
        open::that_detached(target)
    }
}

/// Lanza un programa con argumentos, sin esperar a que termine y sin el entorno del AppImage.
pub fn spawn_program(program: &Path, args: &[String]) -> std::io::Result<()> {
    let mut cmd = Command::new(program);
    cmd.args(args);
    clean_env(&mut cmd);
    detach(&mut cmd);
    let child = cmd.spawn()?;
    reap(child);
    Ok(())
}

/// Entrada/salida a `/dev/null` y, en Unix, grupo de procesos propio (no recibe nuestras señales).
fn detach(cmd: &mut Command) {
    use std::process::Stdio;
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        cmd.process_group(0);
    }
}

/// Espera al hijo en un hilo para que no quede como zombi.
fn reap(mut child: std::process::Child) {
    let spawned = std::thread::Builder::new().name("launch-reap".into()).stack_size(64 * 1024).spawn(move || {
        let _ = child.wait();
    });
    // Sin hilo no se pierde nada: el sistema lo recoge al salir IureDesk.
    drop(spawned);
}

/// Lanza y mira el primer segundo: salir con error cuenta como fallo (para probar el siguiente).
#[cfg(all(unix, not(target_os = "macos")))]
fn spawn_checked(cmd: &mut Command) -> std::io::Result<()> {
    const CHECK: std::time::Duration = std::time::Duration::from_millis(1000);
    const STEP: std::time::Duration = std::time::Duration::from_millis(25);
    detach(cmd);
    let mut child = cmd.spawn()?;
    let start = std::time::Instant::now();
    while start.elapsed() < CHECK {
        if let Some(status) = child.try_wait()? {
            return if status.success() {
                Ok(())
            } else {
                Err(std::io::Error::other(format!("el lanzador terminó con {status}")))
            };
        }
        std::thread::sleep(STEP);
    }
    reap(child);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_only_entries_inside_the_appdir() {
        let dir = OsStr::new("/tmp/.mount_iureDk");
        assert_eq!(strip_dir(OsStr::new("/tmp/.mount_iureDk/usr/lib:/usr/lib"), dir).as_deref(), Some("/usr/lib"));
        assert_eq!(strip_dir(OsStr::new("/tmp/.mount_iureDk/usr/lib"), dir).as_deref(), Some(""));
        assert_eq!(strip_dir(OsStr::new("/usr/lib:/opt/x"), dir), None);
        // Un prefijo de nombre no es estar dentro.
        assert_eq!(strip_dir(OsStr::new("/tmp/.mount_iureDkX/lib"), dir), None);
        assert_eq!(strip_dir(OsStr::new("/usr/lib"), OsStr::new("/")), None);
    }

    #[test]
    fn children_never_see_appimage_vars() {
        // Regresión (dueño 2026-11-03): IureEditor heredaba `APPIMAGE` y registraba su manejador
        // de `iureditor://` apuntando al AppImage de IureDesk.
        let mut cmd = Command::new("env");
        clean_env(&mut cmd);
        let removed: Vec<_> = cmd.get_envs().filter(|(_, v)| v.is_none()).map(|(k, _)| k.to_owned()).collect();
        for v in APPIMAGE_VARS {
            assert!(removed.iter().any(|k| k == OsStr::new(v)), "{v} no se quita");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn spawned_program_runs_without_appimage_vars() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("env.txt");
        let script = format!("env > '{}'", out.display());
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(&script).env("APPIMAGE", "/x/iureDesk.AppImage");
        // `clean_env` gana sobre lo puesto antes en el mismo comando.
        clean_env(&mut cmd);
        assert!(cmd.status().unwrap().success());
        let env = std::fs::read_to_string(&out).unwrap();
        assert!(!env.lines().any(|l| l.starts_with("APPIMAGE=")), "{env}");
    }
}
