//! S1.12: `DocEditor`, el editor de documentos de la suite para egui (primera entrega).
//!
//! **Markdown con estilo al teclear**, como el modo en vivo de Obsidian: el texto es Markdown y se
//! ve con su estilo mientras se escribe (títulos grandes, negrita, cursiva, código en
//! monoespaciado, enlaces en color de acento, listas y casillas resaltadas). Las marcas (`#`,
//! `**`, `` ` ``, `[`…`](url)`) se ven tenues: borrar una marca deshace la conversión, sin modo
//! especial.
//!
//! Es un solo `TextEdit` de egui (selección entre bloques, IME, deshacer, copiar y pegar son los
//! de egui) con un *layouter* que pinta el estilo a partir de `pulldown-cmark`, más las teclas del
//! editor ([`iure_doc::edit`]):
//!
//! - **Intro** sigue una lista, numerada, de tareas o una cita; en un elemento vacío sale de ella.
//! - **Tab** / **Mayús+Tab** sangran o desangran un elemento de lista.
//! - **Ctrl+B** / **Ctrl+I** / **Ctrl+Mayús+X**: negrita, cursiva, tachado (ponen o quitan).
//! - **Ctrl+Mayús+K**: enlace (`Ctrl+K` es la paleta de comandos de la suite).
//!
//! Las marcas en línea (`**`, `*`, `~~`, comillas, sintaxis de enlace, `#` de títulos) se ocultan
//! fuera de la línea del cursor (la negrita se ve como negrita); en esa línea se ven para poder
//! borrarlas. Viñetas, `>`, cercas y `|` de tablas siempre se ven: son estructura.
//!
//! Lo que viene (S1.12, siguientes entregas): diagramas y tablas dibujados en su lugar, pegar HTML
//! como Markdown, corrector.

#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable,
    clippy::indexing_slicing
)]

use std::sync::Arc;

use egui::text::{CCursor, CCursorRange, LayoutJob, TextFormat};
use egui::text_edit::TextEditState;
use egui::{Color32, FontFamily, FontId, Id, Key, Modifiers, Stroke, Ui};
use iure_doc::edit::{self, Edit};
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag};

mod blocks;
pub use blocks::show as show_blocks;

/// Colores y letra del editor.
#[derive(Clone, Debug, PartialEq)]
pub struct EditorStyle {
    pub text: Color32,
    /// Marcas de Markdown y citas.
    pub muted: Color32,
    pub accent: Color32,
    /// Fondo del código.
    pub code_bg: Color32,
    /// Tamaño de la letra del cuerpo.
    pub size: f32,
    /// Familia en negrita (la del ui-kit).
    pub bold_family: FontFamily,
    pub body_family: FontFamily,
}

impl EditorStyle {
    /// Con los colores y la letra del tema actual.
    pub fn from_ui(ui: &Ui) -> Self {
        let c = iurefficient_ui::theme::colors(ui);
        let body = iurefficient_ui::theme::body_font(ui.ctx());
        let bold = iurefficient_ui::theme::semibold_font(ui.ctx(), body.size);
        EditorStyle {
            text: c.text,
            muted: c.text_muted,
            accent: c.accent,
            code_bg: c.surface_alt,
            size: body.size,
            bold_family: bold.family,
            body_family: body.family,
        }
    }
}

/// Formato de un byte del texto (se juntan los iguales en tramos).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Fmt {
    heading: u8,
    bold: bool,
    italic: bool,
    strike: bool,
    code: bool,
    link: bool,
    /// Marca de Markdown (tenue).
    marker: bool,
    /// Marca en línea (`**`, `*`, `~~`, comillas, sintaxis de enlace, `#` de títulos): se oculta
    /// fuera de la línea del cursor.
    inline: bool,
    /// Viñeta, número o casilla (acento).
    bullet: bool,
    quote: bool,
    /// Oculta (marca en línea fuera de la línea del cursor).
    hidden: bool,
}

fn inline_marker(x: &mut Fmt) {
    x.marker = true;
    x.inline = true;
}

/// Formatos byte a byte del texto.
fn formats(text: &str) -> Vec<Fmt> {
    let mut f = vec![Fmt::default(); text.len()];
    let mut set = |r: std::ops::Range<usize>, g: &dyn Fn(&mut Fmt)| {
        for x in f.get_mut(r.start.min(text.len())..r.end.min(text.len())).into_iter().flatten() {
            g(x);
        }
    };
    let opts = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_MATH
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS;
    for (ev, r) in Parser::new_ext(text, opts).into_offset_iter() {
        let slice = text.get(r.clone()).unwrap_or("");
        match ev {
            Event::Start(Tag::Heading { level, .. }) => {
                let n = match level {
                    HeadingLevel::H1 => 1,
                    HeadingLevel::H2 => 2,
                    HeadingLevel::H3 => 3,
                    HeadingLevel::H4 => 4,
                    HeadingLevel::H5 => 5,
                    HeadingLevel::H6 => 6,
                };
                set(r.clone(), &|x| x.heading = n);
                // `## ` del principio.
                let hashes = slice.len() - slice.trim_start_matches('#').len();
                if hashes > 0 {
                    let sp = slice.get(hashes..).map_or(0, |s| s.len() - s.trim_start().len());
                    set(r.start..r.start + hashes + sp, &|x| {
                        x.marker = true;
                        x.inline = true;
                    });
                }
            }
            Event::Start(Tag::Strong) => {
                set(r.clone(), &|x| x.bold = true);
                set(r.start..r.start + 2, &inline_marker);
                set(r.end.saturating_sub(2)..r.end, &inline_marker);
            }
            Event::Start(Tag::Emphasis) => {
                set(r.clone(), &|x| x.italic = true);
                set(r.start..r.start + 1, &inline_marker);
                set(r.end.saturating_sub(1)..r.end, &inline_marker);
            }
            Event::Start(Tag::Strikethrough) => {
                set(r.clone(), &|x| x.strike = true);
                let n = slice.len() - slice.trim_start_matches('~').len();
                set(r.start..r.start + n, &inline_marker);
                set(r.end.saturating_sub(n)..r.end, &inline_marker);
            }
            Event::Code(_) | Event::InlineMath(_) | Event::DisplayMath(_) => {
                set(r.clone(), &|x| x.code = true);
                let n = slice.len() - slice.trim_start_matches(['`', '$']).len();
                set(r.start..r.start + n, &inline_marker);
                set(r.end.saturating_sub(n)..r.end, &inline_marker);
            }
            Event::Start(Tag::Image { .. }) => {
                // `![texto](ruta)`: el texto como enlace; `![` y `](ruta)` son marcas.
                set(r.clone(), &|x| x.link = true);
                if slice.starts_with("![")
                    && let Some(close) = slice.rfind("](")
                {
                    set(r.start..r.start + 2, &inline_marker);
                    set(r.start + close..r.end, &inline_marker);
                }
            }
            Event::Start(Tag::Link { .. }) => {
                set(r.clone(), &|x| x.link = true);
                if slice.starts_with('[')
                    && let Some(close) = slice.rfind("](")
                {
                    set(r.start..r.start + 1, &inline_marker);
                    set(r.start + close..r.end, &inline_marker);
                }
            }
            Event::Start(Tag::CodeBlock(_)) => {
                set(r.clone(), &|x| x.code = true);
                // Las cercas.
                let first = slice.find('\n').unwrap_or(slice.len());
                if slice.trim_start().starts_with(['`', '~']) {
                    set(r.start..r.start + first, &|x| x.marker = true);
                    let body = slice.trim_end_matches('\n');
                    if let Some(last) = body.rfind('\n')
                        && body.get(last + 1..).is_some_and(|l| l.trim_start().starts_with(['`', '~']))
                    {
                        set(r.start + last + 1..r.start + body.len(), &|x| x.marker = true);
                    }
                }
            }
            Event::Start(Tag::Item) => {
                // La viñeta o el número (y la casilla), desde el principio de su línea.
                let ls = text.get(..r.start).and_then(|b| b.rfind('\n')).map_or(0, |i| i + 1);
                let line = text.get(ls..).and_then(|t| t.lines().next()).unwrap_or("");
                // Dentro de una cita, la línea empieza con `> `.
                let quote = line.len() - line.trim_start_matches(['>', ' ']).len();
                let inner = line.get(quote..).unwrap_or("");
                if let Some(p) = edit::prefix(inner) {
                    let at = ls + quote;
                    set(at + p.indent.len()..at + p.len, &|x| x.bullet = true);
                }
            }
            Event::Start(Tag::Table(_)) => {
                // Los `|` y la fila de alineación, tenues.
                for (i, ch) in slice.char_indices() {
                    if ch == '|' {
                        set(r.start + i..r.start + i + 1, &|x| x.marker = true);
                    }
                }
                if let Some(first_nl) = slice.find('\n') {
                    let second =
                        slice.get(first_nl + 1..).and_then(|t| t.find('\n')).map_or(slice.len(), |j| first_nl + 1 + j);
                    set(r.start + first_nl + 1..r.start + second, &|x| x.marker = true);
                }
            }
            Event::Start(Tag::BlockQuote(_)) => {
                set(r.clone(), &|x| x.quote = true);
                // Los `>` de cada línea.
                let mut at = r.start;
                for line in slice.split_inclusive('\n') {
                    let lead = line.len() - line.trim_start().len();
                    if line.trim_start().starts_with('>') {
                        set(at + lead..at + lead + 1, &|x| x.marker = true);
                    }
                    at += line.len();
                }
            }
            Event::Rule | Event::Html(_) | Event::InlineHtml(_) => set(r, &|x| x.marker = true),
            Event::Start(Tag::MetadataBlock(_)) => set(r, &|x| x.marker = true),
            _ => {}
        }
    }
    f
}

/// Factor de tamaño de un título.
fn heading_scale(level: u8) -> f32 {
    match level {
        1 => 1.6,
        2 => 1.35,
        3 => 1.18,
        4 => 1.06,
        _ => 1.0,
    }
}

fn text_format(fmt: Fmt, s: &EditorStyle) -> TextFormat {
    if fmt.hidden {
        // Casi sin ancho e invisible: sigue en el texto (el cursor pasa por ahí) pero no se ve.
        return TextFormat {
            font_id: FontId::new(0.05, s.body_family.clone()),
            color: Color32::TRANSPARENT,
            ..Default::default()
        };
    }
    let size = s.size * if fmt.heading > 0 { heading_scale(fmt.heading) } else { 1.0 };
    let family = if fmt.code {
        FontFamily::Monospace
    } else if fmt.bold || fmt.heading > 0 {
        s.bold_family.clone()
    } else {
        s.body_family.clone()
    };
    let size = if fmt.code { size * 0.92 } else { size };
    let mut color = if fmt.marker {
        s.muted.gamma_multiply(0.75)
    } else if fmt.bullet || fmt.link {
        s.accent
    } else if fmt.quote {
        s.muted
    } else {
        s.text
    };
    if fmt.marker && fmt.bullet {
        color = s.accent;
    }
    TextFormat {
        font_id: FontId::new(size, family),
        color,
        italics: fmt.italic || (fmt.quote && !fmt.marker),
        background: if fmt.code && !fmt.marker { s.code_bg } else { Color32::TRANSPARENT },
        strikethrough: if fmt.strike && !fmt.marker { Stroke::new(1.0, s.text) } else { Stroke::NONE },
        underline: if fmt.link && !fmt.marker { Stroke::new(1.0, s.accent) } else { Stroke::NONE },
        ..Default::default()
    }
}

/// El texto con su estilo, en tramos de formato igual. Las marcas en línea fuera de `reveal`
/// (bytes de la línea del cursor; `None` sin cursor) se ocultan: la negrita se ve como negrita y
/// el enlace como enlace; en la línea donde se escribe se ven para poder borrarlas.
pub fn layout_job(text: &str, s: &EditorStyle, wrap_width: f32, reveal: Option<std::ops::Range<usize>>) -> LayoutJob {
    let mut f = formats(text);
    for (i, x) in f.iter_mut().enumerate() {
        x.hidden = x.inline && !reveal.as_ref().is_some_and(|r| r.contains(&i) || r.end == i);
    }
    let mut job = LayoutJob::default();
    let mut start = 0usize;
    let mut cur = f.first().copied().unwrap_or_default();
    for (i, _) in text.char_indices().skip(1) {
        let here = f.get(i).copied().unwrap_or_default();
        if here != cur {
            job.append(text.get(start..i).unwrap_or(""), 0.0, text_format(cur, s));
            start = i;
            cur = here;
        }
    }
    if start < text.len() || text.is_empty() {
        job.append(text.get(start..).unwrap_or(""), 0.0, text_format(cur, s));
    }
    job.wrap.max_width = wrap_width;
    job
}

/// Lo que pasó en el editor este cuadro.
pub struct Output {
    /// El texto cambió (teclas o atajos del editor).
    pub changed: bool,
    pub response: egui::Response,
}

/// La tecla del editor que toca (antes de que la vea el `TextEdit`).
fn editor_key(ui: &Ui, text: &str, sel: (usize, usize)) -> Option<Edit> {
    let (a, b) = sel;
    let cursor = a.max(b);
    let collapsed = a == b;
    ui.input_mut(|i| {
        if collapsed
            && let Some(e) = edit::on_enter(text, cursor)
            && i.consume_key(Modifiers::NONE, Key::Enter)
        {
            return Some(e);
        }
        // Mayús+Tab primero: `consume_key` sin modificadores también acepta Mayús.
        if let Some(e) = edit::indent(text, cursor, true)
            && i.consume_key(Modifiers::SHIFT, Key::Tab)
        {
            return Some(e);
        }
        if !i.modifiers.shift
            && let Some(e) = edit::indent(text, cursor, false)
            && i.consume_key(Modifiers::NONE, Key::Tab)
        {
            return Some(e);
        }
        let shift_cmd = Modifiers { shift: true, ..Modifiers::COMMAND };
        if i.consume_key(Modifiers::COMMAND, Key::B) {
            return Some(edit::toggle_wrap(text, a, b, "**"));
        }
        if i.consume_key(Modifiers::COMMAND, Key::I) {
            return Some(edit::toggle_wrap(text, a, b, "*"));
        }
        if i.consume_key(shift_cmd, Key::X) {
            return Some(edit::toggle_wrap(text, a, b, "~~"));
        }
        if i.consume_key(shift_cmd, Key::K) {
            return Some(edit::link(text, a, b));
        }
        None
    })
}

/// El editor. `id` identifica su estado (cursor, deshacer) entre cuadros.
pub fn show(ui: &mut Ui, id: Id, text: &mut String, hint: &str) -> Output {
    let style = EditorStyle::from_ui(ui);
    let (changed, out) = field(ui, id, text, hint, 16, &style, style_key(&style));
    Output { changed, response: out.response.response }
}

/// Huella del estilo (parte de la llave de la caché del dibujo).
pub(crate) fn style_key(style: &EditorStyle) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    format!("{style:?}").hash(&mut h);
    h.finish()
}

/// Un campo con estilo al teclear y las teclas del editor; `true` si el texto cambió.
pub(crate) fn field(
    ui: &mut Ui,
    id: Id,
    text: &mut String,
    hint: &str,
    rows: usize,
    style: &EditorStyle,
    style_key: u64,
) -> (bool, egui::text_edit::TextEditOutput) {
    let mut changed = false;
    if ui.memory(|m| m.has_focus(id))
        && let Some(mut state) = TextEditState::load(ui.ctx(), id)
        && let Some(range) = state.cursor.char_range()
    {
        let sel = (range.primary.index.0, range.secondary.index.0);
        if let Some(e) = editor_key(ui, text, sel) {
            *text = e.text;
            state.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(e.sel_start), CCursor::new(e.sel_end))));
            state.store(ui.ctx(), id);
            changed = true;
        }
    }
    // La línea del cursor (en bytes), para mostrar ahí las marcas.
    let reveal = if ui.memory(|m| m.has_focus(id)) {
        TextEditState::load(ui.ctx(), id).and_then(|st| st.cursor.char_range()).map(|r| {
            let (a, b) = (r.primary.index.0.min(r.secondary.index.0), r.primary.index.0.max(r.secondary.index.0));
            let ab = text.char_indices().nth(a).map_or(text.len(), |(i, _)| i);
            let bb = text.char_indices().nth(b).map_or(text.len(), |(i, _)| i);
            let start = text.get(..ab).and_then(|t| t.rfind('\n')).map_or(0, |i| i + 1);
            let end = text.get(bb..).and_then(|t| t.find('\n')).map_or(text.len(), |i| bb + i);
            start..end
        })
    } else {
        None
    };
    // Caché del dibujo: si no cambian el texto, la línea del cursor, el ancho ni el estilo (lo
    // normal mientras el cursor parpadea o se desplaza la vista), no se vuelve a calcular.
    let cache_id = id.with("galley");
    let mut layouter = |ui: &Ui, buf: &dyn egui::TextBuffer, wrap: f32| -> Arc<egui::Galley> {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        buf.as_str().hash(&mut h);
        reveal.hash(&mut h);
        wrap.to_bits().hash(&mut h);
        style_key.hash(&mut h);
        let key = h.finish();
        if let Some((k, g)) = ui.data(|d| d.get_temp::<(u64, Arc<egui::Galley>)>(cache_id))
            && k == key
        {
            return g;
        }
        let g = ui.fonts_mut(|f| f.layout_job(layout_job(buf.as_str(), style, wrap, reveal.clone())));
        ui.data_mut(|d| d.insert_temp(cache_id, (key, Arc::clone(&g))));
        g
    };
    let out = egui::TextEdit::multiline(text)
        .id(id)
        .hint_text(hint)
        .desired_width(f32::INFINITY)
        .desired_rows(rows)
        .frame(egui::Frame::NONE)
        .lock_focus(true)
        .layouter(&mut layouter)
        .show(ui);
    changed |= out.response.changed();
    (changed, out)
}

/// El último dibujo de un campo (para saber en qué fila está el cursor).
pub(crate) fn cached_galley(ui: &Ui, id: Id) -> Option<Arc<egui::Galley>> {
    ui.data(|d| d.get_temp::<(u64, Arc<egui::Galley>)>(id.with("galley"))).map(|(_, g)| g)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt_at(text: &str, needle: &str) -> Fmt {
        let i = text.find(needle).unwrap_or(0);
        formats(text)[i]
    }

    #[test]
    fn styles_follow_the_markdown() {
        let t =
            "# Título\n\nTexto **negrita** y *cursiva* y `código` y [enlace](https://x.mx).\n\n- [ ] tarea\n> cita\n";
        assert_eq!(fmt_at(t, "Título").heading, 1);
        assert!(fmt_at(t, "# ").marker, "el # tenue");
        assert!(fmt_at(t, "negrita").bold && !fmt_at(t, "negrita").marker);
        assert!(fmt_at(t, "**negrita").marker);
        assert!(fmt_at(t, "cursiva").italic);
        assert!(fmt_at(t, "código").code);
        assert!(fmt_at(t, "enlace").link && fmt_at(t, "](https").marker);
        assert!(fmt_at(t, "- [ ]").bullet && fmt_at(t, "[ ] tarea").bullet);
        assert!(fmt_at(t, "cita").quote && fmt_at(t, "> cita").marker);
        assert!(!fmt_at(t, "Texto").bold);
    }

    #[test]
    fn inline_markers_hide_outside_the_cursor_line() {
        let s = EditorStyle {
            text: Color32::BLACK,
            muted: Color32::GRAY,
            accent: Color32::BLUE,
            code_bg: Color32::LIGHT_GRAY,
            size: 14.0,
            bold_family: FontFamily::Proportional,
            body_family: FontFamily::Proportional,
        };
        let t = "uno **dos**\ntres **cuatro**\n- lista";
        let hidden = |job: &LayoutJob, needle: &str| {
            let at = t.find(needle).unwrap_or(0);
            job.sections.iter().any(|x| {
                x.byte_range.start.0 <= at && at < x.byte_range.end.0 && x.format.color == Color32::TRANSPARENT
            })
        };
        // Cursor en la primera línea: sus marcas se ven, las de la segunda no.
        let job = layout_job(t, &s, 300.0, Some(0..11));
        assert!(!hidden(&job, "**dos"));
        assert!(hidden(&job, "**cuatro"));
        assert!(!hidden(&job, "- lista"), "las viñetas siempre se ven");
        // Sin cursor: todas ocultas.
        let job = layout_job(t, &s, 300.0, None);
        assert!(hidden(&job, "**dos") && hidden(&job, "**cuatro"));
    }

    #[test]
    fn layout_covers_every_byte() {
        let s = EditorStyle {
            text: Color32::BLACK,
            muted: Color32::GRAY,
            accent: Color32::BLUE,
            code_bg: Color32::LIGHT_GRAY,
            size: 14.0,
            bold_family: FontFamily::Proportional,
            body_family: FontFamily::Proportional,
        };
        for t in ["", "hola", "# a\n**b** ñú `c`\n- [x] d", "```\ncódigo\n```"] {
            let job = layout_job(t, &s, 300.0, None);
            assert_eq!(job.text, t);
            let covered: usize = job.sections.iter().map(|x| x.byte_range.end.0 - x.byte_range.start.0).sum();
            assert_eq!(covered, t.len(), "{t:?}");
        }
    }
}

#[cfg(test)]
mod perf {
    use super::*;

    /// Unas 100 páginas: cuánto tarda pintar el estilo (se mide; el límite es holgado en depuración).
    #[test]
    fn hundred_pages_layout_time() {
        let page = "## Sección\n\nTexto con **negrita**, *cursiva*, `código` y [enlace](https://x.mx).\n\n- uno\n- [ ] dos\n  - tres\n\n> cita\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n```rust\nfn f() {}\n```\n\n";
        // ~250 KB: unas 100 páginas de texto con formato.
        let text = page.repeat(100 * 15);
        let s = EditorStyle {
            text: Color32::BLACK,
            muted: Color32::GRAY,
            accent: Color32::BLUE,
            code_bg: Color32::LIGHT_GRAY,
            size: 14.0,
            bold_family: FontFamily::Proportional,
            body_family: FontFamily::Proportional,
        };
        let tf = std::time::Instant::now();
        let _f = formats(&text);
        eprintln!("solo formats: {:.1} ms", tf.elapsed().as_secs_f64() * 1000.0);
        let t0 = std::time::Instant::now();
        let job = layout_job(&text, &s, 800.0, Some(0..10));
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        eprintln!("100 páginas ({} KB, {} tramos): {ms:.1} ms", text.len() / 1024, job.sections.len());
        // Lo que hace egui con ese trabajo: los glifos de todo el texto (al teclear, otra vez).
        let ctx = egui::Context::default();
        let mut glyph_ms = 0.0;
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let t = std::time::Instant::now();
            let g = ui.fonts_mut(|f| f.layout_job(job.clone()));
            glyph_ms = t.elapsed().as_secs_f64() * 1000.0;
            eprintln!("glifos de egui: {glyph_ms:.1} ms ({} filas)", g.rows.len());
        });
        out.textures_delta.clear();
        assert!(ms < 2_000.0, "{ms} ms");
    }
}
