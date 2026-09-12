use futures_util::task::ArcWake;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::{any::Any, sync::Arc};
use strake_traits::navigation::NavigationOptions;
use strake_traits::net::NetWaker;
use winit::{event_loop::EventLoopProxy, window::WindowId};

#[cfg(feature = "accessibility")]
use accesskit_xplat::WindowEvent as AccessKitEvent;

#[derive(Debug, Clone)]
pub enum StrakeShellEvent {
    Poll {
        window_id: WindowId,
    },

    /// The renderer for this window has finished its async initialization. The
    /// embedder should call `View::complete_resume` to transition the view into
    /// an active state.
    ResumeReady {
        window_id: WindowId,
    },

    RequestRedraw {
        doc_id: usize,
    },

    /// Close a window programmatically (e.g. a custom titlebar close button).
    /// Handled identically to `WindowEvent::CloseRequested`.
    CloseWindow {
        window_id: WindowId,
    },

    /// An accessibility event from `accesskit`.
    #[cfg(feature = "accessibility")]
    Accessibility {
        window_id: WindowId,
        data: Arc<AccessKitEvent>,
    },

    /// An arbitary event from the Strake embedder
    Embedder(Arc<dyn Any + Send + Sync>),

    /// Navigate to another URL (triggered by e.g. clicking a link)
    Navigate(Box<NavigationOptions>),

    /// Navigate to another URL (triggered by e.g. clicking a link)
    NavigationLoad {
        url: String,
        contents: String,
        retain_scroll_position: bool,
        is_md: bool,
    },

    /// Delivered after the WASM resize-debounce window expires. Route to
    /// `View::apply_pending_resize_if_settled`, which applies the pending
    /// size iff motion has actually settled.
    #[cfg(target_arch = "wasm32")]
    ResizeSettleCheck {
        window_id: WindowId,
    },
}
impl StrakeShellEvent {
    pub fn embedder_event<T: Any + Send + Sync>(value: T) -> Self {
        let boxed = Arc::new(value) as Arc<dyn Any + Send + Sync>;
        Self::Embedder(boxed)
    }
}

#[derive(Clone)]
pub struct StrakeShellProxy(Arc<StrakeShellProxyInner>);
pub struct StrakeShellProxyInner {
    winit_proxy: EventLoopProxy,
    sender: Sender<StrakeShellEvent>,
}

impl StrakeShellProxy {
    pub fn new(winit_proxy: EventLoopProxy) -> (Self, Receiver<StrakeShellEvent>) {
        let (sender, receiver) = channel();
        let proxy = Self(Arc::new(StrakeShellProxyInner {
            winit_proxy,
            sender,
        }));
        (proxy, receiver)
    }

    pub fn wake_up(&self) {
        self.0.winit_proxy.wake_up();
    }
    pub fn send_event(&self, event: impl Into<StrakeShellEvent>) {
        self.send_event_impl(event.into());
    }
    fn send_event_impl(&self, event: StrakeShellEvent) {
        let _ = self.0.sender.send(event);
        self.wake_up();
    }
}

impl NetWaker for StrakeShellProxy {
    fn wake(&self, client_id: usize) {
        self.send_event_impl(StrakeShellEvent::RequestRedraw { doc_id: client_id })
    }
}

/// Create a waker that will send a poll event to the event loop.
///
/// This lets the VirtualDom "come up for air" and process events while the main thread is blocked by the WebView.
///
/// All other IO lives in the Tokio runtime,
pub fn create_waker(proxy: &StrakeShellProxy, id: WindowId) -> std::task::Waker {
    struct DomHandle {
        proxy: StrakeShellProxy,
        id: WindowId,
    }
    impl ArcWake for DomHandle {
        fn wake_by_ref(arc_self: &Arc<Self>) {
            let event = StrakeShellEvent::Poll {
                window_id: arc_self.id,
            };
            arc_self.proxy.send_event(event)
        }
    }

    let proxy = proxy.clone();
    futures_util::task::waker(Arc::new(DomHandle { id, proxy }))
}
