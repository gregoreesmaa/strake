//! Fallback worker supervision: lazy spawn plus hibernate/terminate eviction.
//!
//! The worker is a single process serving every live fallback surface.
//! Policy (issue #5): spawn on first demand only; hibernate once every
//! surface has been occluded for [`HIBERNATE_AFTER`]; terminate once zero
//! surfaces have been live for [`TERMINATE_AFTER`]. All time is caller
//! provided ([`Duration`]) so the policy is deterministic under test.

use std::collections::HashMap;
use std::time::Duration;

use super::{CpuFrame, FallbackBackend, SpawnSpec, SupervisorError};

/// Occlusion/idle time after which an active worker hibernates.
pub const HIBERNATE_AFTER: Duration = Duration::from_secs(15);
/// Zero-surface time after which the worker terminates completely.
pub const TERMINATE_AFTER: Duration = Duration::from_secs(30);

/// DevTools diagnostic emitted whenever the fallback worker engages.
fn spawn_warning(url: &str) -> String {
    format!(
        "[Strake Fallback] Spawning headless Chromium worker for {url}. \
         Migrate to native Strake API to preserve <30MB memory profile."
    )
}

/// Worker process lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerState {
    /// No worker exists: zero RAM, zero CPU, zero cold-start cost.
    Unloaded,
    /// Worker is rendering live surfaces.
    Active,
    /// Worker exists but purged its caches; holds no fresh frames.
    Hibernated,
}

struct Surface {
    visible: bool,
}

/// Owns one fallback worker process behind [`FallbackBackend`].
///
/// Construct with [`Supervisor::new`] (spawns nothing), register fallback
/// surfaces as unhandled elements mount, and call [`Supervisor::poll`] on a
/// timer tick. Embedders pull frames via [`Supervisor::pump_frame`] and push
/// them into [`crate::FallbackWidget`].
pub struct Supervisor<B> {
    backend: B,
    state: WorkerState,
    surfaces: HashMap<u64, Surface>,
    next_surface_id: u64,
    /// When the worker last had any visible surface (`None` while visible).
    occluded_since: Option<Duration>,
    /// When the last surface was removed (`None` while any surface lives).
    emptied_at: Option<Duration>,
    warnings: Vec<String>,
}

impl<B: FallbackBackend> Supervisor<B> {
    /// A supervisor with no worker and no surfaces. Performs no I/O.
    pub fn new(backend: B) -> Self {
        Self {
            backend,
            state: WorkerState::Unloaded,
            surfaces: HashMap::new(),
            next_surface_id: 0,
            occluded_since: None,
            emptied_at: None,
            warnings: Vec::new(),
        }
    }

    /// Current worker lifecycle state.
    pub fn worker_state(&self) -> WorkerState {
        self.state
    }

    /// Number of live fallback surfaces.
    pub fn surface_count(&self) -> usize {
        self.surfaces.len()
    }

    /// The driven backend (frame pulling, test assertions).
    pub fn backend(&self) -> &B {
        &self.backend
    }

    /// Drain pending DevTools diagnostics.
    pub fn take_warnings(&mut self) -> Vec<String> {
        std::mem::take(&mut self.warnings)
    }

    /// Register a fallback surface (e.g. an unhandled element mounted).
    /// Spawns (or wakes) the worker on first demand. New surfaces start visible.
    pub fn add_surface(&mut self, spec: SpawnSpec, _now: Duration) -> Result<u64, SupervisorError> {
        match self.state {
            WorkerState::Unloaded => {
                self.backend
                    .spawn(&spec)
                    .map_err(SupervisorError::SpawnFailed)?;
                self.state = WorkerState::Active;
                self.warnings.push(spawn_warning(&spec.url));
            }
            WorkerState::Hibernated => {
                self.backend.wake();
                self.state = WorkerState::Active;
            }
            WorkerState::Active => {}
        }
        let id = self.next_surface_id;
        self.next_surface_id += 1;
        self.surfaces.insert(id, Surface { visible: true });
        self.occluded_since = None;
        self.emptied_at = None;
        Ok(id)
    }

    /// Unregister a surface. `false` for an unknown id (no-op otherwise).
    pub fn remove_surface(&mut self, id: u64, now: Duration) -> bool {
        if self.surfaces.remove(&id).is_none() {
            return false;
        }
        if self.surfaces.is_empty() {
            self.emptied_at = Some(now);
            self.occluded_since = None;
        } else if !self.surfaces.values().any(|surface| surface.visible) {
            self.occluded_since.get_or_insert(now);
        }
        true
    }

    /// Report surface visibility (viewport intersection). A re-visible
    /// surface wakes a hibernated worker immediately. `false` for unknown ids.
    pub fn set_visible(&mut self, id: u64, visible: bool, now: Duration) -> bool {
        let Some(surface) = self.surfaces.get_mut(&id) else {
            return false;
        };
        surface.visible = visible;
        if visible {
            self.occluded_since = None;
            if self.state == WorkerState::Hibernated {
                self.backend.wake();
                self.state = WorkerState::Active;
            }
        } else if !self.surfaces.values().any(|surface| surface.visible) {
            self.occluded_since.get_or_insert(now);
        }
        true
    }

    /// Run the eviction watchdog at `now`: hibernate a fully occluded worker,
    /// terminate a worker with no surfaces left.
    pub fn poll(&mut self, now: Duration) {
        if self.surfaces.is_empty() {
            if self.state != WorkerState::Unloaded
                && let Some(emptied) = self.emptied_at
                && now.saturating_sub(emptied) >= TERMINATE_AFTER
            {
                self.backend.terminate();
                self.state = WorkerState::Unloaded;
                self.emptied_at = None;
                self.warnings.push(String::from(
                    "[Strake Fallback] Terminating idle worker; memory reclaimed to native baseline.",
                ));
            }
            return;
        }
        if self.state == WorkerState::Active
            && let Some(occluded) = self.occluded_since
            && now.saturating_sub(occluded) >= HIBERNATE_AFTER
        {
            self.backend.hibernate();
            self.state = WorkerState::Hibernated;
        }
    }

    /// Latest worker frame, or `None` unless the worker is [`WorkerState::Active`].
    pub fn pump_frame(&mut self) -> Option<CpuFrame> {
        if self.state != WorkerState::Active {
            return None;
        }
        self.backend.frame()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::testing::FakeBackend;

    fn spec() -> SpawnSpec {
        SpawnSpec {
            url: String::from("https://example.com/call"),
            width: 800,
            height: 600,
            scale: 1.0,
        }
    }

    fn supervisor() -> Supervisor<FakeBackend> {
        Supervisor::new(FakeBackend::new())
    }

    #[test]
    fn new_supervisor_spawns_nothing() {
        let sim = supervisor();
        assert_eq!(sim.worker_state(), WorkerState::Unloaded);
        assert_eq!(sim.surface_count(), 0);
        assert_eq!(
            sim.backend().spawns,
            0,
            "zero cost at rest: no worker at boot"
        );
    }

    #[test]
    fn first_surface_spawns_worker_lazily() {
        let mut sim = supervisor();
        let t = Duration::from_secs(100);
        let id = sim.add_surface(spec(), t).expect("spawn must succeed");
        assert_eq!(sim.worker_state(), WorkerState::Active);
        assert_eq!(sim.surface_count(), 1);
        assert_eq!(sim.backend().spawns, 1);
        let warnings = sim.take_warnings();
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0].contains("https://example.com/call"),
            "DevTools warning names the fallback target, got: {}",
            warnings[0]
        );
        assert!(sim.take_warnings().is_empty(), "warnings drain");
        let _ = id;
    }

    #[test]
    fn second_surface_reuses_worker() {
        let mut sim = supervisor();
        let t = Duration::from_secs(0);
        sim.add_surface(spec(), t).unwrap();
        sim.add_surface(spec(), t).unwrap();
        assert_eq!(sim.backend().spawns, 1, "one worker serves all surfaces");
        assert_eq!(sim.surface_count(), 2);
    }

    #[test]
    fn spawn_failure_reports_error_and_adds_nothing() {
        let mut sim = Supervisor::new(FakeBackend::failing(String::from("no chromium")));
        let err = sim
            .add_surface(spec(), Duration::from_secs(0))
            .expect_err("spawn failure must surface");
        assert_eq!(
            err,
            SupervisorError::SpawnFailed(String::from("no chromium"))
        );
        assert_eq!(sim.surface_count(), 0);
        assert_eq!(sim.worker_state(), WorkerState::Unloaded);
    }

    #[test]
    fn occluded_worker_hibernates_after_timeout() {
        let mut sim = supervisor();
        let t0 = Duration::from_secs(0);
        let id = sim.add_surface(spec(), t0).unwrap();
        assert!(sim.set_visible(id, false, t0));
        sim.poll(t0 + HIBERNATE_AFTER - Duration::from_millis(1));
        assert_eq!(sim.worker_state(), WorkerState::Active);
        sim.poll(t0 + HIBERNATE_AFTER);
        assert_eq!(sim.worker_state(), WorkerState::Hibernated);
        assert_eq!(sim.backend().hibernates, 1);
    }

    #[test]
    fn visible_worker_never_hibernates() {
        let mut sim = supervisor();
        sim.add_surface(spec(), Duration::from_secs(0)).unwrap();
        sim.poll(Duration::from_secs(3600));
        assert_eq!(sim.worker_state(), WorkerState::Active);
        assert_eq!(sim.backend().hibernates, 0);
    }

    #[test]
    fn revisibled_hibernated_worker_wakes_without_respawn() {
        let mut sim = supervisor();
        let id = sim.add_surface(spec(), Duration::from_secs(0)).unwrap();
        sim.set_visible(id, false, Duration::from_secs(0));
        sim.poll(HIBERNATE_AFTER);
        assert_eq!(sim.worker_state(), WorkerState::Hibernated);
        assert!(sim.set_visible(id, true, HIBERNATE_AFTER));
        assert_eq!(sim.worker_state(), WorkerState::Active);
        assert_eq!(sim.backend().wakes, 1);
        assert_eq!(
            sim.backend().spawns,
            1,
            "wake reuses the worker, no respawn"
        );
    }

    #[test]
    fn removing_last_surface_terminates_after_timeout() {
        let mut sim = supervisor();
        let id = sim.add_surface(spec(), Duration::from_secs(0)).unwrap();
        assert!(sim.remove_surface(id, Duration::from_secs(10)));
        assert_eq!(sim.surface_count(), 0);
        sim.poll(Duration::from_secs(10) + TERMINATE_AFTER - Duration::from_millis(1));
        assert_eq!(sim.worker_state(), WorkerState::Active);
        sim.poll(Duration::from_secs(10) + TERMINATE_AFTER);
        assert_eq!(sim.worker_state(), WorkerState::Unloaded);
        assert_eq!(sim.backend().terminates, 1);
    }

    #[test]
    fn demand_after_terminate_respawns() {
        let mut sim = supervisor();
        let id = sim.add_surface(spec(), Duration::from_secs(0)).unwrap();
        sim.remove_surface(id, Duration::from_secs(0));
        sim.poll(TERMINATE_AFTER);
        assert_eq!(sim.worker_state(), WorkerState::Unloaded);
        sim.add_surface(spec(), TERMINATE_AFTER).unwrap();
        assert_eq!(sim.backend().spawns, 2);
        assert_eq!(sim.worker_state(), WorkerState::Active);
    }

    #[test]
    fn unknown_surface_ops_fail_softly() {
        let mut sim = supervisor();
        assert!(!sim.remove_surface(999, Duration::from_secs(0)));
        assert!(!sim.set_visible(999, true, Duration::from_secs(0)));
        assert_eq!(sim.backend().spawns, 0);
    }

    #[test]
    fn pump_frame_only_flows_while_active() {
        let red = CpuFrame::solid(4, 4, [255, 0, 0, 255]);
        let mut sim = Supervisor::new(FakeBackend::with_frame(red.clone()));
        assert_eq!(sim.pump_frame(), None, "no worker, no frames");
        let id = sim.add_surface(spec(), Duration::from_secs(0)).unwrap();
        assert_eq!(sim.pump_frame(), Some(red.clone()));
        sim.set_visible(id, false, Duration::from_secs(0));
        sim.poll(HIBERNATE_AFTER);
        assert_eq!(
            sim.pump_frame(),
            None,
            "hibernated worker holds no fresh frames"
        );
    }
}
