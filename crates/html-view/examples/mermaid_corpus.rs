//! S1.21: PNG de cada diagrama del corpus (`tests/mermaid/*.mmd`), en claro, para revisarlos a ojo.
//! Con `--compare`, cada PNG lleva al lado la referencia de `mermaid.js`
//! (`tests/mermaid/ref/*.png`, hecha con `scripts/mermaid-refs`).
//! Uso: `cargo run -p iuredesk-html-view --example mermaid_corpus -- <carpeta> [--compare]`.
use iuredesk_html_view::diagram;
use iuredesk_html_view::markdown::NoteStyle;
use iuredesk_html_view::{Bytes, RenderOptions, render};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let compare = args.iter().any(|a| a == "--compare");
    let out =
        std::path::PathBuf::from(args.iter().find(|a| !a.starts_with("--")).cloned().unwrap_or_else(|| ".".into()));
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/mermaid");
    let mut files: Vec<_> = std::fs::read_dir(&dir)?.filter_map(Result::ok).map(|e| e.path()).collect();
    files.sort();
    for path in files {
        if path.extension().is_none_or(|e| e != "mmd") {
            continue;
        }
        let code = std::fs::read_to_string(&path)?;
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("diagrama").to_string();
        let svg = match diagram::render(&code, &NoteStyle::default()) {
            Ok(svg) => svg,
            Err(e) => {
                eprintln!("{stem}: {}", e.es);
                continue;
            }
        };
        let reference = std::fs::read(dir.join("ref").join(format!("{stem}.png"))).ok();
        let png = match (compare, reference) {
            (true, Some(r)) => {
                // Lado a lado: el nuestro y el de mermaid.js, a la misma altura de lectura.
                let html = "<!doctype html><html><head><style>body{margin:0;font-family:sans-serif;background:#fff}\
                    table{border-collapse:collapse}td{vertical-align:top;padding:12px;border-right:1px solid #ddd}\
                    h3{margin:0 0 8px 0;font-size:14px;color:#555}img{max-width:760px}</style></head><body><table><tr>\
                    <td><h3>IureDesk (mermaid-svg)</h3><img src=\"cid:ours\"></td>\
                    <td><h3>mermaid.js 11.16</h3><img src=\"cid:ref\" style=\"width:auto;max-height:900px\"></td>\
                    </tr></table></body></html>";
                let mut ro = RenderOptions { viewport_width: 1600, max_height: 2400, ..Default::default() };
                ro.inline_resources.insert("ours".into(), Bytes::from(svg.into_bytes()));
                ro.inline_resources.insert("ref".into(), Bytes::from(r));
                let r = render(html, &ro)?;
                diagram::encode_png(r.width, r.height, &r.rgba).ok_or("png")?
            }
            _ => {
                let r = diagram::rasterize(&svg, 1.5, [255, 255, 255])?;
                diagram::encode_png(r.width, r.height, &r.rgba).ok_or("png")?
            }
        };
        std::fs::write(out.join(format!("{stem}.png")), png)?;
    }
    Ok(())
}
