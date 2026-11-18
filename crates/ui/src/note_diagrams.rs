//! S1.12: los diagramas de las notas dibujados en su lugar dentro del editor (`DocEditor`).
//!
//! El editor de la suite no dibuja: le pasamos este dibujante, que usa el mismo motor que la vista
//! previa (`html-view`: `mermaid-svg` y nuestro dibujo de mapas mentales, pintados con Blitz), con
//! los colores del tema y la distribución de mapas de Preferencias.

use std::hash::{Hash, Hasher};

use iuredesk_html_view::diagram;
use iuredesk_html_view::markdown::NoteStyle;
use iuredesk_html_view::mindmap::Layout;
use iurefficient_ui::lang::pick;

/// Dibuja bloques ```` ```mermaid ```` y mapas mentales de IureEditor (`> [!MINDMAP…]`).
pub(crate) struct NoteDiagrams {
    style: NoteStyle,
    background: [u8; 3],
    layout: Layout,
}

impl NoteDiagrams {
    pub(crate) fn new(ui: &egui::Ui, layout: Layout) -> Self {
        let bg = ui.visuals().panel_fill;
        NoteDiagrams { style: crate::note_preview::style(ui), background: [bg.r(), bg.g(), bg.b()], layout }
    }
}

impl iure_doc_ui::DiagramRenderer for NoteDiagrams {
    fn accepts(&self, block: &str) -> bool {
        diagram::is_diagram_block(block)
    }

    fn key(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        format!("{:?}{:?}{:?}", self.style, self.background, self.layout).hash(&mut h);
        h.finish()
    }

    fn render(&self, block: &str, scale: f32) -> Result<iure_doc_ui::DiagramImage, String> {
        let px = diagram::draw_block(block, &self.style, self.layout, scale, self.background)
            .map_err(|e| pick(&e.en, &e.es).to_string())?;
        Ok(iure_doc_ui::DiagramImage {
            width: px.width as usize,
            height: px.height as usize,
            rgba: px.rgba,
            points: [px.points.0, px.points.1],
        })
    }
}
