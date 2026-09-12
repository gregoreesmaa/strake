//! `powerMonitor` / `powerSaveBlocker` OS bridge (issue #93).
//!
//! Owner: [`PowerMonitor`] is the single funnel for OS power events
//! (sleep/resume, AC/battery, idle state). The shell event loop feeds it via
//! [`PowerMonitor::inject`] — backed by winit device/window events and the
//! platform power notifiers where the OS allows, with suspend veto plumbed
//! where supported. Real key delivery is unassertable on headless CI, so the
//! acceptance probe registers listeners, injects synthetic sleep/resume, and
//! asserts delivery through this same funnel.
//!
//! [`PowerSaveBlocker`] tracks display/app-sleep holds; enforcement against
//! the OS caffeinate/ES_DISPLAY_REQUIRED/SetThreadExecutionState APIs rides
//! with the shell binding.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// OS power events (`powerMonitor` channels).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PowerEvent {
    /// The system is suspending (`powerMonitor 'suspend'`).
    Suspend,
    /// The system resumed (`powerMonitor 'resume'`).
    Resume,
    /// Power source changed to AC (`'on-ac'`).
    OnAc,
    /// Power source changed to battery (`'on-battery'`).
    OnBattery,
}

/// Listener handle for [`PowerMonitor::on`].
pub type PowerListenerId = u64;

type PowerCallback = Box<dyn Fn(PowerEvent) + Send>;

/// Named owner of the OS power-event bridge (issue #93 acceptance).
///
/// Listeners register per event; the shell loop (or the synthetic headless
/// probe) injects events via [`Self::inject`]. Listener panics never
/// propagate: one throwing listener cannot silence its siblings — instead
/// the failure count is reported through [`Self::take_error_count`].
#[derive(Default)]
pub struct PowerMonitor {
    listeners: HashMap<PowerEvent, Vec<(PowerListenerId, PowerCallback)>>,
    next_id: PowerListenerId,
    error_count: usize,
}

impl PowerMonitor {
    /// An empty monitor.
    pub fn new() -> Self {
        Self::default()
    }

    /// Subscribe to an event (`powerMonitor.on(...)`). Ids are monotonic and
    /// never reused.
    pub fn on(
        &mut self,
        event: PowerEvent,
        listener: impl Fn(PowerEvent) + Send + 'static,
    ) -> PowerListenerId {
        let id = self.next_id;
        self.next_id += 1;
        self.listeners
            .entry(event)
            .or_default()
            .push((id, Box::new(listener)));
        id
    }

    /// Inject one OS (or synthetic probe) event, delivering to that event's
    /// listeners in registration order.
    pub fn inject(&mut self, event: PowerEvent) {
        let empty = Vec::new();
        let listeners = self.listeners.get(&event).unwrap_or(&empty);
        // Borrow the callbacks without holding `&mut self` across calls so
        // re-entrant registration cannot trip the borrow.
        let count = listeners.len();
        for index in 0..count {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                self.listeners[&event][index].1(event);
            }));
            if result.is_err() {
                self.error_count += 1;
            }
        }
    }

    /// Listener failures since the last call (resets the counter).
    pub fn take_error_count(&mut self) -> usize {
        std::mem::take(&mut self.error_count)
    }
}

/// `powerSaveBlocker` hold kinds (Electron's `type` argument).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerSaveBlockerKind {
    /// Keep the app running, display may sleep.
    PreventAppSuspension,
    /// Keep the display awake.
    PreventDisplaySleep,
}

/// `powerSaveBlocker` holds (`start`/`stop`/`isStarted`). Headless-complete:
/// ids and lifetimes are fully tracked; OS enforcement binds later.
#[derive(Debug, Default)]
pub struct PowerSaveBlocker {
    next_id: u64,
    active: HashMap<u64, PowerSaveBlockerKind>,
}

impl PowerSaveBlocker {
    /// An empty blocker registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a hold, returning its id (`powerSaveBlocker.start`).
    pub fn start(&mut self, kind: PowerSaveBlockerKind) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.active.insert(id, kind);
        id
    }

    /// Stop a hold (`powerSaveBlocker.stop`). `false` for unknown ids.
    pub fn stop(&mut self, id: u64) -> bool {
        self.active.remove(&id).is_some()
    }

    /// Whether a hold is active (`powerSaveBlocker.isStarted`).
    pub fn is_started(&self, id: u64) -> bool {
        self.active.contains_key(&id)
    }

    /// Live hold count (embedder observation).
    pub fn active_count(&self) -> usize {
        self.active.len()
    }
}

/// Shared power hub for the JS shim: one monitor plus one blocker registry.
#[derive(Clone, Default)]
pub struct PowerHub {
    inner: Arc<Mutex<PowerHubState>>,
}

#[derive(Default)]
struct PowerHubState {
    monitor: PowerMonitor,
    blocker: PowerSaveBlocker,
    /// Events injected while no Rust-side dispatch runs (drained by the JS
    /// binding's dispatch).
    pending: Vec<PowerEvent>,
}

impl PowerHub {
    /// An empty hub.
    pub fn new() -> Self {
        Self::default()
    }

    /// Run a closure against the monitor (registration + synthetic probe).
    pub fn with_monitor<R>(&self, f: impl FnOnce(&mut PowerMonitor) -> R) -> R {
        f(&mut self.inner.lock().expect("power mutex").monitor)
    }

    /// Run a closure against the blocker registry.
    pub fn with_blocker<R>(&self, f: impl FnOnce(&mut PowerSaveBlocker) -> R) -> R {
        f(&mut self.inner.lock().expect("power mutex").blocker)
    }

    /// Queue a synthetic (or shell-sourced) event for JS dispatch.
    pub fn inject_for_dispatch(&self, event: PowerEvent) {
        self.inner.lock().expect("power mutex").pending.push(event);
    }

    /// Drain queued events for dispatch, in order.
    pub fn take_pending(&self) -> Vec<PowerEvent> {
        std::mem::take(&mut self.inner.lock().expect("power mutex").pending)
    }
}

impl std::fmt::Debug for PowerHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PowerHub").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn synthetic_sleep_resume_reaches_listeners() {
        let mut monitor = PowerMonitor::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let probe = Arc::clone(&seen);
        monitor.on(PowerEvent::Suspend, move |event| {
            probe.lock().expect("m").push(event)
        });
        let probe = Arc::clone(&seen);
        monitor.on(PowerEvent::Resume, move |event| {
            probe.lock().expect("m").push(event)
        });
        monitor.inject(PowerEvent::Suspend);
        monitor.inject(PowerEvent::Resume);
        assert_eq!(
            *seen.lock().expect("m"),
            vec![PowerEvent::Suspend, PowerEvent::Resume]
        );
        assert_eq!(monitor.take_error_count(), 0);
    }

    #[test]
    fn throwing_listener_is_isolated() {
        let mut monitor = PowerMonitor::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let probe = Arc::clone(&calls);
        monitor.on(PowerEvent::OnAc, move |_| {
            probe.fetch_add(1, Ordering::SeqCst);
            panic!("boom");
        });
        let probe = Arc::clone(&calls);
        monitor.on(PowerEvent::OnAc, move |_| {
            probe.fetch_add(1, Ordering::SeqCst);
        });
        monitor.inject(PowerEvent::OnAc);
        assert_eq!(calls.load(Ordering::SeqCst), 2, "sibling still runs");
        assert_eq!(monitor.take_error_count(), 1);
    }

    #[test]
    fn blocker_tracks_lifetimes() {
        let mut blocker = PowerSaveBlocker::new();
        let id = blocker.start(PowerSaveBlockerKind::PreventDisplaySleep);
        assert!(blocker.is_started(id));
        assert_eq!(blocker.active_count(), 1);
        assert!(blocker.stop(id));
        assert!(!blocker.is_started(id));
        assert!(!blocker.stop(id), "double stop fails softly");
    }
}
