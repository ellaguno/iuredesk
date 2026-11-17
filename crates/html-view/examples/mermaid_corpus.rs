//! S1.21: PNG de cada diagrama del corpus (`tests/mermaid/*.mmd`), en claro, para revisarlos a ojo
//! (y, cuando existan, compararlos con las referencias de `mermaid.js`).
//! Uso: `cargo run -p iuredesk-html-view --example mermaid_corpus -- <carpeta>`.
use iuredesk_html_view::diagram;
use iuredesk_html_view::markdown::NoteStyle;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out = std::path::PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| ".".into()));
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/mermaid");
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_none_or(|e| e != "mmd") {
            continue;
        }
        let code = std::fs::read_to_string(&path)?;
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("diagrama").to_string();
        match diagram::render(&code, &NoteStyle::default()) {
            Ok(svg) => {
                let r = diagram::rasterize(&svg, 1.5, [255, 255, 255])?;
                let png = diagram::encode_png(r.width, r.height, &r.rgba).ok_or("png")?;
                std::fs::write(out.join(format!("{stem}.png")), png)?;
            }
            Err(e) => eprintln!("{stem}: {}", e.es),
        }
    }
    Ok(())
}
