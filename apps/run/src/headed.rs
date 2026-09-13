//! Headed proof for an Electron app directory (issue #131).
//!
//! `--prove-ipc` proves everything headlessly. This module proves the headed
//! second half named in [`crate`] docs: the booted window's entry page is
//! handed to a real OS surface through [`ShellWindow`] →
//! `into_window_config` → `View::init` on a live winit event loop, using the
//! same [`StrakeApplication`] harness as the `rdme` viewer with the CPU
//! softbuffer renderer.
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
    StrakeApplication, StrakeShellEvent, StrakeShellProxy, create_default_event_loop,
};
use strake_vibey_script::boot_app_dir;
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

    let mut shell_window = ShellWindow::open(
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
    shell_window
        .load_html(&html)
        .map_err(|error| format!("headed: load_html: {error:?}"))?;
    if !shell_window.has_painted() {
        return Err(String::from("headed: entry page did not first-paint"));
    }
    println!(
        "headed: first paint ok, title={:?}",
        shell_window.page_title()
    );
    let body_text = shell_window
        .document()
        .and_then(|doc| doc.find_body_node())
        .map(|node| node.text_content())
        .unwrap_or_default();
    println!("headed: body text before handoff: {body_text:?}");
    if body_text.trim().is_empty() {
        return Err(String::from("headed: entry body has no text content"));
    }
    let config = shell_window
        .into_window_config(VelloCpuWindowRenderer::new())
        .map_err(|error| format!("headed: handoff: {error:?}"))?;
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
