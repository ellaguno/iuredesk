//! Importa uno o varios orígenes en un almacén y muestra el progreso y el informe.
//!
//! ```text
//! cargo run -p iuredesk-import --example import -- <ruta>... --store /tmp/x [--en] [--fresh] [--tz Europe/Madrid]
//! cargo run -p iuredesk-import --example import -- --detect
//! ```

use std::path::PathBuf;

use iuredesk_import::{ImportEvent, ImportOptions, Importer, find_installed};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut paths = Vec::new();
    let mut store_dir: Option<PathBuf> = None;
    let mut opts = ImportOptions::default();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--store" => store_dir = args.next().map(PathBuf::from),
            "--en" => opts.lang = "en".into(),
            "--fresh" => opts.resume = false,
            "--tz" => {
                if let Some(tz) = args.next() {
                    opts.time_zone = tz.parse().map_err(|e| format!("zona: {e}"))?;
                }
            }
            "--detect" => {
                for s in find_installed() {
                    println!("{:?}\t{}\t{} MB\t{}", s.kind, s.label, s.bytes / (1024 * 1024), s.path.display());
                }
                return Ok(());
            }
            _ => paths.push(PathBuf::from(a)),
        }
    }
    let (Some(store_dir), false) = (store_dir, paths.is_empty()) else {
        eprintln!("uso: import <ruta>... --store <dir> [--en] [--fresh] [--tz Zona] | --detect");
        std::process::exit(2);
    };
    let lang = opts.lang.clone();
    let store = iuredesk_store::Store::open(&store_dir)?;
    let started = std::time::Instant::now();
    let report = Importer::new(&store).options(opts).run(&paths, |ev| match ev {
        ImportEvent::Progress(p) => {
            let pct = p.fraction().map(|f| format!("{:5.1}%", f * 100.0)).unwrap_or_else(|| "  ?  ".into());
            eprint!(
                "\r{pct}  {} mensajes, {} contactos, {} eventos  {:<60.60}",
                p.counts.messages, p.counts.contacts, p.counts.events, p.current
            );
        }
        ImportEvent::Skipped(s) => eprintln!("\nomitido: {} — {}", s.location, s.reason.human(&lang)),
        ImportEvent::Notice(n) => eprintln!("\naviso: {}", n.human(&lang)),
        ImportEvent::SourceStarted { kind, path } => eprintln!("\n{} → {}", kind.label(&lang), path.display()),
    })?;
    eprintln!();
    println!("{}", report.summary(&lang));
    println!("{:.1} s", started.elapsed().as_secs_f64());
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
