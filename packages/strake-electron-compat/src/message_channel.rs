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
    ///
    /// A send on a closed port, or to a closed peer, is a silent no-op:
    /// per the HTML `message-port-post-message` steps a disentangled port
    /// resolves its target to null on both sides, and browsers never throw
    /// for closed ports (only for unserializable payloads). There is
    /// deliberately no `Err` surface here, so the QuickJS binding must map
    /// this call to `undefined` unconditionally — never to a JS exception.
    pub fn post_message(&mut self, port: Port, message: Value) {
        if self.is_closed(port) {
            return;
        }
        if !self.is_closed(port.peer()) {
            self.inbox[Self::index(port.peer())].push_back(message);
        }
    }

    /// Drain `port`'s inbox in FIFO order (`onmessage` batch).
    pub fn take_messages(&mut self, port: Port) -> Vec<Value> {
        self.inbox[Self::index(port)].drain(..).collect()
    }

    /// Queued messages on `port`.
    pub fn pending_count(&self, port: Port) -> usize {
        self.inbox[Self::index(port)].len()
    }

    /// `port.close()`: further sends from this port, and sends to it, drop.
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
        channel.post_message(Port::First, json!(1));
        channel.post_message(Port::First, json!(2));
        channel.post_message(Port::Second, json!("back"));
        assert_eq!(
            channel.take_messages(Port::Second),
            vec![json!(1), json!(2)]
        );
        assert_eq!(channel.take_messages(Port::First), vec![json!("back")]);
        assert!(channel.take_messages(Port::First).is_empty());
    }

    #[test]
    fn close_is_silent_noop_both_directions() {
        let mut channel = MessageChannel::new();
        assert!(!channel.is_closed(Port::First));
        channel.close(Port::First);
        assert!(channel.is_closed(Port::First));
        // Sending ON a closed port is a silent no-op (browsers never throw
        // for closed ports): nothing is enqueued anywhere.
        channel.post_message(Port::First, json!(1));
        assert_eq!(channel.pending_count(Port::Second), 0);
        // The live peer can still send; its payloads drop at the closed end.
        channel.post_message(Port::Second, json!(2));
        assert_eq!(
            channel.pending_count(Port::First),
            0,
            "dropped at closed peer"
        );
        channel.close(Port::Second);
        channel.post_message(Port::Second, json!(3));
        channel.post_message(Port::First, json!(4));
        assert_eq!(channel.pending_count(Port::First), 0);
        assert_eq!(channel.pending_count(Port::Second), 0);
    }

    #[test]
    fn peer_is_involutive() {
        assert_eq!(Port::First.peer(), Port::Second);
        assert_eq!(Port::Second.peer(), Port::First);
    }
}
