//! `MessageChannel` / `MessagePort` primitives (issue #15, MVP first step).
//!
//! The issue's amendment orders the work: correct `postMessage` /
//! `MessageChannel` semantics first, zero-copy transferables and
//! sub-15µs tuning later. This module is that first step — a headless,
//! fully-tested port pair with FIFO delivery and close semantics — which the
//! QuickJS worker runtime and the multi-window transport bind to next.
//! Payloads are [`serde_json::Value`] today (JSON first, zero-copy later,
//! per the crate's IPC contract).

use std::collections::VecDeque;

use serde_json::Value;

/// One end of a [`MessageChannel`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Port {
    /// `channel.port1`.
    First,
    /// `channel.port2`.
    Second,
}

impl Port {
    /// The entangled peer.
    pub fn peer(self) -> Self {
        match self {
            Self::First => Self::Second,
            Self::Second => Self::First,
        }
    }
}

/// Failures posting a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelError {
    /// The sending port is closed (programmer error).
    PortClosed,
}

impl std::fmt::Display for ChannelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PortClosed => write!(f, "MessagePort is closed"),
        }
    }
}

impl std::error::Error for ChannelError {}

/// An entangled port pair (`new MessageChannel()`).
///
/// Each port has an inbox; `post_message` on one port enqueues into the
/// peer's inbox. Delivery is synchronous and FIFO; there is no task source
/// yet (the worker runtime drains inboxes on its event loop).
#[derive(Debug, Default)]
pub struct MessageChannel {
    inbox: [VecDeque<Value>; 2],
    closed: [bool; 2],
}

impl MessageChannel {
    /// A new entangled pair.
    pub fn new() -> Self {
        Self::default()
    }

    fn index(port: Port) -> usize {
        match port {
            Port::First => 0,
            Port::Second => 1,
        }
    }

    /// Whether `port` is closed.
    pub fn is_closed(&self, port: Port) -> bool {
        self.closed[Self::index(port)]
    }

    /// `port.postMessage(value)`: enqueue into the peer's inbox.
    /// `Err(PortClosed)` when the sending port is closed; messages to a
    /// closed peer are dropped (matching the spec's neutered-port posture).
    pub fn post_message(&mut self, port: Port, message: Value) -> Result<(), ChannelError> {
        if self.is_closed(port) {
            return Err(ChannelError::PortClosed);
        }
        if !self.is_closed(port.peer()) {
            self.inbox[Self::index(port.peer())].push_back(message);
        }
        Ok(())
    }

    /// Drain `port`'s inbox in FIFO order (`onmessage` batch).
    pub fn take_messages(&mut self, port: Port) -> Vec<Value> {
        self.inbox[Self::index(port)].drain(..).collect()
    }

    /// Queued messages on `port`.
    pub fn pending_count(&self, port: Port) -> usize {
        self.inbox[Self::index(port)].len()
    }

    /// `port.close()`: further sends from this port fail; sends to it drop.
    pub fn close(&mut self, port: Port) {
        self.closed[Self::index(port)] = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ports_exchange_fifo_both_directions() {
        let mut channel = MessageChannel::new();
        channel.post_message(Port::First, json!(1)).expect("send");
        channel.post_message(Port::First, json!(2)).expect("send");
        channel
            .post_message(Port::Second, json!("back"))
            .expect("send");
        assert_eq!(
            channel.take_messages(Port::Second),
            vec![json!(1), json!(2)]
        );
        assert_eq!(channel.take_messages(Port::First), vec![json!("back")]);
        assert!(channel.take_messages(Port::First).is_empty());
    }

    #[test]
    fn close_semantics() {
        let mut channel = MessageChannel::new();
        assert!(!channel.is_closed(Port::First));
        channel.close(Port::First);
        assert!(channel.is_closed(Port::First));
        assert_eq!(
            channel.post_message(Port::First, json!(1)),
            Err(ChannelError::PortClosed),
            "sending on a closed port fails"
        );
        // The live peer can still send; its payloads drop at the closed end.
        channel.post_message(Port::Second, json!(2)).expect("send");
        assert_eq!(
            channel.pending_count(Port::First),
            0,
            "dropped at closed peer"
        );
        channel.close(Port::Second);
        assert_eq!(
            channel.post_message(Port::Second, json!(3)),
            Err(ChannelError::PortClosed)
        );
    }

    #[test]
    fn peer_is_involutive() {
        assert_eq!(Port::First.peer(), Port::Second);
        assert_eq!(Port::Second.peer(), Port::First);
    }
}
