//! Headed proof for an Electron app directory (issue #131).
//!
//! `--prove-ipc` proves everything headlessly. This module proves the headed
//! second half named in [`crate`] docs: the booted window's entry page is
//! painted through the full renderer pipeline ([`paint_app_window`]: preload,
//! page scripts, linked stylesheets) and handed to a real OS surface through
//! [`ShellWindow`] attributes → `WindowConfig` → `View::init` on a live winit
//! event loop, using the same [`StrakeApplication`] harness as the `rdme`
//! viewer with the CPU softbuffer renderer.
//!
//! The run is self-terminating (AGENTS.md): after `open_secs` seconds a timer
//! thread asks for close, the harness closes the compat window (which must
//! fire `window-all-closed` and quit the app), and the event loop exits.
//! Exit status is 0 only when the headless boot was clean, the OS window
//! opened, at least one redraw was requested headed, and the close flow
//! fired `window-all-closed` with the app quitting.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

use anyrender_vello_cpu::VelloCpuWindowRenderer;
use strake_electron_compat::{App, AppEventKind, BrowserWindowOptions, ShellWindow, WindowManager};
use strake_shell::{
    StrakeApplication, StrakeShellEvent, StrakeShellProxy, WindowConfig, create_default_event_loop,
};
use strake_vibey_script::{boot_app_dir, paint_app_window};
use winit::application::ApplicationHandler;
use winit::event::{StartCause, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::window::WindowId;

/// Timer marker: close the headed window and finish the proof.
struct HeadedClose;

/// Shared observation flags (the winit `ApplicationHandler` is consumed by
/// `run_app`, so the report is read back through these).
#[derive(Debug, Default)]
struct HeadedFlags {
    /// An OS window existed after surfaces could be created.
    opened: Cell<bool>,
    /// At least one headed redraw was requested for the window.
    redrawn: Cell<bool>,
    /// The compat `window-all-closed` listener ran during close.
    window_all_closed: Cell<bool>,
    /// `into_window_config` consumed the binding (handoff happened).
    handed_off: Cell<bool>,
}

struct HeadedApp {
    inner: StrakeApplication<VelloCpuWindowRenderer>,
    manager: Rc<RefCell<WindowManager>>,
    app: Rc<RefCell<App>>,
    compat_id: u32,
    flags: Rc<HeadedFlags>,
    closed: bool,
    os_reported: bool,
}

impl HeadedApp {
    fn observe_windows(&mut self) {
        if self.inner.windows.is_empty() {
            return;
        }
        self.flags.opened.set(true);
        if !self.os_reported {
            self.os_reported = true;
            for view in self.inner.windows.values() {
                println!(
                    "headed: os window shell-visible={} os-visible={:?} minimized={:?} pos={:?} size={:?}",
                    view.is_visible,
                    view.window.is_visible(),
                    view.window.is_minimized(),
                    view.window.outer_position(),
                    view.window.outer_size(),
                );
            }
        }
    }

    /// Close the compat window through the lifecycle flow (`manager.close` +
    /// `note_window_closed`), so both the timer path and a real OS close
    /// (user clicking the traffic light) fire `window-all-closed` and quit.
    /// Returns the remaining window count when this call closed something.
    fn drive_compat_close(&mut self) -> Option<usize> {
        if self.closed {
            return None;
        }
        self.closed = true;
        let remaining = {
            let mut manager = self.manager.borrow_mut();
            manager.close(self.compat_id);
            manager.window_count()
        };
        self.app.borrow_mut().note_window_closed(remaining);
        println!(
            "headed: window closed (remaining {remaining}), window-all-closed={} app-quit={}",
            self.flags.window_all_closed.get(),
            self.app.borrow().is_quit(),
        );
        Some(remaining)
    }
}

impl ApplicationHandler for HeadedApp {
    #[cfg(target_os = "macos")]
    fn macos_handler(
        &mut self,
    ) -> Option<&mut dyn winit::platform::macos::ApplicationHandlerExtMacOS> {
        self.inner.macos_handler()
    }

    fn resumed(&mut self, event_loop: &dyn ActiveEventLoop) {
        self.inner.resumed(event_loop);
    }

    fn suspended(&mut self, event_loop: &dyn ActiveEventLoop) {
        self.inner.suspended(event_loop);
    }

    fn can_create_surfaces(&mut self, event_loop: &dyn ActiveEventLoop) {
        self.inner.can_create_surfaces(event_loop);
        self.observe_windows();
    }

    fn destroy_surfaces(&mut self, event_loop: &dyn ActiveEventLoop) {
        self.inner.destroy_surfaces(event_loop);
    }

    fn new_events(&mut self, event_loop: &dyn ActiveEventLoop, cause: StartCause) {
        self.inner.new_events(event_loop, cause);
    }

    fn window_event(
        &mut self,
        event_loop: &dyn ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        if matches!(event, WindowEvent::RedrawRequested) {
            self.flags.redrawn.set(true);
        }
        if matches!(event, WindowEvent::CloseRequested) {
            // A real OS close (traffic light, Cmd+W) drives the same compat
            // close flow as the timer; the delegated handler below then drops
            // the view and exits the loop.
            self.drive_compat_close();
        }
        self.observe_windows();
        self.inner.window_event(event_loop, window_id, event);
    }

    fn proxy_wake_up(&mut self, event_loop: &dyn ActiveEventLoop) {
        while let Ok(event) = self.inner.event_queue.try_recv() {
            match event {
                StrakeShellEvent::Embedder(marker)
                    if marker.downcast_ref::<HeadedClose>().is_some() =>
                {
                    self.drive_compat_close();
                    event_loop.exit();
                }
                event => self.inner.handle_strake_shell_event(event_loop, event),
            }
        }
        self.observe_windows();
    }
}

/// Prove the headed half for `<app-dir>`'s first window: open it on a real OS
/// surface for `open_secs` seconds, then close it through the compat close
/// flow. Prints the headed report; `Err` (exit 1) unless every stage holds.
pub fn prove_headed(app_dir: &Path, open_secs: u64) -> Result<(), String> {
    let report = boot_app_dir(app_dir).map_err(|error| format!("boot: {error}"))?;
    if !report.js_errors.is_empty() {
        return Err(format!("boot: main JS errors: {:?}", report.js_errors));
    }
    let window = report
        .windows
        .first()
        .ok_or_else(|| String::from("boot: app created no windows"))?;
    if let Some(preload) = &window.preload
        && !window.preload_errors.is_empty()
    {
        return Err(format!(
            "boot: preload {preload} errors: {:?}",
            window.preload_errors
        ));
    }
    let entry_target = window.entry_file.clone();
    let plain_path = Path::new(&entry_target);
    let entry_path = if plain_path.is_file() {
        plain_path.to_path_buf()
    } else {
        entry_target
            .strip_prefix("file://")
            .map(Path::new)
            .filter(|path| path.is_file())
            .ok_or_else(|| format!("headed: entry is not a local file: {entry_target}"))?
            .to_path_buf()
    };
    let html = std::fs::read_to_string(&entry_path).map_err(|error| format!("headed: {error}"))?;
    println!(
        "headed: boot clean ({}), window #{} {}x{} entry={}",
        report.app_name,
        window.id,
        window.width,
        window.height,
        entry_path.display(),
    );

    let manager = Rc::new(RefCell::new(WindowManager::new()));
    let app = Rc::new(RefCell::new(App::new(&report.app_name, "0.0.0")));
    let flags = Rc::new(HeadedFlags::default());
    {
        let fired = Rc::clone(&flags);
        app.borrow_mut().on(AppEventKind::WindowAllClosed, move || {
            fired.window_all_closed.set(true);
        });
    }

    // The compat window owns lifecycle + OS attributes; the paint document is
    // built by `paint_app_window`, which runs the full renderer pipeline
    // (issues #145, #146). It cannot live in `ShellWindow`: script execution
    // needs `strake-vibey-script`, which already depends on the compat crate.
    let shell_window = ShellWindow::open(
        Rc::clone(&manager),
        Rc::clone(&app),
        BrowserWindowOptions {
            width: window.width,
            height: window.height,
            title: window.page_title.clone().unwrap_or(report.app_name.clone()),
            show: true,
            ..Default::default()
        },
    );
    let compat_id = shell_window.compat_id();
    // Resolve the window's preload file like boot does: an absolute path, or
    // a name joined onto the app dir. A declared-but-missing preload fails
    // the proof: silently painting without it is exactly issue #145.
    let preload_path = window.preload.as_deref().map(Path::new).and_then(|raw| {
        if raw.is_file() {
            return Some(raw.to_path_buf());
        }
        raw.file_name()
            .map(|name| app_dir.join(name))
            .filter(|path| path.is_file())
    });
    if window.preload.is_some() && preload_path.is_none() {
        return Err(format!(
            "headed: preload {:?} not found under {}",
            window.preload,
            app_dir.display(),
        ));
    }
    let preload_source = preload_path
        .map(|path| std::fs::read_to_string(&path))
        .transpose()
        .map_err(|error| format!("headed: cannot read preload: {error}"))?;
    let painted = paint_app_window(
        &html,
        &entry_path,
        &report.app_name,
        window.width,
        window.height,
        preload_source.as_deref(),
    )
    .map_err(|error| format!("headed: paint: {error}"))?;
    if !painted.js_errors.is_empty() {
        return Err(format!(
            "headed: preload/page script errors: {:?}",
            painted.js_errors
        ));
    }
    println!("headed: first paint ok, title={:?}", painted.title);
    println!("headed: body text before handoff: {:?}", painted.body_text);
    if painted.body_text.trim().is_empty() {
        return Err(String::from("headed: entry body has no text content"));
    }
    // Issue #145: preload DOM effects must reach the screen. The proof runs
    // against apps whose preload stamps `process.versions` (the `0.0.0-strake`
    // fallbacks until #144 lands a real ABI); a clean preload with no stamps
    // in the painted body means the effects were dropped at handoff.
    if painted.had_preload {
        if !painted.body_text.contains("0.0.0-strake") {
            return Err(String::from(
                "headed: preload ran clean but version stamps are missing \
                 from the painted body (preload effects dropped)",
            ));
        }
        println!("headed: preload effects present (version stamps in painted body)");
    }
    // Issue #146: every linked same-origin stylesheet must have fetched, and
    // an author style must have taken effect.
    if painted.expected_stylesheets.is_empty() {
        println!("headed: no linked same-origin stylesheets; author-style assertion skipped");
    } else {
        let missing: Vec<&String> = painted
            .expected_stylesheets
            .iter()
            .filter(|url| !painted.served_urls.contains(url))
            .collect();
        if !missing.is_empty() {
            return Err(format!(
                "headed: linked stylesheets never fetched: {missing:?}"
            ));
        }
        if painted.author_stylesheet_count == 0 {
            return Err(String::from(
                "headed: stylesheets fetched but no author style took effect",
            ));
        }
        println!(
            "headed: author styles applied ({} linked stylesheet(s) fetched, {} in cascade)",
            painted.served_urls.len(),
            painted.author_stylesheet_count,
        );
    }
    let attributes = shell_window
        .window_attributes()
        .ok_or_else(|| String::from("headed: window closed before handoff"))?;
    let config = WindowConfig::with_attributes(
        Box::new(painted.document),
        VelloCpuWindowRenderer::new(),
        attributes,
    );
    flags.handed_off.set(true);

    let event_loop = create_default_event_loop();
    let winit_proxy = event_loop.create_proxy();
    let (proxy, event_queue) = StrakeShellProxy::new(winit_proxy);
    let proof = HeadedApp {
        inner: {
            let mut inner = StrakeApplication::new(proxy.clone(), event_queue);
            inner.add_window(config);
            inner
        },
        manager,
        app: Rc::clone(&app),
        compat_id,
        flags: Rc::clone(&flags),
        closed: false,
        os_reported: false,
    };

    let closer: StrakeShellProxy = proxy.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(open_secs.max(1)));
        closer.send_event(StrakeShellEvent::embedder_event(HeadedClose));
    });

    event_loop
        .run_app(proof)
        .map_err(|error| format!("headed: event loop: {error}"))?;

    if !flags.opened.get() {
        return Err(String::from("headed: no OS window opened"));
    }
    if !flags.redrawn.get() {
        return Err(String::from("headed: OS window never redrew"));
    }
    if !flags.window_all_closed.get() {
        return Err(String::from("headed: close did not fire window-all-closed"));
    }
    if !app.borrow().is_quit() {
        return Err(String::from("headed: app did not quit after close"));
    }
    println!(
        "headed: OPENED + REDREW + window-all-closed + quit ({}x{} proven)",
        window.width, window.height,
    );
    Ok(())
}
