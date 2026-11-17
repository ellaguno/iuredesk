//! S1.12 con egui_kittest: las teclas del editor sobre un texto real.

use egui::text::{CCursor, CCursorRange};
use egui::text_edit::TextEditState;
use egui::{Id, Key, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

struct State {
    text: String,
}

fn harness(text: &str) -> Harness<'static, State> {
    Harness::builder().with_size(egui::vec2(600.0, 400.0)).build_ui_state(
        |ui, st: &mut State| {
            let out = iure_doc_ui::show(ui, Id::unique("ed"), &mut st.text, "Escribe");
            out.response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::MultilineTextInput, true, "editor"));
        },
        State { text: text.to_string() },
    )
}

/// Enfoca el editor y pone el cursor en `at` (índice de carácter).
fn cursor_at(h: &mut Harness<'static, State>, at: usize) {
    h.get_by_label("editor").focus();
    h.run();
    let ctx = h.ctx.clone();
    let id = Id::unique("ed");
    let mut st = TextEditState::load(&ctx, id).unwrap_or_default();
    st.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(at))));
    st.store(&ctx, id);
    h.run();
}

#[test]
fn enter_continues_a_list_and_leaves_it_when_empty() {
    let mut h = harness("- uno");
    cursor_at(&mut h, 5);
    h.key_press(Key::Enter);
    h.run();
    assert_eq!(h.state().text, "- uno\n- ");
    h.key_press(Key::Enter);
    h.run();
    assert_eq!(h.state().text, "- uno\n", "Intro en un elemento vacío sale de la lista");
}

#[test]
fn tab_indents_and_ctrl_b_bolds() {
    let mut h = harness("- uno\n- dos");
    cursor_at(&mut h, 11);
    h.key_press(Key::Tab);
    h.run();
    assert_eq!(h.state().text, "- uno\n  - dos");
    h.key_press_modifiers(Modifiers::SHIFT, Key::Tab);
    h.run();
    assert_eq!(h.state().text, "- uno\n- dos");
    // Selección «dos» y Ctrl+B.
    let ctx = h.ctx.clone();
    let id = Id::unique("ed");
    let mut st = TextEditState::load(&ctx, id).unwrap_or_default();
    st.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(8), CCursor::new(11))));
    st.store(&ctx, id);
    h.run();
    h.key_press_modifiers(Modifiers::COMMAND, Key::B);
    h.run();
    assert_eq!(h.state().text, "- uno\n- **dos**");
}

#[test]
fn plain_enter_outside_lists_is_a_newline() {
    let mut h = harness("párrafo");
    cursor_at(&mut h, 7);
    h.key_press(Key::Enter);
    h.run();
    assert_eq!(h.state().text, "párrafo\n");
}
