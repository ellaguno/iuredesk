//! S1.21: corpus de fidelidad de los diagramas Mermaid más usados en notas (flujo, secuencia,
//! Gantt, mapa mental, pastel, clases, ER, línea de tiempo).
//!
//! Cada uno debe dibujarse, conservar sus etiquetas, salir seguro y con tamaño fijo, en claro y en
//! oscuro. La comparación visual contra `mermaid.js` (capturas de referencia hechas en desarrollo
//! con un navegador sin pantalla) está pendiente; mientras tanto, `cargo run -p iuredesk-html-view
//! --example mermaid_corpus -- <carpeta>` deja los PNG para revisarlos a ojo.

use iuredesk_html_view::diagram;
use iuredesk_html_view::markdown::NoteStyle;

/// (archivo, etiquetas que deben aparecer).
const CASES: &[(&str, &[&str])] = &[
    ("flowchart.mmd", &["Solicitud del cliente", "¿Hay conflicto?", "Asignar abogado", "Despacho"]),
    ("sequence.mmd", &["Cliente", "Abogada", "Envía el contrato", "Revisión en 48 h"]),
    ("gantt.mmd", &["Plan del caso", "Recabar pruebas", "Peritaje", "Audiencia"]),
    ("mindmap.mmd", &["Lanzamiento", "Correo", "Microsoft 365", "Mermaid"]),
    ("pie.mmd", &["Horas por área", "Litigio", "Fiscal"]),
    ("class.mmd", &["Expediente", "Cliente", "numero"]),
    ("er.mmd", &["CLIENTE", "EXPEDIENTE", "DOCUMENTO"]),
    ("timeline.mmd", &["Historia del caso", "Demanda", "Sentencia"]),
];

fn code(file: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/mermaid").join(file);
    std::fs::read_to_string(path).unwrap()
}

/// Texto visible del SVG sin entidades (las etiquetas con acentos o `¿` pueden ir escapadas).
fn visible(svg: &str) -> String {
    svg.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'")
}

#[test]
fn every_common_diagram_draws_with_its_labels() {
    let light = NoteStyle::default();
    let dark = NoteStyle { text: [0xe6, 0xe8, 0xeb], code_bg: [0x24, 0x27, 0x2e], ..NoteStyle::default() };
    for (file, labels) in CASES {
        for style in [&light, &dark] {
            let src = code(file);
            let d = diagram::render_drawn(&src, style).unwrap_or_else(|e| panic!("{file}: {}", e.es));
            let text = visible(&d.svg);
            // Por palabras: `mermaid-svg` puede partir una etiqueta larga en varias líneas.
            for l in *labels {
                assert!(l.split_whitespace().all(|w| text.contains(w)), "{file}: falta «{l}»");
            }
            assert!(!d.svg.contains("width=\"100%\""), "{file}: tamaño fijo");
            assert!(d.width > 50.0 && d.height > 30.0 && d.width < 4_000.0, "{file}: {}×{}", d.width, d.height);
        }
    }
}

#[test]
fn every_common_diagram_rasterizes() {
    for (file, _) in CASES {
        let svg = diagram::render(&code(file), &NoteStyle::default()).unwrap();
        let r = diagram::rasterize(&svg, 1.0, [255, 255, 255]).unwrap();
        // Algo se pintó (no todo blanco).
        assert!(r.rgba.chunks(4).filter(|p| p[0] < 200 || p[1] < 200 || p[2] < 200).count() > 200, "{file}");
    }
}
