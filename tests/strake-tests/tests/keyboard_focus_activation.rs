//! Regression pins for <https://github.com/gregoreesmaa/strake/issues/70>
//! (upstream <https://github.com/DioxusLabs/blitz/issues/839>): keyboard
//! interaction — `:focus-visible` modality, focus repaint, and Enter/Space
//! activation.

use keyboard_types::{Key, Modifiers};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use strake_test_harness::{Harness, key_event};
use strake_traits::events::{KeyState, UiEvent};

fn button_doc() -> Harness {
    Harness::from_html(
        r#"<html><body style="margin:0">
            <button id="b" style="width:120px; height:30px;">click me</button>
        </body></html>"#,
    )
}

fn enter_event(state: KeyState) -> UiEvent {
    let event = key_event(Key::Enter, state, Modifiers::default());
    match state {
        KeyState::Pressed => UiEvent::KeyDown(event),
        KeyState::Released => UiEvent::KeyUp(event),
    }
}

fn space_event(state: KeyState) -> UiEvent {
    let event = key_event(Key::Character(" ".to_string()), state, Modifiers::default());
    match state {
        KeyState::Pressed => UiEvent::KeyDown(event),
        KeyState::Released => UiEvent::KeyUp(event),
    }
}

fn clicks(harness: &mut Harness, events: Vec<UiEvent>) -> usize {
    harness
        .dispatch_recorded(events)
        .iter()
        .filter(|name| name.as_str() == "click")
        .count()
}

/// After keyboard-driven focus, `:focus-visible` matches the focused button.
#[test]
fn tab_focus_matches_focus_visible() {
    let mut harness = button_doc();
    harness.press(Key::Tab);
    let button = harness.node("#b");
    assert_eq!(harness.focused(), Some(button));

    let matched = harness
        .base()
        .query_selector("button:focus-visible")
        .unwrap();
    assert_eq!(
        matched,
        Some(button),
        "Tab-focused button must match :focus-visible"
    );
}

/// Pointer-driven focus must NOT match `:focus-visible`: the ring follows
/// the last interaction modality, and a click disarms it.
#[test]
fn click_focus_does_not_match_focus_visible() {
    let mut harness = button_doc();
    harness.click("#b");
    let button = harness.node("#b");
    assert_eq!(harness.focused(), Some(button));

    // Plain :focus still matches; only the visible ring is modality-gated.
    assert_eq!(
        harness.base().query_selector("button:focus").unwrap(),
        Some(button)
    );
    assert_eq!(
        harness
            .base()
            .query_selector("button:focus-visible")
            .unwrap(),
        None,
        "click-focused button must not match :focus-visible"
    );
}

/// Moving the mouse is not an interaction: it must not disarm a
/// keyboard-armed ring.
#[test]
fn mouse_move_keeps_keyboard_armed_ring() {
    let mut harness = button_doc();
    harness.press(Key::Tab);
    harness.move_mouse_to(5.0, 5.0);
    let button = harness.node("#b");
    assert_eq!(
        harness
            .base()
            .query_selector("button:focus-visible")
            .unwrap(),
        Some(button),
        "pointer movement must not disarm :focus-visible"
    );
}

#[derive(Default)]
struct CountingShell {
    redraws: AtomicUsize,
}

impl strake_traits::shell::ShellProvider for CountingShell {
    fn request_redraw(&self) {
        self.redraws.fetch_add(1, Ordering::SeqCst);
    }
}

/// Tab traversal requests a frame so the focus ring paints.
#[test]
fn tab_focus_requests_redraw() {
    let mut harness = button_doc();
    let counter = Arc::new(CountingShell::default());
    harness
        .base_mut()
        .set_shell_provider(counter.clone() as Arc<dyn strake_traits::shell::ShellProvider>);
    harness.pump();
    let before = counter.redraws.load(Ordering::SeqCst);

    harness.press(Key::Tab);

    assert_eq!(harness.focused(), Some(harness.node("#b")));
    assert!(
        counter.redraws.load(Ordering::SeqCst) > before,
        "keyboard focus change must request a redraw"
    );
}

/// Enter activates the focused button on key down (exactly once per press).
#[test]
fn enter_activates_focused_button_on_key_down() {
    let mut harness = button_doc();
    harness.press(Key::Tab);

    let down_only = clicks(&mut harness, vec![enter_event(KeyState::Pressed)]);
    assert_eq!(down_only, 1, "Enter key-down must click once");

    let up_only = clicks(&mut harness, vec![enter_event(KeyState::Released)]);
    assert_eq!(up_only, 0, "Enter key-up must not click again");
}

/// Space activates on key *up* so holding Space does not repeat-activate.
#[test]
fn space_activates_focused_button_on_key_up_only() {
    let mut harness = button_doc();
    harness.press(Key::Tab);

    let down_only = clicks(&mut harness, vec![space_event(KeyState::Pressed)]);
    assert_eq!(down_only, 0, "Space key-down must not click");

    let up_only = clicks(&mut harness, vec![space_event(KeyState::Released)]);
    assert_eq!(up_only, 1, "Space key-up must click once");
}

/// Text inputs consume Space for editing: no activation, text appears.
#[test]
fn space_in_text_input_edits_without_click() {
    let mut harness = Harness::from_html(
        r#"<html><body style="margin:0">
            <input id="t" type="text" style="width:200px; height:20px;">
        </body></html>"#,
    );
    harness.click("#t");

    let n = clicks(
        &mut harness,
        vec![
            space_event(KeyState::Pressed),
            space_event(KeyState::Released),
        ],
    );
    assert_eq!(n, 0, "Space in a text input must not click");

    let input = harness.node("#t");
    let text = harness
        .base()
        .get_node(input)
        .unwrap()
        .element_data()
        .unwrap()
        .text_input_data()
        .unwrap()
        .editor
        .text()
        .to_string();
    assert_eq!(text, " ");
}
