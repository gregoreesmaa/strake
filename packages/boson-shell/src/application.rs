use crate::event::{BosonShellEvent, BosonShellProxy};

use anyrender::WindowRenderer;
use std::collections::HashMap;
use std::sync::mpsc::Receiver;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;
use winit::window::WindowId;

#[cfg(target_os = "macos")]
use winit::platform::macos::ApplicationHandlerExtMacOS;

use crate::{View, WindowConfig};

pub struct BosonApplication<Rend: WindowRenderer> {
    pub windows: HashMap<WindowId, View<Rend>>,
    pub pending_windows: Vec<WindowConfig<Rend>>,
    pub proxy: BosonShellProxy,
    pub event_queue: Receiver<BosonShellEvent>,
}

impl<Rend: WindowRenderer> BosonApplication<Rend> {
    pub fn new(proxy: BosonShellProxy, event_queue: Receiver<BosonShellEvent>) -> Self {
        BosonApplication {
            windows: HashMap::new(),
            pending_windows: Vec::new(),
            proxy,
            event_queue,
        }
    }

    pub fn add_window(&mut self, window_config: WindowConfig<Rend>) {
        self.pending_windows.push(window_config);
    }

    fn window_mut_by_doc_id(&mut self, doc_id: usize) -> Option<&mut View<Rend>> {
        self.windows.values_mut().find(|w| w.doc.id() == doc_id)
    }

    pub fn handle_boson_shell_event(
        &mut self,
        event_loop: &dyn ActiveEventLoop,
        event: BosonShellEvent,
    ) {
        match event {
            BosonShellEvent::Poll { window_id } => {
                if let Some(window) = self.windows.get_mut(&window_id) {
                    window.poll();
                };
            }
            BosonShellEvent::CloseWindow { window_id } => {
                // Drop window before exiting event loop
                // See https://github.com/rust-windowing/winit/issues/4135
                let window = self.windows.remove(&window_id);
                drop(window);
                if self.windows.is_empty() {
                    event_loop.exit();
                }
            }
            BosonShellEvent::ResumeReady { window_id } => {
                // The renderer fires `on_ready` after it has sent on the
                // channel, so `complete_resume` should always succeed here.
                // If a stale event survives a suspend, dropping it is safe.
                if let Some(window) = self.windows.get_mut(&window_id) {
                    let ok = window.complete_resume();
                    debug_assert!(ok, "ResumeReady received but renderer not ready");
                }
            }
            BosonShellEvent::RequestRedraw { doc_id } => {
                // TODO: Handle multiple documents per window
                if let Some(window) = self.window_mut_by_doc_id(doc_id) {
                    window.request_redraw();
                }
            }

            #[cfg(feature = "accessibility")]
            BosonShellEvent::Accessibility { window_id, data } => {
                if let Some(window) = self.windows.get_mut(&window_id) {
                    match &*data {
                        accesskit_xplat::WindowEvent::InitialTreeRequested => {
                            window.build_accessibility_tree();
                        }
                        accesskit_xplat::WindowEvent::AccessibilityDeactivated => {
                            // TODO
                        }
                        accesskit_xplat::WindowEvent::ActionRequested(_req) => {
                            // TODO
                        }
                    }
                }
            }
            BosonShellEvent::Embedder(_) => {
                // Do nothing. Should be handled by embedders (if required).
            }
            BosonShellEvent::Navigate(_opts) => {
                // Do nothing. Should be handled by embedders (if required).
            }
            BosonShellEvent::NavigationLoad { .. } => {
                // Do nothing. Should be handled by embedders (if required).
            }
            #[cfg(target_arch = "wasm32")]
            BosonShellEvent::ResizeSettleCheck { window_id } => {
                if let Some(window) = self.windows.get_mut(&window_id) {
                    window.apply_pending_resize_if_settled();
                }
            }
        }
    }
}

impl<Rend: WindowRenderer> ApplicationHandler for BosonApplication<Rend> {
    fn can_create_surfaces(&mut self, event_loop: &dyn ActiveEventLoop) {
        // Resume existing windows
        for view in self.windows.values_mut() {
            view.resume();
        }

        // Initialise pending windows. The renderer's resume is non-blocking —
        // on native it finishes inline, on wasm32 it spawns a future that will
        // dispatch BosonShellEvent::ResumeReady when init completes. Either way
        // we insert the view immediately so the event handler can find it.
        for window_config in self.pending_windows.drain(..) {
            let mut view = View::init(window_config, event_loop, &self.proxy);
            view.resume();
            self.windows.insert(view.window_id(), view);
        }
    }

    fn destroy_surfaces(&mut self, _event_loop: &dyn ActiveEventLoop) {
        for view in self.windows.values_mut() {
            view.suspend();
        }
    }

    fn resumed(&mut self, _event_loop: &dyn ActiveEventLoop) {
        // TODO
    }

    fn suspended(&mut self, _event_loop: &dyn ActiveEventLoop) {
        // TODO
    }

    fn window_event(
        &mut self,
        event_loop: &dyn ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        // Exit the app when window close is requested.
        if matches!(event, WindowEvent::CloseRequested) {
            // Drop window before exiting event loop
            // See https://github.com/rust-windowing/winit/issues/4135
            let window = self.windows.remove(&window_id);
            drop(window);
            if self.windows.is_empty() {
                event_loop.exit();
            }
            return;
        }

        if let Some(window) = self.windows.get_mut(&window_id) {
            window.handle_winit_event(event);
        }
        self.proxy.send_event(BosonShellEvent::Poll { window_id });
    }

    fn proxy_wake_up(&mut self, event_loop: &dyn ActiveEventLoop) {
        while let Ok(event) = self.event_queue.try_recv() {
            self.handle_boson_shell_event(event_loop, event);
        }
    }

    #[cfg(target_os = "macos")]
    fn macos_handler(&mut self) -> Option<&mut dyn ApplicationHandlerExtMacOS> {
        Some(self)
    }

    #[cfg(target_os = "ios")]
    fn about_to_wait(&mut self, _event_loop: &dyn ActiveEventLoop) {
        for view in self.windows.values_mut() {
            if view.ios_request_redraw.get() {
                view.window.request_redraw();
            }
        }
    }
}

#[cfg(target_os = "macos")]
impl<Rend: WindowRenderer> ApplicationHandlerExtMacOS for BosonApplication<Rend> {
    fn standard_key_binding(
        &mut self,
        _event_loop: &dyn ActiveEventLoop,
        window_id: WindowId,
        action: &str,
    ) {
        if let Some(window) = self.windows.get_mut(&window_id) {
            window.handle_apple_standard_keybinding(action);
            self.proxy.send_event(BosonShellEvent::Poll { window_id });
        }
    }
}
