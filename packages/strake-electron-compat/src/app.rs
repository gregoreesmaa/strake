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
///
/// Electron names verified against the upstream `app.getPath(name)` docs:
/// `home`, `appData`, `userData` (= `appData` + app name), `sessionData`
/// (= `userData`), `temp`, `exe`, `module`, `desktop`, `documents`,
/// `downloads`, `music`, `pictures`, `videos`, `recent` (Windows-only),
/// `logs`, `crashDumps`; unknown names throw. This MVP maps the seven
/// variants below. [`AppPath::Temp`] ([`std::env::temp_dir`]),
/// [`AppPath::AppData`] (OS env), and [`AppPath::UserData`]
/// (`appData/<name>`) have OS defaults; every other variant returns `None`
/// from [`App::get_path`] until the embedder calls [`App::set_path`]. The
/// remaining OS folders (known-folder documents/downloads/desktop, …) have
/// no sound `std`-only derivation (`std::env::home_dir` is deprecated and
/// env-var guessing diverges from the OS known-folder APIs Electron uses),
/// so they stay explicit rather than guessed — no new crates for this MVP.
/// OS default for [`AppPath::AppData`] (issue #155): the same env Electron
/// reads — `%APPDATA%` on Windows, `~/Library/Application Support` on
/// macOS, `$XDG_CONFIG_HOME` (else `~/.config`) elsewhere. `None` when the
/// env is absent, so sandboxes without a home keep the explicit-`set_path`
/// stance instead of inventing a location.
fn default_app_data() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("APPDATA").map(PathBuf::from)
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Library/Application Support"))
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        if let Some(config) = std::env::var_os("XDG_CONFIG_HOME") {
            return Some(PathBuf::from(config));
        }
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AppPath {
    /// Per-app profile data (Electron `userData`; defaults to
    /// `appData/<name>`, [`App::set_path`] overrides).
    UserData,
    /// Roaming app data (Electron `appData`; OS env default,
    /// [`App::set_path`] overrides).
    AppData,
    /// Desktop directory (requires [`App::set_path`]).
    Desktop,
    /// Documents directory (requires [`App::set_path`]).
    Documents,
    /// Downloads directory (requires [`App::set_path`]).
    Downloads,
    /// Home directory (requires [`App::set_path`]).
    Home,
    /// OS temporary directory (defaults to [`std::env::temp_dir`]).
    Temp,
}

type Listener = Box<dyn Fn()>;

/// Electron `app` module: lifecycle, identity, and paths.
///
/// Listeners registered with [`App::on`] fire in registration order; `ready`
/// fires exactly once and never after `quit()`. `quit()` emits
/// `before-quit` then `will-quit`. `note_window_closed(0)` emits
/// `window-all-closed` once per transition to zero (repeats without an
/// intervening nonzero report are dropped) and quits unless opted out
/// (macOS-style), matching Electron's default. No lifecycle event fires
/// after `quit()`.
pub struct App {
    name: String,
    version: String,
    ready: bool,
    quit: bool,
    quit_on_all_windows_closed: bool,
    listeners: HashMap<AppEventKind, Vec<Listener>>,
    paths: HashMap<AppPath, PathBuf>,
    last_window_count: Option<usize>,
    default_protocol_client: Option<String>,
    app_user_model_id: Option<String>,
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
            last_window_count: None,
            default_protocol_client: None,
            app_user_model_id: None,
        }
    }

    /// Application name (`app.getName`).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Override the application name (`app.setName`, issue #155): real
    /// bundles rename themselves at load (`app.setName("Joplin")`).
    pub fn set_name(&mut self, name: &str) {
        self.name = name.to_string();
    }

    /// Record a default-protocol-client registration
    /// (`app.setAsDefaultProtocolClient`, issue #155). The OS handler
    /// effect stays deferred (see coverage); the recording is real so the
    /// call reports success like Electron does.
    pub fn set_as_default_protocol_client(&mut self, protocol: &str) {
        self.default_protocol_client = Some(protocol.to_string());
    }

    /// Record the Windows application user model id
    /// (`app.setAppUserModelId`, issue #155). Headless has no taskbar
    /// integration, so like Electron off-Windows this records the id with
    /// no further effect.
    pub fn set_app_user_model_id(&mut self, id: &str) {
        self.app_user_model_id = Some(id.to_string());
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
    /// resolves). Later calls are no-ops, as are calls after `quit()`: a
    /// quit app never becomes ready, so embedders may call this
    /// unconditionally when init completes.
    pub fn mark_ready(&mut self) {
        if self.ready || self.quit {
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

    /// Called by the window manager with the remaining window count. The
    /// first zero report — and each later nonzero-to-zero transition —
    /// emits `window-all-closed` (and quits by default); repeated zero
    /// reports without an intervening nonzero count are dropped so poll
    /// loops and post-quit reports cannot re-fire listeners. Reports after
    /// `quit()` are recorded but emit nothing.
    pub fn note_window_closed(&mut self, windows_remaining: usize) {
        let duplicate_zero = windows_remaining == 0 && self.last_window_count == Some(0);
        self.last_window_count = Some(windows_remaining);
        if self.quit || windows_remaining > 0 || duplicate_zero {
            return;
        }
        self.emit(AppEventKind::WindowAllClosed);
        if self.quit_on_all_windows_closed {
            self.quit();
        }
    }

    /// Resolve a named path (`app.getPath`); explicit [`App::set_path`]
    /// values win, then OS defaults.
    ///
    /// [`AppPath::Temp`], [`AppPath::AppData`], and [`AppPath::UserData`]
    /// resolve out of the box: `UserData` is Electron's `appData/<name>`
    /// default (dynamic, so a load-time `setName` is reflected),
    /// `AppData` comes from the OS env. `Desktop`, `Documents`,
    /// `Downloads`, and `Home` still require [`App::set_path`] first —
    /// the Day-1 TS shim throws like Electron does for unknown names
    /// rather than unwrapping.
    pub fn get_path(&self, path: AppPath) -> Option<PathBuf> {
        if let Some(explicit) = self.paths.get(&path) {
            return Some(explicit.clone());
        }
        match path {
            AppPath::AppData => default_app_data(),
            AppPath::UserData => self
                .get_path(AppPath::AppData)
                .map(|base| base.join(&self.name)),
            _ => None,
        }
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

/// Shared event log plus its push closure (test helper).
#[cfg(test)]
type EventLog = (Rc<RefCell<Vec<String>>>, Rc<dyn Fn(&str)>);

/// Test helper: shared event log.
#[cfg(test)]
fn log() -> EventLog {
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
    // Electron's default: `userData` is `appData/<name>` until overridden.
    assert_eq!(
        app.get_path(AppPath::UserData),
        app.get_path(AppPath::AppData)
            .map(|base| base.join("QuickStart")),
        "userData defaults under appData, whatever the OS env provides"
    );
    let custom = std::path::PathBuf::from("/tmp/strake-test-profile");
    app.set_path(AppPath::UserData, custom.clone());
    assert_eq!(app.get_path(AppPath::UserData), Some(custom));
}

#[test]
fn user_data_default_follows_name_and_app_data_override() {
    let mut app = App::new("QuickStart", "1.0.0");
    // Real bundles rename at load (`app.setName("Joplin")`) before any
    // `getPath`: the default follows the current name.
    app.set_name("Joplin");
    assert_eq!(
        app.get_path(AppPath::UserData),
        app.get_path(AppPath::AppData)
            .map(|base| base.join("Joplin")),
    );
    app.set_path(
        AppPath::AppData,
        std::path::PathBuf::from("/tmp/strake-appdata"),
    );
    assert_eq!(
        app.get_path(AppPath::UserData),
        Some(std::path::PathBuf::from("/tmp/strake-appdata/Joplin")),
        "an appData override moves the userData default with it"
    );
}

#[test]
fn name_and_version_are_reported() {
    let app = App::new("QuickStart", "1.0.0");
    assert_eq!(app.name(), "QuickStart");
    assert_eq!(app.version(), "1.0.0");
}

#[test]
fn set_name_updates_reported_name() {
    let mut app = App::new("QuickStart", "1.0.0");
    app.set_name("Joplin");
    assert_eq!(app.name(), "Joplin");
}

#[test]
fn default_protocol_client_is_recorded() {
    let mut app = App::new("QuickStart", "1.0.0");
    assert_eq!(app.default_protocol_client, None);
    app.set_as_default_protocol_client("joplin");
    assert_eq!(app.default_protocol_client, Some(String::from("joplin")));
}

#[test]
fn app_user_model_id_is_recorded() {
    let mut app = App::new("QuickStart", "1.0.0");
    assert_eq!(app.app_user_model_id, None);
    app.set_app_user_model_id("net.cozic.joplin-desktop");
    assert_eq!(
        app.app_user_model_id,
        Some(String::from("net.cozic.joplin-desktop"))
    );
}

// PIN (review PR #79): `ready` must not fire after `quit()`. A quit during
// startup followed by a late `mark_ready()` must leave a dead app silent.
#[test]
fn ready_does_not_fire_after_quit() {
    let (events, push) = log();
    let mut app = App::new("QuickStart", "1.0.0");
    app.on(AppEventKind::Ready, move || push("ready"));
    app.quit();
    app.mark_ready();
    assert!(
        events.borrow().is_empty(),
        "no `ready` after quit, got {:?}",
        *events.borrow()
    );
    assert!(!app.is_ready(), "a quit app never becomes ready");
}

// PIN (review PR #79): `window-all-closed` fires once per transition to
// zero; repeated `note_window_closed(0)` reports must not re-emit.
#[test]
fn window_all_closed_fires_once_per_transition_to_zero() {
    let (events, push) = log();
    let mut app = App::new("QuickStart", "1.0.0");
    app.set_quit_on_all_windows_closed(false);
    app.on(AppEventKind::WindowAllClosed, move || {
        push("window-all-closed")
    });
    app.note_window_closed(0);
    app.note_window_closed(0);
    assert_eq!(
        *events.borrow(),
        vec!["window-all-closed"],
        "duplicate zero reports re-emitted"
    );
    app.note_window_closed(2);
    app.note_window_closed(0);
    assert_eq!(
        *events.borrow(),
        vec!["window-all-closed", "window-all-closed"],
        "a new nonzero-to-zero transition must emit again"
    );
}

// PIN (review PR #79, updated for issue #155): contract lock — `Temp`,
// `AppData`, and `UserData` have sound `std`-only OS defaults (env-based,
// the same source Electron reads); every other `AppPath` is `None` until
// `set_path`.
#[test]
fn only_defaulted_paths_resolve_without_set_path() {
    let app = App::new("QuickStart", "1.0.0");
    assert_eq!(app.get_path(AppPath::Temp), Some(std::env::temp_dir()));
    assert_eq!(
        app.get_path(AppPath::UserData),
        app.get_path(AppPath::AppData)
            .map(|base| base.join("QuickStart")),
        "userData defaults under appData"
    );
    for path in [
        AppPath::Desktop,
        AppPath::Documents,
        AppPath::Downloads,
        AppPath::Home,
    ] {
        assert_eq!(
            app.get_path(path),
            None,
            "{path:?} requires set_path: no sound std-only OS default"
        );
    }
}
