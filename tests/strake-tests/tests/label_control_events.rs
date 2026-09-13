//! Regression pin for <https://github.com/gregoreesmaa/strake/issues/471>
//! (upstream <https://github.com/DioxusLabs/blitz/issues/471>): clicking a
//! `<label>` forwards activation to its associated form control, toggling a
//! checkbox both ways and dispatching its click event.

use strake_test_harness::{Harness, mouse_pointer_event};
use strake_traits::events::UiEvent;

fn label_doc() -> Harness {
    Harness::from_html(
        r#"<html><body style="margin:0">
            <input id="c" type="checkbox" style="width:20px; height:20px;">
            <label id="l" for="c" style="display:block; width:200px; height:30px;">check it</label>
        </body></html>"#,
    )
}

fn is_checked(harness: &Harness) -> bool {
    let input = harness.node("#c");
    harness
        .base()
        .get_node(input)
        .unwrap()
        .element_data()
        .is_some_and(|e| format!("{:?}", e.element_state).contains("CHECKED"))
}

fn click_label(harness: &mut Harness) -> usize {
    let (x, y) = harness.center_of("#l");
    let down = mouse_pointer_event(x, y);
    let clicks = harness
        .dispatch_recorded([UiEvent::PointerDown(down.clone()), UiEvent::PointerUp(down)])
        .iter()
        .filter(|name| name.as_str() == "click")
        .count();
    harness.pump();
    clicks
}

#[test]
fn label_click_checks_checkbox_and_dispatches_click() {
    let mut harness = label_doc();
    assert!(!is_checked(&harness));

    let clicks = click_label(&mut harness);
    assert_eq!(clicks, 1, "label activation must click the control once");
    assert!(is_checked(&harness), "label click must check the box");
}

#[test]
fn label_click_toggles_checkbox_off() {
    let mut harness = label_doc();
    click_label(&mut harness);
    assert!(is_checked(&harness));

    click_label(&mut harness);
    assert!(!is_checked(&harness), "second label click must uncheck");
}
