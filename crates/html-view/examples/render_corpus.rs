//! Pinta todo el corpus de correos a PNG y muestra tiempos.
//!
//! ```text
//! cargo run --release -p iuredesk-html-view --example render_corpus -- <dir_salida> [--width 800] [--scale 1] [--dark] [--only <texto>]
//! ```
//!
//! Incluye `tests/corpus/private/` si existe (correos reales del dueño; .html o .eml).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

#[path = "../tests/common/mod.rs"]
mod common;

use std::path::PathBuf;
use std::time::Instant;

use iuredesk_html_view::{RenderOptions, render, to_plain_text};

fn main() {
    let mut args = std::env::args().skip(1);
    let mut out: Option<PathBuf> = None;
    let mut opts = RenderOptions::default();
    let mut only: Option<String> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--width" => opts.viewport_width = args.next().and_then(|v| v.parse().ok()).unwrap_or(800),
            "--scale" => opts.scale = args.next().and_then(|v| v.parse().ok()).unwrap_or(1.0),
            "--dark" => opts.dark_mode = true,
            "--only" => only = args.next(),
            _ => out = Some(PathBuf::from(a)),
        }
    }
    let out = out.unwrap_or_else(|| {
        eprintln!("uso: render_corpus <dir_salida> [--width N] [--scale F] [--dark] [--only texto]");
        std::process::exit(2);
    });
    std::fs::create_dir_all(&out).expect("crear directorio de salida");

    let t = Instant::now();
    iuredesk_html_view::warm_up();
    println!("carga de fuentes del sistema: {:.0} ms", t.elapsed().as_secs_f64() * 1000.0);
    println!(
        "{:<34} {:>9} {:>11} {:>9} {:>6} {:>5} {:>4}  notas",
        "caso", "bytes", "tamaño px", "ms", "links", "bloq", "trc"
    );

    let mut total_ms = 0.0;
    let mut n = 0;
    for case in common::corpus() {
        if only.as_deref().is_some_and(|o| !case.name.contains(o)) {
            continue;
        }
        let mut o = opts.clone();
        o.inline_resources = common::synth_inline_resources(&case.html);
        o.inline_resources.extend(case.inline.clone());
        let t = Instant::now();
        let result = render(&case.html, &o);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        total_ms += ms;
        n += 1;
        let file = case.name.replace(['/', ' ', '(', ')'], "_");
        match result {
            Ok(r) => {
                let png = common::encode_png(r.width, r.height, &r.rgba);
                std::fs::write(out.join(format!("{file}.png")), png).expect("escribir PNG");
                println!(
                    "{:<34} {:>9} {:>11} {:>9.1} {:>6} {:>5} {:>4}  {}",
                    case.name,
                    case.html.len(),
                    format!("{}x{}", r.width, r.height),
                    ms,
                    r.links.len(),
                    r.blocked.len(),
                    if r.truncated { "sí" } else { "" },
                    if r.dark_applied { "oscuro" } else { "" },
                );
            }
            Err(e) => {
                std::fs::write(out.join(format!("{file}.txt")), to_plain_text(&case.html)).expect("escribir texto");
                println!("{:<34} {:>9} {:>11} {:>9.1}  ERROR: {e}", case.name, case.html.len(), "-", ms);
            }
        }
    }
    println!("{n} correos en {total_ms:.0} ms (media {:.1} ms)", total_ms / f64::from(n.max(1)));
}
