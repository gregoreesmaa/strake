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
}

impl fmt::Display for IpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownChannel(channel) => write!(f, "no IPC handler for channel {channel:?}"),
            Self::HandlerFailed(message) => write!(f, "IPC handler failed: {message}"),
        }
    }
}

impl std::error::Error for IpcError {}

type Handler = Box<dyn Fn(Value) -> Result<Value, String>>;
type Listener = Box<dyn Fn(&Value)>;

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
    listeners: HashMap<String, Vec<Listener>>,
}

impl IpcBus {
    /// An empty bus.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an `ipcMain.handle(channel, handler)` responder,
    /// replacing any previous handler for the channel.
    pub fn handle(
        &mut self,
        channel: &str,
        handler: impl Fn(Value) -> Result<Value, String> + 'static,
    ) {
        self.handlers.insert(channel.to_string(), Box::new(handler));
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

    /// Subscribe an `ipcMain.on` / `ipcRenderer.on` listener.
    pub fn on(&mut self, channel: &str, listener: impl Fn(&Value) + 'static) {
        self.listeners
            .entry(channel.to_string())
            .or_default()
            .push(Box::new(listener));
    }

    /// Drop every listener on a channel (`removeListener`/`removeAllListeners`).
    pub fn remove_all_listeners(&mut self, channel: &str) {
        self.listeners.remove(channel);
    }

    /// `ipcRenderer.send(channel, value)`: broadcast to channel listeners.
    /// Channels without listeners are a silent no-op.
    pub fn send(&self, channel: &str, value: Value) {
        if let Some(listeners) = self.listeners.get(channel) {
            for listener in listeners {
                listener(&value);
            }
        }
    }
}

#[cfg(test)]
use serde_json::json;

#[test]
fn invoke_round_trip_returns_handler_value() {
    let mut bus = IpcBus::new();
    bus.handle("get-data", |args| Ok(json!({"echo": args})));
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
    bus.handle("boom", |_| Err(String::from("kaput")));
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

#[test]
fn removed_routes_go_quiet() {
    let mut bus = IpcBus::new();
    bus.handle("get-data", |args| Ok(args));
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
