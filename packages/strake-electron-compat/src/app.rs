//! `app`: Electron application lifecycle as a deterministic state machine.
//!
//! Models `app.whenReady` / `quit` / `getPath` and the `ready`,
//! `window-all-closed`, `activate`, `before-quit`, `will-quit` events. The
//! embedder drives it: [`App::mark_ready`] when runtime init completes,
//! [`App::note_window_closed`] from the window manager. Real `winit`
//! suspension/activation wires in later without changing this contract.

#[cfg(test)]
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
#[cfg(test)]
use std::rc::Rc;

/// Lifecycle events, mirroring Electron's `app` event names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AppEventKind {
    /// Runtime initialised (`whenReady` callbacks run).
    Ready,
    /// Last window closed.
    WindowAllClosed,
    /// App (re)activated with no windows (macOS dock click).
    Activate,
    /// `quit()` initiated; shutdown follows.
    BeforeQuit,
    /// Shutdown completing.
    WillQuit,
}

/// Named paths served by `app.getPath` / overridden by `app.setPath`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AppPath {
    /// Per-app profile data (`app.setPath` in embeds; no OS default here).
    UserData,
    /// Roaming app data (no OS default in MVP).
    AppData,
    /// Desktop directory (no OS default in MVP).
    Desktop,
    /// Documents directory (no OS default in MVP).
    Documents,
    /// Downloads directory (no OS default in MVP).
    Downloads,
    /// Home directory (no OS default in MVP).
    Home,
    /// OS temporary directory (defaults to [`std::env::temp_dir`]).
    Temp,
}

type Listener = Box<dyn Fn()>;

/// Electron `app` module: lifecycle, identity, and paths.
///
/// Listeners registered with [`App::on`] fire in registration order; `ready`
/// fires exactly once. `quit()` emits `before-quit` then `will-quit`.
/// `note_window_closed(0)` emits `window-all-closed` and quits unless
/// opted out (macOS-style), matching Electron's default.
pub struct App {
    name: String,
    version: String,
    ready: bool,
    quit: bool,
    quit_on_all_windows_closed: bool,
    listeners: HashMap<AppEventKind, Vec<Listener>>,
    paths: HashMap<AppPath, PathBuf>,
}

impl App {
    /// A new application identity. Starts unready and running.
    pub fn new(name: &str, version: &str) -> Self {
        Self {
            name: name.to_string(),
            version: version.to_string(),
            ready: false,
            quit: false,
            quit_on_all_windows_closed: true,
            listeners: HashMap::new(),
            paths: HashMap::from([(AppPath::Temp, std::env::temp_dir())]),
        }
    }

    /// Application name (`app.getName`).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Application version (`app.getVersion`).
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Subscribe to a lifecycle event (`app.on(...)`).
    pub fn on(&mut self, event: AppEventKind, listener: impl Fn() + 'static) {
        self.listeners
            .entry(event)
            .or_default()
            .push(Box::new(listener));
    }

    /// Whether `mark_ready` has run (`app.isReady`).
    pub fn is_ready(&self) -> bool {
        self.ready
    }

    /// Whether `quit()` completed.
    pub fn is_quit(&self) -> bool {
        self.quit
    }

    /// Runtime initialisation finished: fire `ready` once (`app.whenReady`
    /// resolves). Later calls are no-ops.
    pub fn mark_ready(&mut self) {
        if self.ready {
            return;
        }
        self.ready = true;
        self.emit(AppEventKind::Ready);
    }

    /// Begin shutdown: emit `before-quit`, then `will-quit`, then stop.
    pub fn quit(&mut self) {
        if self.quit {
            return;
        }
        self.emit(AppEventKind::BeforeQuit);
        self.emit(AppEventKind::WillQuit);
        self.quit = true;
    }

    /// macOS-style opt-out: stay alive when the last window closes.
    pub fn set_quit_on_all_windows_closed(&mut self, quit: bool) {
        self.quit_on_all_windows_closed = quit;
    }

    /// Called by the window manager with the remaining window count.
    /// Zero remaining emits `window-all-closed` (and quits by default).
    pub fn note_window_closed(&mut self, windows_remaining: usize) {
        if windows_remaining > 0 {
            return;
        }
        self.emit(AppEventKind::WindowAllClosed);
        if self.quit_on_all_windows_closed {
            self.quit();
        }
    }

    /// Resolve a named path (`app.getPath`); `None` until set, except `Temp`.
    pub fn get_path(&self, path: AppPath) -> Option<PathBuf> {
        self.paths.get(&path).cloned()
    }

    /// Override a named path (`app.setPath`).
    pub fn set_path(&mut self, path: AppPath, value: PathBuf) {
        self.paths.insert(path, value);
    }

    fn emit(&self, event: AppEventKind) {
        if let Some(listeners) = self.listeners.get(&event) {
            for listener in listeners {
                listener();
            }
        }
    }
}

/// Test helper: shared event log.
#[cfg(test)]
fn log() -> (Rc<RefCell<Vec<String>>>, Rc<dyn Fn(&str)>) {
    let events = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&events);
    let push: Rc<dyn Fn(&str)> =
        Rc::new(move |event: &str| sink.borrow_mut().push(event.to_string()));
    (events, push)
}

#[test]
fn ready_fires_once() {
    let (events, push) = log();
    let mut app = App::new("QuickStart", "1.0.0");
    assert!(!app.is_ready());
    app.on(AppEventKind::Ready, move || push("ready"));
    app.mark_ready();
    app.mark_ready();
    assert!(app.is_ready());
    assert_eq!(*events.borrow(), vec!["ready"], "ready fires exactly once");
}

#[test]
fn quit_emits_before_quit_then_will_quit() {
    let (events, push) = log();
    let mut app = App::new("QuickStart", "1.0.0");
    app.on(AppEventKind::BeforeQuit, {
        let push = Rc::clone(&push);
        move || push("before-quit")
    });
    app.on(AppEventKind::WillQuit, move || push("will-quit"));
    app.quit();
    assert!(app.is_quit());
    assert_eq!(*events.borrow(), vec!["before-quit", "will-quit"]);
}

#[test]
fn last_window_closed_quits_by_default() {
    let (events, push) = log();
    let mut app = App::new("QuickStart", "1.0.0");
    app.on(AppEventKind::WindowAllClosed, move || {
        push("window-all-closed")
    });
    app.note_window_closed(2);
    assert!(events.borrow().is_empty());
    assert!(!app.is_quit());
    app.note_window_closed(0);
    assert_eq!(*events.borrow(), vec!["window-all-closed"]);
    assert!(app.is_quit(), "Electron quits when the last window closes");
}

#[test]
fn quit_on_all_windows_closed_is_opt_out() {
    let mut app = App::new("QuickStart", "1.0.0");
    app.set_quit_on_all_windows_closed(false);
    app.note_window_closed(0);
    assert!(!app.is_quit(), "macOS-style: stay alive with no windows");
}

#[test]
fn paths_default_temp_and_accept_overrides() {
    let mut app = App::new("QuickStart", "1.0.0");
    assert_eq!(app.get_path(AppPath::Temp), Some(std::env::temp_dir()));
    assert_eq!(app.get_path(AppPath::UserData), None);
    let custom = std::path::PathBuf::from("/tmp/strake-test-profile");
    app.set_path(AppPath::UserData, custom.clone());
    assert_eq!(app.get_path(AppPath::UserData), Some(custom));
}

#[test]
fn name_and_version_are_reported() {
    let app = App::new("QuickStart", "1.0.0");
    assert_eq!(app.name(), "QuickStart");
    assert_eq!(app.version(), "1.0.0");
}
