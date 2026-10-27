//! `cargo xtask icons`: rasteriza el icono de la app (`assets/icons/iuredesk.svg`, dibujado por el
//! proyecto) a PNG de 16 a 1024 px, `.ico` (Windows) y `.icns` (macOS), todo en Rust puro (resvg,
//! image, icns). Para 32 px o menos usa `iuredesk-small.svg`, una versión simplificada que se lee
//! a ese tamaño. Los archivos generados se versionan: el empaquetado no necesita regenerarlos.

use std::path::Path;

use crate::{Res, root};

/// Tamaños PNG (hicolor de freedesktop, Windows y macOS).
pub const SIZES: &[u32] = &[16, 24, 32, 48, 64, 128, 256, 512, 1024];
/// Tamaños dentro del `.ico` (el de 256 se guarda como PNG, que Windows Vista+ acepta).
const ICO_SIZES: &[u32] = &[16, 24, 32, 48, 64, 128, 256];
/// Tamaños dentro del `.icns` (icp4…ic10; 1024 = 512@2x).
const ICNS_SIZES: &[u32] = &[16, 32, 64, 128, 256, 512, 1024];
/// Hasta este tamaño se usa la versión simplificada.
const SMALL_MAX: u32 = 32;

pub fn icons() -> Res {
    let dir = root().join("assets/icons");
    let big = load(&dir.join("iuredesk.svg"))?;
    let small = load(&dir.join("iuredesk-small.svg"))?;
    let mut ico_frames = Vec::new();
    let mut icns = icns::IconFamily::new();
    for &size in SIZES {
        let tree = if size <= SMALL_MAX { &small } else { &big };
        let rgba = render(tree, size)?;
        let png = encode_png(&rgba, size)?;
        let path = dir.join(format!("iuredesk-{size}.png"));
        std::fs::write(&path, &png).map_err(|e| format!("{}: {e}", path.display()))?;
        if ICO_SIZES.contains(&size) {
            ico_frames.push(
                image::codecs::ico::IcoFrame::with_encoded(png.clone(), size, size, image::ExtendedColorType::Rgba8)
                    .map_err(|e| format!("ico {size}: {e}"))?,
            );
        }
        if ICNS_SIZES.contains(&size) {
            let img = icns::Image::from_data(icns::PixelFormat::RGBA, size, size, rgba)
                .map_err(|e| format!("icns {size}: {e}"))?;
            icns.add_icon(&img).map_err(|e| format!("icns {size}: {e}"))?;
        }
    }
    let ico_path = dir.join("iuredesk.ico");
    let file = std::fs::File::create(&ico_path).map_err(|e| format!("{}: {e}", ico_path.display()))?;
    image::codecs::ico::IcoEncoder::new(std::io::BufWriter::new(file))
        .encode_images(&ico_frames)
        .map_err(|e| format!("ico: {e}"))?;
    let icns_path = dir.join("iuredesk.icns");
    let file = std::fs::File::create(&icns_path).map_err(|e| format!("{}: {e}", icns_path.display()))?;
    icns.write(std::io::BufWriter::new(file)).map_err(|e| format!("icns: {e}"))?;
    println!("icons: {} PNG, iuredesk.ico, iuredesk.icns → {}", SIZES.len(), dir.display());
    Ok(())
}

fn load(path: &Path) -> Result<resvg::usvg::Tree, String> {
    let data = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    resvg::usvg::Tree::from_data(&data, &resvg::usvg::Options::default())
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// RGBA sin premultiplicar de `size`×`size`.
fn render(tree: &resvg::usvg::Tree, size: u32) -> Result<Vec<u8>, String> {
    let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size).ok_or("tamaño de icono inválido")?;
    let scale = size as f32 / tree.size().width().max(1.0);
    resvg::render(tree, resvg::tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    Ok(pixmap
        .pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect())
}

fn encode_png(rgba: &[u8], size: u32) -> Result<Vec<u8>, String> {
    use image::ImageEncoder;
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new_with_quality(
        &mut out,
        image::codecs::png::CompressionType::Best,
        image::codecs::png::FilterType::Adaptive,
    )
    .write_image(rgba, size, size, image::ExtendedColorType::Rgba8)
    .map_err(|e| format!("png {size}: {e}"))?;
    Ok(out)
}
