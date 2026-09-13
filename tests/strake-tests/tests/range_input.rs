//! Regression pins for <https://github.com/gregoreesmaa/strake/issues/456>:
//! `<input type="range">` renders a slider and responds to pointer drags
//! and keyboard input.

use anyrender::render_to_buffer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use keyboard_types::{Key, Modifiers};
use std::sync::Arc;
use strake_dom::DocumentConfig;
use strake_html::{HtmlDocument, HtmlProvider};
use strake_paint::paint_scene;
use strake_test_harness::{Harness, key_event, mouse_pointer_event, pointer_event};
use strake_traits::events::{KeyState, UiEvent};
use strake_traits::events::{MouseEventButton, MouseEventButtons};
use strake_traits::shell::{ColorScheme, Viewport};

const SLIDER: &str = r#"<input id="s" type="range" min="0" max="100" step="1" value="25"
    style="width: 200px; height: 24px; margin: 0; border: 0; padding: 0; color: rgb(255, 0, 0);">"#;

fn slider_doc() -> Harness {
    Harness::from_html(&format!(
        r#"<html><body style="margin:0">{SLIDER}</body></html>"#
    ))
}

fn slider_value(harness: &Harness) -> f64 {
    let id = harness.node("#s");
    harness
        .base()
        .get_node(id)
        .unwrap()
        .element_data()
        .unwrap()
        .range_input_value()
        .expect("range slider state")
}

fn pixel(html: &str, x: u32, y: u32) -> [u8; 3] {
    let mut doc = HtmlDocument::from_html(
        html,
        DocumentConfig {
            viewport: Some(Viewport::new(300, 100, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    let buffer = render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| paint_scene(scene, &mut doc, 1.0, 300, 100, 0, 0),
        300,
        100,
    );
    let idx = ((y * 300 + x) * 4) as usize;
    [buffer[idx], buffer[idx + 1], buffer[idx + 2]]
}

/// The slider paints a track, an accent fill up to the thumb, and the thumb
/// itself at the value fraction (25% of a 200px slider).
#[test]
fn range_renders_track_fill_and_thumb() {
    let html = format!(r#"<html><body style="margin:0">{SLIDER}</body></html>"#);
    // Thumb center: radius 9, travel 182, fraction 0.25 -> x ~= 54, y = 12.
    // The thumb is a white disc with an accent ring.
    assert_eq!(pixel(&html, 54, 12), [255, 255, 255], "thumb body");
    assert_eq!(pixel(&html, 63, 12), [255, 0, 0], "thumb ring accent");
    assert_eq!(pixel(&html, 30, 12), [255, 0, 0], "fill must be accent");
    assert_eq!(
        pixel(&html, 150, 12),
        [200, 200, 200],
        "unfilled track must be gray"
    );
}

/// Pointer drag sets the value from the position and fires `input` events.
#[test]
fn range_drag_sets_value_and_fires_input() {
    let mut harness = slider_doc();
    assert_eq!(slider_value(&harness), 25.0);

    // Thumb travel is inset by the 9px thumb radius (182px over the 200px
    // slider): x=145.5 seeks to 75, x=54.5 back to 25.
    let down = mouse_pointer_event(145.5, 12.0);
    let drag = pointer_event(
        strake_traits::events::StrakePointerId::Mouse,
        54.5,
        12.0,
        MouseEventButton::Main,
        MouseEventButtons::from(MouseEventButton::Main),
        Modifiers::default(),
    );
    let up = mouse_pointer_event(54.5, 12.0);
    let names = harness.dispatch_recorded([
        UiEvent::PointerDown(down),
        UiEvent::PointerMove(drag),
        UiEvent::PointerUp(up),
    ]);
    let inputs = names.iter().filter(|n| n.as_str() == "input").count();
    assert_eq!(inputs, 2, "press and drag must each fire input");
    assert_eq!(slider_value(&harness), 25.0);
}

/// Pressing the slider focuses it (and keeps focus through the click).
#[test]
fn range_press_focuses_slider() {
    let mut harness = slider_doc();
    harness.click("#s");
    assert_eq!(harness.focused(), Some(harness.node("#s")));
}

/// Arrow keys step the focused slider; Home/End jump to the ends.
#[test]
fn range_keyboard_steps_value() {
    let mut harness = slider_doc();
    // Clicking the slider center seeks it there first (correct behavior).
    harness.click("#s");
    assert_eq!(slider_value(&harness), 50.0);

    let key = |k: Key| UiEvent::KeyDown(key_event(k, KeyState::Pressed, Modifiers::default()));
    harness.dispatch(key(Key::ArrowRight));
    harness.pump();
    assert_eq!(slider_value(&harness), 51.0);

    harness.dispatch(key(Key::Home));
    harness.pump();
    assert_eq!(slider_value(&harness), 0.0);

    harness.dispatch(key(Key::End));
    harness.pump();
    assert_eq!(slider_value(&harness), 100.0);
}

/// Unparseable values fall back to the midpoint; out-of-range values clamp;
// min above max collapses the range; non-positive steps reset to 1.
#[test]
fn range_value_sanitization() {
    for (attrs, expected) in [
        (r#"value="abc""#, 50.0),
        (r#"value="250""#, 100.0),
        (r#"value="-30""#, 0.0),
        (r#"min="80" max="20""#, 80.0),
    ] {
        let harness = Harness::from_html(&format!(
            r#"<html><body style="margin:0"><input id="s" type="range" {attrs}></body></html>"#
        ));
        assert_eq!(slider_value(&harness), expected, "attrs {attrs}");
    }
}
