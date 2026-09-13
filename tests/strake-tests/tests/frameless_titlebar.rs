//! Pins for <https://github.com/gregoreesmaa/strake/issues/54>
//! (upstream <https://github.com/DioxusLabs/blitz/issues/481>):
//! DOM-driven frameless window controls — `data-drag` moves, edge bands
//! resize (with resize cursors), `data-minimize` / `data-maximize` /
//! `data-fullscreen` / `data-close` trigger controls, F11 toggles
//! fullscreen. All through `ShellProvider`, recorded here by a mock.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use keyboard_types::Key;
use strake_test_harness::{Harness, mouse_pointer_event};
use strake_traits::events::UiEvent;
use strake_traits::shell::{ResizeDirection, ShellProvider};

#[derive(Default)]
struct RecordingShell {
    calls: Mutex<Vec<String>>,
    decorated: AtomicBool,
    maximized: AtomicBool,
    fullscreen: AtomicBool,
}

impl RecordingShell {
    fn frameless() -> Arc<Self> {
        Arc::new(Self {
            decorated: AtomicBool::new(false),
            ..Default::default()
        })
    }

    fn decorated() -> Arc<Self> {
        Arc::new(Self {
            decorated: AtomicBool::new(true),
            ..Default::default()
        })
    }

    fn push(&self, call: String) {
        self.calls.lock().unwrap().push(call);
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

impl ShellProvider for RecordingShell {
    fn drag_window(&self) {
        self.push("drag_window".into());
    }
    fn drag_resize_window(&self, direction: ResizeDirection) {
        self.push(format!("drag_resize:{direction:?}"));
    }
    fn set_fullscreen(&self, fullscreen: bool) {
        self.fullscreen.store(fullscreen, Ordering::SeqCst);
        self.push(format!("set_fullscreen:{fullscreen}"));
    }
    fn is_window_decorated(&self) -> bool {
        self.decorated.load(Ordering::SeqCst)
    }
    fn is_window_maximized(&self) -> bool {
        self.maximized.load(Ordering::SeqCst)
    }
    fn is_window_fullscreen(&self) -> bool {
        self.fullscreen.load(Ordering::SeqCst)
    }
    fn set_window_minimized(&self, minimized: bool) {
        self.push(format!("set_minimized:{minimized}"));
    }
    fn set_window_maximized(&self, maximized: bool) {
        self.maximized.store(maximized, Ordering::SeqCst);
        self.push(format!("set_maximized:{maximized}"));
    }
    fn request_window_close(&self) {
        self.push("request_close".into());
    }
}

fn frameless_doc(html: &str) -> (Harness, Arc<RecordingShell>) {
    let mut harness = Harness::from_html(html);
    let shell = RecordingShell::frameless();
    harness.base_mut().set_shell_provider(shell.clone());
    (harness, shell)
}

fn press(harness: &mut Harness, x: f32, y: f32) -> Vec<String> {
    let down = mouse_pointer_event(x, y);
    harness.dispatch_recorded([UiEvent::PointerDown(down.clone()), UiEvent::PointerUp(down)])
}

const CHROME: &str = r#"<html><body style="margin:0">
    <div id="bar" data-drag style="width:800px; height:30px; background:#eee;">
        <button id="btn" style="width:80px; height:20px;">ok</button>
    </div>
    <div id="content" style="width:800px; height:570px;"></div>
</body></html>"#;

/// Pressing a `data-drag` region starts a window move and dispatches no click.
#[test]
fn drag_region_press_moves_window_without_click() {
    let (mut harness, shell) = frameless_doc(CHROME);
    let names = press(&mut harness, 400.0, 15.0);
    assert_eq!(shell.calls(), vec!["drag_window"]);
    assert!(
        !names.iter().any(|n| n == "click"),
        "drag press must not click: {names:?}"
    );
}

/// A focusable button inside the drag region keeps working: it clicks and
/// never starts a move.
#[test]
fn button_in_drag_region_clicks_without_move() {
    let (mut harness, shell) = frameless_doc(CHROME);
    let (x, y) = harness.center_of("#btn");
    let names = press(&mut harness, x, y);
    assert_eq!(shell.calls(), Vec::<String>::new());
    assert!(
        names.iter().any(|n| n == "click"),
        "button must still click: {names:?}"
    );
}

/// `data-nodrag` carves a non-draggable hole out of a drag region.
#[test]
fn nodrag_region_vetoes_move() {
    let (mut harness, shell) = frameless_doc(
        r#"<html><body style="margin:0">
            <div data-drag style="width:800px; height:30px;">
                <span id="hole" data-nodrag style="display:block; width:100px; height:30px;"></span>
            </div>
        </body></html>"#,
    );
    let (x, y) = harness.center_of("#hole");
    press(&mut harness, x, y);
    assert_eq!(shell.calls(), Vec::<String>::new());
}

/// Window-control markers trigger the provider without any focus veto.
#[test]
fn control_markers_trigger_provider() {
    let (mut harness, shell) = frameless_doc(
        r#"<html><body style="margin:0">
            <button id="min" data-minimize>min</button>
            <button id="max" data-maximize>max</button>
            <button id="full" data-fullscreen>full</button>
            <button id="close" data-close>close</button>
        </body></html>"#,
    );
    for (sel, want) in [
        ("#min", "set_minimized:true"),
        ("#max", "set_maximized:true"),
        ("#full", "set_fullscreen:true"),
        ("#close", "request_close"),
    ] {
        let (x, y) = harness.center_of(sel);
        press(&mut harness, x, y);
        assert!(
            shell.calls().iter().any(|c| c == want),
            "{sel} must trigger {want}: {:?}",
            shell.calls()
        );
    }
    // Maximize toggles back off on the second press.
    let (x, y) = harness.center_of("#max");
    press(&mut harness, x, y);
    assert!(shell.calls().iter().any(|c| c == "set_maximized:false"));
}

/// Edge/corner bands on an undecorated window start directional resizes.
#[test]
fn edge_bands_start_resize() {
    let (mut harness, shell) = frameless_doc(
        r#"<html><body style="margin:0"><div style="width:800px; height:600px;"></div></body></html>"#,
    );
    press(&mut harness, 797.0, 300.0);
    press(&mut harness, 797.0, 597.0);
    press(&mut harness, 400.0, 597.0);
    assert_eq!(
        shell.calls(),
        vec![
            "drag_resize:East",
            "drag_resize:SouthEast",
            "drag_resize:South"
        ]
    );
}

/// Decorated windows ignore drag regions and edge bands entirely.
#[test]
fn decorated_window_ignores_chrome_gestures() {
    let mut harness = Harness::from_html(CHROME);
    let shell = RecordingShell::decorated();
    harness.base_mut().set_shell_provider(shell.clone());
    press(&mut harness, 400.0, 15.0);
    press(&mut harness, 797.0, 300.0);
    assert_eq!(shell.calls(), Vec::<String>::new());
}

/// Maximized windows neither move nor resize from content.
#[test]
fn maximized_window_ignores_move_and_resize() {
    let (mut harness, shell) = frameless_doc(CHROME);
    shell.maximized.store(true, Ordering::SeqCst);
    press(&mut harness, 400.0, 15.0);
    press(&mut harness, 797.0, 300.0);
    assert_eq!(shell.calls(), Vec::<String>::new());
}

/// F11 toggles borderless fullscreen from anywhere.
#[test]
fn f11_toggles_fullscreen() {
    let (mut harness, shell) = frameless_doc(CHROME);
    harness.press(Key::F11);
    assert_eq!(shell.calls(), vec!["set_fullscreen:true"]);
    harness.press(Key::F11);
    assert_eq!(
        shell.calls(),
        vec!["set_fullscreen:true", "set_fullscreen:false"]
    );
}

/// Hovering an undecorated edge band shows the resize cursor.
#[test]
fn edge_hover_shows_resize_cursor() {
    let (mut harness, _shell) = frameless_doc(
        r#"<html><body style="margin:0"><div style="width:800px; height:600px;"></div></body></html>"#,
    );
    let cursor_at = |harness: &mut Harness, x: f32, y: f32| {
        harness.dispatch(UiEvent::PointerMove(mouse_pointer_event(x, y)));
        harness.pump();
        format!("{:?}", harness.base().get_cursor())
    };
    assert_eq!(cursor_at(&mut harness, 797.0, 300.0), "Some(EResize)");
    assert_eq!(cursor_at(&mut harness, 400.0, 300.0), "Some(Default)");
}
