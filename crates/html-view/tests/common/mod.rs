//! Utilidades compartidas por las pruebas y el ejemplo `render_corpus`.
//!
//! El corpus son los `.html` de `tests/corpus/`, más un correo sintético de ~2 MB generado aquí
//! (para no versionar 2 MB), más lo que el dueño deje en `tests/corpus/private/` (ignorado por git):
//! `.html`/`.htm` (el cuerpo HTML del correo). Los `.eml` llegarán cuando `engine` tenga el análisis
//! MIME (mail-parser 1.0 exige rustc 1.98).
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use iuredesk_html_view::Bytes;

pub struct Case {
    pub name: String,
    pub html: String,
    /// Recursos `cid:` que trae el propio caso (reservado para .eml privados).
    pub inline: HashMap<String, Bytes>,
}

pub fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus")
}

pub fn corpus() -> Vec<Case> {
    let mut cases = Vec::new();
    for path in sorted_files(&corpus_dir()) {
        if path.extension().is_some_and(|e| e == "html") {
            cases.push(Case { name: stem(&path), html: read_lossy(&path), inline: HashMap::new() });
        }
    }
    cases.push(Case { name: "13-huge-2mb (generado)".to_owned(), html: huge_email(), inline: HashMap::new() });
    cases.extend(private_cases());
    cases.sort_by(|a, b| a.name.cmp(&b.name));
    cases
}

fn private_cases() -> Vec<Case> {
    let dir = corpus_dir().join("private");
    let mut cases = Vec::new();
    for path in sorted_files(&dir) {
        let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        let name = format!("private/{}", stem(&path));
        match ext.as_str() {
            "html" | "htm" => cases.push(Case { name, html: read_lossy(&path), inline: HashMap::new() }),
            _ => {}
        }
    }
    cases
}

fn sorted_files(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.is_file()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

fn stem(p: &Path) -> String {
    p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

fn read_lossy(p: &Path) -> String {
    String::from_utf8_lossy(&std::fs::read(p).unwrap_or_default()).into_owned()
}

/// Boletín/resumen sintético de ~2 MB: miles de filas de tabla con estilos en línea y enlaces.
pub fn huge_email() -> String {
    let mut s = String::with_capacity(2_200_000);
    s.push_str("<html><body style=\"margin:0;background:#eee;font-family:Arial,sans-serif;\"><table width=\"640\" align=\"center\" cellpadding=\"0\" cellspacing=\"0\" style=\"background:#fff;\">");
    s.push_str("<tr><td style=\"padding:20px;font-size:22px;font-weight:bold;color:#234;\">Resumen de actividad (correo de 2 MB)</td></tr>");
    let mut i = 0u32;
    while s.len() < 2_000_000 {
        i += 1;
        s.push_str(&format!(
            "<tr><td style=\"padding:6px 20px;border-bottom:1px solid #eee;font-size:13px;line-height:18px;color:#333;\">\
             <table width=\"100%\" cellpadding=\"0\" cellspacing=\"0\"><tr>\
             <td width=\"70\" valign=\"top\" style=\"color:#999;font-size:11px;\">#{i:05}</td>\
             <td valign=\"top\"><a href=\"https://app.example.com/items/{i}?ref=digest&amp;u=42\" style=\"color:#1565c0;text-decoration:none;font-weight:bold;\">Movimiento {i} en el expediente {exp}/2026</a><br>\
             <span style=\"color:#666;\">Se registró una actuación con folio F-{folio:08} por parte del usuario operador-{op}. \
             Notas: revisión de documentos, acuerdo de trámite y turno a ponencia.</span></td>\
             <td width=\"90\" align=\"right\" valign=\"top\" style=\"font-family:monospace;color:#2e7d32;\">${amt}.00</td>\
             </tr></table></td></tr>",
            exp = 1000 + i % 977,
            folio = i.wrapping_mul(7919) % 100_000_000,
            op = i % 37,
            amt = (i * 37) % 9000,
        ));
    }
    s.push_str("</table></body></html>");
    s
}

/// Imágenes sintéticas para cada `cid:` del HTML. Si el Content-ID contiene `-<ancho>x<alto>@`,
/// se genera un PNG de ese tamaño; `*.svg@*` produce un SVG.
pub fn synth_inline_resources(html: &str) -> HashMap<String, Bytes> {
    let mut map = HashMap::new();
    let mut rest = html;
    while let Some(i) = rest.find("cid:") {
        rest = &rest[i + 4..];
        let end = rest
            .find(|c: char| c == '"' || c == '\'' || c == ')' || c.is_whitespace() || c == '>')
            .unwrap_or(rest.len());
        let cid = &rest[..end];
        if cid.is_empty() || map.contains_key(cid) || cid.starts_with("no-existe") || cid == "roto" || cid == "none" {
            continue;
        }
        let bytes = if cid.contains(".svg@") {
            Bytes::from_static(br##"<svg xmlns="http://www.w3.org/2000/svg" width="120" height="40" viewBox="0 0 120 40"><rect width="120" height="40" rx="6" fill="#264653"/><circle cx="20" cy="20" r="10" fill="#e9c46a"/><rect x="38" y="14" width="70" height="12" rx="3" fill="#2a9d8f"/></svg>"##)
        } else {
            let (w, h) = size_hint(cid).unwrap_or((64, 64));
            Bytes::from(synth_png(w, h, cid))
        };
        map.insert(cid.to_owned(), bytes);
    }
    map
}

fn size_hint(cid: &str) -> Option<(u32, u32)> {
    let before_at = cid.split('@').next()?;
    let dims = before_at.rsplit('-').next()?;
    let (w, h) = dims.split_once('x')?;
    Some((w.parse().ok()?, h.parse().ok()?))
}

/// PNG con degradado, marco y diagonales, para ver escalado y recorte.
pub fn synth_png(w: u32, h: u32, seed: &str) -> Vec<u8> {
    let hash = seed.bytes().fold(2166136261u32, |a, b| (a ^ u32::from(b)).wrapping_mul(16777619));
    let (r0, g0, b0) = ((hash & 0xff) as u8, ((hash >> 8) & 0xff) as u8, ((hash >> 16) & 0xff) as u8);
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let border = x < 3 || y < 3 || x + 3 >= w || y + 3 >= h;
            let diag = (x + y) % 40 < 2 || (x + h - y) % 40 < 2;
            let t = x as f32 / w.max(1) as f32;
            let px = if border {
                [20, 20, 20, 255]
            } else if diag {
                [255, 255, 255, 255]
            } else {
                [
                    (f32::from(r0) * (1.0 - t) + 40.0 * t) as u8,
                    (f32::from(g0) * (1.0 - t) + 120.0 * t) as u8,
                    (f32::from(b0) * (1.0 - t) + 200.0 * t) as u8,
                    255,
                ]
            };
            rgba.extend_from_slice(&px);
        }
    }
    encode_png(w, h, &rgba)
}

pub fn encode_png(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::Fast);
        let mut writer = enc.write_header().expect("cabecera PNG");
        writer.write_image_data(rgba).expect("datos PNG");
    }
    out
}
