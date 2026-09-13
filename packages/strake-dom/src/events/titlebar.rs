//! DOM-driven frameless window controls (issue #54).
//!
//! When the OS window is undecorated, page content can drive the window the
//! way Electron's `-webkit-app-region` does: `data-drag` regions start a
//! window move, edge/corner bands start a resize (with resize cursors on
//! hover), and `data-minimize` / `data-maximize` / `data-fullscreen` /
//! `data-close` markers trigger window controls. F11 toggles fullscreen.
//! Everything funnels through [`ShellProvider`](strake_traits::shell::ShellProvider),
//! so headless shells simply ignore it.

use markup5ever::LocalName;
use strake_traits::node_id::NodeId;
use strake_traits::shell::ResizeDirection;

use crate::BaseDocument;

/// Width of the resize bands along undecorated window edges, in CSS px.
pub(crate) const TITLEBAR_RESIZE_BAND: f32 = 6.0;

/// A window-chrome gesture decoded from a press on page content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TitlebarAction {
    Move,
    Resize(ResizeDirection),
    Minimize,
    MaximizeToggle,
    FullscreenToggle,
    Close,
}

fn data_attr(name: &str) -> LocalName {
    LocalName::from(name)
}

/// Resize direction for client (unscrolled) coordinates, or `None` when
/// resizing does not apply: decorated windows and maximized/fullscreen
/// windows cannot be resized.
pub(crate) fn titlebar_resize_at_client(
    doc: &BaseDocument,
    client_x: f32,
    client_y: f32,
) -> Option<ResizeDirection> {
    if doc.shell_provider.is_window_decorated() {
        return None;
    }
    if doc.shell_provider.is_window_maximized() || doc.shell_provider.is_window_fullscreen() {
        return None;
    }
    let (w, h) = doc.viewport().logical_size();
    let b = TITLEBAR_RESIZE_BAND;
    let west = client_x < b;
    let east = client_x > w - b;
    let north = client_y < b;
    let south = client_y > h - b;
    match (north, south, west, east) {
        (true, false, true, false) => Some(ResizeDirection::NorthWest),
        (true, false, false, true) => Some(ResizeDirection::NorthEast),
        (false, true, true, false) => Some(ResizeDirection::SouthWest),
        (false, true, false, true) => Some(ResizeDirection::SouthEast),
        (true, false, false, false) => Some(ResizeDirection::North),
        (false, true, false, false) => Some(ResizeDirection::South),
        (false, false, true, false) => Some(ResizeDirection::West),
        (false, false, false, true) => Some(ResizeDirection::East),
        _ => None,
    }
}

/// Resize direction for page coordinates (translates through the scroll
/// offset into client coordinates).
pub(crate) fn titlebar_resize_at(doc: &BaseDocument, x: f32, y: f32) -> Option<ResizeDirection> {
    let scroll = doc.viewport_scroll();
    titlebar_resize_at_client(doc, x - scroll.x as f32, y - scroll.y as f32)
}

/// Window-control marker on `node_id` itself, if any. Controls carry no
/// focus veto: the trigger is itself a button.
fn control_at(doc: &BaseDocument, mut node_id: Option<NodeId>) -> Option<TitlebarAction> {
    while let Some(id) = node_id {
        let node = doc.nodes.get(id)?;
        if let Some(el) = node.element_data() {
            if el.attr(data_attr("data-close")).is_some() {
                return Some(TitlebarAction::Close);
            }
            if el.attr(data_attr("data-minimize")).is_some() {
                return Some(TitlebarAction::Minimize);
            }
            if el.attr(data_attr("data-maximize")).is_some() {
                return Some(TitlebarAction::MaximizeToggle);
            }
            if el.attr(data_attr("data-fullscreen")).is_some() {
                return Some(TitlebarAction::FullscreenToggle);
            }
        }
        node_id = node.parent;
    }
    None
}

/// Decode a main-button press into a titlebar action (issue #54): window
/// controls win, then undecorated edge resize, then `data-drag` moves. A
/// focusable hit or `data-nodrag` vetoes the move so controls and inputs
/// inside a drag region keep working. Moves and resizes are suppressed on
/// maximized/fullscreen windows.
pub(crate) fn titlebar_action_for_press(
    doc: &BaseDocument,
    hit_node_id: NodeId,
    x: f32,
    y: f32,
) -> Option<TitlebarAction> {
    if let Some(action) = control_at(doc, Some(hit_node_id)) {
        return Some(action);
    }
    // Moves and resizes only exist on undecorated windows; maximized and
    // fullscreen windows can do neither.
    if doc.shell_provider.is_window_decorated() {
        return None;
    }
    if doc.shell_provider.is_window_maximized() || doc.shell_provider.is_window_fullscreen() {
        return None;
    }
    if let Some(direction) = titlebar_resize_at(doc, x, y) {
        return Some(TitlebarAction::Resize(direction));
    }
    let hit_element = doc.nearest_non_anonymous_ancestor(hit_node_id)?;
    {
        let node = doc.nodes.get(hit_element)?;
        let element = node.element_data()?;
        if node.is_focussable() || element.attr(data_attr("data-nodrag")).is_some() {
            return None;
        }
    }
    // The hit element itself participates: a bare `data-drag` region with
    // no inner content still starts a move (it passed the veto above).
    let mut node_id = Some(hit_element);
    while let Some(id) = node_id {
        let node = doc.nodes.get(id)?;
        if let Some(element) = node.element_data() {
            if element.attr(data_attr("data-nodrag")).is_some() {
                return None;
            }
            if element.attr(data_attr("data-drag")).is_some() {
                return Some(TitlebarAction::Move);
            }
        }
        node_id = node.parent;
    }
    None
}

/// Carry out a titlebar action through the shell provider.
pub(crate) fn perform_titlebar_action(doc: &mut BaseDocument, action: TitlebarAction) {
    match action {
        TitlebarAction::Move => doc.shell_provider.drag_window(),
        TitlebarAction::Resize(direction) => doc.shell_provider.drag_resize_window(direction),
        TitlebarAction::Minimize => doc.shell_provider.set_window_minimized(true),
        TitlebarAction::MaximizeToggle => {
            let maximized = doc.shell_provider.is_window_maximized();
            doc.shell_provider.set_window_maximized(!maximized);
        }
        TitlebarAction::FullscreenToggle => {
            let fullscreen = doc.shell_provider.is_window_fullscreen();
            doc.shell_provider.set_fullscreen(!fullscreen);
        }
        TitlebarAction::Close => doc.shell_provider.request_window_close(),
    }
    doc.shell_provider.request_redraw();
}
