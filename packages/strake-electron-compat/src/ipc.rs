//! `ipcMain` / `ipcRenderer` over an in-process JSON-string bus.
//!
//! MVP transport (issue #21: JSON-string IPC first, zero-copy later):
//! `handle`/`invoke` request/response plus `send`/`on` fan-out, all carrying
//! [`serde_json::Value`] payloads. Main and renderer share the bus
//! in-process for now; the channel contract is what the later TS shim and
//! multi-process transport bind to.

use std::collections::HashMap;
use std::fmt;

use serde_json::Value;

/// IPC failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IpcError {
    /// No handler (or no live route) for the channel.
    UnknownChannel(String),
    /// The channel handler rejected the invocation.
    HandlerFailed(String),
    /// A handler is already registered for the channel, matching Electron's
    /// `ipcMain.handle` throw ("Attempted to register a second handler").
    DuplicateHandler(String),
}

impl fmt::Display for IpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownChannel(channel) => write!(f, "no IPC handler for channel {channel:?}"),
            Self::HandlerFailed(message) => write!(f, "IPC handler failed: {message}"),
            Self::DuplicateHandler(channel) => write!(
                f,
                "attempted to register a second handler for channel {channel:?}"
            ),
        }
    }
}

impl std::error::Error for IpcError {}

type Handler = Box<dyn Fn(Value) -> Result<Value, String>>;
type Listener = Box<dyn Fn(&Value)>;

/// Handle to one `on` listener, returned for `removeListener`.
pub type ListenerId = u64;

struct ListenerEntry {
    id: ListenerId,
    callback: Listener,
}

/// In-process JSON IPC bus (`ipcMain` + `ipcRenderer`).
///
/// `handle` registers an `ipcMain.handle` responder invoked by
/// `ipcRenderer.invoke`; `on`/`send` is the `ipcMain.on` /
/// `ipcRenderer.send` broadcast. Delivery is synchronous and in registration
/// order. A multi-process (or zero-copy shared-memory) transport replaces
/// this bus later without changing call shapes.
#[derive(Default)]
pub struct IpcBus {
    handlers: HashMap<String, Handler>,
    listeners: HashMap<String, Vec<ListenerEntry>>,
    next_listener_id: ListenerId,
}

impl IpcBus {
    /// An empty bus.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an `ipcMain.handle(channel, handler)` responder. Errors
    /// with [`IpcError::DuplicateHandler`] when the channel already has a
    /// handler, matching Electron's throw; call [`IpcBus::remove_handler`]
    /// first to replace one intentionally.
    pub fn handle(
        &mut self,
        channel: &str,
        handler: impl Fn(Value) -> Result<Value, String> + 'static,
    ) -> Result<(), IpcError> {
        if self.handlers.contains_key(channel) {
            return Err(IpcError::DuplicateHandler(channel.to_string()));
        }
        self.handlers.insert(channel.to_string(), Box::new(handler));
        Ok(())
    }

    /// Remove an `ipcMain.handle` registration.
    pub fn remove_handler(&mut self, channel: &str) {
        self.handlers.remove(channel);
    }

    /// `ipcRenderer.invoke(channel, args)`: call the handler synchronously.
    pub fn invoke(&self, channel: &str, args: Value) -> Result<Value, IpcError> {
        match self.handlers.get(channel) {
            Some(handler) => handler(args).map_err(IpcError::HandlerFailed),
            None => Err(IpcError::UnknownChannel(channel.to_string())),
        }
    }

    /// Subscribe an `ipcMain.on` / `ipcRenderer.on` listener, returning its
    /// handle for [`IpcBus::remove_listener`]. Ids are monotonic and never
    /// reused, so a stale id simply matches nothing.
    pub fn on(&mut self, channel: &str, listener: impl Fn(&Value) + 'static) -> ListenerId {
        let id = self.next_listener_id;
        self.next_listener_id += 1;
        self.listeners
            .entry(channel.to_string())
            .or_default()
            .push(ListenerEntry {
                id,
                callback: Box::new(listener),
            });
        id
    }

    /// Remove one listener by handle (`removeListener`); `false` when the
    /// channel or id is unknown. Removing the last listener drops the
    /// channel entry.
    pub fn remove_listener(&mut self, channel: &str, id: ListenerId) -> bool {
        let empty = {
            let Some(entries) = self.listeners.get_mut(channel) else {
                return false;
            };
            let before = entries.len();
            entries.retain(|entry| entry.id != id);
            if entries.len() == before {
                return false;
            }
            entries.is_empty()
        };
        if empty {
            self.listeners.remove(channel);
        }
        true
    }

    /// Drop every listener on a channel (`removeAllListeners`).
    pub fn remove_all_listeners(&mut self, channel: &str) {
        self.listeners.remove(channel);
    }

    /// `ipcRenderer.send(channel, value)`: broadcast to channel listeners.
    /// Channels without listeners are a silent no-op.
    pub fn send(&self, channel: &str, value: Value) {
        if let Some(listeners) = self.listeners.get(channel) {
            for entry in listeners {
                (entry.callback)(&value);
            }
        }
    }
}

#[cfg(test)]
use serde_json::json;

#[test]
fn invoke_round_trip_returns_handler_value() {
    let mut bus = IpcBus::new();
    bus.handle("get-data", |args| Ok(json!({"echo": args})))
        .expect("first registration");
    let reply = bus
        .invoke("get-data", json!({"n": 1}))
        .expect("known channel");
    assert_eq!(reply, json!({"echo": {"n": 1}}));
}

#[test]
fn invoke_unknown_channel_errors() {
    let bus = IpcBus::new();
    let err = bus
        .invoke("missing", json!(null))
        .expect_err("unknown channel");
    assert_eq!(err, IpcError::UnknownChannel(String::from("missing")));
}

#[test]
fn handler_failure_propagates() {
    let mut bus = IpcBus::new();
    bus.handle("boom", |_| Err(String::from("kaput")))
        .expect("first registration");
    let err = bus.invoke("boom", json!(null)).expect_err("handler error");
    assert_eq!(err, IpcError::HandlerFailed(String::from("kaput")));
}

#[test]
fn send_fans_out_to_all_listeners() {
    let mut bus = IpcBus::new();
    let first = std::rc::Rc::new(std::cell::RefCell::new(0u32));
    let second = std::rc::Rc::new(std::cell::RefCell::new(0u32));
    for counter in [std::rc::Rc::clone(&first), std::rc::Rc::clone(&second)] {
        bus.on("tick", move |_| *counter.borrow_mut() += 1);
    }
    bus.send("tick", json!({}));
    assert_eq!((*first.borrow(), *second.borrow()), (1, 1));
    // Sending with no listeners is a silent no-op.
    bus.send("nobody-listens", json!({}));
}

// PIN (review PR #79): duplicate `handle` registration must error like
// Electron ("Attempted to register a second handler"), not silently swap.
#[test]
fn duplicate_handler_registration_errors() {
    let mut bus = IpcBus::new();
    bus.handle("get-data", Ok)
        .expect("first registration succeeds");
    let err = bus
        .handle("get-data", Ok)
        .expect_err("second registration must error");
    assert_eq!(err, IpcError::DuplicateHandler(String::from("get-data")));
}

// PIN (review PR #79): single-listener removal must leave siblings live.
#[test]
fn single_listener_removal_keeps_siblings() {
    use std::cell::RefCell;
    use std::rc::Rc;
    let mut bus = IpcBus::new();
    let first = Rc::new(RefCell::new(0u32));
    let second = Rc::new(RefCell::new(0u32));
    let first_id = bus.on("tick", {
        let first = Rc::clone(&first);
        move |_| *first.borrow_mut() += 1
    });
    bus.on("tick", {
        let second = Rc::clone(&second);
        move |_| *second.borrow_mut() += 1
    });
    assert!(bus.remove_listener("tick", first_id), "known id removes");
    bus.send("tick", json!({}));
    assert_eq!(
        (*first.borrow(), *second.borrow()),
        (0, 1),
        "removed listener stays quiet, sibling still fires"
    );
    assert!(
        !bus.remove_listener("tick", first_id),
        "double removal reports false"
    );
    assert!(
        !bus.remove_listener("missing", 999),
        "unknown channel reports false"
    );
}

#[test]
fn removed_routes_go_quiet() {
    let mut bus = IpcBus::new();
    bus.handle("get-data", Ok).expect("first registration");
    bus.on("tick", |_| panic!("must not fire after removal"));
    bus.remove_handler("get-data");
    bus.remove_all_listeners("tick");
    assert_eq!(
        bus.invoke("get-data", json!(null))
            .expect_err("removed handler"),
        IpcError::UnknownChannel(String::from("get-data"))
    );
    bus.send("tick", json!(null));
}
