//! S1.19: revisión visual de los mapas mentales. Escribe, por cada distribución y forma, el SVG y
//! un PNG de la nota pintada con Blitz (el mismo camino que la vista previa de IureDesk).
//! Uso: `cargo run -p iuredesk-html-view --example mindmap_shots -- <carpeta>`.
use iuredesk_html_view::markdown::{self, NoteOptions};
use iuredesk_html_view::mindmap::{self, Layout, Palette, RootSize, Shape};
use iuredesk_html_view::{Bytes, RenderOptions, render};

const MD: &str = "> [!MINDMAP|map SHAPE] Lanzamiento de IureDesk\n> - Correo\n>   - IMAP y SMTP\n>   - Microsoft 365\n>   - Gmail\n> - Calendario\n>   - Invitaciones\n> - Notas\n>   - Mapas mentales\n>   - Mermaid\n> - Contactos\n> - Tareas\n>   - Hoy\n";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::path::PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| ".".into()));
    for l in Layout::ALL {
        for s in [Shape::Default, Shape::Cloud, Shape::Hexagon, Shape::Bang, Shape::Circle, Shape::Square] {
            let md = MD.replace("SHAPE", s.token());
            let opts = NoteOptions { mindmap_layout: l, ..Default::default() };
            let note = markdown::to_html(&md, &opts);
            let Some(block) = mindmap::find(&md).into_iter().next() else { continue };
            let svg = mindmap::draw(&block.root, &opts.style, &block.options.look(l));
            std::fs::write(dir.join(format!("{l:?}-{s:?}.svg")), svg)?;
            let mut ro = RenderOptions { viewport_width: 1100, ..Default::default() };
            for (cid, svg) in &note.diagrams {
                ro.inline_resources.insert(cid.clone(), Bytes::from(svg.clone()));
            }
            let r = render(&note.html, &ro)?;
            let file = std::fs::File::create(dir.join(format!("{l:?}-{s:?}.png")))?;
            let mut enc = png::Encoder::new(std::io::BufWriter::new(file), r.width, r.height);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            enc.write_header()?.write_image_data(&r.rgba)?;
        }
    }
    // S1.20: centro largo en cada tamaño (radial) y cada paleta (a ambos lados).
    let long = MD.replace(
        "map SHAPE] Lanzamiento de IureDesk",
        "map ROOT PAL] Lanzamiento de IureDesk para despachos jurídicos en México",
    );
    let shots =
        RootSize::ALL.into_iter().map(|r| (format!("root-{r:?}"), r.token().unwrap_or(""), "layout-radial")).chain(
            Palette::ALL.into_iter().map(|p| (format!("palette-{p:?}"), "root-m", p.token().unwrap_or("layout-both"))),
        );
    for (name, root, pal) in shots {
        let md =
            long.replace("ROOT", root).replace("PAL", if pal.starts_with("palette") { pal } else { "layout-radial" });
        let md = if pal.starts_with("palette") { md.replace("|map ", "|map layout-both ") } else { md };
        let note = markdown::to_html(&md, &NoteOptions::default());
        let mut ro = RenderOptions { viewport_width: 1300, ..Default::default() };
        for (cid, svg) in &note.diagrams {
            ro.inline_resources.insert(cid.clone(), Bytes::from(svg.clone()));
        }
        let r = render(&note.html, &ro)?;
        let file = std::fs::File::create(dir.join(format!("{name}.png")))?;
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), r.width, r.height);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header()?.write_image_data(&r.rgba)?;
    }
    Ok(())
}
