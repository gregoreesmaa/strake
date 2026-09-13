//! Renderer `Notification` plus the OS delivery bridge (issue #92).
//!
//! Shape follows Web Notifications (`title`, `body`, `icon`, `onclick`):
//! the renderer binding (in `strake-vibey-script`) constructs a request and
//! hands it to [`NotificationCenter`], which delivers through a
//! [`NotificationBackend`]. Production delivery (Notification Center on
//! macOS, Toast on Windows, libnotify on Linux) binds this trait in a
//! follow-up; CI and headless tests use the [`RecordingBackend`]
//! pass-through, which records every delivery and replays clicks so
//! `onclick` dispatch stays fully testable without an OS.

use std::sync::{Arc, Mutex};

/// One `new Notification(title, { body, icon })` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationRequest {
    /// Notification title (first constructor argument).
    pub title: String,
    /// Body text (`options.body`).
    pub body: Option<String>,
    /// Icon URL or path (`options.icon`).
    pub icon: Option<String>,
}

/// A delivered notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveredNotification {
    /// Delivery id (for click dispatch).
    pub id: u64,
    /// The original request.
    pub request: NotificationRequest,
    /// Whether the user (or the test probe) clicked it.
    pub clicked: bool,
}

/// OS delivery backend. The trait boundary is the whole "OS delivery bridge":
/// real platforms implement delivery + click callbacks here, while
/// [`RecordingBackend`] stands in headless.
pub trait NotificationBackend: Send + Sync {
    /// Deliver a request, returning its delivery id.
    fn deliver(&self, request: NotificationRequest) -> u64;
    /// Deliveries so far, in order.
    fn delivered(&self) -> Vec<DeliveredNotification>;
    /// Dispatch a click on a delivery (`true` when the id exists).
    fn click(&self, id: u64) -> bool;
}

/// Headless pass-through: records deliveries and replays clicks (issue #92
/// acceptance: notify → recorded delivery → click dispatch → `onclick`).
#[derive(Debug, Default)]
pub struct RecordingBackend {
    inner: Mutex<RecordingState>,
}

#[derive(Debug, Default)]
struct RecordingState {
    next_id: u64,
    delivered: Vec<DeliveredNotification>,
}

impl RecordingBackend {
    /// An empty recorder.
    pub fn new() -> Self {
        Self::default()
    }
}

impl NotificationBackend for RecordingBackend {
    fn deliver(&self, request: NotificationRequest) -> u64 {
        let mut state = self.inner.lock().expect("notification mutex");
        let id = state.next_id;
        state.next_id += 1;
        state.delivered.push(DeliveredNotification {
            id,
            request,
            clicked: false,
        });
        id
    }

    fn delivered(&self) -> Vec<DeliveredNotification> {
        self.inner
            .lock()
            .expect("notification mutex")
            .delivered
            .clone()
    }

    fn click(&self, id: u64) -> bool {
        let mut state = self.inner.lock().expect("notification mutex");
        match state.delivered.iter_mut().find(|item| item.id == id) {
            Some(item) => {
                item.clicked = true;
                true
            }
            None => false,
        }
    }
}

/// Renderer-facing notification hub (`new window.Notification(...)`).
#[derive(Clone)]
pub struct NotificationCenter {
    backend: Arc<dyn NotificationBackend>,
}

impl NotificationCenter {
    /// Deliver through an explicit backend (OS bridge at runtime, recorder
    /// in tests).
    pub fn new(backend: Arc<dyn NotificationBackend>) -> Self {
        Self { backend }
    }

    /// Headless/CI hub backed by [`RecordingBackend`].
    pub fn recording() -> Self {
        Self::new(Arc::new(RecordingBackend::new()))
    }

    /// Show a notification; returns its delivery id for click dispatch.
    pub fn notify(&self, request: NotificationRequest) -> u64 {
        self.backend.deliver(request)
    }

    /// Deliveries so far, in order.
    pub fn delivered(&self) -> Vec<DeliveredNotification> {
        self.backend.delivered()
    }

    /// Dispatch a click (`true` when the id exists). The renderer binding
    /// fires the matching `onclick` after this returns true.
    pub fn click(&self, id: u64) -> bool {
        self.backend.click(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> NotificationRequest {
        NotificationRequest {
            title: String::from("Build done"),
            body: Some(String::from("All tests pass")),
            icon: None,
        }
    }

    #[test]
    fn notify_records_delivery_then_click_dispatch() {
        let center = NotificationCenter::recording();
        let id = center.notify(request());
        let delivered = center.delivered();
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].id, id);
        assert_eq!(delivered[0].request, request());
        assert!(!delivered[0].clicked);

        assert!(center.click(id), "known id dispatches");
        assert!(center.delivered()[0].clicked);
        assert!(!center.click(999), "unknown id dispatches nothing");
    }

    #[test]
    fn deliveries_keep_fifo_order() {
        let center = NotificationCenter::recording();
        let first = center.notify(NotificationRequest {
            title: String::from("one"),
            body: None,
            icon: None,
        });
        let second = center.notify(NotificationRequest {
            title: String::from("two"),
            body: None,
            icon: Some(String::from("icon.png")),
        });
        assert!(first < second);
        let delivered = center.delivered();
        let titles: Vec<&str> = delivered
            .iter()
            .map(|item| item.request.title.as_str())
            .collect();
        assert_eq!(titles, vec!["one", "two"]);
    }
}
