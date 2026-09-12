//! Test doubles for the fallback supervisor: scriptable stand-ins for the
//! headless-Chromium backend so lifecycle and eviction policy are verifiable
//! without a browser binary.

use super::{CpuFrame, FallbackBackend, SpawnSpec};

/// Scriptable [`FallbackBackend`] for tests and embedder development.
///
/// Records every lifecycle call; optionally fails the next spawn or serves a
/// canned [`CpuFrame`].
#[derive(Debug, Default)]
pub struct FakeBackend {
    /// Successful `spawn` calls.
    pub spawns: u32,
    /// `wake` calls.
    pub wakes: u32,
    /// `hibernate` calls.
    pub hibernates: u32,
    /// `terminate` calls.
    pub terminates: u32,
    /// Whether a worker is currently considered live.
    pub live: bool,
    /// When set, the next `spawn` fails with this message instead.
    pub fail_next_spawn: Option<String>,
    /// Frame served by [`FakeBackend::frame`] while live.
    pub next_frame: Option<CpuFrame>,
}

impl FakeBackend {
    /// A backend that spawns successfully and serves no frames.
    pub fn new() -> Self {
        Self::default()
    }

    /// A backend whose next spawn fails with `message`.
    pub fn failing(message: String) -> Self {
        Self {
            fail_next_spawn: Some(message),
            ..Self::default()
        }
    }

    /// A backend serving `frame` on every [`FakeBackend::frame`] call while live.
    pub fn with_frame(frame: CpuFrame) -> Self {
        Self {
            next_frame: Some(frame),
            ..Self::default()
        }
    }
}

impl FallbackBackend for FakeBackend {
    fn spawn(&mut self, _spec: &SpawnSpec) -> Result<(), String> {
        if let Some(message) = self.fail_next_spawn.take() {
            return Err(message);
        }
        self.live = true;
        self.spawns += 1;
        Ok(())
    }

    fn wake(&mut self) {
        self.live = true;
        self.wakes += 1;
    }

    fn hibernate(&mut self) {
        // A hibernated worker holds no fresh frames: drop the cached bitmap so
        // `frame` serves nothing until a fresh post-wake frame arrives (per
        // the `FallbackBackend::frame` freshness contract).
        self.live = false;
        self.next_frame = None;
        self.hibernates += 1;
    }

    fn terminate(&mut self) {
        self.live = false;
        self.next_frame = None;
        self.terminates += 1;
    }

    fn frame(&mut self) -> Option<CpuFrame> {
        if self.live {
            self.next_frame.clone()
        } else {
            None
        }
    }
}
