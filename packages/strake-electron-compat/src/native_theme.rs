//! Headless `nativeTheme` shim (issue #21 step 2).
//!
//! `nativeTheme.shouldUseDarkColors` / `setThemeSource` /
//! `on('updated')` need a testable state machine before any OS theme
//! bridge exists: an explicit [`ThemeSource`] (`system` / `light` /
//! `dark`, matching Electron), a derived dark-colors bit, and an
//! `updated` listener list the runtime fires when the OS scheme flips.
//! Production updates (dark-mode notifications on macOS/Windows/Linux,
//! per #12's theme bullet) drive [`NativeTheme::note_system_change`] in a
//! follow-up; CI and headless tests flip the scheme through that same
//! entry point so listener dispatch stays fully testable without an OS.

use std::sync::{Arc, Mutex};

/// `nativeTheme.themeSource` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeSource {
    /// Follow the OS scheme.
    #[default]
    System,
    /// Force light.
    Light,
    /// Force dark.
    Dark,
}

/// Listener id for `nativeTheme.on('updated')`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ThemeListenerId(pub u64);

/// An `updated` listener: receives the new `shouldUseDarkColors` bit.
type UpdatedCallback = Arc<dyn Fn(bool) + Send + Sync>;

#[derive(Default)]
struct ThemeState {
    source: ThemeSource,
    system_dark: bool,
    next_listener: u64,
    listeners: Vec<(ThemeListenerId, UpdatedCallback)>,
}

/// Renderer-facing theme hub (`nativeTheme` in main and renderer).
#[derive(Clone, Default)]
pub struct NativeTheme {
    state: Arc<Mutex<ThemeState>>,
}

impl std::fmt::Debug for NativeTheme {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.state.lock().expect("theme mutex");
        formatter
            .debug_struct("NativeTheme")
            .field("source", &state.source)
            .field("should_use_dark_colors", &self.dark_locked(&state))
            .field("listeners", &state.listeners.len())
            .finish()
    }
}

impl NativeTheme {
    /// A hub following the OS scheme (`system_dark` is the current OS bit;
    /// headless tests inject it, the runtime reads the live seat).
    pub fn new(system_dark: bool) -> Self {
        Self {
            state: Arc::new(Mutex::new(ThemeState {
                system_dark,
                ..ThemeState::default()
            })),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ThemeState> {
        self.state.lock().expect("theme mutex")
    }

    fn dark_locked(&self, state: &ThemeState) -> bool {
        match state.source {
            ThemeSource::System => state.system_dark,
            ThemeSource::Light => false,
            ThemeSource::Dark => true,
        }
    }

    /// `nativeTheme.shouldUseDarkColors`.
    pub fn should_use_dark_colors(&self) -> bool {
        let state = self.lock();
        self.dark_locked(&state)
    }

    /// `nativeTheme.themeSource`.
    pub fn theme_source(&self) -> ThemeSource {
        self.lock().source
    }

    /// `nativeTheme.themeSource = ...` (fires `updated` when the derived
    /// bit flips).
    pub fn set_theme_source(&self, source: ThemeSource) {
        let pending = {
            let mut state = self.lock();
            let before = self.dark_locked(&state);
            state.source = source;
            let after = self.dark_locked(&state);
            Self::pending_locked(&state, before, after)
        };
        Self::fire(pending);
    }

    /// The OS scheme flipped (runtime entry point; headless tests drive
    /// the same path). Fires `updated` only when following `system` and
    /// the derived bit actually changes.
    pub fn note_system_change(&self, system_dark: bool) {
        let pending = {
            let mut state = self.lock();
            let before = self.dark_locked(&state);
            state.system_dark = system_dark;
            let after = self.dark_locked(&state);
            Self::pending_locked(&state, before, after)
        };
        Self::fire(pending);
    }

    /// `nativeTheme.on('updated', listener)`; the listener receives the new
    /// `shouldUseDarkColors` bit.
    pub fn on_updated(&self, listener: impl Fn(bool) + Send + Sync + 'static) -> ThemeListenerId {
        let mut state = self.lock();
        let id = ThemeListenerId(state.next_listener);
        state.next_listener += 1;
        state.listeners.push((id, Arc::new(listener)));
        id
    }

    /// `removeListener('updated', id)` (`true` when the id existed).
    pub fn remove_updated_listener(&self, id: ThemeListenerId) -> bool {
        let mut state = self.lock();
        let before = state.listeners.len();
        state.listeners.retain(|(known, _)| *known != id);
        state.listeners.len() != before
    }

    /// Snapshot the listeners to fire (cloned `Arc`s so dispatch runs
    /// after the mutex is released — a listener may re-enter the hub).
    /// `None` when the derived bit did not flip.
    fn pending_locked(
        state: &ThemeState,
        before: bool,
        after: bool,
    ) -> Option<(bool, Vec<UpdatedCallback>)> {
        if before == after {
            return None;
        }
        Some((
            after,
            state
                .listeners
                .iter()
                .map(|(_, listener)| Arc::clone(listener))
                .collect(),
        ))
    }

    fn fire(pending: Option<(bool, Vec<UpdatedCallback>)>) {
        if let Some((dark, listeners)) = pending {
            for listener in listeners {
                listener(dark);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn system_source_follows_os_bit() {
        let theme = NativeTheme::new(false);
        assert_eq!(theme.theme_source(), ThemeSource::System);
        assert!(!theme.should_use_dark_colors());
        theme.note_system_change(true);
        assert!(theme.should_use_dark_colors());
    }

    #[test]
    fn explicit_source_overrides_system() {
        let theme = NativeTheme::new(true);
        theme.set_theme_source(ThemeSource::Light);
        assert!(!theme.should_use_dark_colors());
        theme.note_system_change(false);
        assert!(!theme.should_use_dark_colors(), "light pins the bit");
        theme.set_theme_source(ThemeSource::Dark);
        assert!(theme.should_use_dark_colors());
    }

    #[test]
    fn updated_fires_only_on_flip() {
        let theme = NativeTheme::new(false);
        let fired = Arc::new(AtomicBool::new(false));
        let probe = Arc::clone(&fired);
        theme.on_updated(move |dark| probe.store(dark, Ordering::SeqCst));
        theme.note_system_change(false);
        assert!(!fired.load(Ordering::SeqCst), "no flip, no event");
        theme.note_system_change(true);
        assert!(fired.load(Ordering::SeqCst), "flip dispatches updated");
    }

    #[test]
    fn removed_listener_stays_silent() {
        let theme = NativeTheme::new(false);
        let fired = Arc::new(AtomicBool::new(false));
        let probe = Arc::clone(&fired);
        let id = theme.on_updated(move |_| probe.store(true, Ordering::SeqCst));
        assert!(theme.remove_updated_listener(id));
        assert!(!theme.remove_updated_listener(id), "double remove fails");
        theme.note_system_change(true);
        assert!(!fired.load(Ordering::SeqCst));
    }
}
