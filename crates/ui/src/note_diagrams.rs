//! S1.12: los diagramas de las notas dibujados en su lugar dentro del editor (`DocEditor`).
//!
//! El editor de la suite no dibuja: le pasamos este dibujante, que usa el mismo motor que la vista
//! previa (`html-view`: `mermaid-svg` y nuestro dibujo de mapas mentales, pintados con Blitz), con
//! los colores del tema y la distribución de mapas de Preferencias.

use std::hash::{Hash, Hasher};

use iuredesk_html_view::diagram;
use iuredesk_html_view::markdown::NoteStyle;
use iuredesk_html_view::mindmap::{self, Layout};
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

/// El código de un bloque ```` ```mermaid ```` cerrado (sin las cercas).
fn mermaid_code(block: &str) -> Option<String> {
    let mut lines = block.lines();
    let first = lines.next()?.trim_start();
    let fence_char = first.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let n = first.chars().take_while(|c| *c == fence_char).count();
    if n < 3 || !first.get(n..)?.trim().eq_ignore_ascii_case("mermaid") {
        return None;
    }
    let body: Vec<&str> = lines.collect();
    let (last, code) = body.split_last()?;
    let closing = last.trim();
    (closing.len() >= n && closing.chars().all(|c| c == fence_char)).then(|| code.join("\n"))
}

fn is_mindmap(block: &str) -> bool {
    block.trim_start().starts_with('>') && !mindmap::find(block).is_empty()
}

impl iure_doc_ui::DiagramRenderer for NoteDiagrams {
    fn accepts(&self, block: &str) -> bool {
        mermaid_code(block).is_some() || is_mindmap(block)
    }

    fn key(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        format!("{:?}{:?}{:?}", self.style, self.background, self.layout).hash(&mut h);
        h.finish()
    }

    fn render(&self, block: &str, scale: f32) -> Result<iure_doc_ui::DiagramImage, String> {
        let svg = if let Some(code) = mermaid_code(block) {
            diagram::render_drawn(&code, &self.style).map_err(|e| pick(&e.en, &e.es).to_string())?.svg
        } else {
            let map = mindmap::find(block).into_iter().next().ok_or_else(|| {
                pick("This mind map has no branches to draw.", "Este mapa mental no tiene ramas que dibujar.")
                    .to_string()
            })?;
            mindmap::draw(&map.root, &self.style, &map.options.look(self.layout))
        };
        let (w, h) = diagram::svg_size(&svg).unwrap_or((800.0, 600.0));
        let px = diagram::rasterize(&svg, scale, self.background).map_err(|e| {
            format!("{} ({e})", pick("The diagram could not be drawn", "No se pudo dibujar el diagrama"))
        })?;
        Ok(iure_doc_ui::DiagramImage {
            width: px.width as usize,
            height: px.height as usize,
            rgba: px.rgba,
            points: [w, h],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_closed_mermaid_fences_and_mind_maps() {
        assert_eq!(mermaid_code("```mermaid\nflowchart LR\n  A-->B\n```").as_deref(), Some("flowchart LR\n  A-->B"));
        assert_eq!(mermaid_code("~~~~ Mermaid\nx\n~~~~").as_deref(), Some("x"));
        assert!(mermaid_code("```mermaid\nflowchart LR").is_none(), "sin cerrar: se sigue escribiendo");
        assert!(mermaid_code("```rust\nfn f() {}\n```").is_none());
        assert!(mermaid_code("````mermaid\nx\n```").is_none(), "la cerca de cierre es más corta");
        assert!(is_mindmap("> [!MINDMAP|map] Proyecto\n> - Análisis\n> - Diseño"));
        assert!(!is_mindmap("> una cita"));
    }
}
