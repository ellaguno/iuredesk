//! Abrir enlaces y lanzar programas. Un rasgo para que las pruebas no abran nada de verdad.

use std::path::Path;
use std::process::Command;

/// Quien abre los deep links y lanza las apps.
pub trait Opener: Send + Sync {
    /// Abre un enlace (`iureditor://…`, `https://…`) con el manejador del SO.
    fn open_url(&self, url: &str) -> std::io::Result<()>;
    /// Lanza un programa con argumentos, sin esperar a que termine.
    fn launch(&self, program: &Path, args: &[String]) -> std::io::Result<()>;
}

/// El del sistema: `open` (crate en Rust puro) para enlaces; `Command` para lanzar.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemOpener;

impl Opener for SystemOpener {
    fn open_url(&self, url: &str) -> std::io::Result<()> {
        open::that_detached(url)
    }

    fn launch(&self, program: &Path, args: &[String]) -> std::io::Result<()> {
        let is_bundle = program.extension().is_some_and(|e| e.eq_ignore_ascii_case("app"));
        if cfg!(target_os = "macos") && is_bundle {
            let mut c = Command::new("open");
            c.arg("-a").arg(program);
            if !args.is_empty() {
                c.arg("--args").args(args);
            }
            c.spawn().map(drop)
        } else {
            Command::new(program).args(args).spawn().map(drop)
        }
    }
}
