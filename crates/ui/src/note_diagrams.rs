//! S1.12: los diagramas de las notas dibujados en su lugar dentro del editor (`DocEditor`).
//!
//! El editor de la suite no dibuja: le pasamos este dibujante, que usa el mismo motor que la vista
//! previa (`html-view`: `mermaid-svg` y nuestro dibujo de mapas mentales, pintados con Blitz), con
//! los colores del tema y la distribución de mapas de Preferencias.
//!
//! Como en IureEditor 2.0: un mapa mental se ve con su lista (arriba, a la izquierda o solo el
//! mapa), sus nodos se pulsan para añadir ramas o borrarlas, y con clic derecho se eligen
//! distribución, forma, centro, colores y vista; en Mermaid, tema y colores. Las fórmulas
//! (`$$…$$` y `$…$`) se dibujan con Typst ([`NoteFormulas`]).

use std::hash::{Hash, Hasher};

use iuredesk_html_view::diagram::{self, ColorRole, DiagramTheme};
use iuredesk_html_view::markdown::NoteStyle;
use iuredesk_html_view::mindmap::{self, Layout, Palette, RootSize, Shape};
use iurefficient_ui::lang::pick;

use crate::diagram_ui::{choose, color_label, layout_label, palette_label, root_label, shape_label, theme_label};

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
            // Cada nodo de un mapa, pulsable: su número es su línea del bloque.
            hotspots: px.boxes.iter().map(|b| iure_doc_ui::Hotspot { id: b.line, rect: b.rect }).collect(),
        })
    }

    fn placement(&self, block: &str) -> iure_doc_ui::Placement {
        let Some(map) = mindmap::find(block).into_iter().next() else { return iure_doc_ui::Placement::Replace };
        if map.view == mindmap::View::MapOnly {
            iure_doc_ui::Placement::Replace
        } else if mindmap::meta_words(block).iter().any(|w| w.eq_ignore_ascii_case("right")) {
            iure_doc_ui::Placement::Right
        } else {
            iure_doc_ui::Placement::Below
        }
    }

    fn menu(&self, ui: &mut egui::Ui, block: &str) -> Option<String> {
        export_items(ui, block, &self.style, self.layout, self.background);
        if let Some(map) = mindmap::find(block).into_iter().next() {
            return map_menu(ui, block, map.options, self.layout);
        }
        let code = diagram::mermaid_code(block)?;
        let mut opts = diagram::read_options(&code);
        let mut changed = false;
        if let Some(t) = choose(ui, pick("Theme", "Tema"), opts.theme, &DiagramTheme::ALL, |t| theme_label(t).into()) {
            opts.theme = t;
            changed = true;
        }
        ui.menu_button(pick("Colors", "Colores"), |ui| {
            for role in ColorRole::ALL {
                ui.menu_button(color_label(role), |ui| {
                    if let Some(c) = swatches(ui, opts.colors.get(role)) {
                        opts.colors.set(role, c);
                        changed = true;
                        ui.close();
                    }
                });
            }
        });
        changed.then(|| fence_with(block, &diagram::set_options(&code, &opts)))
    }

    fn node_action(&self, block: &str, id: usize, action: iure_doc_ui::NodeAction) -> Option<(String, Option<usize>)> {
        use iure_doc_ui::NodeAction as A;
        mindmap::find(block).into_iter().next()?;
        let edit = match action {
            A::AddChild => mindmap::NodeEdit::AddChild,
            A::AddSibling => mindmap::NodeEdit::AddSibling,
            A::Delete => mindmap::NodeEdit::Delete,
            A::Goto => mindmap::NodeEdit::Goto,
        };
        mindmap::edit_node(block, id, edit, pick("New branch", "Nueva rama"))
    }
}

/// «Exportar como PNG…» y «Exportar como SVG…» (como IureEditor 1.x): el diálogo de guardar y el
/// dibujo van en otro hilo.
fn export_items(ui: &mut egui::Ui, block: &str, style: &NoteStyle, layout: Layout, bg: [u8; 3]) {
    for (png, label) in
        [(true, pick("Export as PNG…", "Exportar como PNG…")), (false, pick("Export as SVG…", "Exportar como SVG…"))]
    {
        if ui.button(label).clicked() {
            let (block, style) = (block.to_string(), style.clone());
            std::thread::spawn(move || {
                let (name, ext) = if png {
                    (pick("diagram.png", "diagrama.png"), "png")
                } else {
                    (pick("diagram.svg", "diagrama.svg"), "svg")
                };
                let Some(path) = rfd::FileDialog::new().set_file_name(name).add_filter(ext, &[ext]).save_file() else {
                    return;
                };
                let data = if png {
                    diagram::draw_block(&block, &style, layout, 2.0, bg)
                        .ok()
                        .and_then(|p| diagram::encode_png(p.width, p.height, &p.rgba))
                } else {
                    diagram::block_svg(&block, &style, layout).ok().map(String::into_bytes)
                };
                if let Some(d) = data {
                    let _ = std::fs::write(path, d);
                }
            });
            ui.close();
        }
    }
    ui.separator();
}

/// Las opciones de un mapa mental (distribución, forma, centro, colores, vista).
fn map_menu(ui: &mut egui::Ui, block: &str, o: mindmap::MapOptions, pref: Layout) -> Option<String> {
    let mut opts = o;
    let mut changed = false;
    let layouts: Vec<Option<Layout>> = std::iter::once(None).chain(Layout::ALL.map(Some)).collect();
    let default = format!("{} ({})", pick("As in Settings", "La de Preferencias"), layout_label(pref));
    let label = |l: Option<Layout>| l.map_or_else(|| default.clone(), |l| layout_label(l).to_string());
    if let Some(l) = choose(ui, pick("Layout", "Distribución"), opts.layout, &layouts, label) {
        opts.layout = l;
        changed = true;
    }
    if let Some(sh) =
        choose(ui, pick("Node shape", "Forma de los nodos"), opts.shape, &Shape::ALL, |s| shape_label(s).into())
    {
        opts.shape = sh;
        changed = true;
    }
    if let Some(r) =
        choose(ui, pick("Center size", "Tamaño del centro"), opts.root, &RootSize::ALL, |r| root_label(r).into())
    {
        opts.root = r;
        changed = true;
    }
    if let Some(p) = choose(ui, pick("Branch colors", "Colores de las ramas"), opts.palette, &Palette::ALL, |p| {
        palette_label(p).into()
    }) {
        opts.palette = p;
        changed = true;
    }
    let words = mindmap::meta_words(block);
    let has = |w: &str| words.iter().any(|x| x.eq_ignore_ascii_case(w));
    let current = if has("map") {
        2
    } else if has("right") {
        1
    } else {
        0
    };
    let mut view: Option<Option<&str>> = None;
    ui.menu_button(pick("View", "Vista"), |ui| {
        for (k, l, v) in [
            (0, pick("List above, map below", "Lista arriba, mapa debajo"), None),
            (1, pick("List on the left, map on the right", "Lista a la izquierda, mapa a la derecha"), Some("right")),
            (2, pick("Map only (hides the list)", "Solo el mapa (oculta la lista)"), Some("map")),
        ] {
            if ui.selectable_label(k == current, l).clicked() {
                view = Some(v);
                ui.close();
            }
        }
    });
    let mut out = changed.then(|| mindmap::set_options(block, 0, &opts)).flatten();
    if let Some(v) = view {
        out = mindmap::set_view(out.as_deref().unwrap_or(block), v);
    }
    out
}

/// El bloque con otro código Mermaid (la misma cerca).
fn fence_with(block: &str, code: &str) -> String {
    let t = block.trim_start();
    let fence: String = t.chars().take_while(|c| *c == '`' || *c == '~').collect();
    let fence = if fence.len() < 3 { "```".to_string() } else { fence };
    format!("{fence}mermaid\n{}\n{fence}", code.trim_end())
}

/// Muestras de color (`Some(None)`: el del tema).
fn swatches(ui: &mut egui::Ui, current: Option<[u8; 3]>) -> Option<Option<[u8; 3]>> {
    const COLORS: [[u8; 3]; 12] = [
        [0x1d, 0x4e, 0xd8],
        [0x0f, 0x76, 0x6e],
        [0x15, 0x80, 0x3d],
        [0xb4, 0x53, 0x09],
        [0xb9, 0x1c, 0x1c],
        [0x6d, 0x28, 0xd9],
        [0xbf, 0xdb, 0xfe],
        [0xbb, 0xf7, 0xd0],
        [0xfe, 0xd7, 0xaa],
        [0xe9, 0xd5, 0xff],
        [0xfe, 0xca, 0xca],
        [0xf3, 0xf4, 0xf6],
    ];
    let mut out = None;
    egui::Grid::new("muestras").spacing(egui::vec2(3.0, 3.0)).show(ui, |ui| {
        for (k, c) in COLORS.iter().enumerate() {
            let (r, resp) = ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::click());
            ui.painter().rect_filled(r, 3.0, egui::Color32::from_rgb(c[0], c[1], c[2]));
            if current == Some(*c) {
                ui.painter().rect_stroke(
                    r,
                    3.0,
                    egui::Stroke::new(2.0, ui.visuals().strong_text_color()),
                    egui::StrokeKind::Outside,
                );
            }
            if resp.clicked() {
                out = Some(Some(*c));
            }
            if k % 6 == 5 {
                ui.end_row();
            }
        }
    });
    if ui.button(pick("As the theme", "Como el tema")).clicked() {
        out = Some(None);
    }
    out
}

/// Fórmulas dibujadas con Typst: en bloque (`$$…$$`) o en línea (`$…$`).
pub(crate) struct NoteFormulas {
    color: [u8; 3],
    size_pt: f32,
    inline: bool,
}

impl NoteFormulas {
    pub(crate) fn new(ui: &egui::Ui, inline: bool) -> Self {
        let c = iurefficient_ui::theme::colors(ui);
        let body = iurefficient_ui::theme::body_font(ui.ctx()).size;
        NoteFormulas {
            color: [c.text.r(), c.text.g(), c.text.b()],
            size_pt: if inline { body } else { body * 1.1 },
            inline,
        }
    }
}

impl iure_doc_ui::DiagramRenderer for NoteFormulas {
    fn accepts(&self, block: &str) -> bool {
        if self.inline {
            let t = block.trim();
            t.len() > 2 && t.starts_with('$') && t.ends_with('$') && !t.starts_with("$$")
        } else {
            iure_doc::math::display_math(block).is_some()
        }
    }

    fn key(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        format!("{:?}{}{}", self.color, self.size_pt, self.inline).hash(&mut h);
        h.finish()
    }

    fn render(&self, block: &str, scale: f32) -> Result<iure_doc_ui::DiagramImage, String> {
        let latex = if self.inline {
            block.trim().trim_matches('$')
        } else {
            iure_doc::math::display_math(block).unwrap_or_default()
        };
        let img = iure_doc_export::math::render_math(latex, !self.inline, self.size_pt, self.color, scale)
            .map_err(|e| pick(&e.en, &e.es).to_string())?;
        Ok(iure_doc_ui::DiagramImage {
            width: img.width as usize,
            height: img.height as usize,
            rgba: img.rgba,
            points: [img.points.0, img.points.1],
            hotspots: Vec::new(),
        })
    }
}

/// Las notas que puede enlazar `[[` (por su título) y sus títulos internos (`[[nota#…`).
pub(crate) struct NotesIndex(pub Vec<(String, String)>);

impl iure_doc_ui::NoteIndex for NotesIndex {
    fn notes(&self) -> Vec<String> {
        self.0.iter().map(|(t, _)| t.clone()).filter(|t| !t.trim().is_empty()).collect()
    }

    fn headings(&self, note: &str) -> Vec<String> {
        let want = note.trim().to_lowercase();
        let Some((_, body)) = self.0.iter().find(|(t, _)| t.trim().to_lowercase() == want) else { return Vec::new() };
        body.lines()
            .filter_map(|l| {
                let t = l.trim_start();
                let n = t.chars().take_while(|c| *c == '#').count();
                ((1..=6).contains(&n) && t.get(n..).is_some_and(|r| r.starts_with(' ')))
                    .then(|| t.get(n..).unwrap_or("").trim().to_string())
            })
            .filter(|h| !h.is_empty())
            .take(500)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_index_lists_titles_and_headings() {
        use iure_doc_ui::NoteIndex;
        let ix = NotesIndex(vec![("Acta".into(), "# Acta\n\n## Pagos\ntexto\n".into()), ("  ".into(), String::new())]);
        assert_eq!(ix.notes(), ["Acta"]);
        assert_eq!(ix.headings("acta"), ["Acta", "Pagos"]);
        assert_eq!(fence_with("~~~mermaid\nx\n~~~", "pie\n"), "~~~mermaid\npie\n~~~");
    }
}
