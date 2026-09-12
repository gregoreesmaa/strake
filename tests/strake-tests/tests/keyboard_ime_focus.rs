//! Keyboard, focus-navigation, and IME regression pins.
//!
//! The input pipeline (`Harness::press/type_text/ime`, Tab focus traversal,
//! `handle_ime_event`) drives issues #10 (IME engine, caret, contenteditable)
//! and #20 (keyboard paths of DnD/print flows). These tests pin the behavior
//! that upcoming work must not regress. Known spec gaps deliberately NOT
//! pinned here: positive-`tabindex` prioritization (traversal is document
//! order) and Backspace/Delete editing keys (unhandled in `keyboard.rs`).

use keyboard_types::{Key, Modifiers};
use strake_test_harness::Harness;
use strake_traits::events::StrakeImeEvent;

fn three_inputs() -> Harness {
    Harness::from_html(
        r#"<html><body style="margin:0">
            <input id="a" type="text" style="width:200px; height:20px;">
            <input id="b" type="text" style="width:200px; height:20px;">
            <input id="c" type="text" style="width:200px; height:20px;">
        </body></html>"#,
    )
}

fn editor_text(harness: &Harness, selector: &str) -> String {
    let node_id = harness.node(selector);
    harness
        .base()
        .get_node(node_id)
        .unwrap()
        .element_data()
        .unwrap()
        .text_input_data()
        .unwrap()
        .editor
        .text()
        .to_string()
}

#[test]
fn tab_moves_focus_through_inputs_in_document_order() {
    let mut harness = three_inputs();
    harness.click("#a");
    assert_eq!(harness.focused(), Some(harness.node("#a")));

    harness.press(Key::Tab);
    assert_eq!(harness.focused(), Some(harness.node("#b")));

    harness.press(Key::Tab);
    assert_eq!(harness.focused(), Some(harness.node("#c")));
}

#[test]
fn shift_tab_moves_focus_backward() {
    let mut harness = three_inputs();
    harness.click("#c");
    assert_eq!(harness.focused(), Some(harness.node("#c")));

    harness.press_with(Key::Tab, Modifiers::SHIFT);
    assert_eq!(harness.focused(), Some(harness.node("#b")));
}

#[test]
fn tabindex_minus_one_is_skipped_by_tab() {
    let mut harness = Harness::from_html(
        r#"<html><body style="margin:0">
            <input id="a" type="text" style="width:200px; height:20px;">
            <input id="skip" type="text" tabindex="-1" style="width:200px; height:20px;">
            <input id="b" type="text" style="width:200px; height:20px;">
        </body></html>"#,
    );
    harness.click("#a");
    harness.press(Key::Tab);
    assert_eq!(
        harness.focused(),
        Some(harness.node("#b")),
        "tabindex=-1 must be excluded from sequential focus navigation"
    );
}

#[test]
fn type_text_inserts_into_focused_input() {
    let mut harness = three_inputs();
    harness.click("#b");
    harness.type_text("hi");
    assert_eq!(editor_text(&harness, "#b"), "hi");
    assert_eq!(editor_text(&harness, "#a"), "");
}

#[test]
fn ime_commit_inserts_text_into_focused_input() {
    let mut harness = three_inputs();
    harness.click("#a");
    harness.ime(StrakeImeEvent::Commit("hello".to_string()));
    assert_eq!(editor_text(&harness, "#a"), "hello");
}

#[test]
fn ime_events_without_text_focus_are_a_safe_noop() {
    let mut harness = three_inputs();
    // Whatever is focused on load (e.g. the body), it is not a text input.
    let initial_focus = harness.focused();
    harness.ime(StrakeImeEvent::Enabled);
    harness.ime(StrakeImeEvent::Preedit("mid".to_string(), None));
    harness.ime(StrakeImeEvent::Commit("x".to_string()));
    harness.ime(StrakeImeEvent::Disabled);
    // No crash, no focus change, and no text smuggled into any input.
    assert_eq!(harness.focused(), initial_focus);
    assert_eq!(editor_text(&harness, "#a"), "");
    assert_eq!(editor_text(&harness, "#b"), "");
}
