//! S1.12: `DocEditor` **por bloques**, para documentos largos (IureEditor 2.0).
//!
//! Con un solo `TextEdit`, egui vuelve a maquetar todo el texto en cada tecla: ~40 ms con 100
//! páginas. Aquí el texto se parte en sus bloques de primer nivel ([`iure_doc::chunks`]) y cada uno
//! es su propio campo con su caché de dibujo; solo se dibujan los que se ven (más una pantalla
//! arriba y abajo), los demás ocupan su alto medido o estimado. Teclear rehace solo el bloque
//! editado.
//!
//! Cada bloque guarda su texto sin el espacio final (`content`) y ese espacio aparte (`sep`):
//! juntos dan el documento byte a byte, así que abrir y guardar sin tocar nada no lo cambia.
//!
//! Entre bloques: **Intro** al final de un bloque abre uno nuevo debajo (dentro de una lista sigue
//! la lista); **Retroceso** al inicio lo une con el anterior y **Supr** al final con el siguiente;
//! las flechas pasan de un bloque a otro. Un bloque que al editarse queda en varios (pegar, dos
//! Intro seguidos) se parte solo.
//!
//! Pendiente (siguientes entregas): selección que cruce bloques y deshacer de todo el documento
//! (hoy cada bloque deshace lo suyo).

use std::hash::{Hash, Hasher};

use egui::text::{CCursor, CCursorRange};
use egui::text_edit::TextEditState;
use egui::{Id, Key, Ui};
use iure_doc::edit;

use crate::{EditorStyle, Output, cached_galley, field, style_key};

/// Un bloque del editor.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Chunk {
    /// Identidad estable (estado del campo: cursor, deshacer).
    pub(crate) uid: u64,
    /// El texto del bloque, sin el espacio que lo sigue.
    pub(crate) content: String,
    /// El espacio que lo sigue (saltos de línea).
    pub(crate) sep: String,
    /// Creado en el editor (Intro al final de un bloque): vacío no escribe nada y, con texto, va
    /// separado del anterior por una línea en blanco.
    pub(crate) born: bool,
    /// Alto (medido, o estimado mientras no se ha dibujado) y el ancho con que se calculó.
    height: f32,
    width: f32,
}

/// Texto y espacio final de un trozo: el texto acaba al final de la línea de su último carácter
/// visible (los espacios de esa línea, que pueden ser un salto forzado, se quedan).
fn split_ws(piece: &str) -> (String, String) {
    let Some((i, c)) = piece.char_indices().rev().find(|(_, c)| !c.is_whitespace()) else {
        return (String::new(), piece.to_string());
    };
    let after = i + c.len_utf8();
    let mut end = piece.get(after..).and_then(|t| t.find('\n')).map_or(piece.len(), |n| after + n);
    if end > after && piece.get(end - 1..end) == Some("\r") {
        end -= 1;
    }
    (piece.get(..end).unwrap_or("").to_string(), piece.get(end..).unwrap_or("").to_string())
}

/// Los bloques de un documento y las operaciones entre ellos (sin egui).
#[derive(Clone, Debug, Default)]
pub(crate) struct Blocks {
    pub(crate) chunks: Vec<Chunk>,
    next_uid: u64,
}

/// Dónde poner el cursor: bloque (`uid`) y posición en caracteres.
pub(crate) type Focus = (u64, usize);

impl Blocks {
    pub(crate) fn new(text: &str) -> Blocks {
        let mut b = Blocks::default();
        b.chunks = b.pieces(text, false);
        if b.chunks.is_empty() {
            let c = b.chunk(String::new(), String::new(), true);
            b.chunks.push(c);
        }
        b
    }

    fn chunk(&mut self, content: String, sep: String, born: bool) -> Chunk {
        self.next_uid = self.next_uid.wrapping_add(1);
        Chunk { uid: self.next_uid, content, sep, born, height: 0.0, width: 0.0 }
    }

    fn pieces(&mut self, text: &str, born: bool) -> Vec<Chunk> {
        iure_doc::chunks(text)
            .into_iter()
            .map(|r| {
                let (c, s) = split_ws(text.get(r).unwrap_or(""));
                self.chunk(c, s, born)
            })
            .collect()
    }

    /// El documento.
    pub(crate) fn text(&self) -> String {
        let mut out = String::with_capacity(self.chunks.iter().map(|c| c.content.len() + c.sep.len()).sum());
        for c in &self.chunks {
            if c.born {
                if c.content.is_empty() {
                    continue;
                }
                if !out.is_empty() {
                    while !out.ends_with("\n\n") {
                        out.push('\n');
                    }
                }
            }
            out.push_str(&c.content);
            out.push_str(&c.sep);
        }
        out
    }

    /// El bloque `i` cambió (el cursor quedó en `cursor`, en caracteres): se vuelve a partir por si
    /// ahora son varios. Devuelve dónde queda el cursor.
    pub(crate) fn edited(&mut self, i: usize, cursor: usize) -> Option<Focus> {
        let old = self.chunks.get(i)?;
        let (uid, born, height, width) = (old.uid, old.born, old.height, old.width);
        let text = format!("{}{}", old.content, old.sep);
        let mut pieces = self.pieces(&text, born);
        if pieces.is_empty() {
            // Quedó vacío: se queda el bloque, sin texto.
            let c = self.chunks.get_mut(i)?;
            c.content.clear();
            return Some((uid, 0));
        }
        if let Some(p) = pieces.first_mut() {
            p.uid = uid;
            p.height = height;
            p.width = width;
        }
        // El cursor, contado sobre el texto entero, cae en uno de los trozos.
        let mut left = cursor;
        let mut target = None;
        for p in &pieces {
            let n = p.content.chars().count();
            let total = n.saturating_add(p.sep.chars().count());
            if left <= n {
                target = Some((p.uid, left));
                break;
            }
            if left < total {
                target = Some((p.uid, n));
                break;
            }
            left -= total;
        }
        let target = target.or_else(|| pieces.last().map(|p| (p.uid, p.content.chars().count())));
        self.chunks.splice(i..=i, pieces);
        target
    }

    /// Intro al final del bloque `i`: uno nuevo y vacío debajo.
    pub(crate) fn break_after(&mut self, i: usize) -> Option<Focus> {
        if i >= self.chunks.len() || self.chunks.len() >= iure_doc::MAX_BLOCKS {
            return None;
        }
        let c = self.chunk(String::new(), "\n\n".to_string(), true);
        let uid = c.uid;
        self.chunks.insert(i + 1, c);
        Some((uid, 0))
    }

    /// Retroceso al inicio del bloque `i`: se une con el anterior.
    pub(crate) fn merge_prev(&mut self, i: usize) -> Option<Focus> {
        let p = i.checked_sub(1)?;
        let cur = self.chunks.get(i)?.clone();
        let prev = self.chunks.get(p)?.clone();
        if cur.born && cur.content.is_empty() {
            self.chunks.remove(i);
            return Some((prev.uid, prev.content.chars().count()));
        }
        if prev.born && prev.content.is_empty() {
            self.chunks.remove(p);
            return Some((cur.uid, 0));
        }
        let at = prev.content.chars().count();
        let c = self.chunks.get_mut(p)?;
        c.content.push_str(&cur.content);
        c.sep = cur.sep;
        c.born = prev.born && cur.born;
        self.chunks.remove(i);
        self.edited(p, at)
    }

    /// Supr al final del bloque `i`: se une con el siguiente.
    pub(crate) fn merge_next(&mut self, i: usize) -> Option<Focus> {
        if i + 1 >= self.chunks.len() {
            return None;
        }
        self.merge_prev(i + 1)
    }

    /// Caracteres del documento antes del bloque `uid`.
    pub(crate) fn offset_of(&self, uid: u64) -> usize {
        let mut n = 0usize;
        for c in &self.chunks {
            if c.uid == uid {
                break;
            }
            n = n.saturating_add(c.content.chars().count()).saturating_add(c.sep.chars().count());
        }
        n
    }

    /// El bloque y la posición de un carácter del documento (en su texto, no en su espacio final).
    pub(crate) fn at_offset(&self, offset: usize) -> Focus {
        let mut left = offset;
        let mut last = (0, 0);
        for c in &self.chunks {
            let n = c.content.chars().count();
            let total = n.saturating_add(c.sep.chars().count());
            if left < total || left <= n {
                return (c.uid, left.min(n));
            }
            left -= total;
            last = (c.uid, n);
        }
        last
    }

    fn index_of(&self, uid: u64) -> Option<usize> {
        self.chunks.iter().position(|c| c.uid == uid)
    }
}

/// Estado del editor entre cuadros.
#[derive(Clone, Default)]
struct State {
    blocks: Blocks,
    /// Huella del texto que el editor conoce (si el anfitrión lo cambia, se vuelve a partir).
    hash: u64,
    /// Cursor pendiente de poner (tras partir o unir bloques).
    focus: Option<Focus>,
}

fn hash_str(s: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// Alto estimado de un bloque que aún no se dibuja.
fn estimate(content: &str, row_h: f32, width: f32, size: f32) -> f32 {
    let per_row = (width / (size * 0.55)).max(10.0) as usize;
    let rows: usize = content.split('\n').take(10_000).map(|l| l.chars().count() / per_row.max(1) + 1).sum();
    let scale = if content.starts_with('#') { 1.6 } else { 1.0 };
    rows as f32 * row_h * scale
}

/// ¿Intro al final de `content` cierra el bloque? (No dentro de una cerca de código abierta.)
fn closes(content: &str) -> bool {
    !content.trim().is_empty() && iure_doc::chunks(&format!("{content}\n\nx")).len() >= 2
}

/// Lo que pide una tecla entre bloques.
enum Act {
    Edited(usize, usize),
    Break(usize),
    MergePrev(usize),
    MergeNext(usize),
    Focus(Focus),
}

/// La tecla entre bloques que toca en el bloque `i` (antes de que la vea su campo).
fn block_key(ui: &Ui, fid: Id, blocks: &Blocks, i: usize) -> Option<Act> {
    let c = blocks.chunks.get(i)?;
    let range = TextEditState::load(ui.ctx(), fid)?.cursor.char_range()?;
    if range.primary.index != range.secondary.index {
        return None;
    }
    let p = range.primary.index.0;
    let len = c.content.chars().count();
    let row = cached_galley(ui, fid).map(|g| {
        let r = g.layout_from_cursor(CCursor::new(p)).row;
        (r == 0, r + 1 >= g.rows.len())
    });
    let (first_row, last_row) = row.unwrap_or((true, true));
    let prev = i.checked_sub(1).and_then(|j| blocks.chunks.get(j));
    let next = blocks.chunks.get(i + 1);
    ui.input_mut(|inp| {
        if !inp.modifiers.is_none() {
            return None;
        }
        let mut take = |k: Key| inp.consume_key(egui::Modifiers::NONE, k);
        if p == len && edit::on_enter(&c.content, p).is_none() && closes(&c.content) && take(Key::Enter) {
            return Some(Act::Break(i));
        }
        if p == 0 && prev.is_some() && take(Key::Backspace) {
            return Some(Act::MergePrev(i));
        }
        if p == len && next.is_some() && take(Key::Delete) {
            return Some(Act::MergeNext(i));
        }
        if let Some(pr) = prev
            && ((first_row && take(Key::ArrowUp)) || (p == 0 && take(Key::ArrowLeft)))
        {
            return Some(Act::Focus((pr.uid, pr.content.chars().count())));
        }
        if let Some(nx) = next
            && ((last_row && take(Key::ArrowDown)) || (p == len && take(Key::ArrowRight)))
        {
            return Some(Act::Focus((nx.uid, 0)));
        }
        None
    })
}

/// El editor por bloques, con su propio desplazamiento. `id` identifica su estado entre cuadros.
pub fn show(ui: &mut Ui, id: Id, text: &mut String, hint: &str) -> Output {
    let style = EditorStyle::from_ui(ui);
    let skey = style_key(&style);
    let h = hash_str(text);
    let mut st = ui.data_mut(|d| d.remove_temp::<State>(id)).unwrap_or_default();
    if st.hash != h || st.blocks.chunks.is_empty() {
        // El anfitrión cambió el texto: se vuelve a partir y el cursor sigue en el mismo punto.
        let keep = st.blocks.chunks.iter().find_map(|c| {
            let fid = id.with(("block", c.uid));
            ui.memory(|m| m.has_focus(fid)).then(|| {
                let at = TextEditState::load(ui.ctx(), fid)
                    .and_then(|s| s.cursor.char_range())
                    .map_or(0, |r| r.primary.index.0);
                st.blocks.offset_of(c.uid).saturating_add(at)
            })
        });
        st.blocks = Blocks::new(text);
        st.hash = h;
        st.focus = keep.map(|g| st.blocks.at_offset(g));
    }
    let row_h = (style.size * 1.35).max(1.0);
    let gap = (style.size * 0.6).round();
    let mut act: Option<Act> = None;
    let mut resp: Option<egui::Response> = None;
    egui::ScrollArea::vertical().id_salt(id.with("scroll")).auto_shrink([false, false]).show_viewport(
        ui,
        |ui, viewport| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let width = ui.available_width();
            // Se dibuja una pantalla más arriba y abajo: desplazar no muestra huecos.
            let margin = viewport.height();
            let mut y = 0.0f32;
            let mut space = 0.0f32;
            let only = st.blocks.chunks.len() == 1;
            for i in 0..st.blocks.chunks.len() {
                let pending = st.focus;
                let Some(c) = st.blocks.chunks.get_mut(i) else { break };
                if (c.width - width).abs() >= 1.0 {
                    c.height = estimate(&c.content, row_h, width, style.size);
                    c.width = width;
                }
                let fid = id.with(("block", c.uid));
                let g = if c.born || c.sep.matches('\n').count() >= 2 { gap } else { 0.0 };
                let wanted = pending.and_then(|(u, at)| (u == c.uid).then_some(at));
                let focused = ui.memory(|m| m.has_focus(fid));
                let seen = y + c.height + g >= viewport.min.y - margin && y <= viewport.max.y + margin;
                if !(seen || focused || wanted.is_some()) {
                    space += c.height + g;
                    y += c.height + g;
                    continue;
                }
                if space > 0.0 {
                    ui.add_space(space);
                    space = 0.0;
                }
                if let Some(at) = wanted {
                    let mut s = TextEditState::load(ui.ctx(), fid).unwrap_or_default();
                    s.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(at))));
                    s.store(ui.ctx(), fid);
                    ui.memory_mut(|m| m.request_focus(fid));
                }
                if focused && act.is_none() {
                    act = block_key(ui, fid, &st.blocks, i);
                }
                let Some(c) = st.blocks.chunks.get_mut(i) else { break };
                let hint = if only && c.content.is_empty() { hint } else { "" };
                let (changed, out) = field(ui, fid, &mut c.content, hint, 1, &style, skey);
                c.height = out.response.response.rect.height();
                // Cada bloque con nombre para lectores de pantalla (el anfitrión nombra el que devuelve).
                let pos = i + 1;
                out.response.response.widget_info(|| {
                    let label = format!("{} {pos}", iurefficient_ui::lang::pick("Block", "Bloque"));
                    egui::WidgetInfo::labeled(egui::Role::MultilineTextInput, true, label)
                });
                if changed && act.is_none() {
                    let at = out.state.cursor.char_range().map_or(0, |r| r.primary.index.0);
                    act = Some(Act::Edited(i, at));
                }
                if wanted.is_some() {
                    out.response.response.scroll_to_me(None);
                    st.focus = None;
                }
                if g > 0.0 {
                    ui.add_space(g);
                }
                y += c.height + g;
                if focused || resp.is_none() {
                    resp = Some(out.response.response);
                }
            }
            if space > 0.0 {
                ui.add_space(space);
            }
        },
    );
    let mut changed = false;
    if let Some(a) = act {
        let (focus, edits) = match a {
            Act::Edited(i, at) => (st.blocks.edited(i, at), true),
            Act::Break(i) => (st.blocks.break_after(i), true),
            Act::MergePrev(i) => (st.blocks.merge_prev(i), true),
            Act::MergeNext(i) => (st.blocks.merge_next(i), true),
            Act::Focus(f) => (Some(f), false),
        };
        // Un cursor en un bloque que ya no existe no se pone.
        st.focus = focus.filter(|(u, _)| st.blocks.index_of(*u).is_some());
        if edits {
            let new = st.blocks.text();
            changed = new != *text;
            *text = new;
            st.hash = hash_str(text);
        }
        ui.ctx().request_repaint();
    }
    ui.data_mut(|d| d.insert_temp(id, st));
    let response = resp.unwrap_or_else(|| ui.allocate_response(egui::Vec2::ZERO, egui::Sense::hover()));
    Output { changed, response }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn contents(b: &Blocks) -> Vec<&str> {
        b.chunks.iter().map(|c| c.content.as_str()).collect()
    }

    #[test]
    fn splits_into_blocks_and_joins_back() {
        let t = "# Título\n\nUno\ndos  \n\n- a\n- b\n\n```\nx\n\ny\n```\n";
        let b = Blocks::new(t);
        assert_eq!(contents(&b), ["# Título", "Uno\ndos  ", "- a\n- b", "```\nx\n\ny\n```"]);
        assert_eq!(b.text(), t);
        assert_eq!(Blocks::new("").text(), "");
        assert_eq!(Blocks::new("a\r\n\r\nb").text(), "a\r\n\r\nb");
    }

    #[test]
    fn two_enters_split_a_block() {
        let mut b = Blocks::new("uno dos\n\ntres\n");
        // «uno» Intro Intro «dos»: el cursor queda al inicio de «dos».
        if let Some(c) = b.chunks.get_mut(0) {
            c.content = "uno\n\ndos".into();
        }
        let f = b.edited(0, 5);
        assert_eq!(contents(&b), ["uno", "dos", "tres"]);
        assert_eq!(f, b.chunks.get(1).map(|c| (c.uid, 0)));
        assert_eq!(b.text(), "uno\n\ndos\n\ntres\n");
    }

    #[test]
    fn enter_at_the_end_opens_a_block_that_writes_nothing_while_empty() {
        let mut b = Blocks::new("uno\n\ntres");
        let f = b.break_after(0);
        assert_eq!(b.text(), "uno\n\ntres", "vacío no cambia el documento");
        let uid = f.map(|f| f.0);
        if let Some(c) = b.chunks.get_mut(1) {
            c.content = "dos".into();
        }
        b.edited(1, 3);
        assert_eq!(b.text(), "uno\n\ndos\n\ntres");
        // Al final de un documento sin salto final.
        let mut b = Blocks::new("uno");
        b.break_after(0);
        if let Some(c) = b.chunks.get_mut(1) {
            c.content = "dos".into();
        }
        assert_eq!(b.text(), "uno\n\ndos\n\n");
        assert!(uid.is_some());
    }

    #[test]
    fn backspace_and_delete_join_blocks() {
        let mut b = Blocks::new("uno\n\ndos\n\ntres\n");
        let f = b.merge_prev(1);
        assert_eq!(contents(&b), ["unodos", "tres"]);
        assert_eq!(f, b.chunks.first().map(|c| (c.uid, 3)));
        let f = b.merge_next(0);
        assert_eq!(contents(&b), ["unodostres"]);
        assert_eq!(f, b.chunks.first().map(|c| (c.uid, 6)));
        assert_eq!(b.text(), "unodostres\n");
        assert_eq!(b.merge_prev(0), None);
        assert_eq!(b.merge_next(0), None);
        // Un bloque nuevo y vacío solo se quita.
        let mut b = Blocks::new("uno\n");
        b.break_after(0);
        let f = b.merge_prev(1);
        assert_eq!(contents(&b), ["uno"]);
        assert_eq!(f, b.chunks.first().map(|c| (c.uid, 3)));
    }

    #[test]
    fn offsets_find_the_same_point_after_reparsing() {
        let b = Blocks::new("uno\n\ndós\n\ntres");
        let dos = b.chunks.get(1).map_or(0, |c| c.uid);
        assert_eq!(b.offset_of(dos), 5);
        let again = Blocks::new("uno\n\ndós\n\ntres");
        assert_eq!(again.at_offset(5 + 2), again.chunks.get(1).map_or((0, 0), |c| (c.uid, 2)));
        // En el espacio entre bloques: al final del texto del anterior.
        assert_eq!(again.at_offset(4), again.chunks.first().map_or((0, 0), |c| (c.uid, 3)));
        // Más allá del final: al final del último.
        assert_eq!(again.at_offset(999), again.chunks.get(2).map_or((0, 0), |c| (c.uid, 4)));
    }

    #[test]
    fn enter_closes_paragraphs_but_not_open_fences() {
        assert!(closes("párrafo"));
        assert!(closes("# Título"));
        assert!(!closes("```rust\nfn f() {}"));
        assert!(!closes(""));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]
        /// Partir en bloques y juntar nunca cambia el documento.
        #[test]
        fn blocks_are_lossless(md in "(\\PC|#|> |- |```|\\| |---|\n|  |\r\n){0,200}") {
            prop_assert_eq!(Blocks::new(&md).text(), md);
        }

        /// Editar un bloque (y volver a partirlo) solo cambia ese trozo del documento.
        #[test]
        fn editing_one_block_keeps_the_rest(md in "(\\PC|#|- |```|\n){0,120}", ins in "(\\PC|\n){0,8}") {
            let mut b = Blocks::new(&md);
            let n = b.chunks.len();
            let i = n / 2;
            let before: String = b.chunks.iter().take(i).map(|c| format!("{}{}", c.content, c.sep)).collect();
            let after: String = b.chunks.iter().skip(i + 1).map(|c| format!("{}{}", c.content, c.sep)).collect();
            let mut cursor = 0;
            if let Some(c) = b.chunks.get_mut(i) {
                c.content.push_str(&ins);
                cursor = c.content.chars().count();
            }
            b.edited(i, cursor);
            let t = b.text();
            prop_assert!(t.starts_with(&before));
            prop_assert!(t.ends_with(&after));
        }
    }
}

#[cfg(test)]
mod perf {
    use super::*;

    /// Unas 100 páginas por bloques: teclear rehace un bloque, no el documento.
    #[test]
    fn hundred_pages_by_blocks() {
        let page = "## Sección\n\nTexto con **negrita**, *cursiva*, `código` y [enlace](https://x.mx).\n\n- uno\n- [ ] dos\n  - tres\n\n> cita\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n```rust\nfn f() {}\n```\n\n";
        let mut text = page.repeat(100 * 15);
        let ctx = egui::Context::default();
        let input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 700.0))),
            ..Default::default()
        };
        let id = Id::unique("perf-blocks");
        let frame = |text: &mut String| {
            let t = std::time::Instant::now();
            let mut out = ctx.run_ui(input(), |ui| {
                show(ui, id, text, "");
            });
            out.textures_delta.clear();
            t.elapsed().as_secs_f64() * 1000.0
        };
        let first = frame(&mut text);
        let steady = frame(&mut text);
        // Una tecla: el bloque 10 cambia (como lo haría su campo) y se rehace.
        let t = std::time::Instant::now();
        let mut st = ctx.data_mut(|d| d.remove_temp::<State>(id)).unwrap_or_default();
        if let Some(c) = st.blocks.chunks.get_mut(10) {
            c.content.push('x');
        }
        st.blocks.edited(10, 1);
        text = st.blocks.text();
        st.hash = hash_str(&text);
        ctx.data_mut(|d| d.insert_temp(id, st));
        let op = t.elapsed().as_secs_f64() * 1000.0;
        let typed = frame(&mut text);
        eprintln!(
            "por bloques, {} KB: primer cuadro {first:.1} ms, estable {steady:.1} ms, tecla {op:.1} ms + cuadro {typed:.1} ms",
            text.len() / 1024
        );
        assert!(steady < 1_000.0 && typed < 1_000.0);
    }
}
