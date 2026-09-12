//! Integration with boson-dom's event driver

use boson_dom::{Document, EventHandler, NodeId};
use boson_traits::events::{DomEvent, EventState};

use crate::runtime::ScriptRuntime;

/// An [`EventHandler`] which dispatches DOM events to JavaScript event listeners
/// before Boson's default actions run.
pub(crate) struct ScriptEventHandler<'rt> {
    pub runtime: &'rt mut ScriptRuntime,
}

impl EventHandler for ScriptEventHandler<'_> {
    fn handle_event(
        &mut self,
        chain: &[NodeId],
        event: &mut DomEvent,
        _doc: &mut dyn Document,
        event_state: &mut EventState,
    ) {
        self.runtime.dispatch_dom_event(chain, event, event_state);
    }
}
