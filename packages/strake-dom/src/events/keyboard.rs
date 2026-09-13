use crate::{BaseDocument, node::GeneratedTextInputEvent, util::ACTION_MOD};
use keyboard_types::{Key, Modifiers};
use markup5ever::local_name;
use strake_traits::node_id::NodeId;
use strake_traits::{
    SmolStr,
    events::{DomEvent, DomEventData, StrakeInputEvent, StrakeKeyEvent},
};

pub(super) enum KeyboardOrTextInputEvent {
    KeyPress(StrakeKeyEvent),
    AppleStandardKeyBinding(SmolStr),
}

pub(crate) fn handle_key_or_input_event<F: FnMut(DomEvent)>(
    doc: &mut BaseDocument,
    target: NodeId,
    event: KeyboardOrTextInputEvent,
    mut dispatch_event: F,
) {
    if let KeyboardOrTextInputEvent::KeyPress(event) = &event {
        if event.key == Key::Tab {
            if event.modifiers.contains(Modifiers::SHIFT) {
                doc.focus_prev_node();
            } else {
                doc.focus_next_node();
            }
            return;
        }

        // Enter activates the focused control on key down (issue #70). Like
        // Tab above, this runs after `:focus-visible` arming in the caller.
        if event.state.is_pressed() && event.key == Key::Enter {
            try_keyboard_activate(doc, target, event.modifiers, &mut dispatch_event);
        }

        // Handle copy (Ctrl+C/Cmd+C) for text selection when no text input is focused
        if event.state.is_pressed() {
            let action_mod = event.modifiers.contains(ACTION_MOD);
            if action_mod {
                if let Key::Character(c) = &event.key {
                    if c.to_lowercase() == "c" {
                        // Check if we have a text selection (and no focused text input)
                        let has_focused_text_input = doc.focus_node_id.is_some_and(|id| {
                            doc.get_node(id)
                                .and_then(|n| n.element_data())
                                .is_some_and(|e| e.text_input_data().is_some())
                        });

                        if !has_focused_text_input {
                            if let Some(text) = doc.get_selected_text() {
                                let _ = doc.shell_provider.set_clipboard_text(text);
                                return;
                            }
                        }
                    }
                }
            }
        }
    }

    if let Some(node_id) = doc.focus_node_id {
        if target != node_id {
            return;
        }

        let node = &mut doc.nodes[node_id];
        let Some(element_data) = node.element_data_mut() else {
            return;
        };

        if let Some(input_data) = element_data.text_input_data_mut() {
            let generated_event = match event {
                KeyboardOrTextInputEvent::KeyPress(strake_key_event) => input_data
                    .apply_keypress_event(
                        &mut doc.font_ctx.lock().unwrap(),
                        &mut doc.layout_ctx,
                        &*doc.shell_provider,
                        strake_key_event,
                    ),
                KeyboardOrTextInputEvent::AppleStandardKeyBinding(command) => input_data
                    .apply_apple_standard_keybinding(
                        &mut doc.font_ctx.lock().unwrap(),
                        &mut doc.layout_ctx,
                        &*doc.shell_provider,
                        &command,
                    ),
            };

            if let Some(generated_event) = generated_event {
                doc.apply_generated_text_input_event(node_id, generated_event, dispatch_event);
            }
        }
    }
}

/// Space activates the focused control on key *up*, so holding Space does not
/// repeat-activate (issue #70). KeyUp otherwise has no default action.
pub(crate) fn handle_keyup(
    doc: &mut BaseDocument,
    target: NodeId,
    event: &StrakeKeyEvent,
    dispatch_event: &mut dyn FnMut(DomEvent),
) {
    if event.state.is_pressed() {
        return;
    }
    if matches!(&event.key, Key::Character(text) if text == " ") {
        try_keyboard_activate(doc, target, event.modifiers, dispatch_event);
    }
}

/// Dispatch a synthetic click on the focused control for keyboard activation
/// (issue #70): Enter on key down, Space on key up. Text inputs consume
/// these keys for editing, so they never activate. Queuing the click reaches
/// script handlers first and then the click default action (checkbox
/// toggling, details expansion, link navigation) exactly like a mouse click,
/// including `preventDefault` handling by the driver.
fn try_keyboard_activate(
    doc: &mut BaseDocument,
    target: NodeId,
    mods: Modifiers,
    dispatch_event: &mut dyn FnMut(DomEvent),
) {
    let focused = doc.focus_node_id.unwrap_or(target);
    let Some(node) = doc.nodes.get(focused) else {
        return;
    };
    let Some(element) = node.element_data() else {
        return;
    };
    // Text inputs (and textareas) consume Enter/Space for editing.
    if element.text_input_data().is_some() {
        return;
    }
    let tag = element.name.local.clone();
    let activatable = tag == local_name!("button")
        || tag == local_name!("summary")
        || (tag == local_name!("input")
            && !matches!(element.attr(local_name!("type")), Some("hidden")))
        || (tag == local_name!("a") && element.attr(local_name!("href")).is_some());
    if !activatable {
        return;
    }
    let Some(node) = doc.nodes.get(focused) else {
        return;
    };
    if let click @ DomEventData::Click(_) = node.synthetic_click_event(mods) {
        dispatch_event(DomEvent::new(focused, click));
    }
}

impl BaseDocument {
    pub(crate) fn apply_generated_text_input_event<F: FnMut(DomEvent)>(
        &mut self,
        node_id: NodeId,
        event: GeneratedTextInputEvent,
        mut dispatch_event: F,
    ) {
        let node = &mut self.nodes[node_id];
        let element_data = node
            .element_data_mut()
            .expect("apply_generated_text_input_event called on a node that is not an element");
        let input_data = element_data
            .text_input_data_mut()
            .expect("apply_generated_text_input_event called on a node that is not a text input");

        match event {
            GeneratedTextInputEvent::Input => {
                let value = input_data.editor.raw_text().to_string();
                dispatch_event(DomEvent::new(
                    node_id,
                    DomEventData::Input(StrakeInputEvent { value }),
                ));
                self.shell_provider.request_redraw();
            }
            GeneratedTextInputEvent::Select | GeneratedTextInputEvent::PreEditChange => {
                self.shell_provider.request_redraw();
            }
            GeneratedTextInputEvent::Submit => {
                // TODO: Generate submit event that can be handled by script
                implicit_form_submission(self, node_id);
            }
        }
    }
}

/// https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#field-that-blocks-implicit-submission
fn implicit_form_submission(doc: &BaseDocument, text_target: NodeId) {
    let Some(form_owner_id) = doc.controls_to_form.get(&text_target) else {
        return;
    };
    if doc
        .controls_to_form
        .iter()
        .filter(|(_control_id, form_id)| *form_id == form_owner_id)
        .filter_map(|(control_id, _)| doc.nodes[*control_id].element_data())
        .filter(|element_data| {
            element_data.attr(local_name!("type")).is_some_and(|t| {
                matches!(
                    t,
                    "text"
                        | "search"
                        | "email"
                        | "url"
                        | "tel"
                        | "password"
                        | "date"
                        | "month"
                        | "week"
                        | "time"
                        | "datetime-local"
                        | "number"
                )
            })
        })
        .count()
        > 1
    {
        return;
    }

    doc.submit_form(*form_owner_id, *form_owner_id);
}
