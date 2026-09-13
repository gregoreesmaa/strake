//! `BrowserWindow`: option mapping plus headless-testable window state.
//!
//! [`BrowserWindowOptions`] mirrors Electron's
//! `BrowserWindowConstructorOptions` subset for the MVP (geometry, frame,
//! transparency, background, title); [`WindowManager`] owns window lifetimes
//! and state transitions, firing `on_last_window_closed` so the embedder can
//! close the [`App`](crate::App) loop. Real `winit`/`strake-shell` windows
//! bind to these options in a follow-up without changing this contract.

use std::collections::{HashMap, VecDeque};

use serde_json::Value;

use crate::IpcBus;

/// Content rectangle (`Electron.Rectangle`): `{ x, y, width, height }`, in
/// device-independent pixels (DIP), matching Electron's DIP rectangles. The
/// shell hands these values to winit as `LogicalSize`/`LogicalPosition`;
/// winit applies the monitor scale factor at handoff to reach physical
/// pixels, so no caller converts by `scale_factor` beforehand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Bounds {
    /// Left edge in DIP.
    pub x: i32,
    /// Top edge in DIP.
    pub y: i32,
    /// Content width in DIP.
    pub width: u32,
    /// Content height in DIP.
    pub height: u32,
}

/// `win.on('closed')` listeners by window id (issue #84).
type ClosedListeners = HashMap<u32, Vec<Box<dyn Fn(u32)>>>;

/// One main-to-renderer `webContents.send` awaiting the pump (issue #91).
#[derive(Debug, Clone, PartialEq)]
pub struct MainSend {
    /// Target channel (`ipcRenderer.on` in the renderer).
    pub channel: String,
    /// JSON payload (already `JSON.stringify`-shaped by the caller).
    pub payload: Value,
}

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

/// `webPreferences` subset (issue #109).
///
/// Only `preload` is recorded: the embedder drains it (see
/// [`WindowManager::pending_preloads`]) and executes the file after document
/// creation, before renderer scripts. Every other `webPreferences` sub-key is
/// accepted by the bindings and ignored here; sandboxed execution with Node
/// integration stays deferred to issue #18.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct WebPreferences {
    /// Absolute (or app-relative) path of the preload script, if given.
    pub preload: Option<String>,
}

/// `BrowserWindowConstructorOptions` (MVP subset).
#[derive(Debug, Clone, PartialEq)]
pub struct BrowserWindowOptions {
    /// Content width in DIP (Electron default 800).
    pub width: u32,
    /// Content height in DIP (Electron default 600).
    pub height: u32,
    /// Minimum content size.
    pub min_size: Option<(u32, u32)>,
    /// Maximum content size.
    pub max_size: Option<(u32, u32)>,
    /// Initial top-left position (`None` = platform default).
    pub position: Option<(i32, i32)>,
    /// Window frame and title bar (Electron default true).
    pub frame: bool,
    /// Whether the user can resize the window (Electron default true,
    /// issue #90).
    pub resizable: bool,
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
    /// `webPreferences` subset (issue #109): `preload` is recorded, every
    /// other sub-key is accepted and ignored.
    pub web_preferences: WebPreferences,
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
            resizable: true,
            title_bar_style: TitleBarStyle::Default,
            transparent: false,
            background_color: None,
            show: true,
            title: String::new(),
            web_preferences: WebPreferences::default(),
        }
    }
}

/// Renderer surface of a window (`win.webContents`).
///
/// MVP: records the pending navigation target (`loadURL`/`loadFile`) and
/// delivers `send` into the [`IpcBus`] for renderer listeners.
/// `executeJavaScript`/DevTools/print need the renderer binding (deferred).
///
/// Main-to-renderer traffic (issue #91) queues per window in [`Self::outbox`]:
/// `queue_send` records one payload and the embedder drains it with
/// [`Self::take_pending_sends`] (or across windows with
/// [`WindowManager::drain_web_contents_sends`]) for delivery to that window's
/// renderer `ipcRenderer.on` listeners.
#[derive(Debug, Default)]
pub struct WebContents {
    pending_url: Option<String>,
    document_title: Option<String>,
    outbox: VecDeque<MainSend>,
}

impl WebContents {
    /// Navigate to a URL (`win.loadURL`).
    pub fn load_url(&mut self, url: &str) {
        self.pending_url = Some(url.to_string());
    }

    /// Record the loaded page's `<title>` (`webContents.getTitle`, issue
    /// #90). The shell binding syncs this from the parsed document on every
    /// load; until then the title is empty, matching Electron.
    pub fn set_document_title(&mut self, title: Option<String>) {
        self.document_title = title;
    }

    /// `webContents.getTitle`: the loaded page's `<title>`, or `""`.
    pub fn get_title(&self) -> &str {
        self.document_title.as_deref().unwrap_or_default()
    }

    /// `win.webContents.send(channel, payload)`: queue one main-to-renderer
    /// payload for this window (issue #91). Delivery is fire-and-forget at
    /// queue time; the pump drains the queue (see [`Self::take_pending_sends`]).
    pub fn queue_send(&mut self, channel: &str, payload: Value) {
        self.outbox.push_back(MainSend {
            channel: channel.to_string(),
            payload,
        });
    }

    /// Queued main-to-renderer payloads, still awaiting the pump.
    pub fn pending_send_count(&self) -> usize {
        self.outbox.len()
    }

    /// Drain this window's queued main-to-renderer payloads in FIFO order.
    pub fn take_pending_sends(&mut self) -> Vec<MainSend> {
        self.outbox.drain(..).collect()
    }

    /// Load a local file (`win.loadFile`): like Electron the path resolves
    /// to an absolute `file:///` URL. Relative paths resolve against the
    /// process working directory (no app root exists yet at this layer),
    /// `file://` prefixes normalize instead of passing through, Windows
    /// `\` separators become `/`, and the path percent-encodes.
    pub fn load_file(&mut self, path: &str) {
        self.pending_url = Some(file_url_from_path(path));
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

/// Resolve a `loadFile` path to an absolute `file:///` URL.
///
/// A leading `file://` is stripped and re-derived (so a previously built
/// `file://relative` is repaired, not passed through), `\` becomes `/`,
/// relative paths join the process working directory, and the result is
/// always `file://` + an absolute, percent-encoded path.
fn file_url_from_path(path: &str) -> String {
    let stripped = path.strip_prefix("file://").unwrap_or(path);
    let slashed = stripped.replace('\\', "/");
    let absolute = if is_absolute_path(&slashed) {
        slashed
    } else {
        match std::env::current_dir() {
            Ok(cwd) => {
                let root = cwd.to_string_lossy();
                format!(
                    "{}/{}",
                    root.trim_end_matches('/'),
                    slashed.trim_start_matches('/')
                )
            }
            Err(_) => format!("/{}", slashed.trim_start_matches('/')),
        }
    };
    let rooted = if absolute.starts_with('/') {
        absolute
    } else {
        format!("/{absolute}")
    };
    format!("file://{}", percent_encode_path(&rooted))
}

/// POSIX-absolute (`/...`) or Windows-absolute (`C:/...`, `C:...`).
fn is_absolute_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    path.starts_with('/')
        || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
}

/// Percent-encode a URL path over UTF-8 bytes, keeping the characters that
/// are legal bare in a `file:` URL path (`/`, `:` for drive letters, and
/// the RFC 3986 unreserved set). `?`/`#` encode so they cannot start a
/// query or fragment.
fn percent_encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~' | b'/' | b':') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
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
    /// Opener window id for `window.open` children (`None` for top-level
    /// `new BrowserWindow` windows, issue #15).
    opener: Option<u32>,
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
            opener: None,
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

    /// Opener window id for `window.open` children (`None` for top-level
    /// windows, issue #15).
    pub fn opener(&self) -> Option<u32> {
        self.opener
    }

    /// `win.isVisible` (MVP: maps `show`/`hide` state).
    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// `win.setResizable` state (issue #90).
    pub fn is_resizable(&self) -> bool {
        self.options.resizable
    }

    /// `win.getBounds`: content position and size (issue #90).
    pub fn bounds(&self) -> Bounds {
        let (x, y) = self.options.position.unwrap_or((0, 0));
        Bounds {
            x,
            y,
            width: self.options.width,
            height: self.options.height,
        }
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
    on_last_closed: Option<Box<dyn Fn(usize)>>,
    /// Per-window `closed` listeners (`win.on('closed')`, issue #84).
    /// Listeners observe only the closed id, so firing them while `close`
    /// holds `&mut self` cannot re-borrow the manager.
    closed_listeners: ClosedListeners,
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

    /// Ids of live windows in ascending creation order
    /// (`BrowserWindow.getAllWindows()`, issue #107). Closed windows vanish
    /// from the list; an empty manager yields an empty list.
    pub fn live_ids(&self) -> Vec<u32> {
        let mut ids: Vec<u32> = self.windows.keys().copied().collect();
        ids.sort_unstable();
        ids
    }

    /// `(window id, preload path)` for live windows that declared a
    /// `webPreferences.preload` (issue #109), in ascending window-id order.
    /// The embedder drains this after creating the renderer document and
    /// executes each file before renderer scripts; sandboxed execution stays
    /// deferred to issue #18.
    pub fn pending_preloads(&self) -> Vec<(u32, String)> {
        let mut ids: Vec<u32> = self.windows.keys().copied().collect();
        ids.sort_unstable();
        ids.into_iter()
            .filter_map(|id| {
                let preload = self
                    .windows
                    .get(&id)?
                    .options
                    .web_preferences
                    .preload
                    .clone()?;
                Some((id, preload))
            })
            .collect()
    }

    /// `new BrowserWindow(options)`: register and return its id.
    pub fn create(&mut self, options: BrowserWindowOptions) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        self.windows.insert(id, BrowserWindow::new(options));
        id
    }

    /// `window.open` from a renderer: register a child window carrying the
    /// opener id (`None` for an unknown opener — a dropped opener cannot
    /// spawn children, issue #15). The child is a plain managed window
    /// otherwise: it loads, shows, sends IPC, and closes independently,
    /// and outlives its opener. The OS surface binds later like any other
    /// window (`ShellWindow::attach`).
    pub fn open_child(&mut self, opener: u32, options: BrowserWindowOptions) -> Option<u32> {
        if !self.windows.contains_key(&opener) {
            return None;
        }
        let id = self.next_id;
        self.next_id += 1;
        let mut child = BrowserWindow::new(options);
        child.opener = Some(opener);
        self.windows.insert(id, child);
        Some(id)
    }

    /// Look up a window.
    pub fn get(&self, id: u32) -> Option<&BrowserWindow> {
        self.windows.get(&id)
    }

    /// Look up a window, mutably.
    pub fn get_mut(&mut self, id: u32) -> Option<&mut BrowserWindow> {
        self.windows.get_mut(&id)
    }

    /// Fire `callback` with the remaining window count when the final
    /// window closes (always zero today). The count is passed in so the
    /// embedder can feed [`App::note_window_closed`](crate::App::note_window_closed)
    /// without touching the manager: the callback runs while `close()`
    /// holds `&mut self`, so re-borrowing the manager inside the callback
    /// (e.g. via `Rc<RefCell<WindowManager>>`) panics — use the count.
    pub fn on_last_window_closed(&mut self, callback: impl Fn(usize) + 'static) {
        self.on_last_closed = Some(Box::new(callback));
    }

    /// `win.close`: destroy the window. `false` for unknown ids.
    pub fn close(&mut self, id: u32) -> bool {
        if self.windows.remove(&id).is_none() {
            return false;
        }
        if let Some(listeners) = self.closed_listeners.remove(&id) {
            for listener in listeners {
                listener(id);
            }
        }
        if self.windows.is_empty()
            && let Some(callback) = &self.on_last_closed
        {
            callback(self.windows.len());
        }
        true
    }

    /// `win.on('closed', listener)` (issue #84): run `listener` with the
    /// window id when this window closes. `false` for unknown ids; other
    /// event names are the JS binding's concern (accepted, never fired).
    pub fn on_closed(&mut self, id: u32, listener: impl Fn(u32) + 'static) -> bool {
        match self.closed_listeners.get_mut(&id) {
            Some(listeners) => {
                listeners.push(Box::new(listener));
            }
            None => {
                if !self.windows.contains_key(&id) {
                    return false;
                }
                self.closed_listeners.insert(id, vec![Box::new(listener)]);
            }
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

    /// `win.setResizable` (issue #90). Unknown ids fail softly.
    pub fn set_resizable(&mut self, id: u32, resizable: bool) -> bool {
        self.mutate(id, |win| win.options.resizable = resizable)
    }

    /// `win.setResizable` state (issue #90). Unknown ids report `false`.
    pub fn is_resizable(&self, id: u32) -> bool {
        self.get(id).is_some_and(|win| win.is_resizable())
    }

    /// `win.isVisible` (issue #90). Unknown ids report `false`.
    pub fn is_visible(&self, id: u32) -> bool {
        self.get(id).is_some_and(|win| win.is_visible())
    }

    /// `win.setBounds` (issue #90): move and resize in one transition.
    /// Unknown ids fail softly.
    pub fn set_bounds(&mut self, id: u32, bounds: Bounds) -> bool {
        self.mutate(id, |win| {
            win.options.position = Some((bounds.x, bounds.y));
            win.options.width = bounds.width;
            win.options.height = bounds.height;
        })
    }

    /// `win.getBounds` (issue #90). `None` for unknown ids.
    pub fn get_bounds(&self, id: u32) -> Option<Bounds> {
        self.get(id).map(|win| win.bounds())
    }

    /// Queue one main-to-renderer `webContents.send` on a window (issue #91).
    /// Unknown ids fail softly (`false`), matching Electron's fire-and-forget
    /// posture for unreachable targets.
    pub fn queue_web_contents_send(&mut self, id: u32, channel: &str, payload: Value) -> bool {
        match self.windows.get_mut(&id) {
            Some(win) => {
                win.web_contents_mut().queue_send(channel, payload);
                true
            }
            None => false,
        }
    }

    /// Queued main-to-renderer payloads across all windows.
    pub fn queued_main_send_count(&self) -> usize {
        self.windows
            .values()
            .map(|win| win.web_contents().pending_send_count())
            .sum()
    }

    /// Drain every window's queued main-to-renderer payloads in window-id
    /// order, each window FIFO (issue #91). The pump delivers each
    /// `(window id, channel, payload)` to that window's renderer
    /// `ipcRenderer.on` listeners.
    pub fn drain_web_contents_sends(&mut self) -> Vec<(u32, MainSend)> {
        let mut ids: Vec<u32> = self.windows.keys().copied().collect();
        ids.sort_unstable();
        let mut drained = Vec::new();
        for id in ids {
            if let Some(win) = self.windows.get_mut(&id) {
                drained.extend(
                    win.web_contents_mut()
                        .take_pending_sends()
                        .into_iter()
                        .map(|send| (id, send)),
                );
            }
        }
        drained
    }

    /// `win.setSize`: update the content size stored on the window options.
    /// The embedder mirrors this into the page viewport and the OS window
    /// (see `ShellWindow::resize`); unknown ids fail softly like the other
    /// state transitions.
    pub fn resize(&mut self, id: u32, width: u32, height: u32) -> bool {
        self.mutate(id, |win| {
            win.options.width = width;
            win.options.height = height;
        })
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
fn open_child_coexists_and_outlives_opener() {
    // Issue #15 MVP: `window.open` basics — the child carries its opener,
    // both windows stay independently manageable, and closing either side
    // never disturbs the other.
    let mut manager = manager();
    let main = manager.create(BrowserWindowOptions::default());
    assert_eq!(manager.get(main).unwrap().opener(), None);
    let child = manager
        .open_child(main, BrowserWindowOptions::default())
        .expect("known opener spawns");
    assert_eq!(manager.get(child).unwrap().opener(), Some(main));
    assert_eq!(manager.live_ids(), vec![main, child]);
    assert_eq!(
        manager.open_child(999, BrowserWindowOptions::default()),
        None
    );

    assert!(manager.close(child), "child closes");
    assert_eq!(manager.live_ids(), vec![main]);
    assert!(manager.get(main).is_some(), "opener survives its child");

    let orphan = manager
        .open_child(main, BrowserWindowOptions::default())
        .expect("opener spawns again");
    assert!(manager.close(main), "opener closes first");
    assert_eq!(manager.get(orphan).unwrap().opener(), Some(main));
    assert!(manager.close(orphan), "orphan still closes cleanly");
    assert!(manager.live_ids().is_empty());
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
    let seen = Rc::new(RefCell::new(Vec::new()));
    {
        let fires = Rc::clone(&fires);
        let seen = Rc::clone(&seen);
        manager.on_last_window_closed(move |remaining| {
            *fires.borrow_mut() += 1;
            seen.borrow_mut().push(remaining);
        });
    }
    let a = manager.create(BrowserWindowOptions::default());
    let b = manager.create(BrowserWindowOptions::default());
    assert!(manager.close(a));
    assert_eq!(*fires.borrow(), 0, "windows remain");
    assert!(manager.close(b));
    assert_eq!(*fires.borrow(), 1, "last close fires exactly once");
    assert_eq!(*seen.borrow(), vec![0], "callback receives zero remaining");
    assert_eq!(manager.window_count(), 0);
}

#[test]
fn last_close_drives_app_shutdown() {
    // The Day-1 loop: the last-close callback feeds the count straight into
    // App (no manager re-borrow), which emits window-all-closed and quits
    // (Electron default).
    use std::cell::RefCell;
    use std::rc::Rc;
    let app = Rc::new(RefCell::new(crate::App::new("QuickStart", "1.0.0")));
    let mut manager = manager();
    let events: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    app.borrow_mut().on(crate::AppEventKind::WindowAllClosed, {
        let events = Rc::clone(&events);
        move || events.borrow_mut().push(String::from("window-all-closed"))
    });
    {
        let app = Rc::clone(&app);
        manager.on_last_window_closed(move |remaining| {
            app.borrow_mut().note_window_closed(remaining);
        });
    }
    let id = manager.create(BrowserWindowOptions::default());
    assert!(manager.close(id));
    assert_eq!(*events.borrow(), vec!["window-all-closed"]);
    assert!(app.borrow().is_quit());
}

// PIN (review PR #79): the Day-1 wiring must survive shared ownership
// (`Rc<RefCell<..>>` on both sides). Before the count-passing callback the
// only wiring re-borrowed the manager inside `close()` and panicked with
// `BorrowMutError`; now the callback never touches the manager.
#[test]
fn last_close_wiring_needs_no_manager_reborrow() {
    use std::cell::RefCell;
    use std::rc::Rc;
    let app = Rc::new(RefCell::new(crate::App::new("QuickStart", "1.0.0")));
    let manager = Rc::new(RefCell::new(manager()));
    {
        let app = Rc::clone(&app);
        manager
            .borrow_mut()
            .on_last_window_closed(move |remaining| {
                assert_eq!(remaining, 0);
                app.borrow_mut().note_window_closed(remaining);
            });
    }
    let id = manager.borrow_mut().create(BrowserWindowOptions::default());
    assert!(manager.borrow_mut().close(id));
    assert!(app.borrow().is_quit());
    assert_eq!(manager.borrow().window_count(), 0);
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

// PIN (review PR #79): relative `loadFile` paths must resolve to absolute
// `file:///` URLs, never `file://<host-ish-first-segment>`.
#[test]
fn load_file_resolves_relative_paths_to_absolute_file_urls() {
    let mut contents = WebContents::default();
    contents.load_file("renderer/index.html");
    let url = contents.pending_url().expect("pending url").to_string();
    assert!(
        url.starts_with("file:///"),
        "relative path must yield an absolute file URL, got {url}"
    );
    assert!(
        url.ends_with("/renderer/index.html"),
        "path tail must survive, got {url}"
    );
}

// PIN (review PR #79): `file://` prefixes normalize, special chars encode,
// Windows separators convert.
#[test]
fn load_file_normalizes_prefix_encodes_and_converts_separators() {
    let mut contents = WebContents::default();
    contents.load_file("file:///app/index.html");
    assert_eq!(
        contents.pending_url(),
        Some("file:///app/index.html"),
        "already-absolute file URL must round-trip"
    );
    contents.load_file("/app/my page.html");
    assert_eq!(
        contents.pending_url(),
        Some("file:///app/my%20page.html"),
        "spaces must be percent-encoded"
    );
    contents.load_file("C:\\app\\index.html");
    assert_eq!(
        contents.pending_url(),
        Some("file:///C:/app/index.html"),
        "Windows separators must convert"
    );
}

// NOTE (review PR #79): a temporary REPRO test on the pre-fix code proved
// the old `Fn()` wiring panicked here with `RefCell already mutably
// borrowed`; it was replaced by `last_close_wiring_needs_no_manager_reborrow`
// above once the callback received the remaining count.

#[test]
fn resizable_defaults_true_and_toggles() {
    let mut manager = manager();
    let id = manager.create(BrowserWindowOptions::default());
    assert!(manager.is_resizable(id), "resizable defaults to true");
    assert!(manager.set_resizable(id, false));
    assert!(!manager.is_resizable(id));
    assert!(!manager.get(id).unwrap().is_resizable());
    assert!(manager.set_resizable(id, true));
    assert!(manager.is_resizable(id));
    assert!(
        !manager.set_resizable(404, false),
        "unknown id fails softly"
    );
    assert!(!manager.is_resizable(404), "unknown id reports false");
}

#[test]
fn bounds_round_trip_through_manager() {
    let mut manager = manager();
    let id = manager.create(BrowserWindowOptions {
        width: 800,
        height: 600,
        position: Some((10, 20)),
        ..Default::default()
    });
    assert_eq!(
        manager.get_bounds(id),
        Some(Bounds {
            x: 10,
            y: 20,
            width: 800,
            height: 600,
        })
    );
    assert!(manager.set_bounds(
        id,
        Bounds {
            x: 100,
            y: 200,
            width: 1024,
            height: 768,
        }
    ));
    assert_eq!(
        manager.get_bounds(id),
        Some(Bounds {
            x: 100,
            y: 200,
            width: 1024,
            height: 768,
        })
    );
    assert!(manager.get(id).unwrap().options().position == Some((100, 200)));
    assert!(
        !manager.set_bounds(id + 999, Bounds::default()),
        "unknown id fails softly"
    );
    assert_eq!(manager.get_bounds(id + 999), None);
}

#[test]
fn closed_listeners_fire_with_id_then_release() {
    use std::cell::RefCell;
    use std::rc::Rc;
    let mut manager = manager();
    let a = manager.create(BrowserWindowOptions::default());
    let b = manager.create(BrowserWindowOptions::default());
    let seen = Rc::new(RefCell::new(Vec::new()));
    for id in [a, b] {
        let seen = Rc::clone(&seen);
        assert!(manager.on_closed(id, move |closed| seen.borrow_mut().push(closed)));
    }
    assert!(!manager.on_closed(404, |_| {}), "unknown id fails softly");
    assert!(manager.close(a));
    assert_eq!(*seen.borrow(), vec![a], "only the closed window fires");
    assert!(manager.close(b));
    assert_eq!(*seen.borrow(), vec![a, b]);
    assert!(!manager.close(a), "second close stays soft");
    assert_eq!(*seen.borrow(), vec![a, b], "no double fire");
}

#[test]
fn manager_is_visible_mirrors_show_hide() {
    let mut manager = manager();
    let id = manager.create(BrowserWindowOptions::default());
    assert!(manager.is_visible(id), "show defaults to true");
    assert!(manager.hide(id));
    assert!(!manager.is_visible(id));
    assert!(manager.show(id));
    assert!(manager.is_visible(id));
    assert!(!manager.is_visible(404), "unknown id reports false");
}

#[test]
fn web_contents_title_defaults_empty_until_synced() {
    let contents = WebContents::default();
    assert_eq!(contents.get_title(), "");
    let mut contents = contents;
    contents.set_document_title(Some(String::from("Settings")));
    assert_eq!(contents.get_title(), "Settings");
}

#[test]
fn web_contents_send_queues_per_window_fifo() {
    let mut manager = manager();
    let a = manager.create(BrowserWindowOptions::default());
    let b = manager.create(BrowserWindowOptions::default());
    assert_eq!(manager.queued_main_send_count(), 0);
    assert!(manager.queue_web_contents_send(a, "tick", json!({"n": 1})));
    assert!(manager.queue_web_contents_send(a, "tick", json!({"n": 2})));
    assert!(manager.queue_web_contents_send(b, "other", json!(true)));
    assert!(
        !manager.queue_web_contents_send(404, "tick", json!(null)),
        "unknown id fails softly"
    );
    assert_eq!(manager.queued_main_send_count(), 3);

    let drained = manager.drain_web_contents_sends();
    assert_eq!(
        drained
            .iter()
            .map(|(id, send)| (*id, send.channel.as_str()))
            .collect::<Vec<_>>(),
        vec![(a, "tick"), (a, "tick"), (b, "other")],
        "window-id order, each window FIFO"
    );
    assert_eq!(drained[0].1.payload, json!({"n": 1}));
    assert_eq!(
        manager.queued_main_send_count(),
        0,
        "drain empties every outbox"
    );
    assert!(manager.drain_web_contents_sends().is_empty());
}

#[test]
fn live_ids_tracks_create_and_close_for_get_all_windows() {
    // Issue #107: `BrowserWindow.getAllWindows()` reflects live windows.
    let mut manager = manager();
    assert!(manager.live_ids().is_empty(), "empty when none");
    let a = manager.create(BrowserWindowOptions::default());
    let b = manager.create(BrowserWindowOptions::default());
    assert_eq!(manager.live_ids(), vec![a, b], "one entry per live window");
    assert!(manager.close(a));
    assert_eq!(manager.live_ids(), vec![b], "closed windows vanish");
    assert!(manager.close(b));
    assert!(manager.live_ids().is_empty(), "empty again when none");
}

#[test]
fn pending_preloads_records_web_preferences_preload() {
    // Issue #109: `webPreferences.preload` is recorded per window for the
    // embedder to execute before renderer scripts.
    let mut manager = manager();
    assert!(manager.pending_preloads().is_empty());
    let plain = manager.create(BrowserWindowOptions::default());
    let _ = plain;
    let with_preload = manager.create(BrowserWindowOptions {
        web_preferences: WebPreferences {
            preload: Some(String::from("/app/preload.js")),
        },
        ..Default::default()
    });
    assert_eq!(
        manager.pending_preloads(),
        vec![(with_preload, String::from("/app/preload.js"))],
        "only windows declaring a preload are listed"
    );
    assert!(manager.close(with_preload));
    assert!(
        manager.pending_preloads().is_empty(),
        "closed windows drop out"
    );
}
