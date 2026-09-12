//! `BrowserWindow`: option mapping plus headless-testable window state.
//!
//! [`BrowserWindowOptions`] mirrors Electron's
//! `BrowserWindowConstructorOptions` subset for the MVP (geometry, frame,
//! transparency, background, title); [`WindowManager`] owns window lifetimes
//! and state transitions, firing `on_last_window_closed` so the embedder can
//! close the [`App`](crate::App) loop. Real `winit`/`strake-shell` windows
//! bind to these options in a follow-up without changing this contract.

use std::collections::HashMap;

use serde_json::Value;

use crate::IpcBus;

/// macOS title-bar modes (`titleBarStyle`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TitleBarStyle {
    /// Normal titled frame.
    #[default]
    Default,
    /// Frameless with traffic lights hidden.
    Hidden,
    /// Frameless with inset traffic lights.
    HiddenInset,
}

/// `BrowserWindowConstructorOptions` (MVP subset).
#[derive(Debug, Clone, PartialEq)]
pub struct BrowserWindowOptions {
    /// Content width in physical pixels (Electron default 800).
    pub width: u32,
    /// Content height in physical pixels (Electron default 600).
    pub height: u32,
    /// Minimum content size.
    pub min_size: Option<(u32, u32)>,
    /// Maximum content size.
    pub max_size: Option<(u32, u32)>,
    /// Initial top-left position (`None` = platform default).
    pub position: Option<(i32, i32)>,
    /// Window frame and title bar (Electron default true).
    pub frame: bool,
    /// `titleBarStyle` (macOS).
    pub title_bar_style: TitleBarStyle,
    /// Transparent background (Electron default false).
    pub transparent: bool,
    /// Background color shown before content loads, e.g. `"#1e1e1e"`.
    pub background_color: Option<String>,
    /// Show immediately after creation (Electron default true).
    pub show: bool,
    /// Window title.
    pub title: String,
}

impl Default for BrowserWindowOptions {
    fn default() -> Self {
        Self {
            width: 800,
            height: 600,
            min_size: None,
            max_size: None,
            position: None,
            frame: true,
            title_bar_style: TitleBarStyle::Default,
            transparent: false,
            background_color: None,
            show: true,
            title: String::new(),
        }
    }
}

/// Renderer surface of a window (`win.webContents`).
///
/// MVP: records the pending navigation target (`loadURL`/`loadFile`) and
/// delivers `send` into the [`IpcBus`] for renderer listeners.
/// `executeJavaScript`/DevTools/print need the renderer binding (deferred).
#[derive(Debug, Default)]
pub struct WebContents {
    pending_url: Option<String>,
}

impl WebContents {
    /// Navigate to a URL (`win.loadURL`).
    pub fn load_url(&mut self, url: &str) {
        self.pending_url = Some(url.to_string());
    }

    /// Load a local file (`win.loadFile`).
    pub fn load_file(&mut self, path: &str) {
        let url = if path.starts_with("file://") {
            path.to_string()
        } else {
            format!("file://{path}")
        };
        self.pending_url = Some(url);
    }

    /// Currently loading (or loaded) target, if any.
    pub fn pending_url(&self) -> Option<&str> {
        self.pending_url.as_deref()
    }

    /// `webContents.send(channel, value)`: deliver to renderer listeners.
    pub fn send(&self, bus: &IpcBus, channel: &str, value: Value) {
        bus.send(channel, value);
    }
}

/// One managed window: options, chrome state, and web contents.
#[derive(Debug)]
pub struct BrowserWindow {
    options: BrowserWindowOptions,
    visible: bool,
    minimized: bool,
    maximized: bool,
    focused: bool,
    always_on_top: bool,
    title: String,
    web_contents: WebContents,
}

impl BrowserWindow {
    fn new(options: BrowserWindowOptions) -> Self {
        let title = options.title.clone();
        let visible = options.show;
        Self {
            options,
            visible,
            minimized: false,
            maximized: false,
            focused: false,
            always_on_top: false,
            title,
            web_contents: WebContents::default(),
        }
    }

    /// Creation options.
    pub fn options(&self) -> &BrowserWindowOptions {
        &self.options
    }

    /// Window title (`win.setTitle` / options title).
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Renderer surface.
    pub fn web_contents(&self) -> &WebContents {
        &self.web_contents
    }

    /// Renderer surface, mutably.
    pub fn web_contents_mut(&mut self) -> &mut WebContents {
        &mut self.web_contents
    }

    /// `win.isVisible` (MVP: maps `show`/`hide` state).
    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// `win.isMinimized`.
    pub fn is_minimized(&self) -> bool {
        self.minimized
    }

    /// `win.isMaximized`.
    pub fn is_maximized(&self) -> bool {
        self.maximized
    }

    /// `win.isFocused`.
    pub fn is_focused(&self) -> bool {
        self.focused
    }

    /// `win.isAlwaysOnTop`.
    pub fn is_always_on_top(&self) -> bool {
        self.always_on_top
    }
}

/// Owns [`BrowserWindow`] lifetimes and chrome state.
///
/// Headless by design so the full lifecycle is unit-testable; the
/// `strake-shell` binding consumes the same options and mirrors state back
/// in a follow-up. Fires `on_last_window_closed` when the final window
/// closes so the embedder can run the `window-all-closed` flow.
#[derive(Default)]
pub struct WindowManager {
    windows: HashMap<u32, BrowserWindow>,
    next_id: u32,
    on_last_closed: Option<Box<dyn Fn()>>,
}

impl WindowManager {
    /// An empty manager.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of live windows.
    pub fn window_count(&self) -> usize {
        self.windows.len()
    }

    /// `new BrowserWindow(options)`: register and return its id.
    pub fn create(&mut self, options: BrowserWindowOptions) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        self.windows.insert(id, BrowserWindow::new(options));
        id
    }

    /// Look up a window.
    pub fn get(&self, id: u32) -> Option<&BrowserWindow> {
        self.windows.get(&id)
    }

    /// Look up a window, mutably.
    pub fn get_mut(&mut self, id: u32) -> Option<&mut BrowserWindow> {
        self.windows.get_mut(&id)
    }

    /// Fire `callback` when the final window closes.
    pub fn on_last_window_closed(&mut self, callback: impl Fn() + 'static) {
        self.on_last_closed = Some(Box::new(callback));
    }

    /// `win.close`: destroy the window. `false` for unknown ids.
    pub fn close(&mut self, id: u32) -> bool {
        if self.windows.remove(&id).is_none() {
            return false;
        }
        if self.windows.is_empty()
            && let Some(callback) = &self.on_last_closed
        {
            callback();
        }
        true
    }

    /// `win.show`.
    pub fn show(&mut self, id: u32) -> bool {
        self.mutate(id, |win| win.visible = true)
    }

    /// `win.hide`.
    pub fn hide(&mut self, id: u32) -> bool {
        self.mutate(id, |win| win.visible = false)
    }

    /// `win.minimize`.
    pub fn minimize(&mut self, id: u32) -> bool {
        self.mutate(id, |win| win.minimized = true)
    }

    /// `win.maximize`.
    pub fn maximize(&mut self, id: u32) -> bool {
        self.mutate(id, |win| {
            win.maximized = true;
            win.minimized = false;
        })
    }

    /// `win.restore` (un-minimize / un-maximize).
    pub fn restore(&mut self, id: u32) -> bool {
        self.mutate(id, |win| {
            win.minimized = false;
            win.maximized = false;
        })
    }

    /// `win.focus`.
    pub fn focus(&mut self, id: u32) -> bool {
        self.mutate(id, |win| win.focused = true)
    }

    /// `win.blur`.
    pub fn blur(&mut self, id: u32) -> bool {
        self.mutate(id, |win| win.focused = false)
    }

    /// `win.setAlwaysOnTop`.
    pub fn set_always_on_top(&mut self, id: u32, on_top: bool) -> bool {
        self.mutate(id, |win| win.always_on_top = on_top)
    }

    /// `win.setTitle`.
    pub fn set_title(&mut self, id: u32, title: &str) -> bool {
        self.mutate(id, |win| win.title = title.to_string())
    }

    fn mutate(&mut self, id: u32, f: impl FnOnce(&mut BrowserWindow)) -> bool {
        match self.windows.get_mut(&id) {
            Some(win) => {
                f(win);
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
use serde_json::json;

#[cfg(test)]
fn manager() -> WindowManager {
    WindowManager::new()
}

#[test]
fn default_options_match_electron() {
    let mut manager = manager();
    let id = manager.create(BrowserWindowOptions::default());
    let win = manager.get(id).expect("created window exists");
    assert_eq!((win.options().width, win.options().height), (800, 600));
    assert!(win.is_visible(), "show defaults to true");
    assert!(!win.is_minimized() && !win.is_maximized());
}

#[test]
fn custom_options_are_stored() {
    let mut manager = manager();
    let id = manager.create(BrowserWindowOptions {
        width: 1024,
        height: 768,
        frame: false,
        transparent: true,
        title: String::from("Editor"),
        background_color: Some(String::from("#1e1e1e")),
        ..Default::default()
    });
    let win = manager.get(id).unwrap();
    assert_eq!(win.options().title, "Editor");
    assert!(!win.options().frame);
    assert!(win.options().transparent);
    assert_eq!(win.options().background_color.as_deref(), Some("#1e1e1e"));
}

#[test]
fn window_state_transitions() {
    let mut manager = manager();
    let id = manager.create(BrowserWindowOptions::default());
    assert!(manager.minimize(id));
    assert!(manager.get(id).unwrap().is_minimized());
    assert!(manager.restore(id));
    assert!(!manager.get(id).unwrap().is_minimized());
    assert!(manager.maximize(id));
    assert!(manager.get(id).unwrap().is_maximized());
    assert!(manager.hide(id));
    assert!(!manager.get(id).unwrap().is_visible());
    assert!(manager.show(id));
    assert!(manager.focus(id));
    assert!(manager.get(id).unwrap().is_focused());
    assert!(manager.blur(id));
    assert!(!manager.get(id).unwrap().is_focused());
    assert!(manager.set_always_on_top(id, true));
    assert!(manager.get(id).unwrap().is_always_on_top());
    assert!(manager.set_title(id, "New title"));
    assert_eq!(manager.get(id).unwrap().title(), "New title");
}

#[test]
fn unknown_window_ops_fail_softly() {
    let mut manager = manager();
    for op in [
        manager.show(404),
        manager.hide(404),
        manager.minimize(404),
        manager.maximize(404),
        manager.restore(404),
        manager.focus(404),
        manager.blur(404),
        manager.close(404),
    ] {
        assert!(!op);
    }
    assert!(manager.get(404).is_none());
}

#[test]
fn closing_last_window_fires_callback_once() {
    use std::cell::RefCell;
    use std::rc::Rc;
    let mut manager = manager();
    let fires = Rc::new(RefCell::new(0u32));
    {
        let fires = Rc::clone(&fires);
        manager.on_last_window_closed(move || *fires.borrow_mut() += 1);
    }
    let a = manager.create(BrowserWindowOptions::default());
    let b = manager.create(BrowserWindowOptions::default());
    assert!(manager.close(a));
    assert_eq!(*fires.borrow(), 0, "windows remain");
    assert!(manager.close(b));
    assert_eq!(*fires.borrow(), 1, "last close fires exactly once");
    assert_eq!(manager.window_count(), 0);
}

#[test]
fn last_close_drives_app_shutdown() {
    // The Day-1 loop: manager's last-close callback feeds App, which emits
    // window-all-closed and quits (Electron default).
    use std::cell::RefCell;
    use std::rc::Rc;
    let mut app = crate::App::new("QuickStart", "1.0.0");
    let mut manager = manager();
    let events: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    app.on(crate::AppEventKind::WindowAllClosed, {
        let events = Rc::clone(&events);
        move || events.borrow_mut().push(String::from("window-all-closed"))
    });
    let id = manager.create(BrowserWindowOptions::default());
    manager.on_last_window_closed(|| {});
    assert!(manager.close(id));
    app.note_window_closed(manager.window_count());
    assert_eq!(*events.borrow(), vec!["window-all-closed"]);
    assert!(app.is_quit());
}

#[test]
fn web_contents_records_navigation_and_sends_ipc() {
    let mut manager = manager();
    let mut bus = IpcBus::new();
    let id = manager.create(BrowserWindowOptions::default());
    let win = manager.get_mut(id).unwrap();
    win.web_contents_mut().load_url("https://example.com/app");
    assert_eq!(
        win.web_contents().pending_url().as_deref(),
        Some("https://example.com/app")
    );
    win.web_contents_mut().load_file("/app/index.html");
    assert_eq!(
        win.web_contents().pending_url().as_deref(),
        Some("file:///app/index.html")
    );
    let received = std::rc::Rc::new(std::cell::RefCell::new(None));
    bus.on("ping", {
        let received = std::rc::Rc::clone(&received);
        move |value| *received.borrow_mut() = Some(value.clone())
    });
    win.web_contents().send(&bus, "ping", json!({"n": 1}));
    assert_eq!(*received.borrow(), Some(json!({"n": 1})));
}
