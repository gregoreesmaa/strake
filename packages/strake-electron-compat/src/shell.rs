//! Compat `BrowserWindow` bound to a real `strake-shell` window (issue #82,
//! Slice 2).
//!
//! [`ShellWindow`] owns one compat-core window together with the DOM document
//! that will paint into it. Everything short of the OS window itself runs
//! headless (no event loop, no display): opening, `loadFile`/`loadURL`
//! through the DOM pipeline, resize, show/hide, and close — including the
//! OS-close → `closed` → `window-all-closed` propagation contract. At runtime
//! the app hands [`ShellWindow::into_window_config`] to
//! [`strake_shell::View::init`](https://github.com/gregoreesmaa/strake/blob/main/packages/strake-shell/src/window.rs)
//! on a live winit event loop, which creates the real OS window from the same
//! attributes the tests pin here.
//!
//! Deliberately out of scope (Slice 3 / later): renderer JS execution inside
//! the page (`webContents.executeJavaScript`), `dialog`/`shell`/`nativeTheme`,
//! and frameless/custom-titlebar eyecandy.

use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;
use std::sync::Arc;

use strake_dom::{BaseDocument, DEFAULT_CSS, DocumentConfig};
use strake_html::{DocumentHtmlParser, HtmlProvider};
use strake_traits::shell::{ColorScheme, Viewport};
use winit::dpi::{LogicalPosition, LogicalSize};
use winit::window::WindowAttributes;

use crate::{App, Bounds, BrowserWindowOptions, Screen, WindowManager};

/// Failures opening or driving a [`ShellWindow`].
#[derive(Debug)]
pub enum ShellError {
    /// The window is already closed (or its document was handed off).
    Destroyed,
    /// `loadFile` could not read the file.
    ReadFile {
        path: String,
        source: std::io::Error,
    },
}

impl fmt::Display for ShellError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Destroyed => write!(f, "window is closed"),
            Self::ReadFile { path, source } => write!(f, "failed to read {path}: {source}"),
        }
    }
}

impl std::error::Error for ShellError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Destroyed => None,
            Self::ReadFile { source, .. } => Some(source),
        }
    }
}

/// One compat `BrowserWindow` with its page document, driving (headless until
/// handoff) a real `strake-shell` window.
///
/// The window manager and app are shared handles so several windows (and the
/// Slice 1 JS host) observe one lifecycle: closing the last `ShellWindow`
/// runs the compat `window-all-closed` flow exactly like `win.close()` from
/// JS.
pub struct ShellWindow {
    manager: Rc<RefCell<WindowManager>>,
    app: Rc<RefCell<App>>,
    compat_id: u32,
    doc: Option<BaseDocument>,
    painted_once: bool,
    closed: bool,
}

impl ShellWindow {
    /// `new BrowserWindow(options)`: register the compat window and build its
    /// empty page document, viewport-seeded from the options size.
    pub fn open(
        manager: Rc<RefCell<WindowManager>>,
        app: Rc<RefCell<App>>,
        options: BrowserWindowOptions,
    ) -> Self {
        let compat_id = manager.borrow_mut().create(options.clone());
        let viewport = Viewport::new(options.width, options.height, 1.0, ColorScheme::Light);
        let config = DocumentConfig {
            viewport: Some(viewport),
            ua_stylesheets: Some(vec![String::from(DEFAULT_CSS)]),
            html_parser_provider: Some(Arc::new(HtmlProvider)),
            ..Default::default()
        };
        Self {
            manager,
            app,
            compat_id,
            doc: Some(BaseDocument::new(config)),
            painted_once: false,
            closed: false,
        }
    }

    /// Compat window id.
    pub fn compat_id(&self) -> u32 {
        self.compat_id
    }

    /// Whether [`ShellWindow::close`] has run.
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// Whether a page has resolved layout at least once (the headless
    /// "first paint": content flowed through style + layout).
    pub fn has_painted(&self) -> bool {
        self.painted_once
    }

    /// The page document (for inspection; `None` after handoff).
    pub fn document(&self) -> Option<&BaseDocument> {
        self.doc.as_ref()
    }

    /// The page `<title>`, if the loaded HTML sets one.
    pub fn page_title(&self) -> Option<String> {
        self.doc
            .as_ref()
            .and_then(|doc| doc.find_title_node())
            .map(|node| node.text_content())
    }

    fn require_open(&self) -> Result<(), ShellError> {
        if self.closed || self.doc.is_none() {
            return Err(ShellError::Destroyed);
        }
        Ok(())
    }

    /// Parse `html` into the page document and resolve layout at the window
    /// size (headless first paint). Syncs the parsed `<title>` into the
    /// compat `webContents` so `getTitle` serves the page title (issue #90).
    pub fn load_html(&mut self, html: &str) -> Result<(), ShellError> {
        self.require_open()?;
        let doc = self.doc.as_mut().ok_or(ShellError::Destroyed)?;
        let mut mutr = doc.mutate();
        DocumentHtmlParser::parse_into_mutator(&mut mutr, html);
        drop(mutr);
        doc.resolve(0.0);
        self.painted_once = true;
        let title = self.page_title();
        if let Some(win) = self.manager.borrow_mut().get_mut(self.compat_id) {
            win.web_contents_mut().set_document_title(title);
        }
        Ok(())
    }

    /// `win.loadFile`: record the navigation in the compat core, then load
    /// the file through the DOM pipeline like [`ShellWindow::load_html`].
    pub fn load_file(&mut self, path: &str) -> Result<(), ShellError> {
        self.require_open()?;
        let html = std::fs::read_to_string(path).map_err(|source| ShellError::ReadFile {
            path: path.to_string(),
            source,
        })?;
        if let Some(win) = self.manager.borrow_mut().get_mut(self.compat_id) {
            win.web_contents_mut().load_file(path);
        }
        self.load_html(&html)
    }

    /// Resize the window: compat geometry, page viewport, and a fresh layout.
    /// Mirrors what the app loop does on an OS resize event before the next
    /// frame.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<(), ShellError> {
        self.require_open()?;
        self.manager
            .borrow_mut()
            .resize(self.compat_id, width, height);
        let doc = self.doc.as_mut().ok_or(ShellError::Destroyed)?;
        doc.set_viewport(Viewport::new(width, height, 1.0, ColorScheme::Light));
        doc.resolve(0.0);
        Ok(())
    }

    /// `win.setBounds` (issue #90): move and resize like [`Self::resize`],
    /// additionally recording the content position in the compat core.
    pub fn set_bounds(&mut self, bounds: Bounds) -> Result<(), ShellError> {
        self.require_open()?;
        self.manager.borrow_mut().set_bounds(self.compat_id, bounds);
        let doc = self.doc.as_mut().ok_or(ShellError::Destroyed)?;
        doc.set_viewport(Viewport::new(
            bounds.width,
            bounds.height,
            1.0,
            ColorScheme::Light,
        ));
        doc.resolve(0.0);
        Ok(())
    }

    /// Center the window on the primary display (issue #96). Returns `true`
    /// once placed; `false` when the snapshot carries no metrics, in which
    /// case the headless fallback position is kept unchanged.
    pub fn center_on_screen(&mut self, screen: &Screen) -> Result<bool, ShellError> {
        self.require_open()?;
        let (width, height) = {
            let manager = self.manager.borrow();
            let win = manager.get(self.compat_id).ok_or(ShellError::Destroyed)?;
            (win.options().width, win.options().height)
        };
        match screen.suggest_centered_position(width, height) {
            Some((x, y)) => {
                self.manager.borrow_mut().set_bounds(
                    self.compat_id,
                    Bounds {
                        x,
                        y,
                        width,
                        height,
                    },
                );
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// `win.show`.
    pub fn show(&mut self) -> Result<(), ShellError> {
        self.require_open()?;
        self.manager.borrow_mut().show(self.compat_id);
        Ok(())
    }

    /// `win.hide`.
    pub fn hide(&mut self) -> Result<(), ShellError> {
        self.require_open()?;
        self.manager.borrow_mut().hide(self.compat_id);
        Ok(())
    }

    /// `win.setTitle` (compat chrome state; the loaded page's `<title>` still
    /// drives the OS chrome at handoff, matching `View::init`).
    pub fn set_title(&mut self, title: &str) -> Result<(), ShellError> {
        self.require_open()?;
        self.manager.borrow_mut().set_title(self.compat_id, title);
        Ok(())
    }

    /// Close the window (OS close button, `win.close`, or app shutdown all
    /// funnel here): destroy the compat window and run the `window-all-closed`
    /// flow when it was the last one. Returns `false` if already closed.
    pub fn close(&mut self) -> bool {
        if self.closed {
            return false;
        }
        self.closed = true;
        let remaining = {
            let mut manager = self.manager.borrow_mut();
            manager.close(self.compat_id);
            manager.window_count()
        };
        self.app.borrow_mut().note_window_closed(remaining);
        true
    }

    /// The exact winit attributes `View::init` will consume at handoff,
    /// derived from live compat state (`None` once closed). Compat [`Bounds`]
    /// are DIP, so geometry travels as `LogicalSize`/`LogicalPosition` and
    /// winit applies the monitor scale factor to reach physical pixels.
    pub fn window_attributes(&self) -> Option<WindowAttributes> {
        if self.closed {
            return None;
        }
        let manager = self.manager.borrow();
        let win = manager.get(self.compat_id)?;
        let options = win.options();
        let mut attrs = WindowAttributes::default()
            .with_title(win.title())
            .with_surface_size(LogicalSize::new(options.width, options.height))
            .with_visible(win.is_visible())
            .with_decorations(options.frame)
            .with_resizable(options.resizable)
            .with_transparent(options.transparent);
        if let Some((x, y)) = options.position {
            attrs = attrs.with_position(LogicalPosition::new(x, y));
        }
        if let Some((w, h)) = options.min_size {
            attrs = attrs.with_min_surface_size(LogicalSize::new(w, h));
        }
        if let Some((w, h)) = options.max_size {
            attrs = attrs.with_max_surface_size(LogicalSize::new(w, h));
        }
        Some(attrs)
    }

    /// Hand the live document and attributes to `strake-shell`: the app builds
    /// `View::init(WindowConfig, event_loop, proxy)` from this, creating the
    /// real OS window. Consumes the binding; the compat window stays live in
    /// the shared manager so later `close()` (e.g. from the OS event loop)
    /// still propagates.
    pub fn into_window_config<Rend: anyrender::WindowRenderer>(
        mut self,
        renderer: Rend,
    ) -> Result<strake_shell::WindowConfig<Rend>, ShellError> {
        self.require_open()?;
        let attrs = self.window_attributes().ok_or(ShellError::Destroyed)?;
        let doc = self.doc.take().ok_or(ShellError::Destroyed)?;
        Ok(strake_shell::WindowConfig::with_attributes(
            Box::new(doc),
            renderer,
            attrs,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE_HTML: &str = r#"<!DOCTYPE html>
<html><head><title>Fixture</title></head>
<body><h1>Hello shell</h1></body></html>"#;

    fn handles() -> (Rc<RefCell<WindowManager>>, Rc<RefCell<App>>) {
        (
            Rc::new(RefCell::new(WindowManager::new())),
            Rc::new(RefCell::new(App::new("QuickStart", "1.0.0"))),
        )
    }

    fn logical_size(attrs: &WindowAttributes) -> Option<(f64, f64)> {
        match attrs.surface_size {
            Some(winit::dpi::Size::Logical(size)) => Some((size.width, size.height)),
            _ => None,
        }
    }

    #[test]
    fn open_load_paint_close_flow() {
        let (manager, app) = handles();
        let mut win = ShellWindow::open(
            Rc::clone(&manager),
            Rc::clone(&app),
            BrowserWindowOptions {
                width: 1024,
                height: 768,
                title: String::from("QuickStart"),
                ..Default::default()
            },
        );
        assert!(!win.is_closed() && !win.has_painted());
        assert_eq!(manager.borrow().window_count(), 1);

        // Load fixture HTML -> first paint through the DOM pipeline.
        win.load_html(FIXTURE_HTML).expect("load fixture");
        assert!(win.has_painted(), "layout resolves (headless first paint)");
        assert_eq!(win.page_title().as_deref(), Some("Fixture"));
        let text = win
            .document()
            .expect("document live")
            .find_body_node()
            .expect("body parsed")
            .text_content();
        assert!(
            text.contains("Hello shell"),
            "page content parsed: {text:?}"
        );

        // Resize mirrors into compat geometry and the page viewport.
        win.resize(800, 600).expect("resize");
        let stored = manager
            .borrow()
            .get(win.compat_id())
            .expect("live")
            .options()
            .clone();
        assert_eq!((stored.width, stored.height), (800, 600));
        assert_eq!(
            logical_size(&win.window_attributes().expect("attrs")),
            Some((800.0, 600.0))
        );

        // Show/hide/set_title drive compat chrome state.
        win.hide().expect("hide");
        assert!(
            !manager
                .borrow()
                .get(win.compat_id())
                .expect("live")
                .is_visible()
        );
        win.show().expect("show");
        win.set_title("Renamed").expect("set_title");
        assert_eq!(
            manager.borrow().get(win.compat_id()).expect("live").title(),
            "Renamed"
        );

        // Close propagates: compat window destroyed, app quits by default.
        assert!(win.close(), "first close reports true");
        assert!(win.is_closed());
        assert_eq!(manager.borrow().window_count(), 0);
        assert!(app.borrow().is_quit(), "last close quits the app");
        assert!(win.window_attributes().is_none(), "no attrs after close");
    }

    #[test]
    fn attributes_mirror_options() {
        let (manager, app) = handles();
        let win = ShellWindow::open(
            manager,
            app,
            BrowserWindowOptions {
                width: 400,
                height: 300,
                frame: false,
                transparent: true,
                show: false,
                position: Some((10, 20)),
                min_size: Some((200, 150)),
                max_size: Some((800, 600)),
                title: String::from("Attrs"),
                ..Default::default()
            },
        );
        let attrs = win.window_attributes().expect("attrs while open");
        assert_eq!(attrs.title, "Attrs");
        assert_eq!(logical_size(&attrs), Some((400.0, 300.0)));
        assert!(!attrs.visible);
        assert!(!attrs.decorations);
        assert!(attrs.transparent);
        assert!(attrs.position.is_some(), "position propagates");
        assert!(attrs.min_surface_size.is_some(), "min size propagates");
        assert!(attrs.max_surface_size.is_some(), "max size propagates");
    }

    #[test]
    fn load_file_records_navigation_and_paints() {
        let path = std::env::temp_dir().join(format!(
            "strake-shell-window-test-{}.html",
            std::process::id()
        ));
        std::fs::write(&path, FIXTURE_HTML).expect("write fixture");
        let path_str = path.to_string_lossy().to_string();

        let (manager, app) = handles();
        let mut win = ShellWindow::open(Rc::clone(&manager), app, BrowserWindowOptions::default());
        win.load_file(&path_str).expect("load_file");

        let pending = manager
            .borrow()
            .get(win.compat_id())
            .expect("live")
            .web_contents()
            .pending_url()
            .map(str::to_string);
        assert!(
            pending.as_deref().is_some_and(|url| url.ends_with(".html")),
            "compat navigation records the file URL, got {pending:?}"
        );
        assert_eq!(win.page_title().as_deref(), Some("Fixture"));
        assert!(win.has_painted());

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn load_missing_file_errors_without_state_change() {
        let (manager, app) = handles();
        let mut win = ShellWindow::open(manager, app, BrowserWindowOptions::default());
        let err = win
            .load_file("/nonexistent/strake-missing-page.html")
            .expect_err("missing file");
        assert!(
            matches!(err, ShellError::ReadFile { .. }),
            "typed IO error, got {err}"
        );
        assert!(!win.has_painted(), "failed load paints nothing");
    }

    #[test]
    fn ops_after_close_fail_and_close_is_idempotent() {
        let (manager, app) = handles();
        let mut win = ShellWindow::open(manager, app, BrowserWindowOptions::default());
        assert!(win.close());
        assert!(!win.close(), "second close reports false");
        assert!(matches!(win.load_html("x"), Err(ShellError::Destroyed)));
        assert!(matches!(win.resize(1, 1), Err(ShellError::Destroyed)));
        assert!(matches!(win.show(), Err(ShellError::Destroyed)));
    }

    #[test]
    fn window_manager_resize_tracks_geometry() {
        let mut manager = WindowManager::new();
        let id = manager.create(BrowserWindowOptions::default());
        assert!(manager.resize(id, 1280, 720));
        let stored = manager.get(id).expect("live").options().clone();
        assert_eq!((stored.width, stored.height), (1280, 720));
        assert!(!manager.resize(404, 1, 1), "unknown id fails softly");
    }

    #[test]
    fn resizable_reaches_window_attributes() {
        let (manager, app) = handles();
        let fixed = ShellWindow::open(
            Rc::clone(&manager),
            Rc::clone(&app),
            BrowserWindowOptions {
                resizable: false,
                ..Default::default()
            },
        );
        assert!(!fixed.window_attributes().expect("attrs").resizable);
        let fluid = ShellWindow::open(manager, app, BrowserWindowOptions::default());
        assert!(fluid.window_attributes().expect("attrs").resizable);
    }

    #[test]
    fn set_bounds_moves_and_resizes_viewport() {
        let (manager, app) = handles();
        let mut win = ShellWindow::open(manager, app, BrowserWindowOptions::default());
        win.set_bounds(Bounds {
            x: 40,
            y: 50,
            width: 1024,
            height: 768,
        })
        .expect("set_bounds");
        let stored = win
            .manager
            .borrow()
            .get(win.compat_id())
            .expect("live")
            .bounds();
        assert_eq!(
            stored,
            Bounds {
                x: 40,
                y: 50,
                width: 1024,
                height: 768,
            }
        );
        assert_eq!(
            logical_size(&win.window_attributes().expect("attrs")),
            Some((1024.0, 768.0))
        );
    }

    #[test]
    fn load_syncs_page_title_into_web_contents() {
        let (manager, app) = handles();
        let mut win = ShellWindow::open(Rc::clone(&manager), app, BrowserWindowOptions::default());
        assert_eq!(
            manager
                .borrow()
                .get(win.compat_id())
                .expect("live")
                .web_contents()
                .get_title(),
            "",
            "no page loaded yet"
        );
        win.load_html(FIXTURE_HTML).expect("load fixture");
        assert_eq!(
            manager
                .borrow()
                .get(win.compat_id())
                .expect("live")
                .web_contents()
                .get_title(),
            "Fixture"
        );
    }

    #[test]
    fn center_on_screen_uses_primary_metrics_with_headless_fallback() {
        use crate::{Display, Screen};
        let screen = Screen::new(vec![Display::new(
            0,
            Bounds {
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
            },
            1.0,
        )]);
        let (manager, app) = handles();
        let mut win = ShellWindow::open(
            Rc::clone(&manager),
            app,
            BrowserWindowOptions {
                width: 800,
                height: 600,
                ..Default::default()
            },
        );
        assert!(win.center_on_screen(&screen).expect("center with metrics"));
        assert_eq!(
            manager
                .borrow()
                .get(win.compat_id())
                .expect("live")
                .bounds(),
            Bounds {
                x: 560,
                y: 240,
                width: 800,
                height: 600,
            }
        );

        let empty = Screen::new(vec![]);
        assert!(
            !win.center_on_screen(&empty)
                .expect("empty metrics must not error"),
            "no metrics: keep fallback"
        );
        assert_eq!(
            manager
                .borrow()
                .get(win.compat_id())
                .expect("live")
                .bounds()
                .x,
            560,
            "fallback position unchanged"
        );
    }
}
